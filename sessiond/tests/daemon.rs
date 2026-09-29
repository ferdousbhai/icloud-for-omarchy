//! End-to-end tests: icloud-sessiond D-Bus activated on a private
//! dbus-daemon, a local HTTP server standing in for Apple, and HOME and the
//! XDG dirs in a temp dir. Nothing touches the real session bus, ~/.config,
//! ~/.local or Apple.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use icloud_session::{BUS_NAME, ERROR_SIGN_IN_REQUIRED, Error, INTERFACE, OBJECT_PATH, Session, SessionReply};
use serde_json::{Value, json};
use zbus::blocking::Connection;

const DSID: &str = "12345";
const DAEMON: &str = env!("CARGO_BIN_EXE_icloud-sessiond");
const CLI: &str = env!("CARGO_BIN_EXE_icloud-session");

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(25));
    }
}

// ---------------------------------------------------------------- server

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn path(&self) -> &str {
        self.url.split('?').next().unwrap()
    }
    fn query(&self) -> HashMap<String, String> {
        url::Url::parse(&format!("http://x{}", self.url))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }
}

struct Reply {
    status: u16,
    body: String,
    set_cookies: Vec<String>,
}

impl Reply {
    fn json(status: u16, body: Value) -> Reply {
        Reply {
            status,
            body: body.to_string(),
            set_cookies: vec![],
        }
    }
    fn cookie(mut self, c: &str) -> Reply {
        self.set_cookies.push(c.to_string());
        self
    }
}

type Handler = dyn Fn(&Seen, usize, &str) -> Reply + Send + Sync;

struct Server {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    /// `handler(request, n, base_url)`, n = how many requests to this path so far.
    fn start(handler: impl Fn(&Seen, usize, &str) -> Reply + Send + Sync + 'static) -> Server {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let (seen2, base) = (seen.clone(), url.clone());
        thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                let entry = Seen {
                    method: request.method().to_string(),
                    url: request.url().to_string(),
                    headers: request
                        .headers()
                        .iter()
                        .map(|h| (h.field.to_string(), h.value.to_string()))
                        .collect(),
                    body,
                };
                let n = {
                    let mut seen = seen2.lock().unwrap();
                    seen.push(entry.clone());
                    seen.iter().filter(|s| s.path() == entry.path()).count()
                };
                let (handler, base) = (handler.clone(), base.clone());
                thread::spawn(move || {
                    let reply = handler(&entry, n, &base);
                    let mut response = tiny_http::Response::from_string(reply.body).with_status_code(reply.status);
                    for c in reply.set_cookies {
                        response.add_header(tiny_http::Header::from_bytes("Set-Cookie", c).unwrap());
                    }
                    let _ = request.respond(response);
                });
            }
        });
        Server { url, seen }
    }

    fn requests(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path() == path)
            .cloned()
            .collect()
    }

    fn count(&self, path: &str) -> usize {
        self.requests(path).len()
    }
}

const VALIDATE: &str = "/setup/ws/1/validate";

fn validate_ok(n: usize, base: &str) -> Reply {
    Reply::json(
        200,
        json!({
            "dsInfo": {"dsid": DSID, "appleId": "someone@example.com", "fullName": "Some One"},
            "webservices": {
                "ckdatabasews": {"url": base, "status": "active"},
                "findme": {"url": findme_url(base), "status": "active"},
                "broken": {"status": "active"}
            }
        }),
    )
    .cookie(&format!(
        "X-APPLE-WEBAUTH-TOKEN=rotated{n}; Domain=.icloud.com; Path=/; Max-Age=2592000; Secure; HttpOnly"
    ))
}

/// Find My's host: the same test server under another name, so requests
/// to it are told apart from the other services' (they carry the Find My
/// jar).
fn findme_url(base: &str) -> String {
    base.replace("127.0.0.1", "localhost")
}

fn signed_out() -> Reply {
    Reply::json(421, json!({"error": "Misdirected Request"}))
}

// ---------------------------------------------------------------- fixture

struct Env {
    dir: tempfile::TempDir,
    bus: Child,
    address: String,
}

struct Opts<'a> {
    setup_url: &'a str,
    signin: Option<&'a str>,
    idle_secs: f64,
    validate_secs: f64,
    retry_secs: f64,
    seed: bool,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Opts {
            setup_url: "http://127.0.0.1:9",
            signin: None,
            idle_secs: 60.0,
            validate_secs: 600.0,
            retry_secs: 60.0,
            seed: true,
        }
    }
}

fn seed_account(token_expires: u64) -> Value {
    json!({
        "apple_id": "someone@example.com",
        "dsid": DSID,
        "cookies": [
            {"name": "X-APPLE-WEBAUTH-USER", "value": "\"v=1:s=0:d=12345\"", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "original", "domain": ".icloud.com", "path": "/", "expires": token_expires},
        ],
        "client_params": {"clientBuildNumber": "2624Build27", "clientId": "auth-client-id", "clientMasteringNumber": "2624Build27M"},
        "webservices": {},
        "validated_at": 0,
        "captured_at": "2026-09-01T00:00:00.000Z",
    })
}

