//! End-to-end tests against a local HTTP server standing in for Apple.
//! Everything lives in temp dirs; nothing touches ~/.config or ~/.cache.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use icloud_session::{Config, Error, Session, status_with};
use serde_json::{Value, json};

const DSID: &str = "12345";
const BIN: &str = env!("CARGO_BIN_EXE_icloud-session");

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
}

struct Reply {
    status: u16,
    body: String,
    set_cookies: Vec<String>,
    delay: Duration,
}

impl Reply {
    fn json(status: u16, body: Value) -> Reply {
        Reply {
            status,
            body: body.to_string(),
            set_cookies: vec![],
            delay: Duration::ZERO,
        }
    }
    fn cookie(mut self, c: &str) -> Reply {
        self.set_cookies.push(c.to_string());
        self
    }
    fn delay(mut self, d: Duration) -> Reply {
        self.delay = d;
        self
    }
}

type Handler = dyn Fn(&Seen, usize) -> Reply + Send + Sync;

struct Server {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start(handler: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static) -> Server {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let (seen2, server2) = (seen.clone(), server.clone());
        thread::spawn(move || {
            for mut request in server2.incoming_requests() {
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
                    seen.iter().filter(|s| path_of(&s.url) == path_of(&entry.url)).count()
                };
                let handler = handler.clone();
                // One thread per request so a slow reply does not serialize clients.
                thread::spawn(move || {
                    let reply = handler(&entry, n);
                    thread::sleep(reply.delay);
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

    fn count(&self, path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| path_of(&s.url) == path)
            .count()
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn path_of(url: &str) -> &str {
    url.split('?').next().unwrap()
}

fn query_of(url: &str) -> Vec<(String, String)> {
    url::Url::parse(&format!("http://x{url}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn validate_ok(n: usize) -> Reply {
    Reply::json(
        200,
        json!({
            "dsInfo": {"dsid": DSID, "appleId": "someone@example.com", "fullName": "Some One"},
            "webservices": {
                "ckdatabasews": {"url": "https://p42-ckdatabasews.icloud.com:443", "status": "active"},
                "findme": {"url": "https://p42-fmipweb.icloud.com:443", "status": "active"},
                "broken": {"status": "active"}
            }
        }),
    )
    .cookie(&format!(
        "X-APPLE-WEBAUTH-TOKEN=rotated{n}; Domain=.icloud.com; Path=/; Secure; HttpOnly"
    ))
}

// ---------------------------------------------------------------- fixture

struct Fixture {
    _dir: tempfile::TempDir,
    config_dir: PathBuf,
    cache_dir: PathBuf,
    setup_url: String,
}

const SESSION: &str = r#"{
  "cookie": "X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=12345\"; X-APPLE-WEBAUTH-TOKEN=original; X-APPLE-DS-WEB-SESSION-TOKEN=ds",
  "clientId": "auth-client-id",
  "clientBuildNumber": "2624Build27",
  "clientMasteringNumber": "2624Build27M",
  "capturedAt": "2026-09-01T00:00:00.000Z",
  "somethingNew": [1, 2, 3]
}
"#;

impl Fixture {
    fn new(setup_url: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("icloud-md");
        let cache_dir = dir.path().join("cache");
        let account = config_dir.join("accounts").join(DSID);
        fs::create_dir_all(&account).unwrap();
        fs::write(account.join("session.local.json"), SESSION).unwrap();
        fs::write(
            account.join("meta.json"),
            json!({"appleId": "meta@example.com", "dsid": DSID}).to_string(),
        )
        .unwrap();
        Fixture {
            _dir: dir,
            config_dir,
            cache_dir,
            setup_url: setup_url.to_string(),
        }
    }

    fn config(&self) -> Config {
        Config {
            setup_url: self.setup_url.clone(),
            ..Config::at(&self.config_dir, &self.cache_dir)
        }
    }

    fn account_dir(&self) -> PathBuf {
        self.config_dir.join("accounts").join(DSID)
    }

    fn session_path(&self) -> PathBuf {
        self.account_dir().join("session.local.json")
    }

    fn session_json(&self) -> Value {
        serde_json::from_slice(&fs::read(self.session_path()).unwrap()).unwrap()
    }

    fn cookie(&self) -> String {
        self.session_json()["cookie"].as_str().unwrap().to_string()
    }

    fn cache_json(&self) -> Value {
        serde_json::from_slice(&fs::read(self.cache_dir.join(format!("{DSID}.json"))).unwrap()).unwrap()
    }

    fn load(&self) -> Session {
        Session::load_with(self.config()).unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("ICLOUD_MD_CONFIG_DIR", &self.config_dir)
            .env("ICLOUD_SESSION_CACHE_DIR", &self.cache_dir)
            .env("ICLOUD_SESSION_SETUP_URL", &self.setup_url)
            .env_remove("ICLOUD_SESSION_MOCK")
            .output()
            .unwrap()
    }

    /// Rewrites the session file, as icloud-md does after a new sign-in.
    fn resave_session(&self) {
        thread::sleep(Duration::from_millis(20));
        fs::write(self.session_path(), SESSION).unwrap();
    }
}

fn stdout_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

// ---------------------------------------------------------------- validate

#[test]
fn validate_mirrors_icloud_md_and_merges_rotation() {
    let server = Server::start(|_, n| validate_ok(n));
    let fx = Fixture::new(&server.url);
    let session = fx.load();
    assert_eq!(session.dsid(), DSID);

    let ws = session.webservices().unwrap();
    assert_eq!(ws.url("ckdatabasews"), Some("https://p42-ckdatabasews.icloud.com:443"));
    assert_eq!(ws.url("findme"), Some("https://p42-fmipweb.icloud.com:443"));
    assert_eq!(ws.url("broken"), None);

    let req = &server.requests()[0];
    assert_eq!(req.method, "POST");
    assert_eq!(path_of(&req.url), "/setup/ws/1/validate");
    let q = query_of(&req.url);
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("clientBuildNumber"), Some("2624Build27"));
    assert_eq!(get("clientMasteringNumber"), Some("2624Build27M"));
    assert_eq!(get("clientId"), Some("auth-client-id"));
    assert_eq!(get("dsid"), Some(DSID));
    assert_eq!(get("requestId").map(str::len), Some(36));
    assert_eq!(req.header("Origin"), Some("https://www.icloud.com"));
    assert_eq!(req.header("Referer"), Some("https://www.icloud.com/"));
    assert_eq!(req.header("Accept"), Some("application/json"));
    assert!(req.header("Cookie").unwrap().contains("X-APPLE-WEBAUTH-TOKEN=original"));

    // Rotated in place, order kept, unknown fields kept.
    assert_eq!(
        fx.cookie(),
        "X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=12345\"; X-APPLE-WEBAUTH-TOKEN=rotated1; X-APPLE-DS-WEB-SESSION-TOKEN=ds"
    );
    assert_eq!(fx.session_json()["somethingNew"], json!([1, 2, 3]));

    let cache = fx.cache_json();
    assert_eq!(cache["apple_id"], "someone@example.com");
    assert_eq!(cache["webservices"]["findme"], "https://p42-fmipweb.icloud.com:443");
    assert!(cache["validated_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(session.apple_id().as_deref(), Some("someone@example.com"));

    // Fresh cache: no second call, from this or a new Session.
    session.webservices().unwrap();
    fx.load().webservices().unwrap();
    assert_eq!(server.count("/setup/ws/1/validate"), 1);
}

#[test]
fn stale_cache_revalidates() {
    let server = Server::start(|_, n| validate_ok(n));
    let fx = Fixture::new(&server.url);
    let eleven_minutes_ago = SystemTime::now() - Duration::from_secs(11 * 60);
    fs::create_dir_all(&fx.cache_dir).unwrap();
    fs::write(
        fx.cache_dir.join(format!("{DSID}.json")),
        json!({
            "validated_at": humantime::format_rfc3339_seconds(eleven_minutes_ago).to_string(),
            "webservices": {"findme": "https://old"},
            "apple_id": "someone@example.com"
        })
        .to_string(),
    )
    .unwrap();
    let ws = fx.load().webservices().unwrap();
    assert_eq!(ws.url("findme"), Some("https://p42-fmipweb.icloud.com:443"));
    assert_eq!(server.count("/setup/ws/1/validate"), 1);
}

#[test]
fn racing_threads_make_one_validate_call() {
    let server = Server::start(|_, n| validate_ok(n).delay(Duration::from_millis(300)));
    let fx = Arc::new(Fixture::new(&server.url));
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (fx, barrier) = (fx.clone(), barrier.clone());
            thread::spawn(move || {
                let session = fx.load(); // separate Session per thread
                barrier.wait();
                session.webservices().unwrap()
            })
        })
        .collect();
    for t in threads {
        assert!(t.join().unwrap().url("ckdatabasews").is_some());
    }
    assert_eq!(server.count("/setup/ws/1/validate"), 1);
    assert!(fx.cookie().contains("X-APPLE-WEBAUTH-TOKEN=rotated1;"));
}

#[test]
fn racing_processes_make_one_validate_call() {
    let server = Server::start(|_, n| validate_ok(n).delay(Duration::from_millis(400)));
    let fx = Arc::new(Fixture::new(&server.url));
    let children: Vec<_> = (0..3)
        .map(|_| {
            let fx = fx.clone();
            thread::spawn(move || fx.cli(&["validate"]))
        })
        .collect();
    for child in children {
        let out = stdout_json(&child.join().unwrap());
        assert_eq!(out["dsid"], DSID);
        assert_eq!(out["apple_id"], "someone@example.com");
        assert_eq!(
            out["webservices"]["ckdatabasews"],
            "https://p42-ckdatabasews.icloud.com:443"
        );
        assert!(out["validated_at"].as_str().unwrap().ends_with('Z'));
    }
    assert_eq!(server.count("/setup/ws/1/validate"), 1);
    // Every process sees the one rotated cookie on its next request.
    assert!(fx.cookie().contains("X-APPLE-WEBAUTH-TOKEN=rotated1;"));
}

#[test]
fn half_signed_in_session_is_sign_in_required() {
    let server = Server::start(|_, _| {
        Reply::json(
            200,
            json!({"dsInfo": {"dsid": DSID, "appleId": "a@b.c"}, "hsaChallengeRequired": true}),
        )
    });
    let fx = Fixture::new(&server.url);
    assert!(matches!(fx.load().webservices(), Err(Error::SignInRequired)));
}

#[test]
fn other_validate_failures_are_http_errors() {
    let server = Server::start(|_, _| Reply::json(503, json!({"error": "down"})));
    let fx = Fixture::new(&server.url);
    match fx.load().webservices() {
        Err(Error::Http { status: 503, body }) => assert!(body.contains("down")),
        other => panic!("{other:?}"),
    }
    assert!(status_with(&fx.config()).signed_in);
}

// ---------------------------------------------------------------- 421 / status

#[test]
fn http_421_is_sign_in_required_until_the_session_file_is_rewritten() {
    for status in [421u16, 401] {
        let server = Server::start(move |_, _| Reply::json(status, json!({"error": "no"})));
        let fx = Fixture::new(&server.url);
        let before = status_with(&fx.config());
        assert!(before.signed_in);
        assert_eq!(before.apple_id.as_deref(), Some("meta@example.com"));

        assert!(matches!(fx.load().webservices(), Err(Error::SignInRequired)));
        assert!(fx.cache_json()["sign_in_required_at"].is_string());
        assert!(!status_with(&fx.config()).signed_in);

        let out = fx.cli(&["validate"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("sign in"));
        let cli_status = stdout_json(&fx.cli(&["status"]));
        assert_eq!(cli_status["signed_in"], false);

        // icloud-md signs in again and rewrites the file: signed in again.
        fx.resave_session();
        assert!(status_with(&fx.config()).signed_in);
        assert_eq!(stdout_json(&fx.cli(&["status"]))["signed_in"], true);
    }
}

#[test]
fn data_call_421_flips_status_and_success_clears_it() {
    let server = Server::start(|seen, n| match path_of(&seen.url) {
        "/setup/ws/1/validate" => validate_ok(n),
        _ => Reply::json(421, json!({})),
    });
    let fx = Fixture::new(&server.url);
    let session = fx.load();
    assert!(matches!(
        session.get(&format!("{}/fmipservice/client/web/refreshClient", server.url)),
        Err(Error::SignInRequired)
    ));
    assert!(!status_with(&fx.config()).signed_in);
    // A later successful validate proves the session works again.
    session.webservices().unwrap();
    assert!(fx.cache_json().get("sign_in_required_at").is_none());
    assert!(status_with(&fx.config()).signed_in);
}

#[test]
fn status_is_offline_and_serializes_exactly() {
    let server = Server::start(|_, n| validate_ok(n));
    let fx = Fixture::new(&server.url);
    let out = stdout_json(&fx.cli(&["status"]));
    let keys: Vec<&str> = out.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["signed_in", "apple_id", "dsid", "expires_at", "validated_at"]);
    assert_eq!(out["signed_in"], true);
    assert_eq!(out["dsid"], DSID);
    assert_eq!(out["apple_id"], "meta@example.com");
    assert_eq!(out["expires_at"], Value::Null);
    assert_eq!(out["validated_at"], Value::Null);
    assert!(server.requests().is_empty(), "status must not touch the network");

    fx.load().webservices().unwrap();
    let out = stdout_json(&fx.cli(&["status"]));
    assert_eq!(out["apple_id"], "someone@example.com");
    let validated = out["validated_at"].as_str().unwrap();
    assert_eq!(validated.len(), "2026-10-28T09:00:00Z".len(), "{validated}");
}

#[test]
fn status_without_any_account_is_signed_out_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("status")
        .env("ICLOUD_MD_CONFIG_DIR", dir.path().join("none"))
        .env("ICLOUD_SESSION_CACHE_DIR", dir.path().join("cache"))
        .env_remove("ICLOUD_SESSION_MOCK")
        .output()
        .unwrap();
    assert_eq!(
        stdout_json(&out),
        json!({"signed_in": false, "apple_id": null, "dsid": null, "expires_at": null, "validated_at": null})
    );
    assert!(!dir.path().join("cache").exists());

    let validate = Command::new(BIN)
        .arg("validate")
        .env("ICLOUD_MD_CONFIG_DIR", dir.path().join("none"))
        .env("ICLOUD_SESSION_CACHE_DIR", dir.path().join("cache"))
        .env_remove("ICLOUD_SESSION_MOCK")
        .output()
        .unwrap();
    assert_eq!(validate.status.code(), Some(2));
}

#[test]
fn deleted_session_file_is_signed_out() {
    let fx = Fixture::new("http://127.0.0.1:9");
    fs::remove_file(fx.session_path()).unwrap();
    let status = status_with(&fx.config());
    assert!(!status.signed_in);
    assert_eq!(status.dsid.as_deref(), Some(DSID));
    assert!(matches!(Session::load_with(fx.config()), Err(Error::SignInRequired)));
}

#[test]
fn newest_account_wins() {
    let fx = Fixture::new("http://127.0.0.1:9");
    thread::sleep(Duration::from_millis(20));
    let other = fx.config_dir.join("accounts").join("99999");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("session.local.json"), SESSION).unwrap();
    assert_eq!(fx.load().dsid(), "99999");
    assert_eq!(Session::load_dsid_with(fx.config(), DSID).unwrap().dsid(), DSID);
}

#[test]
fn corrupt_session_file_is_corrupt() {
    let fx = Fixture::new("http://127.0.0.1:9");
    fs::write(fx.session_path(), "{\"cookie\": \"A=1\", \"clie").unwrap();
    assert!(matches!(Session::load_with(fx.config()), Err(Error::Corrupt(_))));
}

// ---------------------------------------------------------------- data calls

#[test]
fn data_calls_attach_the_jar_append_params_and_merge_rotation() {
    let server = Server::start(|seen, n| match path_of(&seen.url) {
        "/json" => Reply::json(200, json!({"ok": true, "n": n})).cookie("X-APPLE-WEBAUTH-TOKEN=fromdata; Path=/"),
        "/boom" => Reply::json(500, json!({"reason": "boom"})),
        "/file" => Reply::json(200, json!("file-bytes")),
        _ => Reply::json(404, json!({})),
    });
    let fx = Fixture::new(&server.url);
    let session = fx.load();

    let r = session
        .get(&format!("{}/json?existing=1&dsid=override", server.url))
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.json::<Value>().unwrap()["ok"], true);
    let req = server.requests().pop().unwrap();
    let q = query_of(&req.url);
    assert_eq!(
        q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        [
            "existing=1",
            "dsid=override",
            "clientBuildNumber=2624Build27",
            "clientMasteringNumber=2624Build27M",
            "clientId=auth-client-id"
        ]
    );
    assert_eq!(req.header("Origin"), Some("https://www.icloud.com"));
    assert!(req.header("Cookie").unwrap().contains("X-APPLE-WEBAUTH-TOKEN=original"));
    assert!(fx.cookie().contains("X-APPLE-WEBAUTH-TOKEN=fromdata;"));

    // A clone re-reads the file, so it presents the rotated cookie.
    session
        .clone()
        .post_json(&format!("{}/json", server.url), &json!({"q": 1}))
        .unwrap();
    let req = server.requests().pop().unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.header("Content-Type"), Some("application/json"));
    assert_eq!(serde_json::from_slice::<Value>(&req.body).unwrap(), json!({"q": 1}));
    assert!(req.header("Cookie").unwrap().contains("X-APPLE-WEBAUTH-TOKEN=fromdata"));
    assert!(query_of(&req.url).iter().any(|(k, v)| k == "dsid" && v == DSID));

    session
        .post_bytes(&format!("{}/json", server.url), "image/jpeg", b"\xff\xd8".to_vec())
        .unwrap();
    let req = server.requests().pop().unwrap();
    assert_eq!(req.header("Content-Type"), Some("image/jpeg"));
    assert_eq!(req.body, b"\xff\xd8");

    match session.get(&format!("{}/boom", server.url)) {
        Err(Error::Http { status: 500, body }) => assert!(body.contains("boom")),
        other => panic!("{other:?}"),
    }

    let dest = fx.cache_dir.join("out.bin");
    fs::create_dir_all(&fx.cache_dir).unwrap();
    let n = session
        .download(&format!("{}/file?sig=abc", server.url), &dest)
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"\"file-bytes\"");
    assert_eq!(n, 12);
    let req = server.requests().pop().unwrap();
    assert_eq!(req.url, "/file?sig=abc", "download adds no client params");
    assert!(req.header("Cookie").is_some());
    assert_eq!(fs::read_dir(&fx.cache_dir).unwrap().count(), 1, "no temp file left");

