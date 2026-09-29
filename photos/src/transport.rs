//! The HTTP seam between the Photos code and Apple.
//!
//! `cloudkit`, `sync`, `upload` and `thumbs` talk only to [`Transport`], so
//! tests drive them with recorded fixtures and the dev fake server, while the
//! app uses `session::SessionTransport`, a thin wrapper over the
//! `icloud-session` crate, the D-Bus client of `icloud-sessiond` (which owns
//! the cookie jar, `/validate`, rotation merge and the 421 confirmation).
//! `src/session.rs` is the only file that names that crate.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The iCloud web session is gone; the UI shows the sign-in banner.
    #[error("sign in to iCloud required")]
    SignInRequired,
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// CloudKit answered 200 but flagged the operation (per-record or per-zone
    /// `serverErrorCode`).
    #[error("CloudKit {code}: {reason}")]
    CloudKit { code: String, reason: String },
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn is_sign_in(&self) -> bool {
        matches!(self, Error::SignInRequired)
    }

    /// A 4xx on a signed content URL usually means it expired; the caller
    /// re-looks up the record for fresh URLs and tries once more.
    pub fn is_expired_url(&self) -> bool {
        matches!(self, Error::Http { status, .. } if (400..500).contains(status) && *status != 421)
    }
}

/// Everything the Photos code needs from an HTTP client with an iCloud session.
pub trait Transport: Send + Sync {
    /// Base URL of a `webservices` entry, e.g. `ckdatabasews`, `photosupload`.
    fn service_url(&self, key: &str) -> Result<String>;
    /// POST a JSON body, parse a JSON response.
    fn post_json(&self, url: &str, body: &Value) -> Result<Value>;
    /// POST a file as the body, streamed from disk, parse a JSON response.
    fn post_file(&self, url: &str, content_type: &str, path: &Path) -> Result<Value>;
    /// Stream `url` to `dest` (temp file + rename). Returns bytes written.
    fn download(&self, url: &str, dest: &Path) -> Result<u64>;
    /// True for the mock transport: plain `http://` loopback upload targets
    /// are allowed only there.
    fn is_mock(&self) -> bool {
        false
    }
}

/// `ICLOUD_SESSION_MOCK=1`: no Apple account, no cookies. Every web service
/// resolves to `ICLOUD_SESSION_MOCK_URL` (default `http://127.0.0.1:8765`),
/// served by `cargo run --example fake_cloudkit`. HTTP 421/401 map to
/// `SignInRequired` exactly like the real session.
pub struct MockTransport {
    base: String,
    agent: ureq::Agent,
}

impl MockTransport {
    pub fn from_env() -> Self {
        let base = std::env::var("ICLOUD_SESSION_MOCK_URL").unwrap_or_else(|_| "http://127.0.0.1:8765".into());
        Self::new(&base)
    }

    pub fn new(base: &str) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build();
        Self {
            base: base.trim_end_matches('/').to_owned(),
            agent,
        }
    }

    fn map(result: std::result::Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
        match result {
            Ok(r) => Ok(r),
            Err(ureq::Error::Status(401 | 421, _)) => Err(Error::SignInRequired),
            Err(ureq::Error::Status(status, r)) => Err(Error::Http {
                status,
                body: r.into_string().unwrap_or_default(),
            }),
            Err(e) => Err(Error::Other(format!("network: {e}"))),
        }
    }

    fn json(r: ureq::Response) -> Result<Value> {
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut r.into_reader(), &mut body)?;
        serde_json::from_slice(&body).map_err(|e| Error::Other(format!("bad JSON: {e}")))
    }

    pub fn active() -> bool {
        std::env::var("ICLOUD_SESSION_MOCK").is_ok_and(|v| v == "1")
    }

    /// The mock sign-in: tells the fake server the "user" signed in again.
    pub fn reauthenticate(&self) -> Result<()> {
        Self::map(
            self.agent
                .post(&format!("{}/mock/reauthenticate", self.base))
                .send_string(""),
        )?;
        Ok(())
    }
}

impl Transport for MockTransport {
    fn service_url(&self, _key: &str) -> Result<String> {
        Ok(self.base.clone())
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        Self::json(Self::map(
            self.agent
                .post(url)
                .set("Content-Type", "application/json")
                .send_string(&body.to_string()),
        )?)
    }

    fn post_file(&self, url: &str, content_type: &str, path: &Path) -> Result<Value> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Self::json(Self::map(
            self.agent
                .post(url)
                .set("Content-Type", content_type)
                .set("Content-Length", &len.to_string())
                .send(file),
        )?)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let resp = Self::map(self.agent.get(url).call())?;
        write_atomically(dest, &mut resp.into_reader())
    }

    fn is_mock(&self) -> bool {
        true
    }
}

/// Write a stream to `dest` through a sibling temp file, renamed on success
/// (replacing `dest`). Like icloud-session's download, it creates no
/// directories: a missing parent is an error, so the mock hides nothing.
pub fn write_atomically(dest: &Path, reader: &mut dyn std::io::Read) -> Result<u64> {
    let tmp = dest.with_extension(format!("part-{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        let n = std::io::copy(reader, &mut file)?;
        file.sync_all()?;
        std::fs::rename(&tmp, dest)?;
        Ok(n)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The transport the app uses: mock when `ICLOUD_SESSION_MOCK=1`, else the session.
pub fn from_env() -> Result<std::sync::Arc<dyn Transport>> {
    if MockTransport::active() {
        return Ok(std::sync::Arc::new(MockTransport::from_env()));
    }
    Ok(std::sync::Arc::new(crate::session::SessionTransport::connect()?))
}

/// The sign-in banner's inputs: icloud-sessiond's `SignedIn` and `SigningIn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignInState {
    pub signed_in: bool,
    /// The sign-in window is open.
    pub signing_in: bool,
}

/// The sign-in state right now, for `icloud-photos status`: icloud-sessiond's
/// properties (one D-Bus round trip, no request to Apple). In mock mode the
/// fake server is asked, since it is what plays the signed-out account.
pub fn sign_in_state() -> Result<SignInState> {
    if MockTransport::active() {
        let t = MockTransport::from_env();
        let url = format!("{}{}/zones/list", t.base, crate::cloudkit::DB_PATH);
        return match t.post_json(&url, &Value::Object(Default::default())) {
            Ok(_) => Ok(SignInState {
                signed_in: true,
                signing_in: false,
            }),
            Err(Error::SignInRequired) => Ok(SignInState {
                signed_in: false,
                signing_in: false,
            }),
            Err(e) => Err(e),
        };
    }
    crate::session::sign_in_state()
}

/// Opens the interactive sign-in and returns at once; the outcome arrives
/// through [`watch_sign_in`]. In mock mode the fake server is told and
/// `notify` hears "signed in" straight away. Call it off the main loop.
pub fn start_sign_in(notify: &dyn Fn(SignInState)) -> Result<()> {
    if MockTransport::active() {
        MockTransport::from_env().reauthenticate()?;
        notify(SignInState {
            signed_in: true,
            signing_in: false,
        });
        return Ok(());
    }
    crate::session::start_sign_in()
}

/// Reports the sign-in state on a long-lived background thread: once on
/// connecting to icloud-sessiond, then after every change. Nothing in mock
/// mode, where [`start_sign_in`] reports for itself.
pub fn watch_sign_in(notify: Box<dyn Fn(SignInState) + Send>) {
    if MockTransport::active() {
        return;
    }
    std::thread::Builder::new()
        .name("sign-in watch".into())
        .spawn(move || crate::session::watch_sign_in(&*notify))
        .expect("spawn the sign-in watch thread");
}
