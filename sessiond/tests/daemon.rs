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

/// The CLI: the daemon's executable run as `icloud-session`, through a
/// symlink as the package installs it.
fn cli_bin() -> &'static Path {
    static CLI: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CLI.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("icloud-session");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(DAEMON, &link).unwrap();
        link
    })
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Waits for `cond`. `timeout` only bounds a failure, so it is at least
/// 30 s: a loaded CI runner can be that slow, a passing test never waits.
fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let timeout = timeout.max(Duration::from_secs(30));
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(25));
    }
}

// ----------------------------------------------------------------- watch

/// A [`icloud_session::Watch`] read on its own thread, so every wait on it
/// is bounded.
struct Changes {
    first: icloud_session::Status,
    rx: std::sync::mpsc::Receiver<icloud_session::Status>,
}

fn changes(conn: &Connection) -> Changes {
    let watch = icloud_session::watch_on(conn).unwrap();
    let first = watch.current().unwrap().clone();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for status in watch {
            if tx.send(status).is_err() {
                break;
            }
        }
    });
    Changes { first, rx }
}

impl Changes {
    /// The first change that satisfies `cond`, skipping the others. The
    /// change a test causes need not be the next one announced: the
    /// start-up `/validate` (rotating `ExpiresAt`) or the keyring look can
    /// land between subscribing and the test's call.
    fn until(&self, what: &str, cond: impl Fn(&icloud_session::Status) -> bool) -> icloud_session::Status {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(status) if cond(&status) => return status,
                Ok(_) => {}
                Err(e) => panic!("no change to {what}: {e}"),
            }
        }
    }

    /// Waits for the sign-in window to open.
    fn window_opened(&self) -> icloud_session::Status {
        self.until("SigningIn true", |s| s.signing_in)
    }

    /// Waits for the sign-in window to close, returning the status then.
    fn window_closed(&self) -> icloud_session::Status {
        self.until("SigningIn false", |s| !s.signing_in)
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

/// Waits until every `/validate` the server has had (answered by
/// [`validate_ok`], token `rotated<n>`) is stored: the daemon answers
/// callers from memory and writes to disk and the keyring after.
fn wait_stored(env: &Env, server: &Server) {
    wait_until("the last /validate to be stored", Duration::from_secs(30), || {
        let token = match server.count(VALIDATE) {
            0 => "original".to_string(),
            n => format!("rotated{n}"),
        };
        env.account_cookie("X-APPLE-WEBAUTH-TOKEN") == Some(token)
    });
}

fn signed_out() -> Reply {
    Reply::json(421, json!({"error": "Misdirected Request"}))
}

/// Apple accepting every session: `/validate` answers [`validate_ok`],
/// anything else 404.
fn apple() -> Server {
    Server::start(|s, n, base| match s.path() {
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    })
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
    handout_secs: f64,
    retry_secs: f64,
    seed: bool,
    /// The daemon's stderr goes to a file, for [`Env::log`], rather than
    /// to the test's.
    log: bool,
    /// What the bus starts for the daemon's name.
    exec: &'a str,
    /// Started through a wrapper that arms the crash or pause point
    /// [`Env::arm`] wrote, for that one start.
    crash: bool,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Opts {
            setup_url: "http://127.0.0.1:9",
            signin: None,
            idle_secs: 60.0,
            validate_secs: 600.0,
            handout_secs: 6.0 * 3600.0,
            retry_secs: 60.0,
            seed: true,
            log: false,
            exec: DAEMON,
            crash: false,
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
        let exec = if opts.crash {
            write_script(
                root,
                "sessiond",
                &format!(
                    r#"for v in CRASH PAUSE; do
  f='{root}/arm-'$v
  [ -e "$f" ] && export ICLOUD_SESSIOND_${{v}}_AT="$(cat "$f")" && rm -f "$f"
done
exec '{DAEMON}' "$@""#,
                    root = root.display()
                ),
            )
        } else {
            opts.exec.to_string()
        };
        fs::write(
            root.join("services/io.github.ferdousbhai.ICloudSession.service"),
            format!("[D-BUS Service]\nName={BUS_NAME}\nExec={exec}\n"),
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
        let stderr = if opts.log {
            Stdio::from(fs::File::create(root.join("daemon.log")).unwrap())
        } else {
            Stdio::inherit()
        };
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
            .env("ICLOUD_SESSIOND_HANDOUT_SECS", opts.handout_secs.to_string())
            .env("ICLOUD_SESSIOND_RETRY_SECS", opts.retry_secs.to_string())
            // Never the real Secret Service: a file in the temp dir.
            .env("ICLOUD_SESSION_TEST_SECRET_FILE", root.join("secrets.json"))
            .stdout(Stdio::piped())
            .stderr(stderr)
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

    /// `account.json` as written.
    fn account_file(&self) -> Option<Value> {
        fs::read(self.account_path())
            .ok()
            .map(|b| serde_json::from_slice(&b).unwrap())
    }

    /// The test keyring's session item, parsed.
    fn session_secret(&self) -> Option<Value> {
        let items = self.all_secrets();
        let item = items
            .as_array()?
            .iter()
            .find(|i| i["attributes"]["kind"] == "session")?;
        Some(serde_json::from_str(item["secret"].as_str()?).unwrap())
    }

    /// The account as stored: `account.json` with the jars (`cookies`,
    /// `find_my`) from the keyring's session item, as the daemon reads it.
    fn account(&self) -> Option<Value> {
        let mut account = self.account_file()?;
        if let Some(secret) = self.session_secret() {
            account["cookies"] = secret["cookies"].clone();
            if let Some(fm) = secret.get("find_my") {
                account["find_my"] = fm.clone();
            }
        }
        Some(account)
    }

    /// The value of cookie `name` in `account.json`'s main jar.
    fn account_cookie(&self, name: &str) -> Option<String> {
        self.account()?["cookies"]
            .as_array()?
            .iter()
            .find(|c| c["name"] == name)
            .and_then(|c| c["value"].as_str())
            .map(str::to_string)
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(cli_bin())
            .args(args)
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .output()
            .unwrap()
    }

    /// The CLI with `stdin` piped in (not a terminal) and `env` set.
    fn cli_with(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> std::process::Output {
        use std::io::Write;
        let mut child = Command::new(cli_bin())
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

    /// The CLI run piped (the default machine mode): its stdout must be the
    /// success envelope; returns `data`.
    /// `status`'s data: its envelope is printed whether signed in (exit 0)
    /// or not (exit 3).
    fn cli_status(&self) -> Value {
        let out = self.cli(&["status"]);
        let data = envelope_data(&out.stdout);
        let want = if data["signed_in"] == true { 0 } else { 3 };
        assert_eq!(out.status.code(), Some(want), "{data}");
        data
    }

    fn cli_data(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        envelope_data(&out.stdout)
    }

    /// What the daemon wrote to stderr (with `Opts::log`).
    fn log(&self) -> String {
        fs::read_to_string(self.root().join("daemon.log")).unwrap_or_default()
    }

    /// Replaces the test keyring's session item's secret, as a keyring
    /// that damaged it would leave it.
    fn damage_session_secret(&self, secret: &str) {
        let mut items = self.all_secrets();
        let item = items
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|i| i["attributes"]["kind"] == "session")
            .expect("a session item");
        item["secret"] = json!(secret);
        fs::write(self.secrets_path(), items.to_string()).unwrap();
    }

    fn secrets_path(&self) -> PathBuf {
        self.root().join("secrets.json")
    }

    /// The test keyring's items.
    fn all_secrets(&self) -> Value {
        fs::read(self.secrets_path())
            .ok()
            .map_or(json!([]), |b| serde_json::from_slice(&b).unwrap())
    }

    /// The test keyring's stored passwords (every item without a `kind`:
    /// not the session, not the device trust).
    fn secrets(&self) -> Value {
        let items = self.all_secrets();
        Value::Array(
            items
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["attributes"].get("kind").is_none())
                .cloned()
                .collect(),
        )
    }

    /// Puts a password in the test keyring, as `set-password` would.
    fn store_password(&self, password: &str) {
        let mut items: Vec<Value> = self.all_secrets().as_array().unwrap().clone();
        items.retain(|i| i["attributes"].get("kind").is_some());
        items.push(json!({
            "label": "iCloud (icloud-session): someone@example.com",
            "attributes": {"application": "icloud-session", "apple-id": "someone@example.com"},
            "secret": password,
        }));
        fs::write(self.secrets_path(), Value::Array(items).to_string()).unwrap();
    }

    /// Sets the seeded account's last `/validate`, as an earlier daemon
    /// would have left it (before this one starts).
    fn seed_validated_at(&self, at: u64) {
        let mut account = self.account().unwrap();
        account["validated_at"] = json!(at);
        fs::write(self.account_path(), account.to_string()).unwrap();
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
        let owner = dbus.get_name_owner(BUS_NAME.try_into().unwrap()).unwrap();
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
        // Gone from the bus, not just dead: until the bus has noticed, a
        // call to the name still goes to the dead connection. (A client may
        // have started the next daemon already.)
        wait_until("the daemon to leave the bus", Duration::from_secs(30), || {
            dbus.get_name_owner(BUS_NAME.try_into().unwrap())
                .map_or(true, |now| now != owner)
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

/// `{"ok":true,"data","summary"}` from stdout: returns `data`.
fn envelope_data(stdout: &[u8]) -> Value {
    let v: Value =
        serde_json::from_slice(stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(stdout)));
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["summary"].is_string(), "{v}");
    v["data"].clone()
}

/// A machine-mode failure: one error document on stdout, nothing on stderr
/// but progress lines. Returns it.
fn machine_error(out: &std::process::Output) -> Value {
    let err: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(err["ok"], false, "{err}");
    let mut keys: Vec<_> = err.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["error", "ok"], "{err}");
    err
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

/// A fake sign-in window: runs `before` (shell), then prints `capture`.
fn printing_window(dir: &Path, before: &str, capture: &Value) -> String {
    write_script(dir, "signin", &format!("{before}\ncat <<'EOF'\n{capture}\nEOF"))
}

/// The pid a fake window wrote to `pid_file`, once it has started.
fn window_pid(pid_file: &Path) -> String {
    wait_until("the window to start", Duration::from_secs(10), || {
        fs::read_to_string(pid_file).is_ok_and(|p| p.ends_with('\n'))
    });
    fs::read_to_string(pid_file).unwrap().trim().to_string()
}

// ----------------------------------------------------------------- tests

#[test]
fn session_returns_cookie_params_webservices_and_revalidates_when_stale() {
    // Validates after the first wait for `later`: on a slow machine the
    // heartbeat would otherwise revalidate (1 s) while this checks the first.
    let later = Arc::new(AtomicBool::new(false));
    let open = later.clone();
    let server = Server::start(move |s, n, base| match s.path() {
        VALIDATE => {
            while n > 1 && !open.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }
            validate_ok(n, base)
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        validate_secs: 1.0,
        // Past this, Session() validates before it answers.
        handout_secs: 1.0,
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

    // Persisted.
    wait_stored(&env, &server);
    let account = env.account().unwrap();
    assert!(account["validated_at"].as_u64().unwrap() >= now() - 5);
    assert_eq!(account["webservices"]["findme"], findme_url(&server.url));
    assert_eq!(
        env.account_cookie("X-APPLE-WEBAUTH-TOKEN"),
        cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN")
    );
    let mode = fs::metadata(env.account_path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(prop::<u64>(&conn, "ExpiresAt") >= now() + 2_592_000 - 10);

    // Stale after a second: the next Session() validates again first.
    later.store(true, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(2100));
    let (cookie, _, _) = session(&conn).unwrap();
    // (The heartbeat may validate too while this client is active.)
    let token = cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").unwrap();
    let n: usize = token.strip_prefix("rotated").unwrap().parse().unwrap();
    assert!(n >= 2, "{token}");
}

#[test]
fn a_recently_validated_jar_is_handed_out_without_asking_apple() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    // An earlier daemon validated a minute ago and idled out since.
    env.seed_validated_at(now() - 60);
    let (cookie, _, _) = session(&env.conn()).unwrap();
    assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("original"));
    thread::sleep(Duration::from_millis(500));
    assert_eq!(server.count(VALIDATE), 0, "neither start-up nor Session() asked Apple");
}

#[test]
fn a_due_jar_is_handed_out_at_once_and_refreshed_behind_the_callers() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => {
            thread::sleep(Duration::from_millis(1000));
            validate_ok(n, base)
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    // Validated an hour ago: due, but well within the 6 hours.
    env.seed_validated_at(now() - 3600);
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
        started.elapsed() < Duration::from_millis(800),
        "no caller waited for the slow /validate: {:?}",
        started.elapsed()
    );
    // One refresh, however start-up and the callers raced, and its jar is
    // what the next caller gets.
    wait_until("the background refresh", Duration::from_secs(5), || {
        env.account_cookie("X-APPLE-WEBAUTH-TOKEN").as_deref() == Some("rotated1")
    });
    let (cookie, _, _) = session(&env.conn()).unwrap();
    assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("rotated1"));
    assert_eq!(server.count(VALIDATE), 1);
    assert!(env.account().unwrap()["validated_at"].as_u64().unwrap() >= now() - 5);
}

#[test]
fn a_background_refresh_that_finds_the_session_ended_signs_out() {
    let server = Server::start(|s, _, _| match s.path() {
        VALIDATE => {
            thread::sleep(Duration::from_millis(500));
            signed_out()
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    env.seed_validated_at(now() - 3600);
    let conn = env.conn();
    let watch = changes(&conn);
    assert!(watch.first.signed_in);
    let started = Instant::now();
    session(&conn).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    // Announced as any /validate's 421 is.
    let change = watch.until("SignedIn false", |s| !s.signed_in);
    assert_eq!(change.dsid, None);
    assert!(env.account().is_none());
    sign_in_required_message(&conn);
}

/// A `/validate` that answers for another account than the one signed in
/// is a failed validate: nothing of it is kept, and nobody is signed out.
#[test]
fn a_validate_that_answers_for_another_account_changes_nothing() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE => {
            let mut r = validate_ok(n, base);
            r.body = r.body.replace(DSID, "67890").replace("someone@", "other@");
            r
        }
        _ => Reply::json(404, json!({})),
    });
    let env = Env::start(Opts {
        setup_url: &server.url,
        log: true,
        ..Default::default()
    });
    let conn = env.conn();
    let (cookie, _, webservices) = session(&conn).unwrap();
    assert_eq!(server.count(VALIDATE), 1);
    assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("original"));
    assert!(webservices.is_empty(), "{webservices:?}");
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(status.signed_in);
    assert_eq!(status.apple_id.as_deref(), Some("someone@example.com"));
    assert_eq!(status.dsid.as_deref(), Some(DSID));
    assert_eq!(status.full_name, None);
    let account = env.account().unwrap();
    assert_eq!(account["apple_id"], "someone@example.com");
    assert_eq!(account["validated_at"], 0);
    let log = env.log();
    assert!(
        log.contains("/validate answered for another account (dsid 67890, signed in as 12345)"),
        "{log}"
    );
    assert!(!log.contains("rotated") && !log.contains("original"), "{log}");
}

#[test]
fn an_unreachable_apple_answers_at_once() {
    // Nothing listens on the default setup URL: the connect is refused.
    let env = Env::start(Opts {
        retry_secs: 2.0,
        ..Default::default()
    });
    let started = Instant::now();
    let (cookie, _, _) = session(&env.conn()).unwrap();
    assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("original"));
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

#[test]
fn merge_cookies_merges_by_name_and_persists() {
    let server = apple();
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
    assert_eq!(server.count(VALIDATE), 1);
}

/// `MergeCookiesFor` and `MergeFindMyCookiesFor` name the account the
/// cookies were set for: the daemon merges them into that account's jars
/// and refuses, merging nothing, when it holds another account or none.
#[test]
fn merges_that_name_another_account_are_refused() {
    let server = find_my_server(Arc::new(Mutex::new("fmip1".into())), 0);
    let dir = tempfile::tempdir().unwrap();
    let signin = find_window(dir.path(), &find_capture(DSID), "0");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().find_my_authorized);
    let merge = |method: &str, dsid: &str, cookie: &str| {
        conn.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(INTERFACE),
            method,
            &(dsid, vec![cookie]),
        )
        .map(drop)
    };
    let refused = |result: zbus::Result<()>| match result {
        Err(zbus::Error::MethodError(name, _, _)) => name.as_str() == icloud_session::ERROR_SIGN_IN_REQUIRED,
        _ => false,
    };

    merge("MergeCookiesFor", DSID, "MINE=1; Path=/").unwrap();
    merge("MergeFindMyCookiesFor", DSID, "MINE-FM=1; Path=/").unwrap();
    assert!(refused(merge("MergeCookiesFor", "67890", "THEIRS=1; Path=/")));
    assert!(refused(merge("MergeFindMyCookiesFor", "67890", "THEIRS-FM=1; Path=/")));
    let account = env.account().unwrap();
    assert!(cookie_named(&account["cookies"], "MINE").is_some());
    assert!(cookie_named(&account["find_my"]["cookies"], "MINE-FM").is_some());
    assert!(!account.to_string().contains("THEIRS"), "{account}");

    // Signed out, even the account that was signed in is refused, where
    // `MergeCookies` has nothing to do.
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(refused(merge("MergeCookiesFor", DSID, "LATE=1; Path=/")));
    assert!(refused(merge("MergeFindMyCookiesFor", DSID, "LATE=1; Path=/")));
    conn.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(INTERFACE),
        "MergeCookies",
        &(vec!["LATE=1"],),
    )
    .unwrap();
    assert!(env.account().is_none());
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
    // fresh jar is saved, the answer is true.
    let still: bool = call(&conn, "ReportSignInRequired").unwrap();
    assert!(still);
    assert!(prop::<bool>(&conn, "SignedIn"));
    assert_eq!(server.count(VALIDATE), 2);
    assert_eq!(env.account_cookie("X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("rotated2"));

    // Now Apple answers 421: signed out, announced, account forgotten.
    let watch = changes(&conn);
    expired.store(true, Ordering::SeqCst);
    let still: bool = call(&conn, "ReportSignInRequired").unwrap();
    assert!(!still);
    let change = watch.until("SignedIn false", |s| !s.signed_in);
    assert!(!change.signed_in);
    assert_eq!(change.apple_id, None);
    assert_eq!(change.dsid, None);
    assert_eq!(change.expires_at, None);
    assert!(!prop::<bool>(&conn, "SignedIn"));
    assert_eq!(prop::<String>(&conn, "AppleId"), "");
    assert!(env.account().is_none());
    sign_in_required_message(&conn);
}

const SESSION_ENDED: &str =
    "Apple ended the iCloud session (it expired, the password was changed, or it was signed out on another device)";

/// The `SignInRequired` error's message, from `Session()`, which must
/// answer with that error.
fn sign_in_required_message(conn: &Connection) -> String {
    match session(conn) {
        Err(zbus::Error::MethodError(name, msg, _)) if name.as_str() == ERROR_SIGN_IN_REQUIRED => msg.unwrap(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn apple_ending_the_session_says_why_until_a_sign_in_even_across_a_restart() {
    let expired = Arc::new(AtomicBool::new(false));
    let flag = expired.clone();
    let server = Server::start(move |s, n, base| match s.path() {
        VALIDATE if flag.load(Ordering::SeqCst) => signed_out(),
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let capture =
        json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com", "expires": null}]});
    let signin = printing_window(dir.path(), "", &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let reason_file = env.root().join("state/icloud-session/sign-out-reason");
    let conn = env.conn();
    session(&conn).unwrap();
    assert_eq!(prop::<String>(&conn, "SignOutReason"), "");

    let watch = changes(&conn);
    expired.store(true, Ordering::SeqCst);
    assert!(!call::<bool>(&conn, "ReportSignInRequired").unwrap());
    let change = watch.until("SignedIn false", |s| !s.signed_in);
    assert_eq!(change.sign_out_reason.as_deref(), Some(SESSION_ENDED));
    assert_eq!(prop::<String>(&conn, "SignOutReason"), SESSION_ENDED);
    assert_eq!(sign_in_required_message(&conn), SESSION_ENDED);
    let out = env.cli(&["validate", "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let err = machine_error(&out);
    assert_eq!(
        err["error"]["message"],
        format!("sign in to iCloud required: {SESSION_ENDED}")
    );

    // The daemon exits when idle: the next one still says why.
    env.kill_daemon(&conn);
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signed_in);
    assert_eq!(status.sign_out_reason.as_deref(), Some(SESSION_ENDED));
    assert_eq!(sign_in_required_message(&conn), SESSION_ENDED);

    // A sign-in clears it, in memory and on disk.
    expired.store(false, Ordering::SeqCst);
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    let done = watch.window_closed();
    assert!(done.signed_in, "{done:?}");
    assert_eq!(done.sign_out_reason, None);
    assert_eq!(prop::<String>(&conn, "SignOutReason"), "");
    assert!(!reason_file.exists());
    env.kill_daemon(&conn);
    assert_eq!(icloud_session::status_on(&conn).unwrap().sign_out_reason, None);
}

#[test]
fn a_session_item_removed_while_the_daemon_was_away_says_so() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    assert!(env.session_secret().is_some());

    // Deleted in Seahorse, or a sign-out elsewhere, before the next start.
    env.kill_daemon(&conn);
    fs::write(env.secrets_path(), "[]").unwrap();
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signed_in);
    let removed = "The iCloud session was removed from the keyring";
    assert_eq!(status.sign_out_reason.as_deref(), Some(removed));
    assert_eq!(sign_in_required_message(&conn), removed);

    // A sign-out with nobody signed in leaves the reason as it was.
    icloud_session::sign_out_on(&conn).unwrap();
    assert_eq!(prop::<String>(&conn, "SignOutReason"), removed);
}

#[test]
fn sign_in_with_a_fake_window_stores_the_account() {
    let server = apple();
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
    let signin = printing_window(dir.path(), "sleep 0.3", &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    assert_eq!(
        watch.first,
        icloud_session::Status {
            signed_in: false,
            apple_id: None,
            full_name: None,
            dsid: None,
            expires_at: None,
            signing_in: false,
            find_my_authorized: false,
            find_my_password_stored: false,
            sign_out_reason: None,
        }
    );
    sign_in_required_message(&conn);

    icloud_session::sign_in_on(&conn).unwrap();
    let opened = watch.window_opened();
    assert!(!opened.signed_in, "{opened:?}");
    // A second SignIn while the window is open does not open another.
    icloud_session::sign_in_on(&conn).unwrap();
    let done = watch.window_closed();
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

    let captured = account["captured_at"].as_str().unwrap();
    assert!(captured.ends_with('Z') && captured.len() == 24, "{captured}");
    // icloud-md's mirror was retired: nothing is written for it.
    assert!(!env.root().join("home/.config/icloud-md").exists());

    // The session works through the client lib at once.
    let s = Session::connect_on(&conn).unwrap();
    assert_eq!(s.dsid(), DSID);
    assert_eq!(s.webservices().unwrap().url("ckdatabasews"), Some(server.url.as_str()));
}

#[test]
fn sign_in_closed_or_without_params() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("closed-once");
    // First run: the user closes the window. Second: a capture without params.
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com"}]});
    let closed_once = format!("if [ ! -e '{m}' ]; then touch '{m}'; exit 1; fi", m = marker.display());
    let signin = printing_window(dir.path(), &closed_once, &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    let closed = watch.window_closed();
    assert!(!closed.signed_in, "{closed:?}");
    // Closed by the person: nothing went wrong to report.
    assert_eq!(closed.sign_out_reason, None);
    assert_eq!(server.count(VALIDATE), 0);

    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    let done = watch.window_closed();
    assert!(done.signed_in, "{done:?}");
    let q = server.requests(VALIDATE)[0].query();
    assert_eq!(q["clientBuildNumber"], "2624Build27");
    assert_eq!(q["clientMasteringNumber"], "2624Build27");
    assert_eq!(q["clientId"].len(), 36, "a fresh UUID");
}

/// A sign-in the person finished that could not be kept says why, where
/// they look and across a restart, rather than only in the journal.
#[test]
fn a_sign_in_that_cannot_be_kept_says_why() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let capture =
        json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com", "expires": null}]});
    let signin = printing_window(dir.path(), "", &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    // The keyring refuses the session: a directory where its file goes.
    fs::create_dir(env.secrets_path()).unwrap();
    let out = env.cli(&["sign-in", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "sign_in_not_completed", "{err}");
    let message = err["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("The last sign-in could not be kept: storing the session's cookies in the keyring failed"),
        "{message}"
    );
    assert_eq!(err["error"]["hint"], icloud_session::output::KEYRING_HINT, "{err}");
    let reason = env.cli_status()["sign_out_reason"].as_str().unwrap().to_string();
    assert!(reason.contains("keyring"), "{reason}");
    assert_eq!(
        fs::read_to_string(env.root().join("state/icloud-session/sign-out-reason"))
            .unwrap()
            .trim(),
        reason
    );

    // A window that cannot start says how to get it back.
    let env = Env::start(Opts {
        signin: Some("/nonexistent-signin"),
        seed: false,
        ..Default::default()
    });
    let err = machine_error(&env.cli(&["sign-in", "--json"]));
    assert_eq!(err["error"]["code"], "sign_in_not_completed", "{err}");
    let message = err["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("is not installed; reinstall icloud-session"),
        "{message}"
    );
    assert_eq!(
        err["error"]["hint"], "run `icloud-session sign-in` again and finish in the window",
        "{err}"
    );
}

#[test]
fn sign_out_forgets_account_and_profile() {
    let server = apple();
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
    let watch = changes(&conn);
    icloud_session::sign_out_on(&conn).unwrap();
    watch.until("SignedIn false", |s| !s.signed_in);
    assert!(env.account().is_none());
    assert!(env.session_secret().is_none(), "the keyring's session is gone too");
    assert!(!webkit_data.exists());
    assert!(!webkit_cache.exists());
    assert!(matches!(Session::connect_on(&conn), Err(Error::SignInRequired)));
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(!status.signed_in);
    assert_eq!(status.sign_out_reason.as_deref(), Some("Signed out of iCloud"));
}

/// The test keyring's device-trust item, its cookies parsed.
fn trust_item(env: &Env) -> Option<(String, Value)> {
    let items = env.all_secrets();
    let item = items.as_array()?.iter().find(|i| i["attributes"]["kind"] == "trust")?;
    let trust: Value = serde_json::from_str(item["secret"].as_str()?).unwrap();
    // `{dsid, cookies}`; a bare list from before trusts named their account.
    let cookies = trust.get("cookies").cloned().unwrap_or(trust);
    Some((item["label"].as_str()?.to_string(), cookies))
}

/// The account the test keyring's device trust names, if it names one.
fn trust_dsid(env: &Env) -> Option<String> {
    let items = env.all_secrets();
    let item = items.as_array()?.iter().find(|i| i["attributes"]["kind"] == "trust")?;
    let trust: Value = serde_json::from_str(item["secret"].as_str()?).unwrap();
    trust["dsid"].as_str().map(str::to_string)
}

/// The names, sorted, of the cookies in the test keyring's device-trust
/// item, which is labelled with the account.
fn trust_names(env: &Env) -> Vec<String> {
    let (label, trust) = trust_item(env).expect("a trust item");
    assert_eq!(label, "iCloud device trust (icloud-session): someone@example.com");
    let mut names: Vec<String> = trust
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

/// The cookies a fake window run got on its stdin (`--cookies`).
fn given_cookies(dir: &Path, run: usize) -> Value {
    let stdin = fs::read_to_string(dir.join(format!("stdin-{run}"))).unwrap();
    serde_json::from_str(stdin.lines().next().unwrap_or("null")).unwrap()
}

#[test]
fn device_trust_is_kept_in_the_keyring_and_the_old_profile_goes() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({
        "cookies": [
            {"name": "X-APPLE-WEBAUTH-USER", "value": "\"v=1:s=0:d=12345\"", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "captured", "domain": ".icloud.com", "path": "/", "expires": now() + 1000},
            {"name": "X-APPLE-WEBAUTH-HSA-TRUST", "value": "hsa", "domain": ".icloud.com", "path": "/", "expires": now() + 86400},
        ],
        "trust": [
            {"name": "X-APPLE-WEBAUTH-HSA-TRUST", "value": "hsa", "domain": ".icloud.com", "path": "/", "expires": now() + 86400},
            {"name": "DES6a1b2c", "value": "HSARMTKNSRVXWFlatrust", "domain": "idmsa.apple.com", "path": "/", "expires": now() + 86400},
            // Not trust, though the window said so: never kept.
            {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "captured", "domain": ".icloud.com", "path": "/", "expires": null},
            {"name": "myacinfo", "value": "apple-only", "domain": ".apple.com", "path": "/", "expires": null},
        ],
        "clientId": "page-client-id",
    });
    // Records its arguments and stdin per run.
    let signin = write_script(
        dir.path(),
        "signin",
        &format!(
            r#"d='{d}'
n=$(cat "$d/count" 2>/dev/null || echo 0); echo $((n + 1)) > "$d/count"
echo "$*" > "$d/args-$n"
cat > "$d/stdin-$n"
cat <<'EOF'
{capture}
EOF"#,
            d = dir.path().display()
        ),
    );
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    // The profile an older window left: the whole web session in plaintext.
    let webkit_data = env.root().join("data/icloud-session/webkit");
    fs::create_dir_all(&webkit_data).unwrap();
    fs::write(webkit_data.join("cookies.sqlite"), "plaintext").unwrap();

    let conn = env.conn();
    let watch = changes(&conn);
    assert!(!webkit_data.exists(), "removed at start");

    // The first window gets no cookies: nothing is trusted yet.
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().signed_in);
    assert_eq!(
        fs::read_to_string(dir.path().join("args-0")).unwrap().trim(),
        "--cookies"
    );
    assert_eq!(given_cookies(dir.path(), 0), json!([]));
    assert!(!webkit_data.exists(), "the window writes no profile");

    // Only the trust cookies are kept, in their own keyring item.
    assert_eq!(trust_names(&env), ["DES6a1b2c", "X-APPLE-WEBAUTH-HSA-TRUST"]);
    assert!(env.secrets().as_array().unwrap().is_empty(), "no password item");

    // The next window starts with them, and the account's jar.
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    watch.window_closed();
    let given = given_cookies(dir.path(), 1);
    assert_eq!(cookie_named(&given, "DES6a1b2c").unwrap()["domain"], "idmsa.apple.com");
    assert_eq!(
        cookie_named(&given, "X-APPLE-WEBAUTH-HSA-TRUST").unwrap()["value"],
        "hsa"
    );
    assert!(cookie_named(&given, "X-APPLE-WEBAUTH-TOKEN").is_some());
    assert!(cookie_named(&given, "myacinfo").is_none());

    // ForgetPassword keeps them; SignOut does not.
    call::<()>(&conn, "ForgetPassword").unwrap();
    assert!(trust_item(&env).is_some());
    icloud_session::sign_out_on(&conn).unwrap();
    watch.until("SignedIn false", |s| !s.signed_in);
    assert!(trust_item(&env).is_none(), "signing out forgets this browser");
}

/// A sign-out the keyring refused still owes the device trust its removal:
/// a sign-in later in the same daemon's life, with the keyring back, must
/// not hand the signed-out account's trust to its window.
#[test]
fn a_sign_in_after_a_sign_out_the_keyring_refused_starts_without_the_old_trust() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({
        "cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com", "expires": null}],
        "trust": [{"name": "DES6a1b2c", "value": "trusted", "domain": "idmsa.apple.com", "path": "/", "expires": now() + 86400}],
    });
    let record = format!(
        "d='{}'\nn=$(cat \"$d/count\" 2>/dev/null || echo 0); echo $((n + 1)) > \"$d/count\"\ncat > \"$d/stdin-$n\"",
        dir.path().display()
    );
    let signin = printing_window(dir.path(), &record, &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().signed_in);
    assert_eq!(trust_names(&env), ["DES6a1b2c"]);

    // The keyring stops answering, and the user signs out.
    let keyring = fs::read(env.secrets_path()).unwrap();
    fs::remove_file(env.secrets_path()).unwrap();
    fs::create_dir(env.secrets_path()).unwrap();
    assert!(matches!(
        icloud_session::sign_out_on(&conn),
        Err(Error::KeyringUnavailable(_))
    ));
    watch.until("SignedIn false", |s| !s.signed_in);

    // It answers again, the trust still in it, and someone signs in.
    fs::remove_dir(env.secrets_path()).unwrap();
    fs::write(env.secrets_path(), &keyring).unwrap();
    assert!(trust_item(&env).is_some());
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().signed_in);
    assert_eq!(
        given_cookies(dir.path(), 1),
        json!([]),
        "the window got the signed-out trust"
    );
}

/// The same, with the keyring still refusing when the sign-in starts and
/// answering again while its window is open: the session is stored, so the
/// signed-out account's trust must go before the owed removal is forgotten.
#[test]
fn a_keyring_back_during_a_sign_in_does_not_keep_the_signed_out_trust() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let token = json!({"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com", "expires": null});
    let trusted = json!({
        "cookies": [token],
        "trust": [{"name": "DES6a1b2c", "value": "trusted", "domain": "idmsa.apple.com", "path": "/", "expires": now() + 86400}],
    });
    let untrusted = json!({"cookies": [token]});
    // With `restore` there, the window runs it (the keyring answers again
    // while the window is open) and captures no trust.
    let restoring = format!(
        "d='{}'\nif [ -e \"$d/restore\" ]; then . \"$d/restore\"; cat > \"$d/stdin\"; cat <<'EOF'\n{untrusted}\nEOF\nexit 0; fi",
        dir.path().display()
    );
    let signin = printing_window(dir.path(), &restoring, &trusted);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().signed_in);
    assert_eq!(trust_names(&env), ["DES6a1b2c"]);

    // The keyring stops answering, and the user signs out.
    let saved = dir.path().join("keyring");
    fs::rename(env.secrets_path(), &saved).unwrap();
    fs::create_dir(env.secrets_path()).unwrap();
    assert!(matches!(
        icloud_session::sign_out_on(&conn),
        Err(Error::KeyringUnavailable(_))
    ));
    watch.until("SignedIn false", |s| !s.signed_in);
    let pending = env.root().join("state/icloud-session/session-removal-pending");
    assert!(pending.exists());

    // Still refusing when the sign-in starts; back before it is stored.
    fs::write(
        dir.path().join("restore"),
        format!(
            "rmdir '{k}' && mv '{s}' '{k}'\n",
            k = env.secrets_path().display(),
            s = saved.display()
        ),
    )
    .unwrap();
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().signed_in);
    let given: Value = serde_json::from_str(fs::read_to_string(dir.path().join("stdin")).unwrap().trim()).unwrap();
    assert_eq!(given, json!([]));
    assert!(env.session_secret().is_some(), "the sign-in was kept");
    assert!(trust_item(&env).is_none(), "the signed-out trust is still stored");
    assert!(!pending.exists());
}

/// The cookies.sqlite a v0.9.4 sign-in window left (libsoup's
/// `moz_cookies`), with `rows` of (name, value, host, expiry).
fn old_profile(env: &Env, rows: &[(&str, &str, &str, u64)]) -> PathBuf {
    let webkit_data = env.root().join("data/icloud-session/webkit");
    fs::create_dir_all(&webkit_data).unwrap();
    let db = rusqlite::Connection::open(webkit_data.join("cookies.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, name TEXT, value TEXT, host TEXT, path TEXT, \
         expiry INTEGER, lastAccessed INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER)",
    )
    .unwrap();
    for (name, value, host, expiry) in rows {
        db.execute(
            "INSERT INTO moz_cookies (name, value, host, path, expiry, lastAccessed, isSecure, isHttpOnly, sameSite) \
             VALUES (?1, ?2, ?3, '/', ?4, 0, 1, 1, 0)",
            rusqlite::params![name, value, host, *expiry as i64],
        )
        .unwrap();
    }
    webkit_data
}

/// The names of the cookies left in an old profile's cookies.sqlite.
fn old_profile_names(webkit_data: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(webkit_data.join("cookies.sqlite")).unwrap();
    let mut names: Vec<String> = db
        .prepare("SELECT name FROM moz_cookies")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names.sort();
    names
}

/// A v0.9.4 profile's rows: Apple's trust token (`DES…`, which only ever
/// lived there), icloud.com's mark, an expired trust and the session.
fn old_rows() -> [(&'static str, &'static str, &'static str, u64); 5] {
    [
        ("DES6a1b2c", "HSARMTKNSRVXWFlaold", "idmsa.apple.com", now() + 86400),
        ("X-APPLE-WEBAUTH-HSA-TRUST", "hsa-old", ".icloud.com", now() + 86400),
        ("DESexpired", "gone", "idmsa.apple.com", now() - 10),
        ("X-APPLE-WEBAUTH-TOKEN", "old-token", ".icloud.com", now() + 1000),
        ("myacinfo", "apple-session", ".apple.com", now() + 1000),
    ]
}

#[test]
fn the_old_profile_s_device_trust_moves_to_the_keyring() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let signin = write_script(
        dir.path(),
        "signin",
        &format!("cat > '{}/stdin-0'\necho '{{}}'", dir.path().display()),
    );
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let webkit_data = old_profile(&env, &old_rows());

    let conn = env.conn();
    icloud_session::status_on(&conn).unwrap();
    wait_until("the old profile's removal", Duration::from_secs(5), || {
        !webkit_data.exists()
    });
    assert_eq!(trust_names(&env), ["DES6a1b2c", "X-APPLE-WEBAUTH-HSA-TRUST"]);
    let (_, trust) = trust_item(&env).unwrap();
    let des = cookie_named(&trust, "DES6a1b2c").unwrap();
    assert_eq!(des["domain"], "idmsa.apple.com");
    assert_eq!(des["value"], "HSARMTKNSRVXWFlaold");

    // The next window starts with it, so Apple can skip 2FA.
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    watch.window_closed();
    let given = given_cookies(dir.path(), 0);
    assert_eq!(
        cookie_named(&given, "DES6a1b2c").unwrap()["value"],
        "HSARMTKNSRVXWFlaold"
    );
}

#[test]
fn a_keyring_that_refuses_the_old_trust_keeps_only_it_until_the_next_start() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let webkit_data = old_profile(&env, &old_rows());
    let webkit_cache = env.root().join("cache/icloud-session/webkit");
    fs::create_dir_all(&webkit_cache).unwrap();
    // The test keyring cannot be read: a directory where its file goes.
    fs::create_dir(env.secrets_path()).unwrap();

    let conn = env.conn();
    icloud_session::status_on(&conn).unwrap();
    // The session leaves the plaintext file at once; the trust waits.
    wait_until("the old session's removal", Duration::from_secs(5), || {
        old_profile_names(&webkit_data) == ["DES6a1b2c", "X-APPLE-WEBAUTH-HSA-TRUST"]
    });
    assert!(!webkit_cache.exists());

    // The keyring answers by the next start: the trust moves, the file goes.
    fs::remove_dir(env.secrets_path()).unwrap();
    env.kill_daemon(&conn);
    icloud_session::status_on(&conn).unwrap();
    wait_until("the old profile's removal", Duration::from_secs(5), || {
        !webkit_data.exists()
    });
    let (_, trust) = trust_item(&env).expect("the old profile's trust kept");
    assert!(cookie_named(&trust, "DES6a1b2c").is_some());
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

    // A file, streamed: the body arrives whole with its length and its
    // content type, and it carries the jar and params like post_json.
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
    s.post_file(&format!("{}/upload?sig=2", content.url), "image/jpeg", &dest)
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
fn one_executable_answers_to_both_names() {
    let version = |bin: &Path| {
        let out = Command::new(bin).arg("--version").env_clear().output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let v = env!("CARGO_PKG_VERSION");
    assert_eq!(version(Path::new(DAEMON)), format!("icloud-sessiond {v}\n"));
    assert_eq!(version(cli_bin()), format!("icloud-session {v}\n"));
    // Run as the daemon it takes no commands; as the CLI it does.
    // The daemon (not the CLI) rejects stray args with its own usage exit 64,
    // which is internal to D-Bus activation and not the shared CLI contract.
    let out = Command::new(DAEMON).arg("status").env_clear().output().unwrap();
    assert_eq!(out.status.code(), Some(64), "{out:?}");
}

#[test]
fn cli_status_validate_sign_in_and_sign_out() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let capture =
        json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "t", "domain": ".icloud.com", "expires": null}]});
    let signin = printing_window(dir.path(), "", &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });

    // Signed out: the status is still printed, with exit 3.
    let out = env.cli(&["status"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stderr.is_empty());
    let status = envelope_data(&out.stdout);
    let quiet: Value = serde_json::from_slice(&env.cli(&["status", "--quiet"]).stdout).unwrap();
    assert_eq!(quiet, status);
    assert_eq!(
        status,
        json!({"signed_in": false, "apple_id": null, "full_name": null, "dsid": null, "expires_at": null, "signing_in": false, "find_my_authorized": false, "find_my_password_stored": false, "sign_out_reason": null})
    );

    // A machine-mode error is one JSON document on stdout, nothing on
    // stderr, in every machine mode (piped is one).
    for mode in [&[][..], &["--json"], &["--quiet"], &["--count"], &["--ids-only"]] {
        let out = env.cli(&[&["validate"], mode].concat());
        assert_eq!(out.status.code(), Some(3), "{mode:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{mode:?}");
        let err = machine_error(&out);
        assert_eq!(err["error"]["code"], "sign_in_required");
        let mut keys: Vec<_> = err["error"].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["code", "hint", "message"], "{err}");
        assert!(
            err["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("icloud-session sign-in")
        );
    }
    // Text mode: error:/hint: lines on stderr, nothing on stdout.
    let out = env.cli(&["validate", "--styled"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("error: sign in to iCloud required"), "{stderr}");
    assert!(stderr.contains("\nhint: run `icloud-session sign-in`"), "{stderr}");
    let out = env.cli(&["status", "--styled"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Not signed in\n");

    let status = env.cli_data(&["sign-in"]);
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["dsid"], DSID);

    // Piped: the envelope; --quiet: only its data, the bare object.
    let out = env.cli(&["validate"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        envelope["summary"].as_str().unwrap().contains("someone@example.com"),
        "{envelope}"
    );
    let v = envelope_data(&out.stdout);
    assert_eq!(v["dsid"], DSID);
    assert_eq!(v["webservices"]["findme"], findme_url(&server.url));
    let quiet: Value = serde_json::from_slice(&env.cli(&["validate", "--quiet"]).stdout).unwrap();
    assert_eq!(quiet, v);
    let quiet: Value = serde_json::from_slice(&env.cli(&["--quiet", "status"]).stdout).unwrap();
    assert_eq!(quiet, env.cli_data(&["status"]));
    assert_eq!(quiet["signed_in"], true);
    // --ids-only: the webservice keys, sorted; --count: how many.
    let mut keys: Vec<String> = v["webservices"].as_object().unwrap().keys().cloned().collect();
    keys.sort();
    let ids = String::from_utf8(env.cli(&["validate", "--ids-only"]).stdout).unwrap();
    assert_eq!(ids.lines().collect::<Vec<_>>(), keys);
    let count = String::from_utf8(env.cli(&["validate", "--count"]).stdout).unwrap();
    assert_eq!(count.trim(), keys.len().to_string());
    // Text: who, and the services.
    let text = String::from_utf8(env.cli(&["validate", "--styled"]).stdout).unwrap();
    assert!(
        text.starts_with(&format!("Apple ID: someone@example.com (dsid {DSID})\nServices: ")),
        "{text}"
    );
    let text = String::from_utf8(env.cli(&["status", "--styled"]).stdout).unwrap();
    assert!(text.starts_with("Signed in as someone@example.com"), "{text}");
    assert!(text.contains("Find My: not authorized"), "{text}");
    assert!(text.contains("Password: not stored"), "{text}");

    // A window that closes without Find My's session: exit 3, the not-signed-in
    // code every iCloud tool uses (the JSON code tells it from a sign-in), and
    // only the error on stdout.
    let out = env.cli(&["authorize-find-my", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "find_my_auth_not_completed");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("still not authorized"),
        "{err}"
    );
    assert!(
        err["error"]["hint"].as_str().unwrap().contains("authorize-find-my"),
        "{err}"
    );

    let out = env.cli(&["sign-out"]);
    assert!(out.status.success());
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["summary"], "Signed out.");
    assert_eq!(envelope["data"]["signed_in"], false);
    let out = env.cli(&["sign-out", "--styled"]);
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Signed out.\nNot signed in"));

    // --no-wait opens the window and answers at once; status shows the rest.
    let _ = env.cli_data(&["sign-in", "--no-wait"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = env.cli_status();
        if status["signed_in"] == true && status["signing_in"] == false {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the window's sign-in never arrived: {status}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    assert_eq!(env.cli(&["bogus"]).status.code(), Some(2));
    let out = env.cli(&["--json", "sign-in", "--later"]);
    assert_eq!(out.status.code(), Some(2));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "usage");
    assert!(out.stderr.is_empty());
    // The password managers' own CLIs pipe into set-password instead.
    assert_eq!(env.cli(&["set-password", "--from-bitwarden"]).status.code(), Some(2));
    // Every command has its own --help.
    for cmd in [
        "status",
        "sign-in",
        "authorize-find-my",
        "set-password",
        "forget-password",
        "sign-out",
        "validate",
    ] {
        let out = env.cli(&[cmd, "--help"]);
        assert!(out.status.success(), "{cmd} --help");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(&format!("Usage: icloud-session {cmd}")), "{text}");
        assert!(text.contains("Exit codes"), "{text}");
    }
}

#[test]
fn idle_exit_waits_for_connected_clients() {
    let server = apple();
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
fn the_cookies_move_to_the_keyring_and_account_json_keeps_none() {
    let server = apple();
    // The seeded account.json is the old kind, cookies and all.
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    env.seed_stale_find_my();
    assert!(env.account_file().unwrap().get("cookies").is_some());
    let conn = env.conn();
    let (cookie, _, _) = session(&conn).unwrap();
    assert!(cookie.contains("X-APPLE-WEBAUTH-USER"));

    let file = env.account_file().unwrap();
    assert!(file.get("cookies").is_none() && file.get("find_my").is_none(), "{file}");
    let secret = env.session_secret().expect("a session item");
    assert_eq!(secret["dsid"], DSID);
    assert!(cookie_named(&secret["cookies"], "X-APPLE-WEBAUTH-USER").is_some());
    assert!(cookie_named(&secret["find_my"]["cookies"], "X-APPLE-WEBAUTH-FMIP").is_some());

    // dsInfo's name, once validated.
    wait_until("FullName", Duration::from_secs(5), || {
        prop::<String>(&conn, "FullName") == "Some One"
    });
    assert_eq!(env.account_file().unwrap()["full_name"], "Some One");

    // Restarted, the daemon reads the jar back from the keyring.
    env.kill_daemon(&conn);
    let (again, _, _) = session(&conn).unwrap();
    assert_eq!(
        cookie_of(&again, "X-APPLE-WEBAUTH-USER"),
        cookie_of(&cookie, "X-APPLE-WEBAUTH-USER")
    );
    assert_eq!(prop::<String>(&conn, "FullName"), "Some One");

    // With the keyring's item gone, account.json alone signs nobody in.
    env.kill_daemon(&conn);
    fs::write(env.secrets_path(), "[]").unwrap();
    assert!(!icloud_session::status_on(&conn).unwrap().signed_in);
    assert!(env.account_file().is_none());
}

/// A state directory that cannot be written keeps the old account.json,
/// cookies and all: the next start must not put those over the session the
/// keyring has kept up to date since.
#[test]
fn an_account_json_that_cannot_be_rewritten_does_not_go_over_the_keyring_s_session() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let state = env.account_path().parent().unwrap().to_path_buf();
    let writable = |on: bool| {
        let mode = if on { 0o700 } else { 0o500 };
        fs::set_permissions(&state, fs::Permissions::from_mode(mode)).unwrap();
    };
    writable(false);
    let conn = env.conn();
    let (cookie, _, _) = session(&conn).unwrap();
    assert_eq!(cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").as_deref(), Some("rotated1"));
    wait_until("the rotated session in the keyring", Duration::from_secs(5), || {
        env.session_secret()
            .is_some_and(|s| cookie_named(&s["cookies"], "X-APPLE-WEBAUTH-TOKEN").unwrap()["value"] == "rotated1")
    });
    // The file could not be rewritten: it still holds the cookies it had.
    let file = env.account_file().unwrap();
    assert_eq!(
        cookie_named(&file["cookies"], "X-APPLE-WEBAUTH-TOKEN").unwrap()["value"],
        "original"
    );

    env.kill_daemon(&conn);
    session(&conn).unwrap();
    writable(true);
    let sent = server.requests(VALIDATE)[1].header("Cookie").unwrap().to_string();
    assert_eq!(
        cookie_of(&sent, "X-APPLE-WEBAUTH-TOKEN").as_deref(),
        Some("rotated1"),
        "the next start went back to account.json's cookies"
    );
}

/// A daemon killed between its keyring write and its `account.json` write
/// leaves a session in the keyring that `account.json` does not go with:
/// a sign-out cut off after removing the file, a sign-in as someone else
/// cut off before writing it. The next start signs nobody in with it, and
/// a `SignOut()` then leaves no session in the keyring.
#[test]
fn a_sign_out_removes_a_session_the_keyring_holds_for_nobody() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    let has_session = || {
        let items = env.all_secrets();
        items
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["attributes"]["kind"] == "session")
    };
    session(&conn).unwrap();
    wait_stored(&env, &server);

    // The sign-out cut off: no account.json, the session still stored.
    env.kill_daemon(&conn);
    let account = fs::read_to_string(env.account_path()).unwrap();
    fs::remove_file(env.account_path()).unwrap();
    assert!(!icloud_session::status_on(&conn).unwrap().signed_in);
    assert!(has_session());
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(!has_session(), "{}", env.all_secrets());

    // The sign-in as someone else cut off: account.json names the first
    // account, the keyring holds the second's session.
    env.kill_daemon(&conn);
    fs::write(env.account_path(), account).unwrap();
    let other = json!({"dsid": "67890", "cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "other"}]});
    fs::write(
        env.secrets_path(),
        json!([{
            "label": "iCloud session (icloud-session): other@example.com",
            "attributes": {"application": "icloud-session", "kind": "session"},
            "secret": other.to_string(),
        }])
        .to_string(),
    )
    .unwrap();
    wait_until("the keyring's look", Duration::from_secs(30), || {
        !icloud_session::status_on(&conn).unwrap().signed_in
    });
    assert!(has_session());
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(!has_session(), "{}", env.all_secrets());
    assert!(!env.account_path().exists());
}

/// `session-change-pending`: what a daemon killed between the keyring and
/// `account.json`, while signing in or out, leaves behind.
fn change_pending(env: &Env) -> PathBuf {
    env.account_path().with_file_name("session-change-pending")
}

fn has_session_item(env: &Env) -> bool {
    let items = env.all_secrets();
    items
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["attributes"]["kind"] == "session")
}

/// Makes the test keyring hold only another account's session.
fn store_another_session(env: &Env) {
    let other = json!({"dsid": "67890", "cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "other"}]});
    fs::write(
        env.secrets_path(),
        json!([{
            "label": "iCloud session (icloud-session): other@example.com",
            "attributes": {"application": "icloud-session", "kind": "session"},
            "secret": other.to_string(),
        }])
        .to_string(),
    )
    .unwrap();
}

fn signed_out_at_start(conn: &Connection) {
    wait_until("the keyring's look", Duration::from_secs(30), || {
        !icloud_session::status_on(conn).unwrap().signed_in
    });
}

/// A daemon killed between the keyring and `account.json` leaves its
/// marker, and the next start finishes or undoes the change by what the
/// keyring holds: a sign-out that had removed the file has the session
/// removed; a sign-in as someone else whose session is stored has its
/// `account.json` written (the marker holds it); one whose session is
/// not is undone.
#[test]
fn a_start_settles_a_cut_off_sign_in_or_sign_out() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    let marker = change_pending(&env);
    session(&conn).unwrap();
    wait_stored(&env, &server);
    assert!(!marker.exists(), "nothing was cut off");
    let account = fs::read_to_string(env.account_path()).unwrap();
    let keyring = fs::read(env.secrets_path()).unwrap();

    // The sign-out: account.json gone, the session still in the keyring.
    env.kill_daemon(&conn);
    fs::remove_file(env.account_path()).unwrap();
    fs::write(&marker, "").unwrap();
    signed_out_at_start(&conn);
    assert!(!has_session_item(&env), "{}", env.all_secrets());
    assert!(!marker.exists());

    // The sign-in as someone else, its session stored: account.json still
    // names the first account, the keyring holds the second's session.
    let other = json!({"apple_id": "other@example.com", "dsid": "67890", "validated_at": now()});
    env.kill_daemon(&conn);
    fs::write(env.account_path(), &account).unwrap();
    store_another_session(&env);
    fs::write(&marker, json!({"account": other}).to_string()).unwrap();
    wait_until("the keyring's look", Duration::from_secs(30), || {
        prop::<String>(&conn, "Dsid") == "67890"
    });
    assert_eq!(env.account_file().unwrap()["apple_id"], "other@example.com");
    let (cookie, _, _) = session(&conn).unwrap();
    assert_eq!(cookie, "X-APPLE-WEBAUTH-TOKEN=other");
    assert!(!marker.exists());

    // The same, cut off before its session was stored.
    env.kill_daemon(&conn);
    fs::write(env.account_path(), &account).unwrap();
    fs::write(env.secrets_path(), &keyring).unwrap();
    fs::write(&marker, json!({"account": other}).to_string()).unwrap();
    wait_until("the keyring's look", Duration::from_secs(30), || {
        prop::<String>(&conn, "Dsid") == DSID
    });
    assert_eq!(env.account_file().unwrap()["dsid"], DSID);
    assert!(!marker.exists());
}

/// A sign-in and a sign-out that finish leave no marker.
#[test]
fn a_finished_sign_in_or_sign_out_leaves_no_marker() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "captured", "domain": ".icloud.com"}]});
    let signin = printing_window(dir.path(), "", &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().signed_in);
    wait_until("account.json", Duration::from_secs(30), || env.account_path().exists());
    assert!(has_session_item(&env));
    wait_until("the marker to go", Duration::from_secs(30), || {
        !change_pending(&env).exists()
    });
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(!has_session_item(&env) && !env.account_path().exists());
    assert!(!change_pending(&env).exists());
}