    assert!(matches!(
        session.download(&format!("{}/missing", server.url), &fx.cache_dir.join("missing.bin")),
        Err(Error::Http { status: 404, .. })
    ));
    assert!(!fx.cache_dir.join("missing.bin").exists());
}

#[test]
fn every_request_rereads_the_session_file() {
    let server = Server::start(|_, _| Reply::json(200, json!({})));
    let fx = Fixture::new(&server.url);
    let session = fx.load();
    // icloud-md rotates on its own, behind our back.
    fs::write(fx.session_path(), SESSION.replace("=original", "=by-icloud-md")).unwrap();
    session.get(&format!("{}/x", server.url)).unwrap();
    assert!(
        server.requests()[0]
            .header("Cookie")
            .unwrap()
            .contains("X-APPLE-WEBAUTH-TOKEN=by-icloud-md")
    );
}

// ---------------------------------------------------------------- expiry

fn chromium_micros(t: SystemTime) -> i64 {
    (t.duration_since(UNIX_EPOCH).unwrap().as_micros() as i64) + 11_644_473_600 * 1_000_000
}

fn write_cookies_db(path: &Path, rows: &[(&str, &str, i64, i64)]) -> rusqlite::Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies (creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL, name TEXT NOT NULL,
         value TEXT NOT NULL, expires_utc INTEGER NOT NULL, is_persistent INTEGER NOT NULL)",
    )
    .unwrap();
    for (host, name, expires, persistent) in rows {
        conn.execute(
            "INSERT INTO cookies VALUES (0, ?1, ?2, '', ?3, ?4)",
            rusqlite::params![host, name, expires, persistent],
        )
        .unwrap();
    }
    conn
}