impl Env {
    fn start(opts: Opts<'_>) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["home", "state", "data", "cache", "config", "services"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(
            root.join("services/io.github.ferdousbhai.ICloudSession.service"),
            format!("[D-BUS Service]\nName={BUS_NAME}\nExec={DAEMON}\n"),
        )
        .unwrap();
        fs::write(
            root.join("bus.conf"),
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <servicedir>{}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                root.join("services").display()
            ),
        )
        .unwrap();
        if opts.seed {
            let path = root.join("state/icloud-session/account.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, seed_account(now() + 30 * 86400).to_string()).unwrap();
        }
        let signin = opts
            .signin
            .map_or_else(|| "/nonexistent-signin".into(), |s| s.to_string());
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", root.join("bus.conf").display()))
            .args(["--nofork", "--print-address=1"])
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("ICLOUD_SESSION_SETUP_URL", opts.setup_url)
            .env("ICLOUD_SESSION_SIGNIN_BIN", signin)
            .env("ICLOUD_SESSIOND_IDLE_SECS", opts.idle_secs.to_string())
            .env("ICLOUD_SESSIOND_VALIDATE_SECS", opts.validate_secs.to_string())
            .env("ICLOUD_SESSIOND_RETRY_SECS", opts.retry_secs.to_string())
            // Never the real Secret Service: a file in the temp dir.
            .env("ICLOUD_SESSION_TEST_SECRET_FILE", root.join("secrets.json"))
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon runs");
        let mut line = String::new();
        BufReader::new(bus.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let address = line.trim().to_string();
        assert!(address.starts_with("unix:"), "{address}");
        Env { dir, bus, address }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn conn(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .unwrap()
    }

    fn account_path(&self) -> PathBuf {
        self.root().join("state/icloud-session/account.json")
    }

    fn account(&self) -> Option<Value> {
        fs::read(self.account_path())
            .ok()
            .map(|b| serde_json::from_slice(&b).unwrap())
    }

    fn mirror_dir(&self) -> PathBuf {
        self.root().join("home/.config/icloud-md/accounts").join(DSID)
    }

    fn mirror(&self) -> Option<Value> {
        fs::read(self.mirror_dir().join("session.local.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(CLI)
            .args(args)
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .output()
            .unwrap()
    }

    /// The CLI with `stdin` piped in (not a terminal) and `env` set.
    fn cli_with(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> std::process::Output {
        use std::io::Write;
        let mut child = Command::new(CLI)
            .args(args)
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn secrets_path(&self) -> PathBuf {
        self.root().join("secrets.json")
    }

    /// The test keyring's items.
    fn secrets(&self) -> Value {
        fs::read(self.secrets_path())
            .ok()
            .map_or(json!([]), |b| serde_json::from_slice(&b).unwrap())
    }

    /// Puts a password in the test keyring, as `set-password` would.
    fn store_password(&self, password: &str) {
        let items = json!([{
            "label": "iCloud (icloud-session): someone@example.com",
            "attributes": {"application": "icloud-session", "apple-id": "someone@example.com"},
            "secret": password,
        }]);
        fs::write(self.secrets_path(), items.to_string()).unwrap();
    }

    /// Gives the seeded account a Find My jar Find My no longer accepts.
    fn seed_stale_find_my(&self) {
        let mut account = self.account().unwrap();
        account["find_my"] = json!({
            "cookies": [
                {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "onefactor-stale", "domain": ".icloud.com", "path": "/", "expires": null},
                {"name": "X-APPLE-WEBAUTH-FMIP", "value": "stale", "domain": ".icloud.com", "path": "/", "expires": null},
            ],
            "client_params": {"clientBuildNumber": "2624Build27", "clientId": "stale-client-id", "clientMasteringNumber": "2624Build27M"},
            "captured_at": "2026-09-01T00:00:00.000Z",
        });
        fs::write(self.account_path(), account.to_string()).unwrap();
    }

    /// SIGKILLs the running daemon and waits until it has left the bus.
    fn kill_daemon(&self, conn: &Connection) {
        let dbus = zbus::blocking::fdo::DBusProxy::new(conn).unwrap();
        let pid = dbus
            .get_connection_unix_process_id(BUS_NAME.try_into().unwrap())
            .unwrap();
        assert!(
            Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        wait_until("the daemon to die", Duration::from_secs(5), || {
            !Path::new(&format!("/proc/{pid}")).exists() || !self.daemon_running(conn)
        });
    }

    fn daemon_running(&self, conn: &Connection) -> bool {
        zbus::blocking::fdo::DBusProxy::new(conn)
            .unwrap()
            .name_has_owner(BUS_NAME.try_into().unwrap())
            .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // The daemon loses its bus and exits with it.
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

fn call<R: serde::de::DeserializeOwned + zbus::zvariant::Type>(conn: &Connection, method: &str) -> zbus::Result<R> {
    let reply = conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, &())?;
    reply.body().deserialize()
}

fn session(conn: &Connection) -> zbus::Result<SessionReply> {
    call(conn, "Session")
}

fn prop<T: TryFrom<zbus::zvariant::OwnedValue>>(conn: &Connection, name: &str) -> T
where
    T::Error: std::fmt::Debug,
{
    let reply = conn
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(INTERFACE, name),
        )
        .unwrap();
    let v: zbus::zvariant::OwnedValue = reply.body().deserialize().unwrap();
    T::try_from(v).unwrap()
}

fn cookie_of(header: &str, name: &str) -> Option<String> {
    header
        .split("; ")
        .find_map(|p| p.strip_prefix(&format!("{name}=")).map(str::to_string))
}

fn write_script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

// ----------------------------------------------------------------- tests

#[test]
fn session_returns_cookie_params_webservices_and_revalidates_when_stale() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        validate_secs: 1.0,
        ..Default::default()
    });
    let conn = env.conn();
    let (cookie, params, webservices) = session(&conn).unwrap();
    assert_eq!(
        server.count(VALIDATE),
        1,
        "one validate, however start and Session() race"
    );
    assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("rotated1"));
    assert_eq!(
        cookie_of(&cookie, "X-APPLE-WEBAUTH-USER").as_deref(),
        Some("\"v=1:s=0:d=12345\"")
    );
    assert_eq!(params["clientId"], "auth-client-id");
    assert_eq!(params["clientBuildNumber"], "2624Build27");
    assert_eq!(params["clientMasteringNumber"], "2624Build27M");
    assert_eq!(webservices["findme"], findme_url(&server.url));
    assert!(!webservices.contains_key("broken"));

    // The request is icloud-md's checkAuthentication.
    let v = &server.requests(VALIDATE)[0];
    assert_eq!(v.method, "POST");
    let q = v.query();
    assert_eq!(q["clientBuildNumber"], "2624Build27");
    assert_eq!(q["clientMasteringNumber"], "2624Build27M");
    assert_eq!(q["clientId"], "auth-client-id");
    assert_eq!(q["dsid"], DSID);
    assert_eq!(q["requestId"].len(), 36);
    assert_eq!(v.header("Origin"), Some("https://www.icloud.com"));
    assert_eq!(v.header("Referer"), Some("https://www.icloud.com/"));
    assert_eq!(v.header("Accept"), Some("application/json"));
    assert!(v.header("Cookie").unwrap().contains("X-APPLE-WEBAUTH-TOKEN=original"));

    // Persisted, and mirrored for icloud-md.
    let account = env.account().unwrap();
    assert!(account["validated_at"].as_u64().unwrap() >= now() - 5);
    assert_eq!(account["webservices"]["findme"], findme_url(&server.url));
    let mirror = env.mirror().unwrap();
    assert_eq!(mirror["cookie"], cookie);
    let mode = fs::metadata(env.account_path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(prop::<u64>(&conn, "ExpiresAt") >= now() + 2_592_000 - 10);

    // Stale after a second: the next Session() validates again first.
    thread::sleep(Duration::from_millis(2100));
    let (cookie, _, _) = session(&conn).unwrap();
    // (The heartbeat may validate too while this client is active.)
    let token = cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").unwrap();
    let n: usize = token.strip_prefix("rotated").unwrap().parse().unwrap();
    assert!(n >= 2, "{token}");
}

#[test]
fn merge_cookies_merges_by_name_and_persists() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    let expiry = now() + 86400;
    conn.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(INTERFACE),
        "MergeCookies",
        &(vec![
            "X-APPLE-WEBAUTH-TOKEN=merged; Domain=.icloud.com; Path=/; Max-Age=86400; Secure",
            "NEW=1; Path=/",
        ],),
    )
    .unwrap();
    let (cookie, _, _) = session(&conn).unwrap();
    assert_eq!(
        cookie,
        "X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=12345\"; X-APPLE-WEBAUTH-TOKEN=merged; NEW=1"
    );
    let account = env.account().unwrap();
    let names: Vec<&str> = account["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["X-APPLE-WEBAUTH-USER", "X-APPLE-WEBAUTH-TOKEN", "NEW"]);
    let exp = prop::<u64>(&conn, "ExpiresAt");
    assert!(exp.abs_diff(expiry) <= 5, "{exp} vs {expiry}");
    assert_eq!(env.mirror().unwrap()["cookie"], cookie);
    assert_eq!(server.count(VALIDATE), 1);
}

#[test]
fn report_sign_in_required_confirms_with_validate() {
    let expired = Arc::new(AtomicBool::new(false));
    let flag = expired.clone();
    let server = Server::start(move |s, n, base| match s.path() {
        VALIDATE if flag.load(Ordering::SeqCst) => signed_out(),
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();

    // A stray 401: Apple still accepts the session. SignedIn stays, the
    // mirror is rewritten with the fresh jar, the answer is true.
    fs::remove_file(env.mirror_dir().join("session.local.json")).unwrap();
    let still: bool = call(&conn, "ReportSignInRequired").unwrap();
    assert!(still);
    assert!(prop::<bool>(&conn, "SignedIn"));
    assert_eq!(server.count(VALIDATE), 2);
    let mirror = env.mirror().expect("mirror rewritten");
    assert_eq!(
        cookie_of(mirror["cookie"].as_str().unwrap(), "X-APPLE-WEBAUTH-TOKEN").as_deref(),
        Some("rotated2")
    );

    // Now Apple answers 421: signed out, announced, account forgotten.
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    expired.store(true, Ordering::SeqCst);
    let still: bool = call(&conn, "ReportSignInRequired").unwrap();
    assert!(!still);
    let change = watch.next().unwrap();
    assert!(!change.signed_in);
    assert_eq!(change.apple_id, None);
    assert_eq!(change.dsid, None);
    assert_eq!(change.expires_at, None);
    assert!(!prop::<bool>(&conn, "SignedIn"));
    assert_eq!(prop::<String>(&conn, "AppleId"), "");
    assert!(env.account().is_none());
    match session(&conn) {
        Err(zbus::Error::MethodError(name, _, _)) => assert_eq!(name.as_str(), ERROR_SIGN_IN_REQUIRED),
        other => panic!("{other:?}"),
    }
}

#[test]
fn sign_in_with_a_fake_window_stores_the_account() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({
        "cookies": [
            {"name": "X-APPLE-WEBAUTH-USER", "value": "\"v=1:s=0:d=12345\"", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "captured", "domain": ".icloud.com", "path": "/", "expires": now() + 1000},
            {"name": "X-APPLE-DS-WEB-SESSION-TOKEN", "value": "ds", "domain": "setup.icloud.com", "path": "/", "expires": null},
            {"name": "myacinfo", "value": "apple-only", "domain": ".apple.com", "path": "/", "expires": null},
        ],
        "clientId": "page-client-id",
        "clientBuildNumber": "2530Build12",
        "clientMasteringNumber": "2530Hotfix3",
    });
    let signin = write_script(dir.path(), "signin", &format!("sleep 0.3\ncat <<'EOF'\n{capture}\nEOF"));
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    assert_eq!(
        watch.current().unwrap(),
        &icloud_session::Status {
            signed_in: false,
            apple_id: None,
            dsid: None,
            expires_at: None,
            signing_in: false,
            find_my_authorized: false,
            find_my_password_stored: false,
        }
    );
    match session(&conn) {
        Err(zbus::Error::MethodError(name, _, _)) => assert_eq!(name.as_str(), ERROR_SIGN_IN_REQUIRED),
        other => panic!("{other:?}"),
    }

    icloud_session::sign_in_on(&conn).unwrap();
    let opened = watch.next().unwrap();
    assert!(opened.signing_in && !opened.signed_in);
    // A second SignIn while the window is open does not open another.
    icloud_session::sign_in_on(&conn).unwrap();
    let mut done = watch.next().unwrap();
    if done.signing_in {
        done = watch.next().unwrap();
    }
    assert!(done.signed_in && !done.signing_in, "{done:?}");
    assert_eq!(done.apple_id.as_deref(), Some("someone@example.com"));
    assert_eq!(done.dsid.as_deref(), Some(DSID));
    assert!(
        done.expires_at.unwrap() >= now() + 2_592_000 - 10,
        "rotated by validate"
    );

    // The validate used the captured jar (icloud.com cookies only) and params, no dsid yet.
    let v = &server.requests(VALIDATE)[0];
    let q = v.query();
    assert_eq!(q["clientId"], "page-client-id");
    assert_eq!(q["clientBuildNumber"], "2530Build12");
    assert!(!q.contains_key("dsid"));
    let sent = v.header("Cookie").unwrap();
    assert!(sent.contains("X-APPLE-WEBAUTH-TOKEN=captured"));
    assert!(sent.contains("X-APPLE-DS-WEB-SESSION-TOKEN=ds"));
    assert!(!sent.contains("myacinfo"));

    let account = env.account().unwrap();
    assert_eq!(account["dsid"], DSID);
    assert_eq!(account["client_params"]["clientMasteringNumber"], "2530Hotfix3");

    // icloud-md's files, in its shape.
    let mirror = env.mirror().unwrap();
    let keys: Vec<&str> = mirror.as_object().unwrap().keys().map(String::as_str).collect();
    for k in [
        "cookie",
        "clientId",
        "clientBuildNumber",
        "clientMasteringNumber",
        "capturedAt",
    ] {
        assert!(keys.contains(&k), "{k} in {keys:?}");
    }
    assert_eq!(mirror["clientId"], "page-client-id");
    assert!(
        mirror["cookie"]
            .as_str()
            .unwrap()
            .contains("X-APPLE-WEBAUTH-TOKEN=rotated1")
    );
    let captured = mirror["capturedAt"].as_str().unwrap();
    assert!(captured.ends_with('Z') && captured.len() == 24, "{captured}");
    let meta: Value = serde_json::from_slice(&fs::read(env.mirror_dir().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta, json!({"appleId": "someone@example.com", "dsid": DSID}));
    let mode = fs::metadata(env.mirror_dir().join("session.local.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // The session works through the client lib at once.
    let s = Session::connect_on(&conn).unwrap();
    assert_eq!(s.dsid(), DSID);
    assert_eq!(s.webservices().unwrap().url("ckdatabasews"), Some(server.url.as_str()));
}

#[test]
fn sign_in_closed_or_without_params() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("closed-once");
    // First run: the user closes the window. Second: a capture without params.
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com"}]});
    let signin = write_script(
        dir.path(),
        "signin",
        &format!(
            "if [ ! -e '{m}' ]; then touch '{m}'; exit 1; fi\ncat <<'EOF'\n{capture}\nEOF",
            m = marker.display()
        ),
    );
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::sign_in_on(&conn).unwrap();
    assert!(watch.next().unwrap().signing_in);
    let closed = watch.next().unwrap();
    assert!(!closed.signing_in && !closed.signed_in);
    assert_eq!(server.count(VALIDATE), 0);

    icloud_session::sign_in_on(&conn).unwrap();
    assert!(watch.next().unwrap().signing_in);
    let done = watch.next().unwrap();
    assert!(done.signed_in && !done.signing_in);
    let q = server.requests(VALIDATE)[0].query();
    assert_eq!(q["clientBuildNumber"], "2624Build27");
    assert_eq!(q["clientMasteringNumber"], "2624Build27");
    assert_eq!(q["clientId"].len(), 36, "a fresh UUID");
}

#[test]
fn sign_out_forgets_account_profile_and_mirror() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let webkit_data = env.root().join("data/icloud-session/webkit");
    let webkit_cache = env.root().join("cache/icloud-session/webkit");
    fs::create_dir_all(&webkit_data).unwrap();
    fs::write(webkit_data.join("cookies.sqlite"), "x").unwrap();
    fs::create_dir_all(&webkit_cache).unwrap();

    let conn = env.conn();
    session(&conn).unwrap();
    assert!(env.mirror().is_some());
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::sign_out_on(&conn).unwrap();
    let change = watch.next().unwrap();
    assert!(!change.signed_in);
    assert!(env.account().is_none());
    assert!(!webkit_data.exists());
    assert!(!webkit_cache.exists());
    assert!(env.mirror().is_none());
    assert!(
        env.mirror_dir().join("meta.json").exists(),
        "icloud-md's account binding stays"
    );
    assert!(matches!(Session::connect_on(&conn), Err(Error::SignInRequired)));
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signed_in);
}

#[test]
fn icloud_md_rotation_in_the_mirror_is_adopted() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    let mut mirror = env.mirror().unwrap();
    let rotated = mirror["cookie"]
        .as_str()
        .unwrap()
        .replace("X-APPLE-WEBAUTH-TOKEN=rotated1", "X-APPLE-WEBAUTH-TOKEN=fromicloudmd");
    mirror["cookie"] = Value::String(rotated);
    // icloud-md's own write: plain writeFile, not atomic.
    fs::write(
        env.mirror_dir().join("session.local.json"),
        serde_json::to_string_pretty(&mirror).unwrap() + "\n",
    )
    .unwrap();
    wait_until("the daemon to adopt the jar", Duration::from_secs(5), || {
        env.account().is_some_and(|a| a.to_string().contains("fromicloudmd"))
    });
    let (cookie, _, _) = session(&conn).unwrap();
    assert_eq!(
        cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(),
        Some("fromicloudmd")
    );
    // The adopted token keeps the expiry we knew.
    assert!(prop::<u64>(&conn, "ExpiresAt") >= now() + 2_592_000 - 10);
    assert_eq!(server.count(VALIDATE), 1);
}

#[test]
fn icloud_md_rotation_while_stopped_is_adopted_on_start() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    fs::create_dir_all(env.mirror_dir()).unwrap();
    fs::write(
        env.mirror_dir().join("session.local.json"),
        json!({"cookie": "X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=12345\"; X-APPLE-WEBAUTH-TOKEN=newer", "clientId": "auth-client-id",
               "clientBuildNumber": "2624Build27", "clientMasteringNumber": "2624Build27M", "capturedAt": "2026-09-01T00:00:00.000Z"})
        .to_string(),
    )
    .unwrap();
    let conn = env.conn();
    session(&conn).unwrap();
    let v = &server.requests(VALIDATE)[0];
    assert!(v.header("Cookie").unwrap().contains("X-APPLE-WEBAUTH-TOKEN=newer"));
}

#[test]
fn client_lib_against_the_daemon() {
    let data_421 = Arc::new(Mutex::new(0usize));
    let validate_421 = Arc::new(AtomicBool::new(false));
    let (d421, v421) = (data_421.clone(), validate_421.clone());
    let server = Server::start(move |s, n, base| match s.path() {
        VALIDATE if v421.load(Ordering::SeqCst) => signed_out(),
        VALIDATE => validate_ok(n, base),
        "/data" => {
            let mut left = d421.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Reply::json(421, json!({}));
            }
            Reply::json(200, json!({"ok": n})).cookie("DATA=d1; Path=/; Secure")
        }
        "/file" => Reply::json(200, json!("file-body")),
        _ => Reply::json(404, json!({"missing": true})),
    });
    // A content host (not icloud.com, not a listed service): gets no jar and
    // no params, and its cookies stay out of the jar.
    let content = Server::start(|s, _, _| match s.path() {
        "/B/asset" => Reply::json(200, json!("asset")).cookie("X-APPLE-WEBAUTH-TOKEN=hijack; Path=/"),
        "/B/expired" => Reply::json(401, json!({"expired": true})),
        _ => Reply::json(200, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    let s = Session::connect_on(&conn).unwrap();
    assert_eq!(s.dsid(), DSID);
    assert_eq!(s.apple_id(), "someone@example.com");
    let base = s.webservices().unwrap().url("ckdatabasews").unwrap().to_string();

    // A GET carries the jar, the headers and the params, and hands the
    // rotated cookie back to the daemon.
    let r = s.get(&format!("{base}/data?keep=1&clientId=mine")).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.json::<Value>().unwrap(), json!({"ok": 1}));
    let seen = &server.requests("/data")[0];
    assert!(
        seen.header("Cookie")
            .unwrap()
            .contains("X-APPLE-WEBAUTH-TOKEN=rotated1")
    );
    assert_eq!(seen.header("Origin"), Some("https://www.icloud.com"));
    let q = seen.query();
    assert_eq!(q["keep"], "1");
    assert_eq!(q["clientId"], "mine", "a param already in the URL is kept");
    assert_eq!(q["clientBuildNumber"], "2624Build27");
    assert_eq!(q["dsid"], DSID);
    wait_until("DATA cookie persisted", Duration::from_secs(2), || {
        env.account().unwrap().to_string().contains("\"DATA\"")
    });

    // The next request uses the merged jar (the in-process cache was dropped).
    s.post_json(&format!("{base}/data"), &json!({"x": 1})).unwrap();
    let seen = server.requests("/data").pop().unwrap();
    assert_eq!(seen.method, "POST");
    assert!(seen.header("Cookie").unwrap().contains("DATA=d1"));
    assert_eq!(seen.header("Content-Type"), Some("application/json"));

    s.post_bytes(&format!("{base}/data"), "image/jpeg", vec![1, 2, 3])
        .unwrap();
    assert_eq!(
        server.requests("/data").pop().unwrap().header("Content-Type"),
        Some("image/jpeg")
    );

    // A file, streamed: the body arrives whole with its length, and it
    // carries the jar and params like post_bytes.
    let out = tempfile::tempdir().unwrap();
    let upload = out.path().join("upload.bin");
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(&upload, &bytes).unwrap();
    s.post_file(&format!("{base}/data"), "image/heic", &upload).unwrap();
    let seen = server.requests("/data").pop().unwrap();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.body, bytes);
    assert_eq!(seen.header("Content-Length"), Some("200000"));
    assert_eq!(seen.header("Transfer-Encoding"), None);
    assert_eq!(seen.header("Content-Type"), Some("image/heic"));
    assert!(seen.header("Cookie").unwrap().contains("X-APPLE-WEBAUTH-TOKEN="));
    assert_eq!(seen.query()["dsid"], DSID);
    fs::remove_file(&upload).unwrap();

    // Download: no params, temp file renamed into place, parents made.
    let dest = out.path().join("f.json");
    let n = s.download(&format!("{base}/file"), &dest).unwrap();
    assert_eq!(fs::read_to_string(&dest).unwrap(), "\"file-body\"");
    assert_eq!(n, 11);
    assert!(server.requests("/file")[0].query().is_empty());
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 1);
    let nested = out.path().join("new/sub/dir/f.json");
    s.download(&format!("{base}/file"), &nested).unwrap();
    assert_eq!(fs::read_to_string(&nested).unwrap(), "\"file-body\"");
    fs::remove_dir_all(out.path().join("new")).unwrap();

    // Content hosts: no cookies or params out, no Set-Cookie in.
    let asset = out.path().join("asset.json");
    s.download(&format!("{}/B/asset?sig=1", content.url), &asset).unwrap();
    s.post_bytes(&format!("{}/upload?sig=2", content.url), "image/jpeg", vec![1])
        .unwrap();
    for seen in content.requests("/B/asset").iter().chain(&content.requests("/upload")) {
        assert_eq!(seen.header("Cookie"), None, "no jar to a content host");
        assert!(!seen.query().contains_key("dsid"), "no client params to a content host");
    }
    s.get(&format!("{base}/data")).unwrap();
    assert!(
        !server
            .requests("/data")
            .pop()
            .unwrap()
            .header("Cookie")
            .unwrap()
            .contains("hijack"),
        "a content host's Set-Cookie is not merged"
    );

    // Other statuses are Http.
    match s.get(&format!("{base}/nope")) {
        Err(Error::Http { status: 404, body }) => assert!(body.contains("missing")),
        other => panic!("{other:?}"),
    }

    // A content host's 401 (say, an expired signed URL) says nothing about
    // the session: a plain Http error, not reported to the daemon.
    let validates = server.count(VALIDATE);
    match s.download(&format!("{}/B/expired?sig=1", content.url), &out.path().join("x")) {
        Err(Error::Http { status: 401, .. }) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(server.count(VALIDATE), validates);

    // A stray 421: the daemon's validate still succeeds, so one retry.
    let validates = server.count(VALIDATE);
    *data_421.lock().unwrap() = 1;
    let r = s.get(&format!("{base}/data")).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(server.count(VALIDATE), validates + 1);
    assert!(icloud_session::status_on(&conn).unwrap().signed_in);
    // The retry carried the jar the confirming validate rotated.
    let retry = server.requests("/data").pop().unwrap();
    assert!(
        retry
            .header("Cookie")
            .unwrap()
            .contains(&format!("X-APPLE-WEBAUTH-TOKEN=rotated{}", validates + 1))
    );

    // post_file's retry sends the whole file again.
    fs::write(&upload, b"retry me").unwrap();
    *data_421.lock().unwrap() = 1;
    let before = server.count("/data");
    assert_eq!(
        s.post_file(&format!("{base}/data"), "image/jpeg", &upload)
            .unwrap()
            .status,
        200
    );
    let sent = server.requests("/data");
    assert_eq!(sent.len(), before + 2);
    assert_eq!(sent[before].body, b"retry me");
    assert_eq!(sent[before + 1].body, b"retry me");

    // A 421 that persists though Apple accepts the session: Http, still signed in.
    *data_421.lock().unwrap() = 2;
    match s.get(&format!("{base}/data")) {
        Err(Error::Http { status: 421, .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(icloud_session::status_on(&conn).unwrap().signed_in);

    // The session really ended: SignInRequired, and the daemon agrees.
    *data_421.lock().unwrap() = 1;
    validate_421.store(true, Ordering::SeqCst);
    assert!(matches!(s.get(&format!("{base}/data")), Err(Error::SignInRequired)));
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signed_in);
    assert!(matches!(Session::connect_on(&conn), Err(Error::SignInRequired)));
}

#[test]
fn cli_status_validate_sign_in_and_sign_out() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let capture =
        json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com", "expires": null}]});
    let signin = write_script(dir.path(), "signin", &format!("cat <<'EOF'\n{capture}\nEOF"));
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });

    let out = env.cli(&["status"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        status,
        json!({"signed_in": false, "apple_id": null, "dsid": null, "expires_at": null, "signing_in": false, "find_my_authorized": false, "find_my_password_stored": false})
    );

    let out = env.cli(&["validate"]);
    assert_eq!(out.status.code(), Some(2));

    let out = env.cli(&["sign-in"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["dsid"], DSID);

    let out = env.cli(&["validate"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["dsid"], DSID);
    assert_eq!(v["webservices"]["findme"], findme_url(&server.url));

    let out = env.cli(&["sign-out"]);
    assert!(out.status.success());
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["signed_in"], false);

    assert_eq!(env.cli(&["bogus"]).status.code(), Some(64));
}

#[test]
fn idle_exit_waits_for_connected_clients() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        idle_secs: 1.0,
        ..Default::default()
    });
    // An observer that never calls the daemon's object.
    let observer = env.conn();
    assert!(!env.daemon_running(&observer));

    let client = env.conn();
    assert!(icloud_session::status_on(&client).unwrap().signed_in);
    assert!(env.daemon_running(&observer));
    // Past the idle time, but a client that called is still connected.
    thread::sleep(Duration::from_millis(2500));
    assert!(env.daemon_running(&observer), "stays while a client is connected");

    drop(client);
    wait_until("idle exit", Duration::from_secs(10), || !env.daemon_running(&observer));

    // D-Bus activation brings it back on the next call.
    let client = env.conn();
    assert!(icloud_session::status_on(&client).unwrap().signed_in);
}