/// A damaged item is kept for inspection even when a change was cut off.
#[test]
fn a_damaged_session_item_is_kept_whatever_was_cut_off() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    wait_stored(&env, &server);
    for _ in 0..2 {
        env.kill_daemon(&conn);
        env.damage_session_secret("not a session");
        fs::write(change_pending(&env), "").unwrap();
        signed_out_at_start(&conn);
        assert!(has_session_item(&env));
    }
}

/// Another daemon holding the change lock is in the middle of a sign-in or
/// sign-out (one winding down, without the bus name, finishing its window):
/// its session is not cut off, and a daemon starting meanwhile leaves it.
#[test]
fn a_start_leaves_the_session_of_a_change_another_daemon_is_making() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    let marker = change_pending(&env);
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    store_another_session(&env);
    fs::write(&marker, "").unwrap();
    let lock = fs::File::create(marker.with_file_name("change.lock")).unwrap();
    lock.lock().unwrap();
    // The start's look at the keyring is over before it takes the name.
    assert!(!icloud_session::status_on(&conn).unwrap().signed_in);
    assert!(has_session_item(&env) && marker.exists());

    // Killed, its lock goes with it: now it is cut off.
    drop(lock);
    env.kill_daemon(&conn);
    signed_out_at_start(&conn);
    assert!(!has_session_item(&env), "{}", env.all_secrets());
    assert!(!marker.exists());
}

