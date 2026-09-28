//! Client for `icloud-sessiond`, the D-Bus user service that owns one
//! signed-in iCloud web session for every app on the machine.
//!
//! The daemon holds the cookie jar, calls Apple's `/validate`, and signs in
//! through its own WebKitGTK window. This crate asks it for the current
//! cookie header, client parameters and webservices over D-Bus, sends
//! requests straight to Apple with them, hands rotated cookies back
//! (`MergeCookies`), and reports a 421/401 (`ReportSignInRequired`) so the
//! daemon can confirm before it signs every app out.
//!
//! ```no_run
//! # fn main() -> icloud_session::Result<()> {
//! let s = icloud_session::Session::connect()?;
//! let ws = s.webservices()?;
//! let findme = ws.url("findme").unwrap();
//! let r = s.post_json(&format!("{findme}/fmipservice/client/web/refreshClient"), &serde_json::json!({}))?;
//! # let _ = r; Ok(()) }
//! ```
//!
//! Environment:
//! - `ICLOUD_SESSION_MOCK=1`: no D-Bus. A fake signed-in session whose
//!   webservices, and every request, go to `ICLOUD_SESSION_MOCK_URL`
//!   (default `http://127.0.0.1:8765`) keeping path and query.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

/// Well-known bus name of the daemon, on the session bus.
pub const BUS_NAME: &str = "io.github.ferdousbhai.ICloudSession";
/// Object path of the daemon's one object.
pub const OBJECT_PATH: &str = "/io/github/ferdousbhai/ICloudSession";
/// Interface name, same as the bus name.
pub const INTERFACE: &str = "io.github.ferdousbhai.ICloudSession";
/// D-Bus error name the daemon returns from `Session()` when signed out.
pub const ERROR_SIGN_IN_REQUIRED: &str = "io.github.ferdousbhai.ICloudSession.Error.SignInRequired";

/// `Session()`'s reply: cookie header, client params, webservices.
pub type SessionReply = (String, HashMap<String, String>, HashMap<String, String>);

/// Where mock mode sends everything unless `ICLOUD_SESSION_MOCK_URL` says otherwise.
pub const DEFAULT_MOCK_URL: &str = "http://127.0.0.1:8765";
/// The dsid of the mock session.
pub const MOCK_DSID: &str = "mock";
/// The Apple ID of the mock session.
pub const MOCK_APPLE_ID: &str = "mock@example.com";
/// Webservices keys the mock session reports, all pointing at the mock URL.
pub const MOCK_WEBSERVICES: &[&str] = &[
    "account",
    "calendar",
    "ckdatabasews",
    "ckdeviceservice",
    "contacts",
    "docws",
    "drivews",
    "findme",
    "iworkexportws",
    "keyvalue",
    "photos",
    "photosupload",
    "push",
    "reminders",
    "streams",
    "ubiquity",
    "uploadimagews",
];

/// How long a `Session()` answer is reused in-process.
pub const SESSION_CACHE_TTL: Duration = Duration::from_secs(60);

const ORIGIN: &str = "https://www.icloud.com";
const REFERER: &str = "https://www.icloud.com/";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Signed out, or Apple answered 421/401 and the daemon confirmed it.
    /// Apps show a sign-in banner that calls [`sign_in`].
    #[error("sign in to iCloud required")]
    SignInRequired,
    /// Any other non-2xx answer, including a 421/401 that persists after a
    /// retry although the daemon's own `/validate` still succeeds.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    /// `icloud-sessiond` could not be reached or failed.
    #[error("icloud-sessiond: {0}")]
    Service(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<zbus::Error> for Error {
    fn from(e: zbus::Error) -> Error {
        match &e {
            zbus::Error::MethodError(name, _, _) if name.as_str() == ERROR_SIGN_IN_REQUIRED => Error::SignInRequired,
            zbus::Error::MethodError(name, Some(msg), _) => Error::Service(format!("{}: {msg}", name.as_str())),
            _ => Error::Service(e.to_string()),
        }
    }
}

impl From<zbus::fdo::Error> for Error {
    fn from(e: zbus::fdo::Error) -> Error {
        Error::from(zbus::Error::from(e))
    }
}

/// The `webservices` map from `/validate`, e.g. `ckdatabasews`, `findme`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Webservices {
    pub urls: HashMap<String, String>,
}

impl Webservices {
    pub fn url(&self, key: &str) -> Option<&str> {
        self.urls.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Network(format!("bad JSON: {e}")))
    }
}