#[test]
fn a_second_daemon_does_not_take_over() {
    let env = Env::start(Opts::default());
    let conn = env.conn();
    icloud_session::status_on(&conn).unwrap();
    let out = Command::new(DAEMON)
        .env_clear()
        .env("DBUS_SESSION_BUS_ADDRESS", &env.address)
        .env("HOME", env.root().join("home"))
        .env("XDG_STATE_HOME", env.root().join("state"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(env.daemon_running(&conn));
}

#[test]
fn an_unreadable_account_file_is_set_aside() {
    let env = Env::start(Opts {
        seed: false,
        ..Default::default()
    });
    fs::create_dir_all(env.account_path().parent().unwrap()).unwrap();
    fs::write(env.account_path(), "{\"dsid\": truncated").unwrap();
    let conn = env.conn();
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signed_in);
    assert!(!env.account_path().exists());
    assert_eq!(
        fs::read_to_string(env.account_path().with_extension("json.bad")).unwrap(),
        "{\"dsid\": truncated"
    );
}

#[test]
fn offline_session_hands_out_the_cached_session_and_backs_off() {
    let server = Server::start(|s, _, _| match s.path() {
        VALIDATE => {
            thread::sleep(Duration::from_millis(400));
            Reply::json(503, json!({}))
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        retry_secs: 2.0,
        ..Default::default()
    });
    // Callers that queue behind a failing /validate share its answer and
    // get the session as it is, instead of trying Apple one after another.
    let started = Instant::now();
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let conn = env.conn();
            thread::spawn(move || session(&conn).unwrap())
        })
        .collect();
    for caller in callers {
        let (cookie, _, _) = caller.join().unwrap();
        assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("original"));
    }
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.count(VALIDATE), 1);

    // Within the retry window, Session() does not call Apple at all.
    let conn = env.conn();
    session(&conn).unwrap();
    assert_eq!(server.count(VALIDATE), 1);
    assert!(prop::<bool>(&conn, "SignedIn"));

    // After it, one caller tries again.
    thread::sleep(Duration::from_millis(2100));
    session(&conn).unwrap();
    assert_eq!(server.count(VALIDATE), 2);
}