#[test]
fn expiry_is_read_from_the_chromium_cookies_db() {
    let fx = Fixture::new("http://127.0.0.1:9");
    let expires = UNIX_EPOCH + Duration::from_secs(4_102_444_800); // 2100-01-01T00:00:00Z
    let earlier = UNIX_EPOCH + Duration::from_secs(4_000_000_000);
    let db = fx.account_dir().join("browser-profile/Default/Cookies");
    let conn = write_cookies_db(
        &db,
        &[
            (".icloud.com", "X-APPLE-WEBAUTH-TOKEN", chromium_micros(earlier), 1),
            (".icloud.com", "X-APPLE-WEBAUTH-TOKEN", chromium_micros(expires), 1),
            (
                ".icloud.com",
                "X-APPLE-WEBAUTH-TOKEN",
                chromium_micros(expires) + 10_000_000_000,
                0,
            ),
            (
                "www.icloud.com",
                "X-APPLE-WEBAUTH-TOKEN",
                chromium_micros(expires) + 20_000_000_000,
                1,
            ),
            (".icloud.com", "OTHER", chromium_micros(expires) + 30_000_000_000, 1),
        ],
    );
    drop(conn);

    assert_eq!(icloud_session::expiry::token_expiry(&db), Some(expires));
    assert_eq!(fx.load().expires_at(), Some(expires));
    assert_eq!(
        status_with(&fx.config()).expires_at.as_deref(),
        Some("2100-01-01T00:00:00Z")
    );
    assert_eq!(stdout_json(&fx.cli(&["status"]))["expires_at"], "2100-01-01T00:00:00Z");

    // Chromium keeps the DB exclusively locked while running: read a copy.
    let locker = rusqlite::Connection::open(&db).unwrap();
    locker
        .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    assert_eq!(icloud_session::expiry::token_expiry(&db), Some(expires));
    locker.execute_batch("COMMIT;").unwrap();
}