/// A session item whose secret is `damaged` (empty, or not a session)
/// signs out at the next start, and says so; `account.json` is moved
/// aside, not deleted, and the item is left for inspection.
fn a_damaged_session_item_is_reported_and_kept(damaged: &str, says: &str) {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        log: true,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    assert!(env.session_secret().is_some());
    let account = fs::read_to_string(env.account_path()).unwrap();

    env.kill_daemon(&conn);
    env.damage_session_secret(damaged);
    assert!(!icloud_session::status_on(&conn).unwrap().signed_in);
    assert!(matches!(Session::connect_on(&conn), Err(Error::SignInRequired)));

    assert!(!env.account_path().exists());
    assert_eq!(
        fs::read_to_string(env.account_path().with_extension("json.bad")).unwrap(),
        account
    );
    let items = env.all_secrets();
    let item = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["attributes"]["kind"] == "session")
        .expect("the damaged item is kept");
    assert_eq!(item["secret"], damaged);
    let log = env.log();
    assert!(log.contains(says), "{log}");
    assert!(log.contains("account.json.bad"), "{log}");
    assert!(!log.contains("holds no session item"), "{log}");
    if !damaged.is_empty() {
        assert!(!log.contains(damaged), "the log shows the secret: {log}");
    }
}

#[test]
fn an_empty_session_item_is_reported_and_account_json_moved_aside() {
    a_damaged_session_item_is_reported_and_kept("", "the keyring's session item is empty (0 bytes)");
}