#[test]
fn a_slow_heartbeat_does_not_hold_up_the_idle_exit() {
    let server = Server::start(|s, _, _| match s.path() {
        VALIDATE => {
            thread::sleep(Duration::from_secs(8));
            Reply::json(503, json!({}))
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        idle_secs: 1.0,
        validate_secs: 1.0,
        ..Default::default()
    });
    let observer = env.conn();
    let client = env.conn();
    assert!(icloud_session::status_on(&client).unwrap().signed_in);
    thread::sleep(Duration::from_millis(1500));
    drop(client);
    wait_until("idle exit", Duration::from_secs(4), || !env.daemon_running(&observer));
}

#[test]
fn reports_that_arrive_together_share_one_validate() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => {
            thread::sleep(Duration::from_millis(400));
            validate_ok(n, base)
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    session(&env.conn()).unwrap();
    assert_eq!(server.count(VALIDATE), 1);
    let reporters: Vec<_> = (0..3)
        .map(|_| {
            let conn = env.conn();
            thread::spawn(move || call::<bool>(&conn, "ReportSignInRequired").unwrap())
        })
        .collect();
    for reporter in reporters {
        assert!(reporter.join().unwrap());
    }
    assert_eq!(server.count(VALIDATE), 2);
    // A later report asks Apple again.
    assert!(call::<bool>(&env.conn(), "ReportSignInRequired").unwrap());
    assert_eq!(server.count(VALIDATE), 3);
}

#[test]
fn a_late_validate_of_the_old_jar_leaves_a_new_sign_in_alone() {
    // The start-up /validate of the stored jar is slow and ends in 421; the
    // sign-in that finishes meanwhile must survive it.
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE if s.header("Cookie").unwrap_or_default().contains("=original") => {
            thread::sleep(Duration::from_millis(1500));
            signed_out()
        }
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "captured", "domain": ".icloud.com"}]});
    let signin = write_script(dir.path(), "signin", &format!("cat <<'EOF'\n{capture}\nEOF"));
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::sign_in_on(&conn).unwrap();
    while watch.next().unwrap().signing_in {}
    wait_until("the old jar's validate", Duration::from_secs(5), || {
        server
            .requests(VALIDATE)
            .iter()
            .any(|r| r.header("Cookie").unwrap_or_default().contains("=original"))
    });
    thread::sleep(Duration::from_millis(1800));
    assert!(prop::<bool>(&conn, "SignedIn"), "the 421 was about the old jar");
    let account = env.account().expect("the new account stays");
    assert!(!account.to_string().contains("\"original\""));
}

#[test]
fn sign_out_closes_an_open_sign_in_window() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "late", "domain": ".icloud.com"}]});
    // The window: records its pid, then "signs in" after a while.
    let signin = write_script(
        dir.path(),
        "signin",
        &format!(
            "echo $$ > '{}'\nsleep 1.5\ncat <<'EOF'\n{capture}\nEOF",
            pid_file.display()
        ),
    );
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let webkit_data = env.root().join("data/icloud-session/webkit");
    fs::create_dir_all(&webkit_data).unwrap();
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::sign_in_on(&conn).unwrap();
    assert!(watch.next().unwrap().signing_in);
    wait_until("the window to start", Duration::from_secs(5), || {
        fs::read_to_string(&pid_file).is_ok_and(|p| p.ends_with('\n'))
    });
    let pid = fs::read_to_string(&pid_file).unwrap().trim().to_string();

    icloud_session::sign_out_on(&conn).unwrap();
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signing_in && !status.signed_in, "{status:?}");
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the window was killed and reaped"
    );
    assert!(!webkit_data.exists());

    // Whatever the window would have printed never lands.
    thread::sleep(Duration::from_millis(2000));
    assert!(!prop::<bool>(&conn, "SignedIn"));
    assert!(env.account().is_none());
    assert_eq!(server.count(VALIDATE), 1, "only the start-up validate");
}

#[test]
fn the_mirror_is_watched_again_after_its_directory_goes_away() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        idle_secs: 2.0,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    let session_file = env.mirror_dir().join("session.local.json");
    let rotate = |token: &str| {
        let mut mirror = env.mirror().unwrap();
        let cookie = mirror["cookie"].as_str().unwrap();
        let old = cookie_of(cookie, "X-APPLE-WEBAUTH-TOKEN").unwrap();
        mirror["cookie"] = Value::String(cookie.replace(&format!("TOKEN={old}"), &format!("TOKEN={token}")));
        fs::write(&session_file, serde_json::to_string_pretty(&mirror).unwrap() + "\n").unwrap();
    };
    let adopted = |token: &str| {
        let (cookie, _, _) = session(&conn).unwrap();
        cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref() == Some(token)
    };

    // Deleted, then our next store writes the mirror again and re-watches.
    let saved = env.mirror().unwrap();
    fs::remove_dir_all(env.mirror_dir()).unwrap();
    thread::sleep(Duration::from_millis(200));
    conn.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(INTERFACE),
        "MergeCookies",
        &(vec!["OTHER=1; Path=/"],),
    )
    .unwrap();
    wait_until("the mirror to be rewritten", Duration::from_secs(2), || {
        env.mirror().is_some()
    });
    rotate("afterstore");
    wait_until("the rotation to be adopted", Duration::from_secs(5), || {
        adopted("afterstore")
    });

    // Deleted, then icloud-md makes it again before we store anything.
    fs::remove_dir_all(env.mirror_dir()).unwrap();
    thread::sleep(Duration::from_millis(200));
    fs::create_dir_all(env.mirror_dir()).unwrap();
    fs::write(&session_file, saved.to_string()).unwrap();
    rotate("afterrecreate");
    wait_until("the rotation to be adopted", Duration::from_secs(5), || {
        adopted("afterrecreate")
    });
}

/// `watch.next()`, or None after `timeout`.
fn next_within(watch: icloud_session::Watch, timeout: Duration) -> Option<icloud_session::Status> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut watch = watch;
        let _ = tx.send(watch.next());
    });
    rx.recv_timeout(timeout).ok().flatten()
}

#[test]
fn watch_notices_the_daemon_restarting() {
    let env = Env::start(Opts::default());
    let conn = env.conn();
    let watch = icloud_session::watch_on(&conn).unwrap();
    assert!(watch.current().unwrap().signed_in);
    // The next instance starts from a different state.
    fs::remove_file(env.account_path()).unwrap();
    let observer = env.conn();
    env.kill_daemon(&observer);
    let status = next_within(watch, Duration::from_secs(5)).expect("the watch noticed");
    assert!(!status.signed_in, "{status:?}");
    assert!(env.daemon_running(&observer), "re-read from a new instance");
}

