//! Client for `icloud-sessiond`, the D-Bus user service that owns one
//! signed-in iCloud web session for every app on the machine.
//!
//! The daemon holds the cookie jar, calls Apple's `/validate`, and signs in
//! through its own WebKitGTK window. This crate asks it for the current
//! cookie header, client parameters and webservices over D-Bus, sends
//! requests straight to Apple with them, hands rotated cookies back
//! (`MergeCookies`), and reports a 421/401 (`ReportSignInRequired`) so the
//! daemon can confirm before it signs every app out. Find My wants its own
//! session: the password entered again on icloud.com/find, a one-factor
//! sign-in the daemon keeps apart ([`authorize_find_my`]), or makes itself
//! from a password the user stored in the keyring (`icloud-session
//! set-password`). Requests to the `findme` host carry that jar; without
//! one, or on a 450 the daemon cannot answer with a new one, they return
//! [`Error::FindMyAuthRequired`].
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
//! - `ICLOUD_SESSION_MOCK` set to anything but empty or `0`: no D-Bus. A
//!   fake signed-in session whose webservices, and every request, go to
//!   `ICLOUD_SESSION_MOCK_URL` (default `http://127.0.0.1:8765`) keeping
//!   path and query; [`sign_in`] posts to the fake's `/mock/reauthenticate`.

pub mod cli;
pub mod time;

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_lite::{Stream, StreamExt};
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
/// D-Bus error name the daemon returns from `FindMySession()` when Find My
/// has not been authorized.
pub const ERROR_FIND_MY_AUTH_REQUIRED: &str = "io.github.ferdousbhai.ICloudSession.Error.FindMyAuthRequired";

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
    /// Apps show a sign-in banner that calls [`sign_in`]. Also returned by a
    /// [`Session`] whose account is no longer the daemon's (signed in as
    /// someone else since): connect again.
    #[error("sign in to iCloud required")]
    SignInRequired,
    /// Find My has no session of its own yet, or answered HTTP 450: it
    /// wants the Apple ID password entered again. Apps show a banner that
    /// calls [`authorize_find_my`] (not [`sign_in`]).
    #[error("Find My needs the Apple ID password")]
    FindMyAuthRequired,
    /// Any other non-2xx answer, including a 421/401 that persists after a
    /// retry although the daemon's own `/validate` still succeeds.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    /// The host's name did not resolve, or nothing answered the connect
    /// (refused, or no answer within 10 s): most likely no network. Every
    /// other request would fail the same way, so a sync can skip its run
    /// rather than try each one.
    #[error("offline: {0}")]
    Offline(String),
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
            zbus::Error::MethodError(name, _, _) if name.as_str() == ERROR_FIND_MY_AUTH_REQUIRED => {
                Error::FindMyAuthRequired
            }
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
    /// The sign-in window is open (also while authorizing Find My).
    pub signing_in: bool,
    /// Find My was authorized with [`authorize_find_my`] and has not asked
    /// for the password again since. False when the daemon predates it.
    pub find_my_authorized: bool,
    /// The Apple ID password is stored in the keyring, so the daemon
    /// re-authorizes Find My by itself (`icloud-session set-password`).
    /// False when signed out, or when the daemon predates it.
    pub find_my_password_stored: bool,
}

/// The mock base URL when mock mode is on (`ICLOUD_SESSION_MOCK` set to
/// anything but empty or `0`), without a trailing slash.
pub fn mock_url() -> Option<String> {
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
        expires_at: Some(time::now_secs() + 30 * 24 * 3600),
        signing_in: false,
        find_my_authorized: true,
        find_my_password_stored: false,
    }
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
    fn report_find_my_auth_required(&self) -> zbus::Result<bool>;
    #[zbus(name = "FindMySession")]
    fn find_my_session(&self) -> zbus::Result<(String, HashMap<String, String>)>;
    fn merge_find_my_cookies(&self, set_cookies: &[&str]) -> zbus::Result<()>;
    fn sign_in(&self) -> zbus::Result<()>;
    fn authorize_find_my(&self) -> zbus::Result<()>;
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
        find_my_authorized: bool_of("FindMyAuthorized"),
        find_my_password_stored: bool_of("FindMyPasswordStored"),
    }
}