#[test]
fn a_corrupt_session_item_is_reported_and_account_json_moved_aside() {
    a_damaged_session_item_is_reported_and_kept(
        "{\"dsid\": \"12345\", \"cook",
        "the keyring's session item is not a session (23 bytes",
    );
}

#[test]
fn a_keyring_that_cannot_be_read_holds_back_the_session_until_it_can() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    // The test keyring cannot be read: a directory where its file goes.
    fs::create_dir(env.secrets_path()).unwrap();
    let conn = env.conn();
    // Still signed in (no sign-in would help), but no jar to hand out.
    match Session::connect_on(&conn) {
        Err(Error::KeyringUnavailable(why)) => assert!(why.contains("keyring"), "{why}"),
        other => panic!("{:?}", other.map(|_| ())),
    }
    let status = icloud_session::status_on(&conn).unwrap();
    assert!(status.signed_in);
    assert_eq!(status.apple_id.as_deref(), Some("someone@example.com"));
    // A client's 421 report gets the same answer, not "signed out".
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE).unwrap();
    match proxy.call::<_, _, bool>("ReportSignInRequired", &()) {
        Err(zbus::Error::MethodError(name, _, _)) => {
            assert_eq!(name.as_str(), icloud_session::ERROR_KEYRING_UNAVAILABLE)
        }
        other => panic!("{other:?}"),
    }
    // Nothing lost: the old account.json, cookies and all, is kept.
    assert!(env.account_file().unwrap().get("cookies").is_some());

    assert!(!prop::<bool>(&conn, "FindMyPasswordStored"));

    // The CLI: a required service unavailable, exit 5, whether through the
    // client library (validate) or a direct daemon call (set-password).
    let out = env.cli(&["validate", "--json"]);
    assert_eq!(out.status.code(), Some(5), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "keyring_unavailable", "{err}");
    let out = env.cli_with(&["set-password", "--json"], "pw\n", &[]);
    assert_eq!(out.status.code(), Some(5), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "keyring_unavailable", "{err}");

    // The keyring answers again (with a password stored): the next
    // Session() moves the cookies, and finds the password.
    fs::remove_dir(env.secrets_path()).unwrap();
    env.store_password("hunter2");
    session(&conn).unwrap();
    assert!(prop::<bool>(&conn, "SignedIn"));
    assert!(prop::<bool>(&conn, "FindMyPasswordStored"));
    assert!(env.account_file().unwrap().get("cookies").is_none());
    assert!(env.session_secret().is_some());
}

#[test]
fn a_sign_out_the_keyring_refuses_says_so_and_the_next_start_finishes_it() {
    let server = apple();
    let env = Env::start(Opts {
        setup_url: &server.url,
        ..Default::default()
    });
    let conn = env.conn();
    session(&conn).unwrap();
    assert!(env.session_secret().is_some());

    // The keyring stops answering, and the user signs out.
    let keyring = fs::read(env.secrets_path()).unwrap();
    fs::remove_file(env.secrets_path()).unwrap();
    fs::create_dir(env.secrets_path()).unwrap();
    match icloud_session::sign_out_on(&conn) {
        Err(Error::KeyringUnavailable(why)) => assert!(why.contains("signed out"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!icloud_session::status_on(&conn).unwrap().signed_in);
    assert!(env.account_file().is_none());
    let pending = env.root().join("state/icloud-session/session-removal-pending");
    assert!(pending.exists());

    // Signing out again while it still refuses says so again.
    assert!(matches!(
        icloud_session::sign_out_on(&conn),
        Err(Error::KeyringUnavailable(_))
    ));

    // It answers again: signing out again removes the live cookies it kept.
    fs::remove_dir(env.secrets_path()).unwrap();
    fs::write(env.secrets_path(), &keyring).unwrap();
    assert!(env.session_secret().is_some());
    icloud_session::sign_out_on(&conn).unwrap();
    assert!(env.session_secret().is_none());
    assert!(!pending.exists());

    // So does the next start, when a sign-out still owes the removal.
    fs::write(env.secrets_path(), &keyring).unwrap();
    fs::write(&pending, "").unwrap();
    env.kill_daemon(&conn);
    assert!(!icloud_session::status_on(&conn).unwrap().signed_in);
    wait_until("the session's removal", Duration::from_secs(5), || {
        env.session_secret().is_none()
    });
    assert!(!pending.exists());
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
    // sign-in that finishes meanwhile must survive it. The window finishes
    // only once that /validate has arrived, and the 421 is checked for only
    // once it has been answered.
    let dir = tempfile::tempdir().unwrap();
    let (seen, answered) = (dir.path().join("seen"), dir.path().join("answered"));
    let (seen_by_server, answered_by_server) = (seen.clone(), answered.clone());
    let server = Server::start(move |s, n, base| match s.path() {
        VALIDATE if s.header("Cookie").unwrap_or_default().contains("=original") => {
            fs::write(&seen_by_server, "").unwrap();
            thread::sleep(Duration::from_millis(1500));
            fs::write(&answered_by_server, "").unwrap();
            signed_out()
        }
        VALIDATE => validate_ok(n, base),
        _ => Reply::json(404, json!({})),
    });
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "captured", "domain": ".icloud.com"}]});
    let wait_seen = format!(
        "i=0; while [ ! -e '{}' ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done",
        seen.display()
    );
    let signin = printing_window(dir.path(), &wait_seen, &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    watch.window_closed();
    assert!(seen.exists(), "the old jar's /validate came first");
    wait_until("the old jar's 421", Duration::from_secs(10), || answered.exists());
    // The daemon's handling of that 421 (a forget, if it were wrong) is
    // immediate; give it a moment to land before checking it did not.
    thread::sleep(Duration::from_millis(300));
    assert!(prop::<bool>(&conn, "SignedIn"), "the 421 was about the old jar");
    let account = env.account().expect("the new account stays");
    assert!(!account.to_string().contains("\"original\""));
}

#[test]
fn sign_out_closes_an_open_sign_in_window() {
    let server = apple();
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "late", "domain": ".icloud.com"}]});
    // The window: records its pid, then "signs in" after a while.
    let slow = format!("echo $$ > '{}'\nsleep 1.5", pid_file.display());
    let signin = printing_window(dir.path(), &slow, &capture);
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let webkit_data = env.root().join("data/icloud-session/webkit");
    fs::create_dir_all(&webkit_data).unwrap();
    let conn = env.conn();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    let pid = window_pid(&pid_file);

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
    let mut cli = Command::new(cli_bin())
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
    assert_eq!(status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    // Only the error on stdout (piped: a machine mode); it says still signed out.
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "sign_in_not_completed", "{err}");
    assert!(
        err["error"]["message"].as_str().unwrap().contains("signed out"),
        "{err}"
    );
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
    let signin = printing_window(dir.path(), "", &capture);
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
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    let done = watch.window_closed();
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