#[test]
fn cli_sign_in_ends_when_the_daemon_dies() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let signin = write_script(
        dir.path(),
        "signin",
        &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()),
    );
    let env = Env::start(Opts {
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let mut cli = Command::new(CLI)
        .arg("sign-in")
        .env_clear()
        .env("DBUS_SESSION_BUS_ADDRESS", &env.address)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let conn = env.conn();
    wait_until("the window to open", Duration::from_secs(5), || {
        env.daemon_running(&conn) && prop::<bool>(&conn, "SigningIn")
    });
    env.kill_daemon(&conn);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = cli.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = cli.kill();
            panic!("icloud-session sign-in still waiting after the daemon died");
        }
        thread::sleep(Duration::from_millis(25));
    };
    let out = cli.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["signing_in"], false);
    assert_eq!(printed["signed_in"], false);
    if let Ok(pid) = fs::read_to_string(&pid_file) {
        let _ = Command::new("kill").arg(pid.trim()).status();
    }
}

#[test]
fn a_session_refuses_to_serve_another_account() {
    // The captured jar belongs to a different Apple account.
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE if s.header("Cookie").unwrap_or_default().contains("=other") => {
            let mut r = validate_ok(n, base);
            r.body = r.body.replace(DSID, "67890").replace("someone@", "other@");
            r
        }
        VALIDATE => validate_ok(n, base),
        "/data" => Reply::json(200, json!({})).cookie("DATA=1; Path=/"),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "other", "domain": ".icloud.com"}]});
    let signin = write_script(dir.path(), "signin", &format!("cat <<'EOF'\n{capture}\nEOF"));
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let s = Session::connect_on(&conn).unwrap();
    let base = s.webservices().unwrap().url("ckdatabasews").unwrap().to_string();
    s.get(&format!("{base}/data")).unwrap(); // drops the in-process cache

    icloud_session::sign_out_on(&conn).unwrap();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::sign_in_on(&conn).unwrap();
    let done = loop {
        let status = watch.next().unwrap();
        if !status.signing_in {
            break status;
        }
    };
    assert_eq!(done.dsid.as_deref(), Some("67890"));

    let sent = server.count("/data");
    assert!(matches!(s.get(&format!("{base}/data")), Err(Error::SignInRequired)));
    assert_eq!(
        server.count("/data"),
        sent,
        "nothing sent with the new jar and the old dsid"
    );
    let again = Session::connect_on(&conn).unwrap();
    assert_eq!(again.dsid(), "67890");
    again.get(&format!("{base}/data")).unwrap();
    assert_eq!(server.requests("/data").pop().unwrap().query()["dsid"], "67890");
}

// ------------------------------------------------------------- Find My

const INIT_CLIENT: &str = "/fmipservice/client/web/initClient";

/// What the Find My window prints: the one-factor session Find My's
/// password step makes (a token /validate refuses), with its session-only
/// cookies. `$n`, the window's run count, numbers the FMIP cookie.
fn find_capture(dsid: &str) -> String {
    json!({
        "cookies": [
            {"name": "X-APPLE-WEBAUTH-USER", "value": "\"v=1:s=0:d=12345\"", "domain": ".icloud.com", "path": "/", "expires": now() + 1000},
            {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "onefactor", "domain": ".icloud.com", "path": "/", "expires": now() + 1000},
            {"name": "X-APPLE-WEBAUTH-FMIP", "value": "fmip$n", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "X-APPLE-WEBAUTH-LOGIN", "value": "login", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "X-APPLE-UNIQUE-CLIENT-ID", "value": "ucid", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "X-APPLE-WEBAUTH-VALIDATE", "value": "v", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "x-apple-group", "value": "g", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "fmip-web", "value": "p129", "domain": "p129-fmipweb.icloud.com", "path": "/", "expires": null},
        ],
        "clientId": "find-client-id",
        "dsid": dsid,
    })
    .to_string()
}

/// A fake window that honours `--find`: records its arguments and pid,
/// counts its runs, then prints `capture` for `--find` and fails otherwise.
fn find_window(dir: &Path, capture: &str, delay: &str) -> String {
    write_script(
        dir,
        "signin",
        &format!(
            "echo \"$*\" > '{args}'\necho $$ > '{pid}'\nn=$(($(cat '{count}' 2>/dev/null || echo 0) + 1))\necho $n > '{count}'\nsleep {delay}\n[ \"$1\" = --find ] || exit 1\ncat <<EOF\n{capture}\nEOF",
            args = dir.join("args").display(),
            pid = dir.join("pid").display(),
            count = dir.join("count").display(),
        ),
    )
}

fn cookie_named<'a>(cookies: &'a Value, name: &str) -> Option<&'a Value> {
    cookies.as_array().unwrap().iter().find(|c| c["name"] == name)
}

/// Waits for the window to close, returning the status then.
fn window_closed(watch: &mut icloud_session::Watch) -> icloud_session::Status {
    loop {
        let status = watch.next().unwrap();
        if !status.signing_in {
            return status;
        }
    }
}

/// Apple's side: `/validate` refuses the one-factor token; Find My answers
/// the FMIP value in `accepted` (after `slow_ms` for any other) and 450
/// otherwise.
fn find_my_server(accepted: Arc<Mutex<String>>, slow_ms: u64) -> Server {
    Server::start(move |s, n, base| {
        let cookie = s.header("Cookie").unwrap_or_default().to_string();
        match s.path() {
            VALIDATE if cookie.contains("=onefactor") => signed_out(),
            VALIDATE => validate_ok(n, base),
            INIT_CLIENT => {
                let want = format!("X-APPLE-WEBAUTH-FMIP={}", accepted.lock().unwrap());
                if cookie.split("; ").any(|c| c == want) {
                    return Reply::json(200, json!({"content": []})).cookie("FMIP-ROTATED=r; Path=/");
                }
                thread::sleep(Duration::from_millis(slow_ms));
                Reply {
                    status: 450,
                    body: String::new(),
                    set_cookies: vec![],
                }
            }
            "/data" => Reply::json(200, json!({})),
            _ => Reply::json(404, json!({})),
        }
    })
}

#[test]
fn authorize_find_my_keeps_a_separate_jar_until_a_450() {
    let accepted = Arc::new(Mutex::new("fmip1".to_string()));
    let server = find_my_server(accepted.clone(), 0);
    let dir = tempfile::tempdir().unwrap();
    let signin = find_window(dir.path(), &find_capture(DSID), "0.2");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
    let s = Session::connect_on(&conn).unwrap();
    let ws = s.webservices().unwrap();
    let init = format!("{}{INIT_CLIENT}", ws.url("findme").unwrap());
    let data = format!("{}/data", ws.url("ckdatabasews").unwrap());

    // No Find My session yet and no password stored: nothing is sent,
    // no sign-in is tried; the manual path is the only one.
    assert!(!prop::<bool>(&conn, "FindMyPasswordStored"));
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(server.count(INIT_CLIENT), 0);
    assert!(!dir.path().join("count").exists(), "no window run");
    let validates = server.count(VALIDATE);
    let before = env.account().unwrap();
    let mirror_before = env.mirror();

    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::authorize_find_my_on(&conn).unwrap();
    assert!(watch.next().unwrap().signing_in);
    let done = window_closed(&mut watch);
    assert!(done.signed_in && done.find_my_authorized, "{done:?}");
    assert_eq!(done.dsid.as_deref(), Some(DSID));
    assert_eq!(fs::read_to_string(dir.path().join("args")).unwrap().trim(), "--find");

    // Not validated, and the main account and the mirror are untouched.
    assert_eq!(server.count(VALIDATE), validates);
    let account = env.account().unwrap();
    for key in ["cookies", "client_params", "dsid", "validated_at", "captured_at"] {
        assert_eq!(account[key], before[key], "{key}");
    }
    assert_eq!(env.mirror(), mirror_before);
    // The Find My jar, session-only cookies included.
    let fm = &account["find_my"];
    assert_eq!(fm["client_params"]["clientId"], "find-client-id");
    for name in [
        "X-APPLE-WEBAUTH-FMIP",
        "X-APPLE-WEBAUTH-LOGIN",
        "X-APPLE-UNIQUE-CLIENT-ID",
        "X-APPLE-WEBAUTH-VALIDATE",
        "x-apple-group",
    ] {
        let c = cookie_named(&fm["cookies"], name).unwrap_or_else(|| panic!("{name} kept"));
        assert!(c["expires"].is_null(), "{name} stays session-only");
    }
    assert_eq!(
        cookie_named(&fm["cookies"], "fmip-web").unwrap()["domain"],
        "p129-fmipweb.icloud.com"
    );

    // Find My gets the Find My jar and params; its Set-Cookie goes back
    // into that jar. Other services keep the main jar.
    s.post_json(&init, &json!({})).unwrap();
    let sent = server.requests(INIT_CLIENT).pop().unwrap();
    let cookie = sent.header("Cookie").unwrap();
    assert!(cookie.contains("X-APPLE-WEBAUTH-TOKEN=onefactor"), "{cookie}");
    assert_eq!(sent.query()["clientId"], "find-client-id");
    assert_eq!(sent.query()["dsid"], DSID);
    let account = env.account().unwrap();
    assert!(cookie_named(&account["find_my"]["cookies"], "FMIP-ROTATED").is_some());
    assert!(cookie_named(&account["cookies"], "FMIP-ROTATED").is_none());
    s.get(&data).unwrap();
    let cookie = server
        .requests("/data")
        .pop()
        .unwrap()
        .header("Cookie")
        .unwrap()
        .to_string();
    assert!(!cookie.contains("FMIP") && !cookie.contains("onefactor"), "{cookie}");

    // Find My asks for the password again: reported, the jar forgotten.
    *accepted.lock().unwrap() = "none".into();
    let sent = server.count(INIT_CLIENT);
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(server.count(INIT_CLIENT), sent + 1, "no retry with the same jar");
    let changed = watch.next().unwrap();
    assert!(changed.signed_in && !changed.find_my_authorized, "{changed:?}");
    assert!(env.account().unwrap().get("find_my").is_none());
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(server.count(INIT_CLIENT), sent + 1, "nothing sent without a jar");
    assert!(prop::<bool>(&conn, "SignedIn"));
    assert_eq!(
        fs::read_to_string(dir.path().join("args")).unwrap().trim(),
        "--find",
        "no automatic sign-in since the manual one"
    );

    // The CLI authorizes again; signing out forgets it.
    *accepted.lock().unwrap() = "fmip2".into();
    let out = env.cli(&["authorize-find-my"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["find_my_authorized"], true);
    assert_eq!(printed["signing_in"], false);
    s.post_json(&init, &json!({})).unwrap();
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
    assert!(env.account().is_none());
}

#[test]
fn a_450_with_a_jar_replaced_meanwhile_retries_once() {
    // A request goes out with the first Find My jar and is slow to fail;
    // the user authorizes again meanwhile. The 450 about the old jar must
    // not forget the new one: the request is retried with it instead.
    let accepted = Arc::new(Mutex::new("fmip1".to_string()));
    let server = find_my_server(accepted.clone(), 1500);
    let dir = tempfile::tempdir().unwrap();
    let signin = find_window(dir.path(), &find_capture(DSID), "0");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::authorize_find_my_on(&conn).unwrap();
    assert!(window_closed(&mut watch).find_my_authorized);
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());

    *accepted.lock().unwrap() = "fmip2".into();
    let s2 = s.clone();
    let slow = thread::spawn(move || s2.post_json(&init, &json!({})));
    wait_until("the request", Duration::from_secs(5), || server.count(INIT_CLIENT) == 1);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    wait_until("the second window", Duration::from_secs(5), || {
        env.account().unwrap()["find_my"].to_string().contains("fmip2")
    });
    slow.join().unwrap().unwrap();
    assert_eq!(server.count(INIT_CLIENT), 2);
    assert!(prop::<bool>(&conn, "FindMyAuthorized"));
}