/// The daemon's properties in one value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub signed_in: bool,
    pub apple_id: Option<String>,
    pub dsid: Option<String>,
    /// Unix seconds when the X-APPLE-WEBAUTH-TOKEN cookie expires; `None`
    /// for a session-only cookie or when unknown.
    pub expires_at: Option<u64>,
    /// The sign-in window is open.
    pub signing_in: bool,
}

fn mock_url() -> Option<String> {
    let on = std::env::var("ICLOUD_SESSION_MOCK").is_ok_and(|v| !v.is_empty() && v != "0");
    on.then(|| {
        std::env::var("ICLOUD_SESSION_MOCK_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_MOCK_URL.to_string())
            .trim_end_matches('/')
            .to_string()
    })
}

fn mock_status() -> Status {
    Status {
        signed_in: true,
        apple_id: Some(MOCK_APPLE_ID.to_string()),
        dsid: Some(MOCK_DSID.to_string()),
        expires_at: Some(unix_now() + 30 * 24 * 3600),
        signing_in: false,
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ------------------------------------------------------------------ D-Bus

#[zbus::proxy(
    interface = "io.github.ferdousbhai.ICloudSession",
    default_service = "io.github.ferdousbhai.ICloudSession",
    default_path = "/io/github/ferdousbhai/ICloudSession",
    gen_async = false,
    blocking_name = "DaemonProxy"
)]
trait Daemon {
    #[zbus(name = "Session")]
    fn session(&self) -> zbus::Result<SessionReply>;
    fn merge_cookies(&self, set_cookies: &[&str]) -> zbus::Result<()>;
    fn report_sign_in_required(&self) -> zbus::Result<bool>;
    fn sign_in(&self) -> zbus::Result<()>;
    fn sign_out(&self) -> zbus::Result<()>;
}

fn proxy(conn: &Connection) -> Result<DaemonProxy<'static>> {
    Ok(DaemonProxy::builder(conn)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?)
}

fn session_bus() -> Result<Connection> {
    Ok(Connection::session()?)
}

fn read_status(conn: &Connection) -> Result<Status> {
    let props = zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)?
        .path(OBJECT_PATH)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?;
    let all = props.get_all(zbus::names::InterfaceName::from_static_str_unchecked(INTERFACE))?;
    Ok(status_from(&all))
}

fn status_from(all: &HashMap<String, OwnedValue>) -> Status {
    let bool_of = |k: &str| all.get(k).and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
    let str_of = |k: &str| {
        all.get(k)
            .and_then(|v| <&str>::try_from(v).ok())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Status {
        signed_in: bool_of("SignedIn"),
        apple_id: str_of("AppleId"),
        dsid: str_of("Dsid"),
        expires_at: all
            .get("ExpiresAt")
            .and_then(|v| u64::try_from(v).ok())
            .filter(|&t| t > 0),
        signing_in: bool_of("SigningIn"),
    }
}

/// Asks the daemon to open its sign-in window (no-op if already open).
/// Returns at once; the outcome arrives as [`Status`] changes ([`watch`]).
pub fn sign_in() -> Result<()> {
    if mock_url().is_some() {
        return Ok(());
    }
    sign_in_on(&session_bus()?)
}

/// [`sign_in`] on a given bus connection.
pub fn sign_in_on(conn: &Connection) -> Result<()> {
    Ok(proxy(conn)?.sign_in()?)
}

/// Forgets the account and the sign-in window's WebKit profile.
pub fn sign_out() -> Result<()> {
    if mock_url().is_some() {
        return Ok(());
    }
    sign_out_on(&session_bus()?)
}

/// [`sign_out`] on a given bus connection.
pub fn sign_out_on(conn: &Connection) -> Result<()> {
    Ok(proxy(conn)?.sign_out()?)
}

/// The daemon's properties, one D-Bus round trip (starts the daemon if needed).
pub fn status() -> Result<Status> {
    if mock_url().is_some() {
        return Ok(mock_status());
    }
    status_on(&session_bus()?)
}

/// [`status`] on a given bus connection.
pub fn status_on(conn: &Connection) -> Result<Status> {
    read_status(conn)
}

/// A blocking iterator yielding the new [`Status`] after every change.
/// Run it on its own thread. In mock mode it never yields.
pub fn watch() -> Result<Watch> {
    if mock_url().is_some() {
        return Ok(Watch { inner: None });
    }
    watch_on(&session_bus()?)
}

/// [`watch`] on a given bus connection.
pub fn watch_on(conn: &Connection) -> Result<Watch> {
    let props = zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)?
        .path(OBJECT_PATH)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?;
    // Subscribe before reading, so no change falls between the two.
    let signals = props.receive_properties_changed()?;
    let last = read_status(conn)?;
    Ok(Watch {
        inner: Some(WatchInner {
            conn: conn.clone(),
            signals,
            last,
        }),
    })
}