/// A `Session` keeps a `Session()` answer for minutes. With one still in
/// hand after a sign-out and a sign-in as someone else, what Apple sets for
/// the first account never goes into the second's jar, and the second's
/// Find My jar is never sent as the first.
#[test]
fn a_session_still_cached_never_mixes_its_account_with_the_next() {
    let server = Server::start(|s, n, base| match s.path() {
        VALIDATE if s.header("Cookie").unwrap_or_default().contains("=other") => {
            let mut r = validate_ok(n, base);
            r.body = r.body.replace(DSID, "67890").replace("someone@", "other@");
            r
        }
        VALIDATE => validate_ok(n, base),
        "/data" => Reply::json(200, json!({})).cookie("X-APPLE-WEBAUTH-TOKEN=of-the-first; Path=/"),
        INIT_CLIENT => Reply::json(200, json!({"content": []})),
        _ => Reply::json(404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let capture = json!({"cookies": [{"name": "X-APPLE-WEBAUTH-TOKEN", "value": "other", "domain": ".icloud.com"}]});
    let signin = write_script(
        dir.path(),
        "signin",
        &format!(
            "if [ \"$1\" = --find ]; then n=1; cat <<EOF\n{}\nEOF\nelse cat <<'EOF'\n{capture}\nEOF\nfi",
            find_capture("67890")
        ),
    );
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    // Two sessions of the first account, each with its answer cached.
    let (merging, finding) = (Session::connect_on(&conn).unwrap(), Session::connect_on(&conn).unwrap());
    let data = format!("{}/data", merging.webservices().unwrap().url("ckdatabasews").unwrap());
    let init = init_client_url(&finding);

    icloud_session::sign_out_on(&conn).unwrap();
    let watch = changes(&conn);
    icloud_session::sign_in_on(&conn).unwrap();
    watch.window_opened();
    assert_eq!(watch.window_closed().dsid.as_deref(), Some("67890"));
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().find_my_authorized);
    let token = env.account_cookie("X-APPLE-WEBAUTH-TOKEN").unwrap();

    assert!(matches!(merging.get(&data), Err(Error::SignInRequired)));
    assert_eq!(
        env.account_cookie("X-APPLE-WEBAUTH-TOKEN").unwrap(),
        token,
        "the first account's cookie is not merged into the second's jar"
    );
    assert!(matches!(merging.get(&data), Err(Error::SignInRequired)));
    assert_eq!(server.count("/data"), 1, "nothing more is sent");

    assert!(matches!(
        finding.post_json(&init, &json!({})),
        Err(Error::SignInRequired)
    ));
    assert_eq!(
        server.count(INIT_CLIENT),
        0,
        "the second account's Find My jar is not sent as the first"
    );
}

// ------------------------------------------------------------- Find My

const INIT_CLIENT: &str = "/fmipservice/client/web/initClient";

/// The Find My initClient URL of the session's findme service.
fn init_client_url(s: &Session) -> String {
    format!("{}{INIT_CLIENT}", s.webservices().unwrap().url("findme").unwrap())
}

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
    wait_stored(&env, &server);
    let validates = server.count(VALIDATE);
    let before = env.account().unwrap();

    let watch = changes(&conn);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    let done = watch.window_closed();
    assert!(done.signed_in && done.find_my_authorized, "{done:?}");
    assert_eq!(done.dsid.as_deref(), Some(DSID));
    assert_eq!(
        fs::read_to_string(dir.path().join("args")).unwrap().trim(),
        "--find --cookies"
    );

    // Not validated, and the main account is untouched.
    assert_eq!(server.count(VALIDATE), validates);
    let account = env.account().unwrap();
    for key in ["cookies", "client_params", "dsid", "validated_at", "captured_at"] {
        assert_eq!(account[key], before[key], "{key}");
    }
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
    let changed = watch.until("FindMyAuthorized false", |s| !s.find_my_authorized);
    assert!(changed.signed_in, "{changed:?}");
    assert!(env.account().unwrap().get("find_my").is_none());
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(server.count(INIT_CLIENT), sent + 1, "nothing sent without a jar");
    assert!(prop::<bool>(&conn, "SignedIn"));
    assert_eq!(
        fs::read_to_string(dir.path().join("args")).unwrap().trim(),
        "--find --cookies",
        "no automatic sign-in since the manual one"
    );

    // The CLI authorizes again; signing out forgets it.
    *accepted.lock().unwrap() = "fmip2".into();
    let printed = env.cli_data(&["authorize-find-my"]);
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
    let watch = changes(&conn);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().find_my_authorized);
    let s = Session::connect_on(&conn).unwrap();
    let init = init_client_url(&s);

    *accepted.lock().unwrap() = "fmip2".into();
    let s2 = s;
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
    let watch = changes(&conn);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    let done = watch.window_closed();
    assert!(done.signed_in && !done.find_my_authorized, "{done:?}");
    assert_eq!(done.dsid.as_deref(), Some(DSID));
    let account = env.account().unwrap();
    assert!(account.get("find_my").is_none());
    assert!(!account.to_string().contains("onefactor"));
}

/// Signed out there is no account to authorize Find My for: no window
/// opens, and the caller is told to sign in.
#[test]
fn authorize_find_my_while_signed_out_opens_no_window() {
    let dir = tempfile::tempdir().unwrap();
    let signin = find_window(dir.path(), &find_capture(DSID), "0");
    let env = Env::start(Opts {
        signin: Some(&signin),
        seed: false,
        ..Default::default()
    });
    let conn = env.conn();
    assert!(matches!(
        icloud_session::authorize_find_my_on(&conn),
        Err(Error::SignInRequired)
    ));
    let out = env.cli(&["authorize-find-my", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "sign_in_required", "{err}");
    assert_eq!(err["error"]["hint"], icloud_session::output::SIGN_IN_HINT, "{err}");
    let err = machine_error(&env.cli(&["authorize-find-my", "--no-wait", "--json"]));
    assert_eq!(err["error"]["code"], "sign_in_required", "{err}");
    assert!(!prop::<bool>(&conn, "SigningIn"));
    assert!(!dir.path().join("count").exists(), "no window run");
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
    let watch = changes(&conn);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    // A SignIn while the Find My window is open opens no second window.
    icloud_session::sign_in_on(&conn).unwrap();
    let pid_file = dir.path().join("pid");
    let pid = window_pid(&pid_file);

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
            // Trusted, as if Apple asked for a code in the shown window.
            "trust": [
                {"name": "DES0auto", "value": "autotrust", "domain": "idmsa.apple.com", "path": "/", "expires": now() + 86400},
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
echo $$ > "$d/pid"
IFS= read -r jar || {{ echo "no cookies on stdin" >> "$d/bad"; exit 1; }}
IFS= read -r id || {{ echo "no Apple ID on stdin" >> "$d/bad"; exit 1; }}
IFS= read -r pw || {{ echo "no password on stdin" >> "$d/bad"; exit 1; }}
rest=$(cat)
[ "$*" = "--find --autofill --cookies" ] || {{ echo "args: $*" >> "$d/bad"; exit 1; }}
case "$jar" in *'"X-APPLE-WEBAUTH-TOKEN"'*) ;; *) echo "not the account's jar: $jar" >> "$d/bad"; exit 1 ;; esac
[ "$id" = someone@example.com ] || {{ echo "Apple ID: $id" >> "$d/bad"; exit 1; }}
[ -z "$rest" ] || {{ echo "more than two lines on stdin" >> "$d/bad"; exit 1; }}
case "$*" in *"$pw"*) echo "the password in argv" >> "$d/bad"; exit 1 ;; esac
env | grep -qF -- "$pw" && {{ echo "the password in env" >> "$d/bad"; exit 1; }}
printf '%s' "$pw" > "$d/got"
sleep "$(cat "$d/delay" 2>/dev/null || echo 0)" >/dev/null
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
    stored_password_env_retrying(server, password, 0.5)
}

/// [`stored_password_env`] whose failed Find My sign-in is retried after
/// `retry_secs`.
fn stored_password_env_retrying(
    server: &Server,
    password: Option<&str>,
    retry_secs: f64,
) -> (Env, Connection, Autofill) {
    let autofill = Autofill::new();
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&autofill.bin),
        retry_secs,
        ..Default::default()
    });
    env.seed_stale_find_my();
    if let Some(p) = password {
        env.store_password(p);
    }
    let conn = env.conn();
    wait_keyring_look(&conn, password.is_some());
    (env, conn, autofill)
}

/// Waits until the daemon has looked in the keyring for a password and
/// says whether one is `stored`.
fn wait_keyring_look(conn: &Connection, stored: bool) {
    wait_until("the keyring look", Duration::from_secs(5), || {
        prop::<bool>(conn, "FindMyPasswordStored") == stored
    });
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
    // Both requests' stale tries are answered 450 only once both have
    // arrived: neither can start late enough to pick up the new jar.
    let stale = Arc::new(Mutex::new(0usize));
    let server = Server::start(move |s, n, base| {
        let cookie = s.header("Cookie").unwrap_or_default().to_string();
        match s.path() {
            VALIDATE => validate_ok(n, base),
            INIT_CLIENT if cookie.split("; ").any(|c| c == "X-APPLE-WEBAUTH-FMIP=auto1") => {
                Reply::json(200, json!({"content": []})).cookie("FMIP-ROTATED=r; Path=/")
            }
            INIT_CLIENT => {
                *stale.lock().unwrap() += 1;
                wait_until("both stale requests", Duration::from_secs(30), || {
                    *stale.lock().unwrap() >= 2
                });
                Reply {
                    status: 450,
                    body: String::new(),
                    set_cookies: vec![],
                }
            }
            _ => Reply::json(404, json!({})),
        }
    });
    let (env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    autofill.set_delay("0.3");
    let s = Session::connect_on(&conn).unwrap();
    let init = init_client_url(&s);
    wait_stored(&env, &server);
    let before = env.account().unwrap();
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
    // captured; the main jar and the validate count are untouched.
    let account = env.account().unwrap();
    for key in ["cookies", "client_params", "dsid", "validated_at", "captured_at"] {
        assert_eq!(account[key], before[key], "{key}");
    }
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
    wait_keyring_look(&conn, true);
    assert!(!prop::<bool>(&conn, "FindMyAuthorized"));
    let s = Session::connect_on(&conn).unwrap();
    let init = init_client_url(&s);
    s.post_json(&init, &json!({})).unwrap();
    assert_eq!(server.count(INIT_CLIENT), 1, "sent once, with the new jar");
    assert_eq!(autofill.runs(), 1);
    assert!(prop::<bool>(&conn, "FindMyAuthorized"));
}

/// A Find My autofill window can wait minutes on a 2FA code with its
/// caller gone: the daemon neither exits under it nor loses what it
/// captures.
#[test]
fn the_idle_exit_waits_for_the_autofill_window() {
    let (_accepted, server) = accepting("auto1");
    let autofill = Autofill::new();
    autofill.set_delay("3");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&autofill.bin),
        idle_secs: 0.5,
        ..Default::default()
    });
    env.store_password(PASSWORD);
    let observer = env.conn();
    // `FindMySession()` from a process that goes away once the window is
    // up, as a CLI given up on would.
    let mut caller = Command::new("dbus-send")
        .env_clear()
        .env("DBUS_SESSION_BUS_ADDRESS", &env.address)
        .args([
            "--session".to_string(),
            "--print-reply".to_string(),
            format!("--dest={BUS_NAME}"),
            OBJECT_PATH.to_string(),
            format!("{INTERFACE}.FindMySession"),
        ])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    window_pid(&autofill.file("pid"));
    let _ = caller.kill();
    let _ = caller.wait();
    let open = Instant::now();
    while open.elapsed() < Duration::from_millis(2500) {
        assert!(env.daemon_running(&observer), "the daemon left with the window open");
        thread::sleep(Duration::from_millis(50));
    }
    wait_until("the Find My jar", Duration::from_secs(10), || {
        env.account()
            .and_then(|a| a.get("find_my").and_then(|f| find_my_cookie(&f["cookies"])))
            .as_deref()
            == Some("auto1")
    });
    assert_eq!(autofill.runs(), 1);
    wait_until("idle exit", Duration::from_secs(10), || !env.daemon_running(&observer));
}

/// SignOut closes the hidden Find My window too, as it does a sign-in one.
#[test]
fn sign_out_closes_the_autofill_window() {
    let (_accepted, server) = accepting("auto1");
    let autofill = Autofill::new();
    autofill.set_delay("20");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&autofill.bin),
        ..Default::default()
    });
    env.store_password(PASSWORD);
    let conn = env.conn();
    let caller = env.conn();
    let asked = Instant::now();
    let call = thread::spawn(move || call::<(String, HashMap<String, String>)>(&caller, "FindMySession").is_ok());
    let pid_file = autofill.file("pid");
    let pid = window_pid(&pid_file);

    icloud_session::sign_out_on(&conn).unwrap();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the window was killed and reaped"
    );
    assert!(!call.join().unwrap(), "signed out: no Find My jar");
    assert!(
        asked.elapsed() < Duration::from_secs(15),
        "the call did not wait for the window"
    );
    assert!(env.account().is_none());
}

/// The device trust a Find My window captured (the user ticked "Trust this
/// browser" at a 2FA prompt there) is kept as the main sign-in's is.
#[test]
fn find_my_windows_keep_the_device_trust() {
    // AuthorizeFindMy's window (`--find`).
    let server = find_my_server(Arc::new(Mutex::new("fmip1".into())), 0);
    let dir = tempfile::tempdir().unwrap();
    let mut capture: Value = serde_json::from_str(&find_capture(DSID)).unwrap();
    capture["trust"] = json!([
        {"name": "X-APPLE-WEBAUTH-HSA-TRUST", "value": "hsa", "domain": ".icloud.com", "path": "/", "expires": now() + 86400},
        {"name": "DES0find", "value": "findtrust", "domain": "idmsa.apple.com", "path": "/", "expires": now() + 86400},
        // Not trust: never kept.
        {"name": "X-APPLE-WEBAUTH-TOKEN", "value": "onefactor", "domain": ".icloud.com", "path": "/", "expires": null},
    ]);
    let signin = find_window(dir.path(), &capture.to_string(), "0");
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&signin),
        ..Default::default()
    });
    let conn = env.conn();
    assert!(trust_item(&env).is_none());
    let watch = changes(&conn);
    icloud_session::authorize_find_my_on(&conn).unwrap();
    watch.window_opened();
    assert!(watch.window_closed().find_my_authorized);
    assert_eq!(trust_names(&env), ["DES0find", "X-APPLE-WEBAUTH-HSA-TRUST"]);
    icloud_session::sign_out_on(&conn).unwrap();
    watch.until("SignedIn false", |s| !s.signed_in);
    assert!(trust_item(&env).is_none(), "signing out forgets this browser");

    // The stored password's window (`--find --autofill`).
    let (_accepted, server) = accepting("auto1");
    let autofill = Autofill::new();
    let env = Env::start(Opts {
        setup_url: &server.url,
        signin: Some(&autofill.bin),
        ..Default::default()
    });
    env.store_password(PASSWORD);
    let conn = env.conn();
    wait_keyring_look(&conn, true);
    let s = Session::connect_on(&conn).unwrap();
    s.post_json(&init_client_url(&s), &json!({})).unwrap();
    assert_eq!(autofill.runs(), 1);
    assert_eq!(trust_names(&env), ["DES0auto"]);
}