#[test]
fn a_find_my_jar_of_another_apple_id_is_not_kept() {
    let server = find_my_server(Arc::new(Mutex::new("fmip1".into())), 0);
    let dir = tempfile::tempdir().unwrap();
    let signin = find_window(dir.path(), &find_capture("67890"), "0");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::authorize_find_my_on(&conn).unwrap();
    let done = window_closed(&mut watch);
    assert!(done.signed_in && !done.find_my_authorized, "{done:?}");
    assert_eq!(done.dsid.as_deref(), Some(DSID));
    let account = env.account().unwrap();
    assert!(account.get("find_my").is_none());
    assert!(!account.to_string().contains("onefactor"));
}

#[test]
fn sign_out_during_authorize_find_my_drops_its_result() {
    let server = find_my_server(Arc::new(Mutex::new("fmip1".into())), 0);
    let dir = tempfile::tempdir().unwrap();
    let signin = find_window(dir.path(), &find_capture(DSID), "1.5");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    icloud_session::authorize_find_my_on(&conn).unwrap();
    assert!(watch.next().unwrap().signing_in);
    // A SignIn while the Find My window is open opens no second window.
    icloud_session::sign_in_on(&conn).unwrap();
    let pid_file = dir.path().join("pid");
    wait_until("the window to start", Duration::from_secs(5), || {
        fs::read_to_string(&pid_file).is_ok_and(|p| p.ends_with('\n'))
    });
    let pid = fs::read_to_string(&pid_file).unwrap().trim().to_string();

    icloud_session::sign_out_on(&conn).unwrap();
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(
        !status.signing_in && !status.signed_in && !status.find_my_authorized,
        "{status:?}"
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the window was killed and reaped"
    );

    thread::sleep(Duration::from_millis(2000));
    assert!(!prop::<bool>(&conn, "SignedIn"));
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
    assert!(env.account().is_none());
    assert_eq!(server.count(VALIDATE), 1, "only the start-up validate");
}

// ------------------------------------------- Find My with a stored password

const PASSWORD: &str = "correct horse";

/// The sign-in window in `--find --autofill` mode, faked: it counts its
/// runs, reads the Apple ID and the password from stdin (exactly two
/// lines) and, as Apple's page would, prints a one-factor capture whose
/// FMIP is `auto<n>` when the password is the one in `good`, exits 3 when
/// it is not, and exits 1 while `fail` exists (Apple unreachable). Anything
/// out of place (other arguments, another Apple ID, the password in its
/// argv or env) is written to `bad` and fails the run.
struct Autofill {
    dir: tempfile::TempDir,
    bin: String,
}

impl Autofill {
    fn new() -> Autofill {
        let dir = tempfile::tempdir().unwrap();
        let capture = json!({
            "cookies": [
                {"name": "X-APPLE-WEBAUTH-USER", "value": "\"v=1:s=1:d=12345\"", "domain": ".icloud.com", "path": "/", "expires": null},
                {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "onefactor-auto", "domain": ".icloud.com", "path": "/", "expires": null},
                {"name": "X-APPLE-WEBAUTH-FMIP", "value": "auto$n", "domain": ".icloud.com", "path": "/", "expires": null},
            ],
            "dsid": DSID,
            "clientId": "autofill-client-id",
            "clientBuildNumber": "2640Build5",
            "clientMasteringNumber": "2640Build5M",
        });
        let bin = write_script(
            dir.path(),
            "signin",
            &format!(
                r#"d='{d}'
n=$(($(cat "$d/count" 2>/dev/null || echo 0) + 1)); echo $n > "$d/count"
IFS= read -r id || {{ echo "no Apple ID on stdin" >> "$d/bad"; exit 1; }}
IFS= read -r pw || {{ echo "no password on stdin" >> "$d/bad"; exit 1; }}
rest=$(cat)
[ "$*" = "--find --autofill" ] || {{ echo "args: $*" >> "$d/bad"; exit 1; }}
[ "$id" = someone@example.com ] || {{ echo "Apple ID: $id" >> "$d/bad"; exit 1; }}
[ -z "$rest" ] || {{ echo "more than two lines on stdin" >> "$d/bad"; exit 1; }}
case "$*" in *"$pw"*) echo "the password in argv" >> "$d/bad"; exit 1 ;; esac
env | grep -qF -- "$pw" && {{ echo "the password in env" >> "$d/bad"; exit 1; }}
printf '%s' "$pw" > "$d/got"
sleep "$(cat "$d/delay" 2>/dev/null || echo 0)"
[ -e "$d/fail" ] && exit 1
[ "$pw" = "$(cat "$d/good")" ] || exit 3
cat <<EOF
{capture}
EOF"#,
                d = dir.path().display()
            ),
        );
        let autofill = Autofill { dir, bin };
        autofill.set_good(PASSWORD);
        autofill
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The password Apple accepts.
    fn set_good(&self, password: &str) {
        fs::write(self.file("good"), password).unwrap();
    }

    fn set_failing(&self, failing: bool) {
        if failing {
            fs::write(self.file("fail"), "").unwrap();
        } else {
            let _ = fs::remove_file(self.file("fail"));
        }
    }

    fn set_delay(&self, secs: &str) {
        fs::write(self.file("delay"), secs).unwrap();
    }

    /// How many times the window ran; every run got what it should.
    fn runs(&self) -> usize {
        if let Ok(bad) = fs::read_to_string(self.file("bad")) {
            panic!("the autofill window was run wrong: {bad}");
        }
        fs::read_to_string(self.file("count")).map_or(0, |n| n.trim().parse().unwrap())
    }

    /// The password the last run read from its stdin.
    fn last_password(&self) -> String {
        fs::read_to_string(self.file("got")).unwrap()
    }
}

/// An env whose account holds a stale Find My jar and, if given, a
/// stored password; the daemon is up and has looked in the keyring.
fn stored_password_env(server: &Server, password: Option<&str>) -> (Env, Connection, Autofill) {
    let autofill = Autofill::new();
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&autofill.bin),
        retry_secs: 0.5,
        ..Default::default()
    });
    env.seed_stale_find_my();
    if let Some(p) = password {
        env.store_password(p);
    }
    let conn = env.conn();
    let stored = password.is_some();
    wait_until("the keyring look", Duration::from_secs(5), || {
        prop::<bool>(&conn, "FindMyPasswordStored") == stored
    });
    (env, conn, autofill)
}

fn find_my_cookie(jar: &Value) -> Option<String> {
    cookie_named(jar, "X-APPLE-WEBAUTH-FMIP").map(|c| c["value"].as_str().unwrap().to_string())
}

fn accepting(fmip: &str) -> (Arc<Mutex<String>>, Server) {
    let accepted = Arc::new(Mutex::new(fmip.to_string()));
    let server = find_my_server(accepted.clone(), 0);
    (accepted, server)
}

#[test]
fn a_450_signs_in_with_the_stored_password_and_retries_once() {
    let (_accepted, server) = accepting("auto1");
    let (env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    autofill.set_delay("0.3");
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());
    let before = env.account().unwrap();
    let mirror_before = env.mirror();
    let validates = server.count(VALIDATE);

    // Two requests at once, both with the stale jar: one sign-in serves both.
    let requests: Vec<_> = (0..2)
        .map(|_| {
            let (s, init) = (s.clone(), init.clone());
            thread::spawn(move || s.post_json(&init, &json!({})))
        })
        .collect();
    for r in requests {
        r.join().unwrap().unwrap();
    }
    assert_eq!(autofill.runs(), 1, "one sign-in for both");
    let inits = server.requests(INIT_CLIENT);
    assert_eq!(inits.len(), 4, "each request sent twice: stale, then retried once");
    assert!(
        inits
            .iter()
            .filter(|r| r.header("Cookie").unwrap().contains("FMIP=stale"))
            .count()
            == 2
    );
    assert!(
        inits
            .iter()
            .filter(|r| r.header("Cookie").unwrap().contains("FMIP=auto1"))
            .count()
            == 2
    );

    // The sign-in: the window in --find --autofill mode, handed the Apple
    // ID and the password on its stdin alone (the fake checks argv and env).
    assert_eq!(autofill.last_password(), PASSWORD);

    // Kept as the Find My jar only, with the client params the window
    // captured; the main jar, the mirror and the validate count are
    // untouched.
    let account = env.account().unwrap();
    for key in ["cookies", "client_params", "dsid", "validated_at", "captured_at"] {
        assert_eq!(account[key], before[key], "{key}");
    }
    assert_eq!(env.mirror(), mirror_before);
    assert_eq!(server.count(VALIDATE), validates);
    assert_eq!(find_my_cookie(&account["find_my"]["cookies"]).as_deref(), Some("auto1"));
    assert_eq!(
        account["find_my"]["client_params"],
        json!({"clientId": "autofill-client-id", "clientBuildNumber": "2640Build5", "clientMasteringNumber": "2640Build5M"})
    );
    assert!(prop::<bool>(&conn, "FindMyAuthorized"));
    // Nothing of the password on disk but the (test) keyring.
    assert!(!account.to_string().contains(PASSWORD));
}