/// Asks the daemon to open its sign-in window (no-op if already open).
/// Returns at once; the outcome arrives as [`Status`] changes ([`watch`]).
/// In mock mode it tells the fake server instead
/// (`POST {mock url}/mock/reauthenticate`), which signs its account back in.
pub fn sign_in() -> Result<()> {
    if let Some(base) = mock_url() {
        Session::mock(&base).post_json(&format!("{base}/mock/reauthenticate"), &serde_json::json!({}))?;
        return Ok(());
    }
    sign_in_on(&session_bus()?)
}

/// [`sign_in`] on a given bus connection.
pub fn sign_in_on(conn: &Connection) -> Result<()> {
    Ok(proxy(conn)?.sign_in()?)
}

/// Asks the daemon to open its sign-in window on Find My, where Apple asks
/// for the Apple ID password before Find My answers (no-op if a window is
/// already open). Returns at once; `signing_in` is true while the window is
/// open, and `find_my_authorized` turns true once it is done ([`watch`]).
/// What it captures is kept as a separate Find My session, used for the
/// `findme` host only; the main session is untouched.
pub fn authorize_find_my() -> Result<()> {
    if mock_url().is_some() {
        return Ok(());
    }
    authorize_find_my_on(&session_bus()?)
}

/// [`authorize_find_my`] on a given bus connection.
pub fn authorize_find_my_on(conn: &Connection) -> Result<()> {
    Ok(proxy(conn)?.authorize_find_my()?)
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
/// It also notices the daemon going away or being restarted, and then
/// yields the status re-read from the new instance (D-Bus activation
/// starts one), with `signing_in` false if none can be reached.
/// Run it on its own thread. In mock mode it never yields.
pub fn watch() -> Result<Watch> {
    if mock_url().is_some() {
        return Ok(Watch { inner: None });
    }
    watch_on(&session_bus()?)
}

/// How long [`watch_forever`] waits before reconnecting at first, and at most.
const WATCH_RETRY_MIN: Duration = Duration::from_secs(2);
const WATCH_RETRY_MAX: Duration = Duration::from_secs(60);

/// Calls `f` with the status on every (re)connect to the daemon, then after
/// every change, until `f` returns false. A [`watch`] ends when
/// icloud-sessiond goes away (it exits when idle, or is restarted); this
/// opens it again, which D-Bus-activates the daemon, after 2 s, doubling up
/// to 60 s while it cannot be reached. An outage is reported once on
/// stderr. Blocks: run it on its own thread. In mock mode it returns at once.
pub fn watch_forever(mut f: impl FnMut(Status) -> bool) {
    if mock_url().is_some() {
        return;
    }
    let (mut retry, mut reported) = (WATCH_RETRY_MIN, false);
    loop {
        let started = Instant::now();
        match watch() {
            Ok(mut watch) => {
                reported = false;
                let first = watch.current().cloned();
                for status in first.into_iter().chain(&mut watch) {
                    if !f(status) {
                        return;
                    }
                }
            }
            Err(e) if !reported => {
                reported = true;
                eprintln!("icloud-session: sign-in status unavailable: {e}");
            }
            Err(_) => {}
        }
        // A watch that lasted a while ended normally: reconnect promptly.
        if started.elapsed() > WATCH_RETRY_MAX {
            retry = WATCH_RETRY_MIN;
        }
        std::thread::sleep(retry);
        retry = (retry * 2).min(WATCH_RETRY_MAX);
    }
}

/// [`watch`] on a given bus connection.
pub fn watch_on(conn: &Connection) -> Result<Watch> {
    // Subscribe before reading, so no change falls between the two.
    let events = futures_lite::future::block_on(async {
        let props = zbus::fdo::PropertiesProxy::builder(conn.inner())
            .destination(BUS_NAME)?
            .path(OBJECT_PATH)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let changes = props.receive_properties_changed().await?;
        let dbus = zbus::fdo::DBusProxy::new(conn.inner()).await?;
        let owners = dbus.receive_name_owner_changed_with_args(&[(0, BUS_NAME)]).await?;
        let owners = owners.map(|signal| match signal.args() {
            Ok(args) if args.new_owner().is_some() => Event::OwnerAppeared,
            _ => Event::OwnerGone,
        });
        Ok::<_, zbus::Error>(changes.map(Event::Changed).or(owners))
    })?;
    let last = read_status(conn)?;
    Ok(Watch {
        inner: Some(WatchInner {
            conn: conn.clone(),
            events: Box::pin(events),
            last,
        }),
    })
}

/// See [`watch`].
pub struct Watch {
    inner: Option<WatchInner>,
}

enum Event {
    Changed(zbus::fdo::PropertiesChanged),
    /// The daemon left the bus.
    OwnerGone,
    /// A daemon took the name. It announces every property right after,
    /// so nothing is read here: a read now would see the state as of now,
    /// and the older changes still queued behind this event would then be
    /// replayed on top of it (a closed window reported open again).
    OwnerAppeared,
}

struct WatchInner {
    conn: Connection,
    events: Pin<Box<dyn Stream<Item = Event> + Send>>,
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
            let next = match futures_lite::future::block_on(w.events.next())? {
                Event::OwnerAppeared => continue,
                Event::OwnerGone => read_status(&w.conn).unwrap_or_else(|_| Status {
                    signing_in: false,
                    ..w.last.clone()
                }),
                Event::Changed(signal) => {
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
                    next
                }
            };
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
        "FindMyAuthorized" => status.find_my_authorized = parsed.find_my_authorized,
        "FindMyPasswordStored" => status.find_my_password_stored = parsed.find_my_password_stored,
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
    Unauthorized {
        status: u16,
        body: String,
    },
    /// HTTP 450 from a session host, with the cookie header it was sent.
    FindMyAuth {
        cookie: String,
    },
}

/// The request a public method asked for, before cookies and params.
struct Request<'a> {
    method: &'a str,
    url: &'a str,
    content_type: Option<&'a str>,
    body: Body<'a>,
    client_params: bool,
    accept: &'a str,
}