#[test]
fn expired_token_or_session_only_cookie() {
    let fx = Fixture::new("http://127.0.0.1:9");
    let db = fx.account_dir().join("browser-profile/Default/Cookies");
    let past = SystemTime::now() - Duration::from_secs(3600);
    write_cookies_db(
        &db,
        &[(".icloud.com", "X-APPLE-WEBAUTH-TOKEN", chromium_micros(past), 1)],
    );
    assert!(!status_with(&fx.config()).signed_in);

    fs::remove_file(&db).unwrap();
    write_cookies_db(&db, &[(".icloud.com", "X-APPLE-WEBAUTH-TOKEN", 0, 0)]);
    let status = status_with(&fx.config());
    assert!(status.signed_in);
    assert_eq!(status.expires_at, None);
    assert_eq!(
        icloud_session::expiry::token_expiry(&fx.account_dir().join("nope")),
        None
    );
}

// ---------------------------------------------------------------- mock

#[test]
fn mock_mode_sends_everything_to_the_mock_url() {
    let server = Server::start(|seen, _| match path_of(&seen.url) {
        "/database/1/com.apple.photos.cloud/production/private/records/query" => {
            Reply::json(200, json!({"records": []}))
        }
        _ => Reply::json(421, json!({})),
    });
    let session = Session::load_with(Config::mock(&server.url)).unwrap();
    assert_eq!(session.dsid(), "mock");
    let ws = session.webservices().unwrap();
    assert_eq!(ws.url("ckdatabasews"), Some(server.url.as_str()));
    assert_eq!(ws.url("findme"), Some(server.url.as_str()));

    let url = format!(
        "{}/database/1/com.apple.photos.cloud/production/private/records/query",
        ws.url("ckdatabasews").unwrap()
    );
    assert_eq!(
        session.post_json(&url, &json!({})).unwrap().json::<Value>().unwrap(),
        json!({"records": []})
    );
    // Absolute Apple URLs (e.g. asset downloads) are rewritten to the mock host.
    let apple =
        "https://p42-ckdatabasews.icloud.com/database/1/com.apple.photos.cloud/production/private/records/query?x=1";
    assert!(session.post_json(apple, &json!({})).is_ok());
    let req = server.requests().pop().unwrap();
    assert!(
        req.url
            .starts_with("/database/1/com.apple.photos.cloud/production/private/records/query?x=1&")
    );
    assert!(query_of(&req.url).iter().any(|(k, v)| k == "dsid" && v == "mock"));
    assert!(matches!(
        session.get(&format!("{}/expired", server.url)),
        Err(Error::SignInRequired)
    ));
    assert_eq!(server.count("/setup/ws/1/validate"), 0);
    assert!(session.expires_at().is_some());
}