/// See [`watch`].
pub struct Watch {
    inner: Option<WatchInner>,
}

struct WatchInner {
    conn: Connection,
    signals: zbus::blocking::fdo::PropertiesChangedIterator,
    last: Status,
}

impl Watch {
    /// The status as of the last yielded change (or the start of the watch).
    pub fn current(&self) -> Option<&Status> {
        self.inner.as_ref().map(|w| &w.last)
    }
}

impl Iterator for Watch {
    type Item = Status;

    fn next(&mut self) -> Option<Status> {
        let w = self.inner.as_mut()?;
        loop {
            let signal = w.signals.next()?;
            let Ok(args) = signal.args() else { continue };
            if args.interface_name().as_str() != INTERFACE {
                continue;
            }
            let mut next = w.last.clone();
            for (name, value) in args.changed_properties() {
                let Ok(value) = OwnedValue::try_from(value) else {
                    continue;
                };
                apply_property(&mut next, name, &value);
            }
            if !args.invalidated_properties().is_empty()
                && let Ok(fresh) = read_status(&w.conn)
            {
                next = fresh;
            }
            if next != w.last {
                w.last = next.clone();
                return Some(next);
            }
        }
    }
}

fn apply_property(status: &mut Status, name: &str, value: &OwnedValue) {
    let mut one = HashMap::new();
    one.insert(name.to_string(), value.try_clone().expect("plain values clone"));
    let parsed = status_from(&one);
    match name {
        "SignedIn" => status.signed_in = parsed.signed_in,
        "AppleId" => status.apple_id = parsed.apple_id,
        "Dsid" => status.dsid = parsed.dsid,
        "ExpiresAt" => status.expires_at = parsed.expires_at,
        "SigningIn" => status.signing_in = parsed.signing_in,
        _ => {}
    }
}

// ---------------------------------------------------------------- Session

/// A signed-in account, as the daemon hands it out. Cheap to clone and
/// safe to share between threads; every call is blocking.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` in mock mode.
    conn: Option<Connection>,
    mock_url: Option<String>,
    agent: ureq::Agent,
    cached: Mutex<Option<(Instant, Snapshot)>>,
    apple_id: String,
    dsid: String,
}

/// One `Session()` answer.
#[derive(Clone)]
struct Snapshot {
    cookie: String,
    params: HashMap<String, String>,
    webservices: HashMap<String, String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("dsid", &self.inner.dsid)
            .field("mock", &self.inner.mock_url.is_some())
            .finish()
    }
}

enum Sent {
    Ok(Box<ureq::Response>),
    Unauthorized { status: u16, body: String },
}

/// The request a public method asked for, before cookies and params.
struct Request<'a> {
    method: &'a str,
    url: &'a str,
    content_type: Option<&'a str>,
    body: Option<&'a [u8]>,
    client_params: bool,
    accept: &'a str,
}

impl Session {
    /// Connects to `icloud-sessiond` on the session bus (D-Bus activates it)
    /// and fetches the session. `SignInRequired` when signed out.
    /// With `ICLOUD_SESSION_MOCK=1`, a fake session and no D-Bus.
    pub fn connect() -> Result<Session> {
        match mock_url() {
            Some(base) => Ok(Session::mock(&base)),
            None => Session::connect_on(&session_bus()?),
        }
    }

    /// [`Session::connect`] on a given bus connection.
    pub fn connect_on(conn: &Connection) -> Result<Session> {
        let status = read_status(conn)?;
        if !status.signed_in {
            return Err(Error::SignInRequired);
        }
        let session = Session::new(
            Some(conn.clone()),
            None,
            status.apple_id.unwrap_or_default(),
            status.dsid.unwrap_or_default(),
        );
        session.snapshot()?;
        Ok(session)
    }

    /// A mock session sending everything to `base_url` (what
    /// `ICLOUD_SESSION_MOCK=1` gives [`Session::connect`]).
    pub fn mock(base_url: &str) -> Session {
        Session::new(
            None,
            Some(base_url.trim_end_matches('/').to_string()),
            MOCK_APPLE_ID.to_string(),
            MOCK_DSID.to_string(),
        )
    }