#[test]
fn a_wrong_stored_password_falls_back_to_the_manual_path_without_a_loop() {
    let (accepted, server) = accepting("auto1");
    let (env, conn, autofill) = stored_password_env(&server, Some("wrong"));
    let s = Session::connect_on(&conn).unwrap();
    let init = init_client_url(&s);

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
    // Credentials rejected: exit 3, code password_rejected.
    let out = env.cli_with(&["set-password", "--json"], "also wrong\n", &[]);
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "password_rejected", "{err}");
    assert!(err["error"]["message"].as_str().unwrap().contains("refused"), "{err}");
    assert!(err["error"]["hint"].as_str().unwrap().contains("set-password"), "{err}");
    assert_eq!(env.secrets()[0]["secret"], "wrong");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("also wrong"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("also wrong"));

    // set-password with the right one (stdin, not a terminal): verified
    // with a sign-in, stored, and Find My authorized by it.
    let out = env.cli_with(&["set-password"], &format!("{PASSWORD}\n"), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed = envelope_data(&out.stdout);
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
    // A failure the daemon reports is exit 1, not an unreachable daemon.
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let err = machine_error(&out);
    assert_eq!(err["error"]["code"], "error", "{err}");
    let message = err["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("stored the password, but the Find My sign-in with it failed"),
        "{message}"
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
    let init = init_client_url(&s);
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
    // A back-off long enough that the second request is inside it however
    // loaded the machine is.
    let (_env, conn, autofill) = stored_password_env_retrying(&server, Some(PASSWORD), 10.0);
    autofill.set_failing(true);
    let s = Session::connect_on(&conn).unwrap();
    let init = init_client_url(&s);
    let failed = Instant::now();
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert!(
        failed.elapsed() < Duration::from_secs(10),
        "the machine is too slow for the test"
    );
    assert_eq!(autofill.runs(), 1, "backing off");
    autofill.set_failing(false);
    // Tried again once the back-off is over, and not before.
    wait_until("the retry after the back-off", Duration::from_secs(60), || {
        s.post_json(&init, &json!({})).is_ok()
    });
    assert!(
        failed.elapsed() >= Duration::from_secs(10),
        "retried during the back-off"
    );
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
    assert_eq!(out.status.code(), Some(3));

    let printed = env.cli_data(&["forget-password"]);
    assert_eq!(env.secrets(), json!([]));
    assert_eq!(printed["find_my_password_stored"], false);
    assert_eq!(autofill.runs(), 0);
}

#[test]
fn forget_password_while_signed_in_stops_automatic_sign_in() {
    let (_accepted, server) = accepting("auto1");
    let (env, conn, autofill) = stored_password_env(&server, Some(PASSWORD));
    let watch = changes(&conn);
    let out = env.cli(&["forget-password"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    watch.until("FindMyPasswordStored false", |s| !s.find_my_password_stored);
    let s = Session::connect_on(&conn).unwrap();
    let init = init_client_url(&s);
    assert!(matches!(s.post_json(&init, &json!({})), Err(Error::FindMyAuthRequired)));
    assert_eq!(autofill.runs(), 0);
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
    let printed = env.cli_data(&["status"]);
    assert_eq!(printed["signed_in"], true, "{printed}");
    assert_eq!(printed["find_my_password_stored"], true, "{printed}");
}

/// The CLI's error document (stdout, stderr empty), run with `--json` against `address`.
fn cli_error_at(address: &str, args: &[&str]) -> (Option<i32>, Value) {
    let out = Command::new(cli_bin())
        .args(args)
        .arg("--json")
        .env_clear()
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{args:?}");
    (out.status.code(), machine_error(&out))
}

#[test]
fn an_unreachable_daemon_is_session_service_exit_5() {
    // No session bus at all.
    let dir = tempfile::tempdir().unwrap();
    let nobus = format!("unix:path={}", dir.path().join("no-bus").display());
    for cmd in ["status", "validate", "forget-password", "sign-out"] {
        let (code, err) = cli_error_at(&nobus, &[cmd]);
        assert_eq!(code, Some(5), "{cmd}: {err}");
        assert_eq!(err["error"]["code"], "session_service", "{cmd}: {err}");
    }
    // A bus that cannot start the daemon.
    let env = Env::start(Opts {
        exec: "/bin/false",
        ..Default::default()
    });
    for cmd in ["status", "forget-password"] {
        let (code, err) = cli_error_at(&env.address, &[cmd]);
        assert_eq!(code, Some(5), "{cmd}: {err}");
        assert_eq!(err["error"]["code"], "session_service", "{cmd}: {err}");
    }
}

#[test]
fn skill_prints_the_markdown_and_install_remove_report_paths() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(cli_bin())
            .args(args)
            .env_clear()
            .env("HOME", home.path())
            .output()
            .unwrap()
    };
    // `skill` alone: the Markdown as it is, even piped.
    let out = run(&["skill"]);
    assert!(out.status.success());
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.starts_with("---\n"), "{body}");
    assert_eq!(String::from_utf8(run(&["skill", "--json"]).stdout).unwrap(), body);

    let out = run(&["skill", "install"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let data = envelope_data(&out.stdout);
    let skill_md = home.path().join(".agents/skills/icloud/SKILL.md");
    assert_eq!(fs::read_to_string(&skill_md).unwrap(), body);
    let skill_path = skill_md.to_str().unwrap();
    assert!(
        data["paths"].as_array().unwrap().iter().any(|p| p == skill_path),
        "{data}"
    );
    assert!(data["notices"].is_array(), "{data}");

    let out = run(&["skill", "remove", "--ids-only"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ids = String::from_utf8(out.stdout).unwrap();
    assert!(!ids.trim().is_empty() && !ids.contains('{'), "{ids}");
    assert!(!skill_md.exists());
    let out = run(&["skill", "remove", "--styled"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("Removed nothing"),
        "{out:?}"
    );
}

// ------------------------------------------------------- crash injection

/// The daemon killed at each of its crash points (`ICLOUD_SESSIOND_CRASH_AT`,
/// debug builds only: see `src/crash.rs`) in the middle of each change it
/// makes, then started again. A first run of each change with `trace`
/// lists the points it passes (its start-up's included); then, for each,
/// a fresh run is killed there and the next start must find the stores
/// whole: one account's `account.json`, session, Find My jar and device
/// trust (the old one or the new one), or none and nothing left in the
/// keyring for nobody; and a sign-in and a sign-out must work after it.
///
/// Two accounts: A (`DSID`, cookie values not starting `b-`) and B
/// (`B_DSID`, `b-…`), so a cookie of the wrong account is told apart.
#[cfg(debug_assertions)]
mod crash {
    use super::*;

    pub(super) const B_DSID: &str = "67890";

    pub(super) fn apple_id(dsid: &str) -> &'static str {
        if dsid == B_DSID {
            "other@example.com"
        } else {
            "someone@example.com"
        }
    }

    /// The account a cookie value is A's or B's.
    fn owner(value: &str) -> &'static str {
        if value.starts_with("b-") { B_DSID } else { DSID }
    }

    /// Apple for both accounts: `/validate` answers for the account of the
    /// token it is sent (421 for A's once `ended`), rotating it unless
    /// `still`.
    pub(super) fn two_accounts(ended: Arc<AtomicBool>, still: Arc<AtomicBool>) -> Server {
        Server::start(move |s, n, base| {
            let cookie = s.header("Cookie").unwrap_or_default().to_string();
            let token = cookie_of(&cookie, "X-APPLE-WEBAUTH-TOKEN").unwrap_or_default();
            let dsid = owner(&token);
            match s.path() {
                VALIDATE if dsid == DSID && ended.load(Ordering::SeqCst) => signed_out(),
                VALIDATE => {
                    let who = if dsid == B_DSID { "b" } else { "a" };
                    let reply = Reply::json(
                        200,
                        json!({
                            "dsInfo": {"dsid": dsid, "appleId": apple_id(dsid), "fullName": who},
                            "webservices": {"ckdatabasews": {"url": base}, "findme": {"url": findme_url(base)}},
                        }),
                    );
                    if still.load(Ordering::SeqCst) {
                        return reply;
                    }
                    reply.cookie(&format!(
                        "X-APPLE-WEBAUTH-TOKEN={who}-rotated{n}; Domain=.icloud.com; Path=/; Max-Age=2592000"
                    ))
                }
                _ => Reply::json(404, json!({})),
            }
        })
    }

    /// A fake window for the account named in `<dir>/who` (`a` or `b`):
    /// a sign-in capture, or with `--find` (and `--autofill`) a one-factor
    /// one with Find My's cookie; each with that account's device trust
    /// (none while `<dir>/notrust` exists). While `<dir>/hold` exists it
    /// waits.
    pub(super) fn two_window(dir: &Path) -> String {
        let cookie = |name: &str, value: &str| {
            format!(
                r#"{{"name": "{name}", "value": "{value}", "domain": ".icloud.com", "path": "/", "expires": null}}"#
            )
        };
        let user = cookie("X-APPLE-WEBAUTH-USER", r#"\"v=1:s=0:d=$dsid\""#);
        let trust = r#""trust": [$tc]"#;
        let trust_cookie = format!(
            r#"{{"name": "DES$who", "value": "$who-trust", "domain": "idmsa.apple.com", "path": "/", "expires": {}}}"#,
            now() + 86400
        );
        let end = "EOF";
        write_script(
            dir,
            "signin",
            &format!(
                r#"d='{d}'
cat > /dev/null
while [ -e "$d/hold" ]; do sleep 0.02; done
who=$(cat "$d/who" 2>/dev/null || echo a)
case $who in b) dsid={B_DSID} ;; *) dsid={DSID} ;; esac
tc=
[ -e "$d/notrust" ] || tc=$(cat <<{end}
{trust_cookie}
{end}
)
if [ "$1" = --find ]; then
cat <<{end}
{{"cookies": [{user}, {token1}, {fmip}], {trust}, "dsid": "$dsid", "clientId": "$who-find"}}
{end}
else
cat <<{end}
{{"cookies": [{user}, {token}], {trust}, "clientId": "$who-client"}}
{end}
fi"#,
                d = dir.display(),
                token1 = cookie("X-APPLE-WEBAUTH-TOKEN", "$who-onefactor"),
                fmip = cookie("X-APPLE-WEBAUTH-FMIP", "$who-fmip"),
                token = cookie("X-APPLE-WEBAUTH-TOKEN", "$who-captured"),
            ),
        )
    }

    pub(super) struct Rig {
        pub(super) env: Env,
        pub(super) conn: Connection,
        pub(super) server: Server,
        signin: String,
        pub(super) window: tempfile::TempDir,
        /// Apple ends A's session.
        pub(super) ended: Arc<AtomicBool>,
        /// `/validate` rotates no cookie.
        pub(super) still: Arc<AtomicBool>,
        from: std::cell::Cell<usize>,
    }

    impl Rig {
        pub(super) fn new(seed: bool) -> Rig {
            Rig::idling(seed, 60.0)
        }

        /// A rig whose daemons exit after `idle_secs`.
        pub(super) fn idling(seed: bool, idle_secs: f64) -> Rig {
            let ended = Arc::new(AtomicBool::new(false));
            let still = Arc::new(AtomicBool::new(false));
            let server = two_accounts(ended.clone(), still.clone());
            let window = tempfile::tempdir().unwrap();
            let signin = two_window(window.path());
            let env = Env::start(Opts {
                setup_url: &server.url,
                signin: Some(&signin),
                seed,
                log: true,
                crash: true,
                idle_secs,
                ..Default::default()
            });
            let conn = env.conn();
            Rig {
                env,
                conn,
                server,
                signin,
                window,
                ended,
                still,
                from: std::cell::Cell::new(0),
            }
        }

        /// The next daemon started stops at `spec` (`CRASH` or `PAUSE`).
        pub(super) fn arm(&self, what: &str, spec: &str) {
            fs::write(self.env.root().join(format!("arm-{what}")), spec).unwrap();
        }

        /// What the daemons logged since [`Rig::mark`].
        pub(super) fn log_since(&self) -> String {
            self.env.log().get(self.from.get()..).unwrap_or_default().to_string()
        }

        pub(super) fn mark(&self) {
            self.from.set(self.env.log().len());
        }

        pub(super) fn status(&self) -> icloud_session::Status {
            icloud_session::status_on(&self.conn).unwrap()
        }

        /// Waits until the status says `cond`.
        pub(super) fn until(&self, what: &str, cond: impl Fn(&icloud_session::Status) -> bool) {
            wait_until(what, Duration::from_secs(30), || cond(&self.status()));
        }

        /// Opens the sign-in window for `who`, without waiting.
        pub(super) fn start_sign_in(&self, who: &str) {
            fs::write(self.window.path().join("who"), who).unwrap();
            let _ = icloud_session::sign_in_on(&self.conn);
        }

        /// Signs in as `who` and waits until it is stored.
        pub(super) fn sign_in(&self, who: &str) {
            self.start_sign_in(who);
            self.signed_in_as(who);
        }

        /// Waits until a sign-in as `who` is stored.
        pub(super) fn signed_in_as(&self, who: &str) {
            let dsid = if who == "b" { B_DSID } else { DSID };
            self.until(&format!("signed in as {who}"), |s| {
                !s.signing_in && s.dsid.as_deref() == Some(dsid)
            });
            wait_until("the sign-in stored", Duration::from_secs(30), || {
                self.env.account_file().is_some_and(|f| f["dsid"] == dsid) && !change_pending(&self.env).exists()
            });
        }

        /// Kills the daemon: the next call starts another (armed, if
        /// [`Rig::arm`] said so).
        pub(super) fn restart(&self) {
            if self.env.daemon_running(&self.conn) {
                self.env.kill_daemon(&self.conn);
            }
        }

        /// The stores, as the next start finds them, hold one account
        /// whole, or none and no session in the keyring (with
        /// `forgets_trust`, no device trust either). Its dsid.
        pub(super) fn check(&self, case: &str, forgets_trust: bool) -> Option<String> {
            let status = self.status();
            let secrets = self.env.all_secrets();
            assert!(
                !change_pending(&self.env).exists(),
                "{case}: session-change-pending is left"
            );
            let trust = trust_item(&self.env);
            if !status.signed_in {
                assert!(
                    self.env.account_file().is_none(),
                    "{case}: signed out with account.json"
                );
                // A damaged item is kept for inspection; a session is not.
                let live = secrets.as_array().unwrap().iter().any(|i| {
                    i["attributes"]["kind"] == "session"
                        && serde_json::from_str::<Value>(i["secret"].as_str().unwrap_or_default())
                            .is_ok_and(|v| v.get("dsid").is_some())
                });
                assert!(!live, "{case}: a session for nobody: {secrets}");
                if forgets_trust {
                    assert!(trust.is_none(), "{case}: the device trust of a sign-out: {secrets}");
                }
                return None;
            }
            let dsid = status.dsid.clone().unwrap();
            let file = self.env.account_file();
            let file = file.unwrap_or_else(|| panic!("{case}: signed in as {dsid} without account.json"));
            assert_eq!(file["dsid"], dsid.as_str(), "{case}: {file}");
            assert_eq!(file["apple_id"], apple_id(&dsid), "{case}: {file}");
            let secret = self.env.session_secret();
            let secret = secret.unwrap_or_else(|| panic!("{case}: signed in as {dsid} with no session: {secrets}"));
            assert_eq!(secret["dsid"], dsid.as_str(), "{case}: {secrets}");
            let mine = |jar: &Value, what: &str| {
                for c in jar.as_array().unwrap() {
                    let (name, value) = (c["name"].as_str().unwrap(), c["value"].as_str().unwrap());
                    let ok = match name {
                        "X-APPLE-WEBAUTH-USER" => value.contains(&format!("d={dsid}")),
                        _ => owner(value) == dsid,
                    };
                    assert!(ok, "{case}: {what} of {dsid} holds {name}={value}: {secrets}");
                }
            };
            mine(&secret["cookies"], "the jar");
            if let Some(fm) = secret.get("find_my") {
                mine(&fm["cookies"], "the Find My jar");
            }
            if let Some((label, cookies)) = &trust {
                assert_eq!(
                    label,
                    &format!("iCloud device trust (icloud-session): {}", apple_id(&dsid)),
                    "{case}"
                );
                mine(cookies, "the device trust");
                if let Some(owner) = trust_dsid(&self.env) {
                    assert_eq!(owner, dsid, "{case}: the device trust of {owner}: {secrets}");
                }
            }
            let (header, _, _) = session(&self.conn).unwrap();
            let token = cookie_of(&header, "X-APPLE-WEBAUTH-TOKEN").unwrap();
            assert_eq!(owner(&token), dsid, "{case}: {header}");
            // Read in: no reason it was signed out is kept.
            let reason = self.env.account_path().with_file_name("sign-out-reason");
            assert!(
                !reason.exists(),
                "{case}: signed in with a sign-out reason: {}",
                fs::read_to_string(&reason).unwrap_or_default()
            );
            Some(dsid)
        }

        /// A sign-in as B, then a sign-out, work, and leave nothing behind.
        pub(super) fn sign_in_and_out_work(&self, case: &str) {
            self.sign_in("b");
            assert_eq!(self.check(case, false).as_deref(), Some(B_DSID), "{case}");
            icloud_session::sign_out_on(&self.conn).unwrap();
            assert!(self.env.account_file().is_none(), "{case}");
            let secrets = self.env.all_secrets();
            assert!(
                !has_session_item(&self.env) && trust_item(&self.env).is_none(),
                "{case}: left after a sign-out: {secrets}"
            );
        }
    }

    /// One change the daemon makes, cut off at each point it passes.
    struct Change<'a> {
        name: &'a str,
        /// The state before: the daemon that made it is killed after.
        setup: &'a dyn Fn(&Rig),
        /// Starts the change (and the next daemon, with its call).
        act: &'a dyn Fn(&Rig),
        /// Waits until it is done (in the traced run).
        done: &'a dyn Fn(&Rig),
        /// After the crash, before the next start (a keyring back).
        after: &'a dyn Fn(&Rig),
        /// A sign-out: when it took the account, the device trust must go
        /// with it.
        forgets_trust: bool,
        /// Points it must pass.
        passes: &'a [&'a str],
        /// Who may be signed in after it, cut off anywhere: the account
        /// before it, or after it (`None`: nobody).
        outcomes: &'a [Option<&'a str>],
        seed: bool,
    }

    impl Default for Change<'_> {
        fn default() -> Self {
            Change {
                name: "",
                setup: &|rig| {
                    rig.sign_in("a");
                    rig.restart();
                },
                act: &|_| {},
                done: &|_| {},
                after: &|_| {},
                forgets_trust: false,
                passes: &[],
                outcomes: &[Some(DSID)],
                seed: false,
            }
        }
    }

    const PREFIX: &str = "icloud-sessiond: crash point ";

    fn run(change: &Change<'_>) {
        let name = change.name;
        let rig = Rig::new(change.seed);
        (change.setup)(&rig);
        rig.arm("CRASH", "trace");
        rig.mark();
        (change.act)(&rig);
        (change.done)(&rig);
        let trace: Vec<String> = rig
            .log_since()
            .lines()
            .filter_map(|l| l.strip_prefix(PREFIX))
            .filter(|p| !p.ends_with("secrets.json"))
            .map(str::to_string)
            .collect();
        for point in change.passes {
            assert!(
                trace.iter().any(|p| p == point),
                "{name}: never passes {point}: {trace:?}"
            );
        }
        drop(rig);
        eprintln!("{name}: cut off at each of {trace:?}");
        for (i, point) in trace.iter().enumerate() {
            let n = trace[..=i].iter().filter(|p| *p == point).count();
            let case = format!("{name} cut off at {point}:{n}");
            let rig = Rig::new(change.seed);
            (change.setup)(&rig);
            rig.arm("CRASH", &format!("{point}:{n}"));
            rig.mark();
            (change.act)(&rig);
            wait_until(&case, Duration::from_secs(30), || {
                rig.log_since().contains(&format!("crash injected at {point}"))
            });
            wait_until("the killed daemon to leave the bus", Duration::from_secs(30), || {
                !rig.env.daemon_running(&rig.conn)
            });
            (change.after)(&rig);
            let dsid = rig.check(&case, change.forgets_trust);
            assert!(
                change.outcomes.contains(&dsid.as_deref()),
                "{case}: signed in as {dsid:?}, neither before nor after"
            );
            rig.sign_in_and_out_work(&case);
        }
    }

    /// Makes the test keyring refuse everything (a directory where its
    /// file goes); returns what it held.
    fn keyring_down(env: &Env) -> Vec<u8> {
        let held = fs::read(env.secrets_path()).unwrap_or_else(|_| b"[]".to_vec());
        let _ = fs::remove_file(env.secrets_path());
        fs::create_dir(env.secrets_path()).unwrap();
        held
    }

    fn keyring_up(env: &Env, held: &[u8]) {
        if env.secrets_path().is_dir() {
            fs::remove_dir(env.secrets_path()).unwrap();
            fs::write(env.secrets_path(), held).unwrap();
        }
    }

    fn kept_keyring(rig: &Rig) -> PathBuf {
        rig.env.root().join("kept-keyring")
    }

    #[test]
    fn a_sign_in_as_another_account_cut_off_anywhere() {
        run(&Change {
            name: "sign-in A to B",
            outcomes: &[Some(DSID), Some(B_DSID)],
            act: &|rig| rig.start_sign_in("b"),
            done: &|rig| rig.signed_in_as("b"),
            passes: &[
                "captured:sign-in",
                "lock:change",
                "write:session-change-pending",
                "set:session",
                "set:trust",
                "write:account.json",
                "remove:session-change-pending",
                "unlock:change",
            ],
            ..Default::default()
        });
    }

    #[test]
    fn a_sign_in_again_as_the_same_account_cut_off_anywhere() {
        run(&Change {
            name: "sign-in A to A",
            act: &|rig| rig.start_sign_in("a"),
            done: &|rig| {
                rig.until("the window closed", |s| !s.signing_in);
                wait_until("stored", Duration::from_secs(30), || !change_pending(&rig.env).exists());
            },
            passes: &["captured:sign-in", "set:session", "write:account.json"],
            ..Default::default()
        });
    }

    #[test]
    fn a_sign_in_after_a_sign_out_cut_off_anywhere() {
        run(&Change {
            name: "sign-in after a sign-out",
            outcomes: &[None, Some(B_DSID)],
            setup: &|rig| {
                rig.sign_in("a");
                icloud_session::sign_out_on(&rig.conn).unwrap();
                rig.restart();
            },
            act: &|rig| rig.start_sign_in("b"),
            done: &|rig| rig.signed_in_as("b"),
            passes: &["set:session", "write:account.json", "remove:sign-out-reason"],
            ..Default::default()
        });
    }

    #[test]
    fn a_sign_out_cut_off_anywhere() {
        run(&Change {
            name: "sign-out",
            outcomes: &[Some(DSID), None],
            act: &|rig| {
                let _ = icloud_session::sign_out_on(&rig.conn);
            },
            done: &|rig| rig.until("signed out", |s| !s.signed_in),
            forgets_trust: true,
            passes: &[
                "lock:change",
                "write:session-change-pending",
                "write:sign-out-reason",
                "remove:account.json",
                "remove:session",
                "remove:trust",
                "remove:session-change-pending",
                "unlock:change",
            ],
            ..Default::default()
        });
    }

    #[test]
    fn apple_ending_the_session_cut_off_anywhere() {
        run(&Change {
            name: "Apple ends the session",
            outcomes: &[Some(DSID), None],
            act: &|rig| {
                rig.ended.store(true, Ordering::SeqCst);
                let _ = call::<bool>(&rig.conn, "ReportSignInRequired");
            },
            done: &|rig| rig.until("signed out", |s| !s.signed_in),
            after: &|rig| rig.ended.store(false, Ordering::SeqCst),
            passes: &["remove:account.json", "remove:session", "write:sign-out-reason"],
            ..Default::default()
        });
    }

    #[test]
    fn a_refresh_cut_off_anywhere() {
        run(&Change {
            name: "refresh",
            act: &|rig| {
                let _ = call::<bool>(&rig.conn, "ReportSignInRequired");
            },
            done: &|rig| {
                wait_until("the rotated cookie stored", Duration::from_secs(30), || {
                    rig.env
                        .account_cookie("X-APPLE-WEBAUTH-TOKEN")
                        .is_some_and(|t| t.contains("rotated2"))
                })
            },
            passes: &["write:account.json", "set:session"],
            ..Default::default()
        });
    }

    #[test]
    fn a_cookie_merge_cut_off_anywhere() {
        run(&Change {
            name: "cookie merge",
            act: &|rig| {
                let cookies = vec!["X-APPLE-WEBAUTH-TOKEN=a-merged; Domain=.icloud.com; Path=/".to_string()];
                let _ = rig.conn.call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(INTERFACE),
                    "MergeCookiesFor",
                    &(DSID, cookies),
                );
            },
            done: &|rig| {
                wait_until("the merged cookie stored", Duration::from_secs(30), || {
                    rig.env.account_cookie("X-APPLE-WEBAUTH-TOKEN").as_deref() == Some("a-merged")
                })
            },
            passes: &["write:account.json", "set:session"],
            ..Default::default()
        });
    }

    #[test]
    fn a_find_my_authorization_cut_off_anywhere() {
        run(&Change {
            name: "Find My authorization",
            act: &|rig| {
                let _ = icloud_session::authorize_find_my_on(&rig.conn);
            },
            done: &|rig| rig.until("Find My authorized", |s| s.find_my_authorized && !s.signing_in),
            passes: &["captured:find-my", "set:session", "set:trust"],
            ..Default::default()
        });
    }

    #[test]
    fn a_find_my_sign_in_with_the_password_cut_off_anywhere() {
        run(&Change {
            name: "SetPassword",
            act: &|rig| {
                let _ = rig.conn.call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(INTERFACE),
                    "SetPassword",
                    &("hunter2",),
                );
            },
            done: &|rig| {
                rig.until("the password stored", |s| {
                    s.find_my_password_stored && s.find_my_authorized
                })
            },
            passes: &["captured:autofill", "set:session", "set:trust", "set:password"],
            ..Default::default()
        });
    }

    #[test]
    fn forgetting_the_password_cut_off_anywhere() {
        run(&Change {
            name: "ForgetPassword",
            setup: &|rig| {
                rig.sign_in("a");
                rig.env.store_password("hunter2");
                rig.restart();
            },
            act: &|rig| {
                let _ = call::<()>(&rig.conn, "ForgetPassword");
            },
            done: &|rig| rig.until("no password", |s| !s.find_my_password_stored),
            passes: &["remove:password"],
            ..Default::default()
        });
    }

    #[test]
    fn moving_an_old_account_file_s_cookies_to_the_keyring_cut_off_anywhere() {
        run(&Change {
            name: "the cookies' move to the keyring",
            setup: &|rig| rig.env.seed_validated_at(now()),
            act: &|rig| {
                let _ = icloud_session::status_on(&rig.conn);
            },
            done: &|rig| {
                wait_until("moved", Duration::from_secs(30), || {
                    rig.env.account_file().is_some_and(|f| f.get("cookies").is_none())
                })
            },
            passes: &["set:session", "write:account.json"],
            seed: true,
            ..Default::default()
        });
    }

    #[test]
    fn a_sign_out_the_keyring_refuses_cut_off_anywhere() {
        run(&Change {
            name: "sign-out with the keyring refusing",
            outcomes: &[Some(DSID), None],
            act: &|rig| {
                fs::write(kept_keyring(rig), keyring_down(&rig.env)).unwrap();
                let _ = icloud_session::sign_out_on(&rig.conn);
            },
            done: &|rig| rig.until("signed out", |s| !s.signed_in),
            after: &|rig| keyring_up(&rig.env, &fs::read(kept_keyring(rig)).unwrap()),
            forgets_trust: true,
            passes: &["remove:account.json", "write:session-removal-pending"],
            ..Default::default()
        });
    }

    #[test]
    fn a_start_finishing_an_owed_removal_cut_off_anywhere() {
        run(&Change {
            name: "the owed removal at start",
            outcomes: &[None],
            setup: &|rig| {
                rig.sign_in("a");
                let held = keyring_down(&rig.env);
                let _ = icloud_session::sign_out_on(&rig.conn);
                rig.restart();
                keyring_up(&rig.env, &held);
            },
            act: &|rig| {
                let _ = icloud_session::status_on(&rig.conn);
            },
            done: &|rig| {
                wait_until("the removal", Duration::from_secs(30), || {
                    !rig.env
                        .account_path()
                        .with_file_name("session-removal-pending")
                        .exists()
                })
            },
            forgets_trust: true,
            passes: &["remove:session", "remove:trust", "remove:session-removal-pending"],
            ..Default::default()
        });
    }

    #[test]
    fn a_damaged_session_item_set_aside_cut_off_anywhere() {
        run(&Change {
            name: "a damaged session item",
            outcomes: &[None],
            setup: &|rig| {
                rig.sign_in("a");
                rig.restart();
                rig.env.damage_session_secret("not a session");
            },
            act: &|rig| {
                let _ = icloud_session::status_on(&rig.conn);
            },
            done: &|rig| rig.until("signed out", |s| !s.signed_in),
            passes: &["rename:account.json", "write:sign-out-reason"],
            ..Default::default()
        });
    }

    /// Makes the test keyring's device trust a bare cookie list, as a
    /// daemon from before trusts named their account stored it.
    fn unattribute_trust(env: &Env) {
        let mut items = env.all_secrets();
        let item = items
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|i| i["attributes"]["kind"] == "trust")
            .expect("a trust item");
        let (_, cookies) = trust_item(env).unwrap();
        item["secret"] = json!(cookies.to_string());
        fs::write(env.secrets_path(), items.to_string()).unwrap();
        assert_eq!(trust_dsid(env), None);
    }

    fn no_trust_of_its_own(rig: &Rig) {
        fs::write(rig.window.path().join("notrust"), "").unwrap();
    }

    /// A sign-in whose window captured no trust keeps the stored one only
    /// if it is the same account's: a trust is one Apple ID's. One stored
    /// before trusts named their account is the account's signed in then,
    /// and is named once kept.
    #[test]
    fn a_sign_in_without_a_trust_of_its_own_keeps_only_its_own_account_s() {
        let rig = Rig::new(false);
        rig.sign_in("a");
        assert_eq!(trust_dsid(&rig.env).as_deref(), Some(DSID));
        no_trust_of_its_own(&rig);
        rig.sign_in("a");
        assert_eq!(
            trust_dsid(&rig.env).as_deref(),
            Some(DSID),
            "the same account keeps its trust"
        );
        rig.sign_in("b");
        assert!(trust_item(&rig.env).is_none(), "{}", rig.env.all_secrets());

        // Unattributed: A's, signed in when it is read.
        fs::remove_file(rig.window.path().join("notrust")).unwrap();
        rig.sign_in("a");
        unattribute_trust(&rig.env);
        no_trust_of_its_own(&rig);
        rig.sign_in("a");
        assert_eq!(trust_dsid(&rig.env).as_deref(), Some(DSID), "named once kept");
        assert_eq!(trust_item(&rig.env).unwrap().1[0]["value"], "a-trust");
        unattribute_trust(&rig.env);
        rig.sign_in("b");
        assert!(trust_item(&rig.env).is_none(), "{}", rig.env.all_secrets());
    }

    #[test]
    fn a_sign_in_as_another_account_without_a_trust_of_its_own_cut_off_anywhere() {
        run(&Change {
            name: "sign-in A to B without a trust",
            outcomes: &[Some(DSID), Some(B_DSID)],
            act: &|rig| {
                no_trust_of_its_own(rig);
                rig.start_sign_in("b");
            },
            done: &|rig| rig.signed_in_as("b"),
            passes: &["remove:trust", "set:session", "write:account.json"],
            ..Default::default()
        });
    }

    #[test]
    fn a_sign_in_naming_an_unattributed_trust_cut_off_anywhere() {
        run(&Change {
            name: "sign-in A to A naming its trust",
            setup: &|rig| {
                rig.sign_in("a");
                unattribute_trust(&rig.env);
                rig.restart();
            },
            act: &|rig| {
                no_trust_of_its_own(rig);
                rig.start_sign_in("a");
            },
            done: &|rig| {
                wait_until("the trust named", Duration::from_secs(30), || {
                    trust_dsid(&rig.env).as_deref() == Some(DSID) && !change_pending(&rig.env).exists()
                })
            },
            passes: &["set:session", "set:trust", "write:account.json"],
            ..Default::default()
        });
    }

    /// A sign-in as B cut off once its session was stored, and a keyring
    /// that cannot be read when the next daemon starts: the change is
    /// settled when the keyring answers, before the account is read from
    /// it, rather than A's `account.json` beside B's session being read as
    /// another Apple ID's.
    #[test]
    fn a_cut_off_sign_in_is_settled_once_the_keyring_answers() {
        let rig = Rig::new(false);
        rig.sign_in("a");
        rig.restart();
        rig.arm("CRASH", "set:session:1");
        rig.mark();
        rig.start_sign_in("b");
        wait_until("the crash", Duration::from_secs(30), || {
            rig.log_since().contains("crash injected at set:session")
        });
        wait_until("the killed daemon to leave the bus", Duration::from_secs(30), || {
            !rig.env.daemon_running(&rig.conn)
        });
        let held = keyring_down(&rig.env);
        assert!(rig.status().signed_in);
        assert!(change_pending(&rig.env).exists());
        keyring_up(&rig.env, &held);
        let (cookie, _, _) = session(&rig.conn).unwrap();
        assert!(cookie.contains("X-APPLE-WEBAUTH-TOKEN=b-"), "{cookie}");
        assert_eq!(rig.status().dsid.as_deref(), Some(B_DSID));
        rig.restart();
        let case = "a cut-off sign-in settled late";
        assert_eq!(rig.check(case, false).as_deref(), Some(B_DSID));
        rig.sign_in_and_out_work(case);
    }

    /// A sign-out cut off once `account.json` was gone, and a keyring that
    /// cannot be read when the next daemon starts, so the sign-out is not
    /// settled then. A sign-in once the keyring is back settles it before
    /// the window opens: the window gets no device trust the sign-out
    /// forgot, and the sign-in keeps none of it.
    #[test]
    fn a_cut_off_sign_out_is_settled_before_the_next_sign_in_window() {
        let rig = Rig::new(false);
        rig.sign_in("a");
        rig.restart();
        rig.arm("CRASH", "remove:account.json:1");
        rig.mark();
        let _ = icloud_session::sign_out_on(&rig.conn);
        wait_until("the killed daemon to leave the bus", Duration::from_secs(30), || {
            !rig.env.daemon_running(&rig.conn)
        });
        let held = keyring_down(&rig.env);
        assert!(!rig.status().signed_in);
        assert!(change_pending(&rig.env).exists());
        keyring_up(&rig.env, &held);
        assert!(
            trust_item(&rig.env).is_some(),
            "the cut-off sign-out's trust is still there"
        );

        no_trust_of_its_own(&rig);
        rig.sign_in("a");
        let secrets = rig.env.all_secrets();
        assert!(
            trust_item(&rig.env).is_none(),
            "the signed-out trust was kept: {secrets}"
        );
        rig.restart();
        let case = "a cut-off sign-out settled before a sign-in";
        assert_eq!(rig.check(case, false).as_deref(), Some(DSID));
        fs::remove_file(rig.window.path().join("notrust")).unwrap();
        rig.sign_in_and_out_work(case);
    }

    /// Kills the daemon at `point` of what `act` starts, and waits until
    /// it has left the bus.
    fn cut_off(rig: &Rig, point: &str, act: impl Fn(&Rig)) {
        rig.arm("CRASH", point);
        rig.mark();
        act(rig);
        wait_until(point, Duration::from_secs(30), || {
            rig.log_since().contains("crash injected at")
        });
        wait_until("the killed daemon to leave the bus", Duration::from_secs(30), || {
            !rig.env.daemon_running(&rig.conn)
        });
    }

    fn sign_out(rig: &Rig) {
        let _ = icloud_session::sign_out_on(&rig.conn);
    }

    /// The next start settling what a crash cut off: until no marker is
    /// left.
    fn start_settling(rig: &Rig) {
        let _ = icloud_session::status_on(&rig.conn);
    }

    fn settled(rig: &Rig) {
        wait_until("settled", Duration::from_secs(30), || {
            !change_pending(&rig.env).exists()
        });
    }

    /// The start settling a sign-in cut off once its session was stored is
    /// cut off itself: the start after it settles it still.
    #[test]
    fn settling_a_cut_off_sign_in_cut_off_anywhere() {
        run(&Change {
            name: "settling a sign-in A to B",
            outcomes: &[Some(DSID), Some(B_DSID)],
            setup: &|rig| {
                rig.sign_in("a");
                rig.restart();
                cut_off(rig, "set:session:1", |rig| rig.start_sign_in("b"));
            },
            act: &start_settling,
            done: &settled,
            passes: &["lock:change", "write:account.json", "remove:session-change-pending"],
            ..Default::default()
        });
    }

    #[test]
    fn settling_a_cut_off_sign_in_after_a_sign_out_cut_off_anywhere() {
        run(&Change {
            name: "settling a sign-in after a sign-out",
            outcomes: &[None, Some(B_DSID)],
            setup: &|rig| {
                rig.sign_in("a");
                sign_out(rig);
                rig.restart();
                cut_off(rig, "set:session:1", |rig| rig.start_sign_in("b"));
            },
            act: &start_settling,
            done: &settled,
            passes: &[
                "write:account.json",
                "remove:sign-out-reason",
                "remove:session-change-pending",
            ],
            ..Default::default()
        });
    }

    #[test]
    fn settling_a_cut_off_sign_out_cut_off_anywhere() {
        run(&Change {
            name: "settling a sign-out",
            outcomes: &[None],
            setup: &|rig| {
                rig.sign_in("a");
                rig.restart();
                cut_off(rig, "remove:account.json:1", sign_out);
            },
            act: &start_settling,
            done: &settled,
            forgets_trust: true,
            passes: &["remove:session", "remove:trust", "remove:session-change-pending"],
            ..Default::default()
        });
    }

    /// A sign-in cut off once its session was stored, a start with the
    /// keyring down, then the keyring back and a `Session()` that settles
    /// it: cut off anywhere from that start on.
    #[test]
    fn settling_once_the_keyring_answers_cut_off_anywhere() {
        run(&Change {
            name: "settling a sign-in once the keyring answers",
            outcomes: &[Some(DSID), Some(B_DSID)],
            setup: &|rig| {
                rig.sign_in("a");
                rig.restart();
                cut_off(rig, "set:session:1", |rig| rig.start_sign_in("b"));
                fs::write(kept_keyring(rig), keyring_down(&rig.env)).unwrap();
            },
            act: &|rig| {
                let _ = icloud_session::status_on(&rig.conn);
                // Killed at its start: no next daemon yet.
                if rig.log_since().contains("crash injected at") {
                    return;
                }
                keyring_up(&rig.env, &fs::read(kept_keyring(rig)).unwrap());
                let _ = session(&rig.conn);
            },
            done: &settled,
            after: &|rig| keyring_up(&rig.env, &fs::read(kept_keyring(rig)).unwrap()),
            passes: &["write:account.json", "remove:session-change-pending"],
            ..Default::default()
        });
    }

    /// A Find My authorization whose window has closed, and a sign-out
    /// before it is stored, or while it is (the sign-out waits for the
    /// change: `lock:change:4` is the write's, after the start's and the
    /// window's settling): signed out, and nothing of it is kept.
    #[test]
    fn a_sign_out_racing_a_find_my_authorization_keeps_nothing_of_it() {
        for point in ["captured:find-my", "lock:change:4", "set:session"] {
            let rig = Rig::new(false);
            rig.sign_in("a");
            rig.restart();
            let paused = rig.env.root().join("paused");
            rig.arm("PAUSE", &format!("{point}@{}", paused.display()));
            icloud_session::authorize_find_my_on(&rig.conn).unwrap();
            rig.paused(&paused);
            let conn = rig.env.conn();
            let out = thread::spawn(move || icloud_session::sign_out_on(&conn));
            thread::sleep(Duration::from_millis(200));
            fs::remove_file(&paused).unwrap();
            out.join().unwrap().unwrap();
            rig.until("signed out, the window closed", |s| !s.signed_in && !s.signing_in);
            thread::sleep(Duration::from_millis(300));
            let case = format!("a sign-out racing a Find My authorization paused at {point}");
            let status = rig.status();
            assert!(!status.signed_in && !status.find_my_authorized, "{case}: {status:?}");
            rig.restart();
            assert_eq!(rig.check(&case, true), None, "{case}");
            rig.sign_in_and_out_work(&case);
        }
    }

    // ------------------------------------------------------- two daemons

    impl Rig {
        /// The unique bus name of the daemon owning the name now.
        fn owner(&self) -> String {
            zbus::blocking::fdo::DBusProxy::new(&self.conn)
                .unwrap()
                .get_name_owner(BUS_NAME.try_into().unwrap())
                .unwrap()
                .to_string()
        }

        fn on_bus(&self, unique: &str) -> bool {
            zbus::blocking::fdo::DBusProxy::new(&self.conn)
                .unwrap()
                .name_has_owner(unique.try_into().unwrap())
                .unwrap()
        }

        /// Waits until the armed pause is reached; `paused` is its file.
        fn paused(&self, paused: &Path) {
            wait_until("the daemon to pause", Duration::from_secs(30), || paused.exists());
        }

        /// Another daemon, started by hand rather than by the bus, as the
        /// bus would start it.
        fn another_daemon(&self) -> Child {
            let root = self.env.root();
            Command::new(DAEMON)
                .env_clear()
                .env("DBUS_SESSION_BUS_ADDRESS", &self.env.address)
                .env("HOME", root.join("home"))
                .env("XDG_STATE_HOME", root.join("state"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CACHE_HOME", root.join("cache"))
                .env("ICLOUD_SESSION_SETUP_URL", &self.server.url)
                .env("ICLOUD_SESSION_SIGNIN_BIN", &self.signin)
                .env("ICLOUD_SESSION_TEST_SECRET_FILE", self.env.secrets_path())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }
    }

    /// A daemon winding down (idle, the bus name given up) is asked to
    /// sign in by a caller that addressed it directly, while the next
    /// daemon starts; it finishes the sign-in, as B, after that one has
    /// read A from the stores. The next daemon follows (it says B), its
    /// own writes since keep the stores whole, and the start after it
    /// finds B.
    #[test]
    fn a_sign_in_a_winding_down_daemon_finishes_is_followed_by_the_next() {
        let rig = Rig::idling(false, 1.0);
        rig.sign_in("a");
        rig.restart();
        let paused = rig.env.root().join("paused");
        rig.arm("PAUSE", &format!("release:name@{}", paused.display()));
        fs::write(rig.window.path().join("hold"), "").unwrap();
        let first = {
            let c = rig.env.conn();
            icloud_session::status_on(&c).unwrap();
            rig.owner()
        };
        rig.paused(&paused);

        // The caller that had it by its unique name opens a window there.
        let direct = rig.env.conn();
        fs::write(rig.window.path().join("who"), "b").unwrap();
        direct
            .call_method(Some(first.as_str()), OBJECT_PATH, Some(INTERFACE), "SignIn", &())
            .unwrap();
        // The next daemon starts, signed in as A.
        assert_eq!(rig.status().dsid.as_deref(), Some(DSID));
        assert_ne!(rig.owner(), first);
        fs::remove_file(&paused).unwrap();

        // The window closes: the winding-down daemon stores B and exits.
        fs::remove_file(rig.window.path().join("hold")).unwrap();
        wait_until("the first daemon to exit", Duration::from_secs(30), || {
            !rig.on_bus(&first)
        });
        rig.until("the next daemon to follow the sign-in", |s| {
            s.dsid.as_deref() == Some(B_DSID)
        });
        let (cookie, _, _) = session(&rig.conn).unwrap();
        assert!(cookie.contains("X-APPLE-WEBAUTH-TOKEN=b-"), "{cookie}");

        // Its own writes since: a merge for A is refused, a /validate
        // stores B.
        let cookies = vec!["X-APPLE-WEBAUTH-TOKEN=a-merged; Domain=.icloud.com; Path=/".to_string()];
        assert!(
            rig.conn
                .call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(INTERFACE),
                    "MergeCookiesFor",
                    &(DSID, cookies)
                )
                .is_err()
        );
        rig.still.store(true, Ordering::SeqCst);
        assert!(call::<bool>(&rig.conn, "ReportSignInRequired").unwrap());
        rig.still.store(false, Ordering::SeqCst);
        rig.restart();
        let case = "a sign-in finished by a daemon winding down";
        assert_eq!(rig.check(case, false).as_deref(), Some(B_DSID));
        rig.sign_in_and_out_work(case);
    }

    /// The same with a Find My authorization, which changes only the jars
    /// in the keyring: the next daemon follows it too, rather than handing
    /// out no Find My jar and storing its own jars over it.
    #[test]
    fn a_find_my_authorization_a_winding_down_daemon_finishes_is_followed_by_the_next() {
        let rig = Rig::idling(false, 1.0);
        rig.sign_in("a");
        rig.restart();
        let paused = rig.env.root().join("paused");
        rig.arm("PAUSE", &format!("release:name@{}", paused.display()));
        fs::write(rig.window.path().join("hold"), "").unwrap();
        let first = {
            let c = rig.env.conn();
            icloud_session::status_on(&c).unwrap();
            rig.owner()
        };
        rig.paused(&paused);

        let direct = rig.env.conn();
        direct
            .call_method(
                Some(first.as_str()),
                OBJECT_PATH,
                Some(INTERFACE),
                "AuthorizeFindMy",
                &(),
            )
            .unwrap();
        assert!(!rig.status().find_my_authorized);
        assert_ne!(rig.owner(), first);
        fs::remove_file(&paused).unwrap();

        fs::remove_file(rig.window.path().join("hold")).unwrap();
        wait_until("the first daemon to exit", Duration::from_secs(30), || {
            !rig.on_bus(&first)
        });
        rig.until("the next daemon to follow the authorization", |s| s.find_my_authorized);
        let (cookie, _): (String, HashMap<String, String>) = call(&rig.conn, "FindMySession").unwrap();
        assert!(cookie.contains("X-APPLE-WEBAUTH-FMIP=a-fmip"), "{cookie}");

        // Its own writes since keep it.
        assert!(call::<bool>(&rig.conn, "ReportSignInRequired").unwrap());
        rig.restart();
        let case = "a Find My authorization finished by a daemon winding down";
        assert_eq!(rig.check(case, false).as_deref(), Some(DSID));
        let secret = rig.env.session_secret().unwrap();
        assert!(secret.get("find_my").is_some(), "{secret}");
        rig.sign_in_and_out_work(case);
    }

    /// A second daemon starting while the first is between its keyring
    /// write and its `account.json` write, signing in as B: it waits for
    /// that change rather than reading half of it (A's `account.json`, B's
    /// session) as a keyring holding another account's session.
    #[test]
    fn a_daemon_starting_while_another_signs_in_leaves_the_sign_in_alone() {
        let rig = Rig::new(false);
        rig.sign_in("a");
        rig.restart();
        let paused = rig.env.root().join("paused");
        rig.arm("PAUSE", &format!("set:session@{}", paused.display()));
        rig.start_sign_in("b");
        rig.paused(&paused);

        let mut second = rig.another_daemon();
        // Its start is over (or waiting for the first daemon's change)
        // once it has given up on the bus name.
        let exited = second.wait().unwrap();
        assert!(!exited.success());
        fs::remove_file(&paused).unwrap();
        rig.signed_in_as("b");
        let reason = rig.env.account_path().with_file_name("sign-out-reason");
        assert!(!reason.exists(), "{}", fs::read_to_string(&reason).unwrap_or_default());
        rig.restart();
        let case = "a sign-in while another daemon started";
        assert_eq!(rig.check(case, false).as_deref(), Some(B_DSID));
        rig.sign_in_and_out_work(case);
    }

    /// A second daemon starting while the first settles a change a crash
    /// cut off, held at each of its settling steps: whichever gets the bus
    /// name (the first, or the second while the first is still before
    /// taking it, then exiting mid-settle), the stores end whole.
    #[test]
    fn a_daemon_starting_while_another_settles_a_cut_off_change() {
        for sign_in in [true, false] {
            let (name, cut, outcomes, steps): (_, _, &[_], &[_]) = match sign_in {
                true => (
                    "sign-in",
                    "set:session:1",
                    &[Some(DSID), Some(B_DSID)],
                    &["write:account.json", "remove:session-change-pending"],
                ),
                false => (
                    "sign-out",
                    "remove:account.json:1",
                    &[None],
                    &["remove:session", "remove:trust", "remove:session-change-pending"],
                ),
            };
            let act = |rig: &Rig| match sign_in {
                true => rig.start_sign_in("b"),
                false => sign_out(rig),
            };
            for step in steps {
                let case = format!("a daemon starting while another settles a {name}, held at {step}");
                let rig = Rig::new(false);
                rig.sign_in("a");
                rig.restart();
                cut_off(&rig, cut, act);
                let paused = rig.env.root().join("paused");
                rig.arm("PAUSE", &format!("{step}@{}", paused.display()));
                let conn = rig.env.conn();
                let first = thread::spawn(move || icloud_session::status_on(&conn).map(|_| ()));
                rig.paused(&paused);
                let mut second = rig.another_daemon();
                thread::sleep(Duration::from_millis(300));
                fs::remove_file(&paused).unwrap();
                let _ = first.join().unwrap();
                settled(&rig);
                rig.restart();
                let _ = second.kill();
                let _ = second.wait();
                let dsid = rig.check(&case, !sign_in);
                assert!(outcomes.contains(&dsid.as_deref()), "{case}: signed in as {dsid:?}");
                rig.sign_in_and_out_work(&case);
            }
        }
    }
}