#[test]
fn with_no_find_my_jar_the_stored_password_signs_in_first() {
    let (_accepted, server) = accepting("auto1");
    let autofill = Autofill::new();
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&autofill.bin),
        ..Default::default()
    });
    env.store_password(PASSWORD);
    let conn = env.conn();
    wait_until("the keyring look", Duration::from_secs(5), || {
        prop::<bool>(&conn, "FindMyPasswordStored")
    });
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());
    s.post_json(&init, &json!({})).unwrap();
    assert_eq!(server.count(INIT_CLIENT), 1, "sent once, with the new jar");
    assert_eq!(autofill.runs(), 1);
    assert!(prop::<bool>(&conn, "FindMyAuthorized"));
}

#[test]
fn a_wrong_stored_password_falls_back_to_the_manual_path_without_a_loop() {
    let (accepted, server) = accepting("auto1");
    let (env, conn, autofill) = stored_password_env(&server, Some("wrong"));
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());

    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(server.count(INIT_CLIENT), 1, "not retried");
    assert_eq!(autofill.runs(), 1);
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
    assert!(env.account().unwrap().get("find_my").is_none());
    // Refused once: not tried again with the same password, whoever asks.
    for _ in 0..3 {
        assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    }
    assert!(!call::<bool>(&conn, "ReportFindMyAuthRequired").unwrap());
    assert_eq!(autofill.runs(), 1);
    assert_eq!(server.count(INIT_CLIENT), 1);
    assert!(prop::<bool>(&conn, "FindMyPasswordStored"));

    // set-password with the wrong one: refused, nothing stored.
    let out = env.cli_with(&["set-password"], "also wrong\n", &[]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("refused"));
    assert_eq!(env.secrets()[0]["secret"], "wrong");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("also wrong"));

    // set-password with the right one (stdin, not a terminal): verified
    // with a sign-in, stored, and Find My authorized by it.
    let out = env.cli_with(&["set-password"], &format!("{PASSWORD}\n"), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["find_my_password_stored"], true);
    assert_eq!(printed["find_my_authorized"], true);
    let secrets = env.secrets();
    assert_eq!(secrets.as_array().unwrap().len(), 1);
    assert_eq!(secrets[0]["secret"], PASSWORD);
    assert_eq!(secrets[0]["label"], "iCloud (icloud-session): someone@example.com");
    assert_eq!(
        secrets[0]["attributes"],
        json!({"application": "icloud-session", "apple-id": "someone@example.com"})
    );
    // One trailing newline dropped: the window read exactly two lines.
    assert_eq!(autofill.last_password(), PASSWORD);
    *accepted.lock().unwrap() = format!("auto{}", autofill.runs());
    s.post_json(&init, &json!({})).unwrap();

    // A sign-in that fails for another reason says nothing about the
    // password: it is stored anyway, and the failure reported.
    autofill.set_good("changed on Apple's side too");
    autofill.set_failing(true);
    let out = env.cli_with(&["set-password"], "changed on Apple's side too\n", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("stored the password, but the Find My sign-in with it failed"),
        "{stderr}"
    );
    assert_eq!(env.secrets()[0]["secret"], "changed on Apple's side too");
    assert!(prop::<bool>(&conn, "FindMyPasswordStored"));
}

#[test]
fn a_session_find_my_refuses_at_once_is_not_signed_in_again() {
    // The password works, but Find My answers 450 to the new session too:
    // one sign-in, one retry, then the manual path and no more sign-ins.
    let (_accepted, server) = accepting("never");
    let (_env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(server.count(INIT_CLIENT), 2);
    assert_eq!(autofill.runs(), 1);
    for _ in 0..3 {
        assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    }
    assert_eq!(server.count(INIT_CLIENT), 2);
    assert_eq!(autofill.runs(), 1);
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
}

#[test]
fn an_unreachable_find_my_sign_in_backs_off() {
    let (_accepted, server) = accepting("auto2");
    let (_env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    autofill.set_failing(true);
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(autofill.runs(), 1, "backing off");
    autofill.set_failing(false);
    thread::sleep(Duration::from_millis(600));
    s.post_json(&init, &json!({})).unwrap();
    assert_eq!(autofill.runs(), 2);
}

#[test]
fn sign_out_keeps_the_stored_password_and_forget_password_removes_it() {
    let (_accepted, server) = accepting("auto1");
    let (env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(!prop::<bool>(&conn, "FindMyPasswordStored"), "false while signed out");
    assert_eq!(env.secrets()[0]["secret"], PASSWORD, "the user's, kept");
    // Signed out: set-password has no account to check it against.
    let out = env.cli_with(&["set-password"], PASSWORD, &[]);
    assert_eq!(out.status.code(), Some(2));

    let out = env.cli(&["forget-password"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(env.secrets(), json!([]));
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["find_my_password_stored"], false);
    assert_eq!(autofill.runs(), 0);
}

#[test]
fn forget_password_while_signed_in_stops_automatic_sign_in() {
    let (_accepted, server) = accepting("auto1");
    let (env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    let mut watch = icloud_session::watch_on(&conn).unwrap();
    let out = env.cli(&["forget-password"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!watch.next().unwrap().find_my_password_stored);
    let s = Session::connect_on(&conn).unwrap();
    let init = format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap());
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(autofill.runs(), 0);
}

// ------------------------------------------ set-password from a manager

/// Fake `bw` and `op` in a directory of their own: each logs its arguments
/// (item names, ids and session keys, never a password). Listing logins,
/// both answer three: "Apple" for the Apple ID on apple.com and icloud.com
/// (id `*-apple`), "iCloud kid" on icloud.com for another Apple ID (for the
/// Apple ID too with `$FAKE_TWO_MINE`), and a non-Apple site for the Apple
/// ID that must be ignored; with `$FAKE_NO_APPLE` only that last one. Any
/// `bw-*`/`op-*` id holds the password, and so does the item named
/// `$FAKE_FOUND`; `$FAKE_AMBIGUOUS` names an item that matches twice.
/// `$FAKE_BW_STATUS` is bw's status (locked by default), with
/// `$FAKE_UNLOCK_FAIL` its unlock fails; `$FAKE_OP_SIGNED_OUT` /
/// `$FAKE_LOCKED` make op whoami fail / op stay signed out. bw answers
/// `get` and `list` only with `BW_SESSION=KEY123` in its env; a signed-out
/// op only with `--session TOK` after `op signin`.
fn fake_managers(dir: &Path) -> String {
    let log = dir.join("log");
    write_script(
        dir,
        "bw",
        &format!(
            r#"echo "bw $*" >> '{log}'
kid_user=kid@example.com; [ -n "$FAKE_TWO_MINE" ] && kid_user=someone@example.com
case "$1" in
  status) printf '{{"status":"%s"}}' "${{FAKE_BW_STATUS:-locked}}"; exit 0 ;;
  unlock|login) [ "$2" = --raw ] || exit 64
    [ -n "$FAKE_UNLOCK_FAIL" ] && {{ echo "Invalid master password." >&2; exit 1; }}
    printf KEY123; exit 0 ;;
  lock) echo "Your vault is locked."; exit 0 ;;
esac
[ "$1" = --nointeraction ] || exit 64
[ "$BW_SESSION" = KEY123 ] || {{ echo "Vault is locked." >&2; exit 1; }}
if [ "$2" = list ]; then
  [ "$3" = items ] && [ "$4" = --search ] || exit 64
  other='{{"id":"bw-notapple","type":1,"name":"Not Apple","login":{{"username":"someone@example.com","password":"x","uris":[{{"uri":"https://notapple.com/login"}}]}}}}'
  apple='{{"id":"bw-apple","type":1,"name":"Apple","login":{{"username":"someone@example.com","password":"x","uris":[{{"uri":"https://appleid.apple.com"}},{{"uri":"https://www.icloud.com/"}}]}}}}'
  kid="{{\"id\":\"bw-kid\",\"type\":1,\"name\":\"iCloud kid\",\"login\":{{\"username\":\"$kid_user\",\"password\":\"x\",\"uris\":[{{\"uri\":\"https://www.icloud.com\"}}]}}}}"
  note='{{"id":"bw-note","type":2,"name":"apple.com notes","notes":"x"}}'
  if [ -n "$FAKE_NO_APPLE" ]; then echo "[$other]"; exit 0; fi
  case "$5" in
    apple.com) echo "[$apple,$other,$note]" ;;
    icloud.com) echo "[$apple,$kid]" ;;
    *) echo "[]" ;;
  esac
  exit 0
fi
[ "$2" = get ] && [ "$3" = password ] || exit 64
case "$4" in bw-*) printf '%s' "$FAKE_PASSWORD"; exit 0 ;; esac
[ "$4" = "$FAKE_AMBIGUOUS" ] && {{ printf 'More than one result was found. Try getting a specific object by `id` instead. The following objects were found:\nid-1\nid-2\n' >&2; exit 1; }}
[ "$4" = "$FAKE_FOUND" ] && {{ printf '%s' "$FAKE_PASSWORD"; exit 0; }}
echo "Not found." >&2; exit 1"#,
            log = log.display()
        ),
    );
    write_script(
        dir,
        "op",
        &format!(
            r#"echo "op $*" >> '{log}'
kid_user=kid@example.com; [ -n "$FAKE_TWO_MINE" ] && kid_user=someone@example.com
case "$1" in
  whoami) [ -n "$FAKE_OP_SIGNED_OUT" ] && exit 1; exit 0 ;;
  signin) [ -n "$FAKE_LOCKED" ] && {{ echo "[ERROR] sign in failed" >&2; exit 1; }}; echo TOK; exit 0 ;;
  signout) exit 0 ;;
esac
if [ -n "$FAKE_OP_SIGNED_OUT" ]; then
  for a in "$@"; do last2="$last1"; last1="$a"; done
  [ "$last2" = --session ] && [ "$last1" = TOK ] || {{ echo "[ERROR] account is not signed in" >&2; exit 1; }}
fi
if [ "$1" = read ]; then
  [ "$2" = "$FAKE_FOUND" ] && {{ echo "$FAKE_PASSWORD"; exit 0; }}
  echo "[ERROR] could not read secret: not found" >&2; exit 1
fi
if [ "$1" = item ] && [ "$2" = list ]; then
  [ "$3 $4 $5 $6" = "--categories Login --format json" ] || exit 64
  other='{{"id":"op-bank","title":"Bank","category":"LOGIN","additional_information":"someone@example.com","urls":[{{"primary":true,"href":"https://bank.example/"}}]}}'
  apple='{{"id":"op-apple","title":"Apple ID","category":"LOGIN","additional_information":"someone@example.com","urls":[{{"primary":true,"href":"https://appleid.apple.com/"}}]}}'
  kid="{{\"id\":\"op-kid\",\"title\":\"iCloud kid\",\"category\":\"LOGIN\",\"additional_information\":\"$kid_user\",\"urls\":[{{\"href\":\"icloud.com\"}}]}}"
  if [ -n "$FAKE_NO_APPLE" ]; then echo "[$other]"; else echo "[$other,$apple,$kid]"; fi
  exit 0