#[test]
fn mock_mode_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .env("ICLOUD_SESSION_MOCK", "1")
            .env("ICLOUD_SESSION_MOCK_URL", "http://127.0.0.1:9999")
            .env("ICLOUD_MD_CONFIG_DIR", dir.path().join("config"))
            .env("ICLOUD_SESSION_CACHE_DIR", dir.path().join("cache"))
            .output()
            .unwrap()
    };
    let status = stdout_json(&run(&["status"]));
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["dsid"], "mock");
    assert_eq!(status["apple_id"], "mock@example.com");
    let validate = stdout_json(&run(&["validate"]));
    assert_eq!(validate["webservices"]["findme"], "http://127.0.0.1:9999");
    assert_eq!(validate["webservices"]["ckdatabasews"], "http://127.0.0.1:9999");
    stdout_json(&run(&["reauthenticate"]));
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        0,
        "mock mode touches no files"
    );
}

// ---------------------------------------------------------------- reauthenticate / CLI

#[test]
fn reauthenticate_runs_icloud_md_and_maps_failure() {
    let fx = Fixture::new("http://127.0.0.1:9");
    let bin_dir = tempfile::tempdir().unwrap();
    let script = |name: &str, body: &str| {
        let path = bin_dir.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let args_file = bin_dir.path().join("args");
    let ok = script("ok", &format!("echo \"$@\" > {}", args_file.display()));
    let fail = script("fail", "exit 1");
    let run = |bin: &Path, args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .env("ICLOUD_MD_CONFIG_DIR", &fx.config_dir)
            .env("ICLOUD_SESSION_CACHE_DIR", &fx.cache_dir)
            .env("ICLOUD_MD_BIN", bin)
            .env("ICLOUD_NOTES_VAULT", fx.config_dir.join("no-vault"))
            .env("XDG_DOCUMENTS_DIR", fx.config_dir.join("no-documents"))
            .env_remove("ICLOUD_SESSION_MOCK")
            .output()
            .unwrap()
    };
    let out = stdout_json(&run(&ok, &["reauthenticate", "/some/vault"]));
    assert_eq!(out["signed_in"], true);
    assert_eq!(
        fs::read_to_string(&args_file).unwrap().trim(),
        "reauthenticate /some/vault"
    );
    assert_eq!(run(&fail, &["reauthenticate"]).status.code(), Some(2));
    assert_eq!(
        run(Path::new("/nonexistent/icloud-md"), &["reauthenticate"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(run(&ok, &["bogus"]).status.code(), Some(64));
}

#[test]
fn reauthenticate_runs_in_the_notes_vault_bound_to_the_account() {
    let fx = Fixture::new("http://127.0.0.1:9");
    let dirs = tempfile::tempdir().unwrap();
    let vault = |name: &str, dsid: &str| {
        let path = dirs.path().join(name);
        fs::create_dir_all(path.join(".icloud-md")).unwrap();
        fs::write(
            path.join(".icloud-md/state.json"),
            format!(r#"{{"account":{{"appleId":"someone@example.com","dsid":"{dsid}"}}}}"#),
        )
        .unwrap();
        path
    };
    let other = vault("other", "999");
    let ours = vault("ours", DSID);
    let args_file = dirs.path().join("args");
    let bin = dirs.path().join("icloud-md");
    fs::write(&bin, format!("#!/bin/sh\necho \"$@\" > {}\n", args_file.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    let config = Config {
        icloud_md_bin: bin,
        notes_vaults: vec![dirs.path().join("missing"), other, ours.clone()],
        ..fx.config()
    };
    icloud_session::reauthenticate_with(&config, None).unwrap();
    assert_eq!(
        fs::read_to_string(&args_file).unwrap().trim(),
        format!("reauthenticate {}", ours.display())
    );

    let none = Config {
        notes_vaults: vec![dirs.path().join("missing")],
        ..config
    };
    icloud_session::reauthenticate_with(&none, None).unwrap();
    assert_eq!(fs::read_to_string(&args_file).unwrap().trim(), "reauthenticate");
}