enum Body<'a> {
    None,
    Bytes(&'a [u8]),
    /// Streamed from the file, opened afresh for every attempt.
    File(&'a Path),
}

impl Session {
    /// Connects to `icloud-sessiond` on the session bus (D-Bus activates it)
    /// and fetches the session. `SignInRequired` when signed out.
    /// In mock mode ([`mock_url`]), a fake session and no D-Bus.
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
            // A connect that takes longer is a network that is down: fail
            // with `Offline` rather than sit for long.
            .timeout_connect(Duration::from_secs(10))
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
    /// `SignInRequired` once the daemon holds a different account.
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
        // The cookies must belong to the account this Session was made for:
        // after a sign-out and a sign-in as someone else, they would go out
        // with the old dsid. Read after the reply, so a switch in between
        // is caught too. The app connects again for the new account.
        let now = read_status(conn)?;
        if !now.signed_in || now.dsid.as_deref() != Some(self.inner.dsid.as_str()) {
            *cached = None;
            return Err(Error::SignInRequired);
        }
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
    /// (it revalidates when that is older than 10 minutes, in the
    /// background unless it is older than 6 hours).
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
    /// fresh jar. Requests to the `findme` host carry the Find My jar from
    /// `FindMySession()` instead (`FindMyAuthRequired` when there is none),
    /// its `Set-Cookie`s go to `MergeFindMyCookies`, and its 450 (or
    /// 421/401) → `ReportFindMyAuthRequired`: one retry if the daemon signed
    /// in to Find My again with the stored password, else `FindMyAuthRequired`.
    /// Other non-2xx → `Http`.
    pub fn get(&self, url: &str) -> Result<Response> {
        self.request(Request {
            method: "GET",
            url,
            content_type: None,
            body: Body::None,
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
            body: Body::Bytes(&bytes),
            client_params: true,
            accept: "application/json",
        })
    }

    /// POST a file's contents with a content type, streamed from disk
    /// rather than read into memory (large uploads). Same behaviour as
    /// [`Session::get`], including the one retry after a 421/401,
    /// which reads the file again from the start.
    pub fn post_file(&self, url: &str, content_type: &str, path: &Path) -> Result<Response> {
        self.request(Request {
            method: "POST",
            url,
            content_type: Some(content_type),
            body: Body::File(path),
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

    /// Streams a URL to `dest` (written to a temp file, renamed on success),
    /// creating `dest`'s parent directories as needed. Returns bytes
    /// written. Cookies attached; no client params appended.
    pub fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let response = self.send(&Request {
            method: "GET",
            url,
            content_type: None,
            body: Body::None,
            client_params: false,
            accept: "*/*",
        })?;
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
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
    /// `Set-Cookie`s to the daemon; maps every other status. On 421/401 from
    /// an icloud.com or service host the daemon confirms with Apple: still
    /// signed in → one retry with the fresh jar; signed out →
    /// `SignInRequired`.
    fn send(&self, request: &Request<'_>) -> Result<ureq::Response> {
        match self.send_once(request)? {
            Sent::Ok(response) => Ok(*response),
            Sent::FindMyAuth { cookie } => self.find_my_auth_required(request, &cookie),
            Sent::Unauthorized { .. } if !self.report_sign_in_required()? => Err(Error::SignInRequired),
            Sent::Unauthorized { .. } => match self.send_once(request)? {
                Sent::Ok(response) => Ok(*response),
                Sent::FindMyAuth { .. } => {
                    self.report_find_my_auth_required();
                    Err(Error::FindMyAuthRequired)
                }
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

    /// After a 450 sent with the Find My jar `cookie`: retry once if the
    /// daemon's Find My jar changed since (authorized again meanwhile), or
    /// if reporting the 450 made the daemon sign in to Find My again with
    /// the stored password. Otherwise, or if the retry also answers 450
    /// (reported too), `FindMyAuthRequired`. Never more than one retry, so
    /// a 450 cannot loop.
    fn find_my_auth_required(&self, request: &Request<'_>, cookie: &str) -> Result<ureq::Response> {
        let Some(conn) = &self.inner.conn else {
            return Err(Error::FindMyAuthRequired);
        };
        let changed = match proxy(conn)?.find_my_session() {
            Ok((now, _)) => now != cookie,
            // None held (another client's report is re-authorizing): report.
            Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == ERROR_FIND_MY_AUTH_REQUIRED => false,
            Err(e) => return Err(e.into()),
        };
        if !changed && !self.report_find_my_auth_required() {
            return Err(Error::FindMyAuthRequired);
        }
        match self.send_once(request)? {
            Sent::Ok(response) => Ok(*response),
            Sent::Unauthorized { status, body } => Err(Error::Http { status, body }),
            Sent::FindMyAuth { .. } => {
                self.report_find_my_auth_required();
                Err(Error::FindMyAuthRequired)
            }
        }
    }

    /// `ReportFindMyAuthRequired()`: true when the daemon holds a new Find
    /// My jar (it signed in again with the stored password). False in mock
    /// mode or when the report fails.
    fn report_find_my_auth_required(&self) -> bool {
        let Some(conn) = &self.inner.conn else {
            return false;
        };
        proxy(conn)
            .and_then(|p| Ok(p.report_find_my_auth_required()?))
            .unwrap_or(false)
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
        // The jar is `.icloud.com` cookies: like a browser, send them (and
        // take Set-Cookie back) only to icloud.com and the service hosts
        // Apple listed, never to the signed content hosts (icloud-content.com
        // downloads and upload URLs). Judged on the URL as given, so mock
        // mode behaves as the real hosts would.
        let icloud = is_session_host(request.url, &snap.webservices);
        // Find My has its own jar (a one-factor sign-in the main session
        // cannot stand in for): the `findme` host gets it instead.
        let find_my = match &self.inner.conn {
            Some(conn) if is_find_my_host(request.url, &snap.webservices) => Some(proxy(conn)?.find_my_session()?),
            _ => None,
        };
        let (cookie, params) = match &find_my {
            Some((cookie, params)) => (cookie, params),
            None => (&snap.cookie, &snap.params),
        };
        let url = self.prepare(request, params, icloud)?;
        let mut req = self
            .inner
            .agent
            .request(request.method, &url)
            .set("Origin", ORIGIN)
            .set("Referer", REFERER)
            .set("Accept", request.accept);
        if icloud && !cookie.is_empty() {
            req = req.set("Cookie", cookie);
        }
        if let Some(content_type) = request.content_type {
            req = req.set("Content-Type", content_type);
        }
        let result = match request.body {
            Body::None => req.call(),
            Body::Bytes(body) => req.send_bytes(body),
            Body::File(path) => {
                let file = fs::File::open(path)?;
                let len = file.metadata()?.len();
                req.set("Content-Length", &len.to_string()).send(file)
            }
        };
        match result {
            Ok(response) => {
                let set_cookies: Vec<&str> = response.all("set-cookie");
                if icloud
                    && !set_cookies.is_empty()
                    && let Some(conn) = &self.inner.conn
                {
                    if find_my.is_some() {
                        proxy(conn)?.merge_find_my_cookies(&set_cookies)?;
                    } else {
                        proxy(conn)?.merge_cookies(&set_cookies)?;
                        self.forget_snapshot();
                    }
                }
                Ok(Sent::Ok(Box::new(response)))
            }
            // Find My wants the password again (pyicloud's
            // FIND_MY_REAUTH_REQUIRED, empty body), or ended its session.
            Err(ureq::Error::Status(450 | 401 | 421, _)) if find_my.is_some() => {
                Ok(Sent::FindMyAuth { cookie: cookie.clone() })
            }
            // Only the session's own hosts judge the session; a content
            // host's 401 (an expired signed URL) is a plain HTTP error.
            Err(ureq::Error::Status(status @ (401 | 421), response)) if icloud => Ok(Sent::Unauthorized {
                status,
                body: read_body_lossy(response),
            }),
            // Mock mode, where Find My shares the (empty) main jar.
            Err(ureq::Error::Status(450, _)) if icloud => Ok(Sent::FindMyAuth { cookie: cookie.clone() }),
            Err(ureq::Error::Status(status, response)) => Err(Error::Http {
                status,
                body: read_body_lossy(response),
            }),
            Err(ureq::Error::Transport(t)) if is_offline(&t) => Err(Error::Offline(t.to_string())),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// The final URL: client params added on icloud.com hosts, and in mock
    /// mode rewritten to the mock base keeping path and query.
    fn prepare(&self, request: &Request<'_>, params: &HashMap<String, String>, icloud: bool) -> Result<String> {
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
        if request.client_params && icloud {
            let present: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
            let param = |k: &str| params.get(k).map(String::as_str).unwrap_or_default();
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

/// The name did not resolve or nothing answered the connect: no network,
/// rather than a slow or broken server.
fn is_offline(t: &ureq::Transport) -> bool {
    matches!(t.kind(), ureq::ErrorKind::Dns | ureq::ErrorKind::ConnectionFailed)
}

/// Whether `url`'s host domain-matches `.icloud.com` or is one of the
/// service hosts in `webservices`.
fn is_session_host(url: &str, webservices: &HashMap<String, String>) -> bool {
    let Some((host, port)) = origin(url) else {
        return false;
    };
    host == "icloud.com"
        || host.ends_with(".icloud.com")
        || webservices
            .values()
            .any(|service| origin(service) == Some((host.clone(), port)))
}

/// Whether `url` is on the `findme` web service's host.
fn is_find_my_host(url: &str, webservices: &HashMap<String, String>) -> bool {
    webservices
        .get("findme")
        .and_then(|f| origin(f))
        .is_some_and(|f| origin(url) == Some(f))
}

/// Lower-cased host and port of a URL.
fn origin(url: &str) -> Option<(String, Option<u16>)> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| Some((u.host_str()?.to_ascii_lowercase(), u.port_or_known_default())))
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
