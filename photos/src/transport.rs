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
    /// An icloud-session failure as that crate has it (sign-in required,
    /// an HTTP status, offline, the daemon or the keyring out of reach, ...),
    /// so the command line reports it exactly as every iCloud tool does.
    #[error(transparent)]
    Session(#[from] icloud_session::Error),
    /// CloudKit answered 200 but flagged the operation (per-record or per-zone
    /// `serverErrorCode`).
    #[error("CloudKit {code}: {reason}")]
    CloudKit { code: String, reason: String },
    /// The account's `webservices` has no entry for this service (iCloud
    /// Photos is not enabled for the Apple ID); holds the service key.
    #[error("iCloud did not offer the {0} service for this account")]
    NotOffered(String),
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The iCloud web session is gone; the UI shows the sign-in banner.
    pub fn sign_in() -> Error {
        Error::Session(icloud_session::Error::SignInRequired)
    }

    /// A non-2xx answer.
    pub fn http(status: u16, body: impl Into<String>) -> Error {
        Error::Session(icloud_session::Error::Http {
            status,
            body: body.into(),
        })
    }

    pub fn is_sign_in(&self) -> bool {
        matches!(self, Error::Session(icloud_session::Error::SignInRequired))
    }

    /// The HTTP status, when the failure is one.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Error::Session(icloud_session::Error::Http { status, .. }) => Some(*status),
            _ => None,
        }
    }

    /// A 4xx on a signed content URL usually means it expired; the caller
    /// re-looks up the record for fresh URLs and tries once more.
    pub fn is_expired_url(&self) -> bool {
        self.http_status()
            .is_some_and(|status| (400..500).contains(&status) && status != 421)
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
