//! The HTTP seam between the Photos code and Apple.
//!
//! `cloudkit`, `sync`, `upload` and `thumbs` talk only to [`Transport`], so
//! tests drive them with recorded fixtures, while the app uses
//! `session::SessionTransport`, a thin wrapper over the `icloud-session`
//! crate, the D-Bus client of `icloud-sessiond` (which owns the cookie jar,
//! `/validate`, rotation merge and the 421 confirmation). In that crate's
//! mock mode the same transport talks to the dev fake server instead.
//! `src/session.rs` is the only file that uses its session client.

use std::path::Path;

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

    /// The same error again, for reporting one failure against several
    /// items (I/O and database errors keep their message only).
    pub fn duplicate(&self) -> Error {
        match self {
            Error::SignInRequired => Error::SignInRequired,
            Error::Http { status, body } => Error::Http {
                status: *status,
                body: body.clone(),
            },
            Error::CloudKit { code, reason } => Error::CloudKit {
                code: code.clone(),
                reason: reason.clone(),
            },
            Error::Other(m) => Error::Other(m.clone()),
            Error::Io(e) => Error::Io(std::io::Error::new(e.kind(), e.to_string())),
            Error::Db(e) => Error::Other(e.to_string()),
        }
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
    /// [`Transport::download`] for a cache file (a thumbnail, a viewer
    /// image): iCloud still has it, so it isn't flushed to disk.
    fn download_cache(&self, url: &str, dest: &Path) -> Result<u64> {
        self.download(url, dest)
    }
}

/// The sign-in banner's inputs: icloud-sessiond's `SignedIn`, `SigningIn`
/// and `SignOutReason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInState {
    pub signed_in: bool,
    /// The sign-in window is open.
    pub signing_in: bool,
    /// Why the account was signed out, when the daemon says.
    pub sign_out_reason: Option<String>,
}