    fn new(conn: Option<Connection>, mock_url: Option<String>, apple_id: String, dsid: String) -> Session {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(120))
            .timeout_write(Duration::from_secs(120))
            .user_agent(concat!("icloud-session/", env!("CARGO_PKG_VERSION")))
            .build();
        Session {
            inner: Arc::new(Inner {
                conn,
                mock_url,
                agent,
                cached: Mutex::new(None),
                apple_id,
                dsid,
            }),
        }
    }

    pub fn apple_id(&self) -> &str {
        &self.inner.apple_id
    }

    pub fn dsid(&self) -> &str {
        &self.inner.dsid
    }

    fn mock_url(&self) -> Option<&str> {
        self.inner.mock_url.as_deref()
    }

    /// The daemon's `Session()`, reused in-process for up to
    /// [`SESSION_CACHE_TTL`], dropped after any cookie rotation.
    fn snapshot(&self) -> Result<Snapshot> {
        if let Some(base) = self.mock_url() {
            let params = ["clientBuildNumber", "clientMasteringNumber", "clientId"]
                .map(|k| (k.to_string(), "mock".to_string()))
                .into();
            return Ok(Snapshot {
                cookie: String::new(),
                params,
                webservices: mock_webservices(base),
            });
        }
        let mut cached = self.inner.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, snap)) = cached.as_ref()
            && at.elapsed() < SESSION_CACHE_TTL
        {
            return Ok(snap.clone());
        }
        let conn = self.inner.conn.as_ref().expect("a real session has a connection");
        let (cookie, params, webservices) = proxy(conn)?.session()?;
        let snap = Snapshot {
            cookie,
            params,
            webservices,
        };
        *cached = Some((Instant::now(), snap.clone()));
        Ok(snap)
    }

    fn forget_snapshot(&self) {
        *self.inner.cached.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The `webservices` map from the daemon's last `/validate`
    /// (it revalidates when that is older than 10 minutes).
    pub fn webservices(&self) -> Result<Webservices> {
        Ok(Webservices {
            urls: self.snapshot()?.webservices,
        })
    }

    /// GET with the cookie header, `Origin`/`Referer: https://www.icloud.com`,
    /// and the client query params (clientBuildNumber, clientMasteringNumber,
    /// clientId, dsid) appended unless the URL has them. `Set-Cookie`s go
    /// back to the daemon. 421/401 → the daemon confirms with Apple:
    /// signed out → `SignInRequired`; still signed in → one retry with the
    /// fresh jar. Other non-2xx → `Http`.
    pub fn get(&self, url: &str) -> Result<Response> {
        self.request(Request {
            method: "GET",
            url,
            content_type: None,
            body: None,
            client_params: true,
            accept: "application/json",
        })
    }

    /// POST a JSON body, same behaviour as [`Session::get`].
    pub fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<Response> {
        let bytes = serde_json::to_vec(body).map_err(|e| Error::Network(format!("cannot encode JSON: {e}")))?;
        self.request(Request {
            method: "POST",
            url,
            content_type: Some("application/json"),
            body: Some(&bytes),
            client_params: true,
            accept: "application/json",
        })
    }

    /// POST raw bytes with a content type (uploads), same behaviour as [`Session::get`].
    pub fn post_bytes(&self, url: &str, content_type: &str, body: Vec<u8>) -> Result<Response> {
        self.request(Request {
            method: "POST",
            url,
            content_type: Some(content_type),
            body: Some(&body),
            client_params: true,
            accept: "application/json",
        })
    }

    fn request(&self, request: Request<'_>) -> Result<Response> {
        let response = self.send(&request)?;
        let status = response.status();
        let body = read_body(response)?;
        Ok(Response { status, body })
    }

    /// Streams a URL to `dest` (written to a temp file, renamed on success).
    /// Returns bytes written. Cookies attached; no client params appended.
    pub fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let response = self.send(&Request {
            method: "GET",
            url,
            content_type: None,
            body: None,
            client_params: false,
            accept: "*/*",
        })?;
        let tmp = temp_sibling(dest);
        let result = (|| -> Result<u64> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(&tmp)?;
            let written = io::copy(&mut response.into_reader(), &mut file)
                .map_err(|e| Error::Network(format!("download interrupted: {e}")))?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&tmp, dest)?;
            Ok(written)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Sends one request. Returns the 2xx response after handing its
    /// `Set-Cookie`s to the daemon; maps every other status. On 421/401 the
    /// daemon confirms with Apple: still signed in → one retry with the
    /// fresh jar; signed out → `SignInRequired`.
    fn send(&self, request: &Request<'_>) -> Result<ureq::Response> {
        match self.send_once(request)? {
            Sent::Ok(response) => Ok(*response),
            Sent::Unauthorized { .. } if !self.report_sign_in_required()? => Err(Error::SignInRequired),
            Sent::Unauthorized { .. } => match self.send_once(request)? {
                Sent::Ok(response) => Ok(*response),
                Sent::Unauthorized { status, body } => {
                    if self.report_sign_in_required()? {
                        Err(Error::Http { status, body })
                    } else {
                        Err(Error::SignInRequired)
                    }
                }
            },
        }
    }

    /// `ReportSignInRequired()`: true when the daemon still has a session.
    fn report_sign_in_required(&self) -> Result<bool> {
        self.forget_snapshot();
        match &self.inner.conn {
            Some(conn) => Ok(proxy(conn)?.report_sign_in_required()?),
            None => Ok(false),
        }
    }

    fn send_once(&self, request: &Request<'_>) -> Result<Sent> {
        let snap = self.snapshot()?;
        let url = self.prepare(request, &snap)?;
        let mut req = self
            .inner
            .agent
            .request(request.method, &url)
            .set("Origin", ORIGIN)
            .set("Referer", REFERER)
            .set("Accept", request.accept);
        if !snap.cookie.is_empty() {
            req = req.set("Cookie", &snap.cookie);
        }
        if let Some(content_type) = request.content_type {
            req = req.set("Content-Type", content_type);
        }
        let result = match request.body {
            Some(body) => req.send_bytes(body),
            None => req.call(),
        };
        match result {
            Ok(response) => {
                let set_cookies: Vec<&str> = response.all("set-cookie");
                if !set_cookies.is_empty()
                    && let Some(conn) = &self.inner.conn
                {
                    proxy(conn)?.merge_cookies(&set_cookies)?;
                    self.forget_snapshot();
                }
                Ok(Sent::Ok(Box::new(response)))
            }
            Err(ureq::Error::Status(status @ (401 | 421), response)) => Ok(Sent::Unauthorized {
                status,
                body: read_body_lossy(response),
            }),
            Err(ureq::Error::Status(status, response)) => Err(Error::Http {
                status,
                body: read_body_lossy(response),
            }),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// The final URL: client params added, and in mock mode rewritten to
    /// the mock base keeping path and query.
    fn prepare(&self, request: &Request<'_>, snap: &Snapshot) -> Result<String> {
        let bad_url = |e: url::ParseError| Error::Network(format!("bad URL {}: {e}", request.url));
        let mut url = url::Url::parse(request.url).map_err(bad_url)?;
        if let Some(base) = self.mock_url()
            && !request.url.starts_with(base)
        {
            let mut rewritten = format!("{base}{}", url.path());
            if let Some(query) = url.query() {
                rewritten.push('?');
                rewritten.push_str(query);
            }
            url = url::Url::parse(&rewritten).map_err(bad_url)?;
        }
        if request.client_params {
            let present: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
            let param = |k: &str| snap.params.get(k).map(String::as_str).unwrap_or_default();
            let wanted = [
                ("clientBuildNumber", param("clientBuildNumber")),
                ("clientMasteringNumber", param("clientMasteringNumber")),
                ("clientId", param("clientId")),
                ("dsid", self.inner.dsid.as_str()),
            ];
            let missing: Vec<_> = wanted.iter().filter(|(k, _)| !present.iter().any(|p| p == k)).collect();
            if !missing.is_empty() {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in missing {
                    pairs.append_pair(k, v);
                }
            }
        }
        Ok(url.into())
    }
}

fn mock_webservices(base: &str) -> HashMap<String, String> {
    MOCK_WEBSERVICES
        .iter()
        .map(|k| (k.to_string(), base.to_string()))
        .collect()
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique sibling path for a temp file, so `rename` stays on one filesystem.
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

fn read_body(response: ureq::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| Error::Network(format!("reading response: {e}")))?;
    Ok(body)
}

fn read_body_lossy(response: ureq::Response) -> String {
    let mut body = String::new();
    let _ = response.into_reader().take(64 * 1024).read_to_string(&mut body);
    body
}
