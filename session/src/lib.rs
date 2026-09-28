//! Shared iCloud web session, read from icloud-md's session file.
//!
//! CONTRACT STUB: the public signatures below are what icloud-findmy,
//! icloud-photos and the CLI build against. Bodies are filled in by the
//! icloud-session implementation; signatures may gain items but must not change.

use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Missing session file, or HTTP 421/401 after icloud-md's silent recovery.
    /// Apps show a sign-in banner that runs `icloud-session reauthenticate`.
    #[error("sign in to iCloud required")]
    SignInRequired,
    #[error("session file is corrupt: {0}")]
    Corrupt(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The `webservices` map from `/validate`, e.g. `ckdatabasews`, `findme`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// One signed-in Apple account. Cheap to clone; every request re-reads the
/// session file, so a clone never holds stale cookies.
#[derive(Debug, Clone)]
pub struct Session {
    _private: (),
}

impl Session {
    /// Newest account under `~/.config/icloud-md/accounts`. `SignInRequired` if none.
    /// With `ICLOUD_SESSION_MOCK=1`, a fake session (see `mock` docs in README).
    pub fn load() -> Result<Session> { unimplemented!() }
    /// A specific account directory's session.
    pub fn load_dsid(dsid: &str) -> Result<Session> { let _ = dsid; unimplemented!() }
    pub fn dsid(&self) -> &str { unimplemented!() }
    /// Apple ID from the last `/validate`, if known.
    pub fn apple_id(&self) -> Option<String> { unimplemented!() }
    /// Cached `/validate` result if younger than ~10 minutes, otherwise
    /// validates under the lock, merges rotated cookies, updates the cache.
    pub fn webservices(&self) -> Result<Webservices> { unimplemented!() }
    /// GET with the cookie jar, `Origin: https://www.icloud.com`, and the
    /// client query params (clientBuildNumber, clientMasteringNumber, clientId, dsid)
    /// appended. Set-Cookie rotation is merged back into the session file.
    /// 421/401 → `SignInRequired`; other non-2xx → `Http`.
    pub fn get(&self, url: &str) -> Result<Response> { let _ = url; unimplemented!() }
    /// POST a JSON body, same behaviour as `get`.
    pub fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<Response> { let _ = (url, body); unimplemented!() }
    /// POST raw bytes with a content type (uploads), same behaviour as `get`.
    pub fn post_bytes(&self, url: &str, content_type: &str, body: Vec<u8>) -> Result<Response> { let _ = (url, content_type, body); unimplemented!() }
    /// Streams a URL to `dest` (written to a temp file, renamed on success).
    /// Returns bytes written. Cookies attached; no client params appended.
    pub fn download(&self, url: &str, dest: &Path) -> Result<u64> { let _ = (url, dest); unimplemented!() }
    /// Runs `icloud-md reauthenticate` interactively and waits for it.
    pub fn reauthenticate() -> Result<()> { unimplemented!() }
    /// When the persistent X-APPLE-WEBAUTH-TOKEN cookie expires, read from
    /// the Chromium Cookies DB in this account's browser profile.
    pub fn expires_at(&self) -> Option<SystemTime> { unimplemented!() }
}

/// Offline status for `icloud-session status` (no network call).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
    pub signed_in: bool,
    pub apple_id: Option<String>,
    pub dsid: Option<String>,
    /// RFC 3339 UTC, e.g. "2026-10-28T09:00:00Z".
    pub expires_at: Option<String>,
    pub validated_at: Option<String>,
}

pub fn status() -> Status { unimplemented!() }