fi
[ "$1" = item ] && [ "$2" = get ] && [ "$4" = --fields ] && [ "$5" = label=password ] && [ "$6" = --reveal ] || exit 64
case "$3" in op-*) echo "$FAKE_PASSWORD"; exit 0 ;; esac
[ "$3" = "$FAKE_AMBIGUOUS" ] && {{ printf '[ERROR] More than one item matches "%s". Try again and specify the item by its ID:\n\t* for the item "%s" in vault Private: aaa\n\t* for the item "%s" in vault Work: bbb\n' "$3" "$3" "$3" >&2; exit 1; }}
[ "$3" = "$FAKE_FOUND" ] && {{ echo "$FAKE_PASSWORD"; exit 0; }}
echo "[ERROR] \"$3\" isn't an item. Specify the item with its UUID, name, or domain." >&2; exit 1"#,
            log = log.display()
        ),
    );
    format!("{}:/usr/bin:/bin", dir.display())
}

fn log_lines(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn set_password_from_bitwarden_and_1password() {
    let (_accepted, server) = accepting("auto1");
    let (env, _conn, autofill) = stored_password_env(&server, None);
    let dir = tempfile::tempdir().unwrap();
    let path = fake_managers(dir.path());
    let run = |args: &[&str], vars: &[(&str, &str)]| {
        let _ = fs::remove_file(dir.path().join("log"));
        let mut all = vec![("PATH", path.as_str()), ("FAKE_PASSWORD", PASSWORD)];
        all.extend_from_slice(vars);
        let out = env.cli_with(args, "", &all);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!stderr.contains(PASSWORD), "{stderr}");
        (out.status.code(), stderr, log_lines(dir.path()))
    };
    let stored = || env.secrets().as_array().unwrap().len();

    // Locked (no BW_SESSION): unlocked on the terminal, the key passed to
    // the listings and the lookup in their env, locked again after. With
    // no ITEM: the logins saved for apple.com and icloud.com, the one for
    // the Apple ID fetched by its id; the non-Apple one is ignored.
    let get = |item: &str| format!("bw --nointeraction get password {item}");
    let list = |site: &str| format!("bw --nointeraction list items --search {site}");
    let (code, err, log) = run(&["set-password", "--from-bitwarden"], &[]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        log,
        [
            "bw status".to_string(),
            "bw unlock --raw".into(),
            list("apple.com"),
            list("icloud.com"),
            get("bw-apple"),
            "bw lock".into(),
        ]
    );
    assert!(err.contains("\"Apple\""), "{err}");
    assert!(!err.contains("KEY123"), "{err}");
    assert_eq!(env.secrets()[0]["secret"], PASSWORD);
    assert_eq!(autofill.last_password(), PASSWORD);
    let (code, err, _) = run(&["forget-password"], &[]);
    assert_eq!(code, Some(0), "{err}");

    // Signed out of Bitwarden: bw login instead.
    let (code, err, log) = run(
        &["set-password", "--from-bitwarden"],
        &[("FAKE_BW_STATUS", "unauthenticated")],
    );
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        log,
        [
            "bw status".to_string(),
            "bw login --raw".into(),
            list("apple.com"),
            list("icloud.com"),
            get("bw-apple"),
            "bw lock".into()
        ]
    );
    run(&["forget-password"], &[]);

    // BW_SESSION exported: used as it is, not locked after. An explicit
    // ITEM: looked up by name, nothing listed.
    let (code, err, log) = run(
        &["set-password", "--from-bitwarden", "Work Apple"],
        &[("FAKE_FOUND", "Work Apple"), ("BW_SESSION", "KEY123")],
    );
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(log, [get("Work Apple")]);
    run(&["forget-password"], &[]);

    let logins = autofill.runs();
    // The unlock fails: blamed on the master password, locked anyway.
    let (code, err, log) = run(&["set-password", "--from-bitwarden"], &[("FAKE_UNLOCK_FAIL", "1")]);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("mistyped master password") && err.contains("bw login"),
        "{err}"
    );
    assert_eq!(log, ["bw status", "bw unlock --raw", "bw lock"]);
    // A BW_SESSION that does not unlock it.
    let (code, err, log) = run(&["set-password", "--from-bitwarden"], &[("BW_SESSION", "stale")]);
    assert_eq!(code, Some(1));
    assert!(err.contains("locked") && err.contains("bw unlock --raw"), "{err}");
    assert_eq!(log.len(), 1, "stops at a locked vault");
    // No Apple login saved.
    let (code, err, log) = run(&["set-password", "--from-bitwarden"], &[("FAKE_NO_APPLE", "1")]);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("no Bitwarden login saved for apple.com or icloud.com"),
        "{err}"
    );
    assert_eq!(log.len(), 5, "status, unlock, two listings, lock");
    // An ITEM that names nothing.
    let (code, err, log) = run(&["set-password", "--from-bitwarden", "Nope"], &[]);
    assert_eq!(code, Some(1));
    assert!(err.contains("no Bitwarden item") && err.contains("\"Nope\""), "{err}");
    assert_eq!(
        log,
        [
            "bw status".to_string(),
            "bw unlock --raw".into(),
            get("Nope"),
            "bw lock".into()
        ]
    );
    // An ITEM that names two.
    let (code, err, log) = run(
        &["set-password", "--from-bitwarden", "Apple ID"],
        &[("FAKE_AMBIGUOUS", "Apple ID")],
    );
    assert_eq!(code, Some(1));
    assert!(err.contains("more than one") && err.contains("id-1"), "{err}");
    assert_eq!(log.len(), 4, "stops at an ambiguous item, then locks");
    assert_eq!(log[3], "bw lock");
    // Two Apple logins for the Apple ID and no terminal to ask on: both
    // listed (no passwords), none fetched.
    let (code, err, log) = run(&["set-password", "--from-bitwarden"], &[("FAKE_TWO_MINE", "1")]);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("several Apple logins match; pass the one to use as ITEM"),
        "{err}"
    );
    assert!(err.contains("1) Apple") && err.contains("iCloud kid"), "{err}");
    assert!(!err.contains("Not Apple"), "{err}");
    assert!(!log.iter().any(|l| l.contains(" get ")), "{log:?}");
    assert_eq!(log.last().map(String::as_str), Some("bw lock"));
    assert_eq!(autofill.runs(), logins, "nothing to check");
    assert_eq!(stored(), 0);

    // 1Password: its Login items, the one for the Apple ID fetched by id.
    let op_get = |id: &str| format!("op item get {id} --fields label=password --reveal");
    let op_list = "op item list --categories Login --format json";
    let (code, err, log) = run(&["set-password", "--from-1password"], &[]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(log, ["op whoami".to_string(), op_list.into(), op_get("op-apple")]);
    assert_eq!(env.secrets()[0]["secret"], PASSWORD, "op's trailing newline dropped");
    run(&["forget-password"], &[]);
    let reference = "op://Private/Apple/password";
    let (code, err, log) = run(
        &["set-password", "--from-1password", reference],
        &[("FAKE_FOUND", reference)],
    );
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(log, ["op whoami".to_string(), format!("op read {reference}")]);
    run(&["forget-password"], &[]);
    // Not signed in: op signin on the terminal, its session passed on, and
    // signed out after.
    let (code, err, log) = run(&["set-password", "--from-1password"], &[("FAKE_OP_SIGNED_OUT", "1")]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        log,
        [
            "op whoami".to_string(),
            "op signin --raw".into(),
            format!("{op_list} --session TOK"),
            format!("{} --session TOK", op_get("op-apple")),
            "op signout --session TOK".into(),
        ]
    );
    run(&["forget-password"], &[]);
    let (code, err, _) = run(
        &["set-password", "--from-1password"],
        &[("FAKE_OP_SIGNED_OUT", "1"), ("FAKE_LOCKED", "1")],
    );
    assert_eq!(code, Some(1));
    assert!(
        err.contains("omarchy-install-service-1password") && err.contains("Integrate with 1Password CLI"),
        "{err}"
    );
    let (code, err, log) = run(
        &["set-password", "--from-1password", "Apple"],
        &[("FAKE_AMBIGUOUS", "Apple")],
    );
    assert_eq!(code, Some(1));
    assert!(err.contains("more than one") && err.contains("vault Work"), "{err}");
    assert_eq!(log.len(), 2);
    let (code, err, log) = run(&["set-password", "--from-1password"], &[("FAKE_TWO_MINE", "1")]);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("several Apple logins match") && err.contains("iCloud kid") && !err.contains("Bank"),
        "{err}"
    );
    assert_eq!(log, ["op whoami", op_list]);
    let (code, err, _) = run(&["set-password", "--from-1password"], &[("FAKE_NO_APPLE", "1")]);
    assert_eq!(code, Some(1));
    assert!(err.contains("no 1Password login saved"), "{err}");
    let (code, err, _) = run(&["set-password", "--from-1password", "Nope"], &[]);
    assert_eq!(code, Some(1));
    assert!(err.contains("no 1Password item") && err.contains("\"Nope\""), "{err}");
    assert_eq!(stored(), 0);

    // A wrong password from a manager is refused like a typed one.
    autofill.set_good("something else");
    let (code, err, _) = run(&["set-password", "--from-bitwarden"], &[]);
    assert_eq!(code, Some(1));
    assert!(err.contains("refused"), "{err}");
    assert_eq!(stored(), 0);

    // Not installed.
    let empty = tempfile::tempdir().unwrap();
    for (flag, hint) in [
        ("--from-bitwarden", "pacman -S bitwarden-cli"),
        ("--from-1password", "omarchy-install-service-1password"),
    ] {
        let out = env.cli_with(
            &["set-password", flag],
            "",
            &[("PATH", &empty.path().display().to_string())],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("not installed") && stderr.contains(hint), "{stderr}");
    }
}

#[test]
fn the_call_that_starts_the_daemon_already_sees_the_stored_password() {
    // `icloud-session status` starts the daemon and reads its properties at
    // once: the keyring look must be done by then, not announced later.
    let (_accepted, server) = accepting("auto1");
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    env.store_password(PASSWORD);
    let out = env.cli(&["status"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["signed_in"], true, "{printed}");
    assert_eq!(printed["find_my_password_stored"], true, "{printed}");
}
