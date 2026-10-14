//! CloudKit database client and transports.
//!
//! Request shapes, paging and response parsing (originally derived from
//! icloud-md). Authentication and `/validate` are icloud-session's job:
//! [`transport::LiveTransport`] sends through `icloud_session::Session`, which
//! adds cookies and the `clientBuildNumber`/`clientMasteringNumber`/`clientId`/
//! `dsid` query parameters itself. [`transport::ReplayTransport`] serves the
//! same requests from a differential-harness cassette
//! (`tests/differential/README.md`).

pub mod client;
pub mod transport;
pub mod types;

pub use client::Database;
pub use transport::{LiveTransport, ReplayTransport, Transport};
pub use types::*;

/// Errors from the CloudKit layer. Exit-code mapping lives in
/// `cmd::errors`: `SignInRequired` and `SessionRejected` are exit 3;
/// `ZoneFetchFailed` and `RequestFailed` are icloud-md's known
/// `CloudKit*Error`s, exit 5 (iCloud failed the request; a `ZONE_NOT_FOUND`
/// zone is exit 4, an HTTP 4xx other than 401/421 exit 1); `Network`,
/// `Offline`, `SessionService`, `KeyringUnavailable` and
/// `UnexpectedResponse` (`bad_response`) exit 5.
///
/// Not in the plan's sketch: `Conflict`. A changeTag conflict is not an error
/// in icloud-md - it arrives per record inside an HTTP 200 and is returned as
/// [`RecordUpdateResult::Rejected`].
#[derive(Debug, thiserror::Error)]
pub enum CkError {
    /// No session, or Apple answered 421/401 and icloud-sessiond confirmed it.
    /// icloud-md's "session expired" paths all land here.
    #[error("sign in to iCloud required")]
    SignInRequired,
    /// `CloudKitZoneFetchFailedError`: a zone-level failure inside an HTTP 200
    /// `changes/zone` response. `BAD_REQUEST` on an incremental fetch triggers
    /// the from-scratch refetch; `ZONE_NOT_FOUND` on a shared zone skips it.
    #[error("changes/zone failed for a zone: {server_error_code} ({reason})")]
    ZoneFetchFailed { server_error_code: String, reason: String },
    /// `CloudKitRequestFailedError`, with icloud-md's message verbatim, e.g.
    /// `changes/zone request failed (private db): HTTP 500` or
    /// `Attachment download failed: HTTP 403`; `status` is the HTTP status
    /// when there was one (a 4xx is exit 1, anything else exit 5).
    #[error("{message}")]
    RequestFailed { message: String, status: Option<u16> },
    /// ckdatabasews kept answering 401/421 although icloud-sessiond still
    /// holds the sign-in (icloud-session's `Http` 401/421, which it only
    /// returns after the daemon confirmed the session and one retry):
    /// `session_rejected`, exit 3, fixed by signing in again.
    #[error("Apple refused the iCloud session although icloud-sessiond still holds it (HTTP {status})")]
    SessionRejected { status: u16 },
    /// A non-2xx answer as the transport saw it, before the client wraps it
    /// into `RequestFailed` with the operation name.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// A response whose shape icloud-md rejects ("Unexpected response shape
    /// from ..."), message verbatim, or one that is not JSON at all:
    /// `bad_response`, exit 5.
    #[error("{0}")]
    UnexpectedResponse(String),
    /// `NotesUnavailableError`: the session's webservices have no
    /// `ckdatabasews`. `cmd::errors` should map it to its own
    /// `Error::NotesUnavailable` (exit 3, with the hint).
    #[error("Authenticated, but the account reported no ckdatabasews host - can't reach Notes.")]
    NotesUnavailable,
    #[error("network: {0}")]
    Network(String),
    /// No network at all (a name that does not resolve, a refused or
    /// timed-out connect): icloud-session's `Offline`, which it reports
    /// at once rather than after a long timeout.
    #[error("offline: {0}")]
    Offline(String),
    /// icloud-sessiond could not be reached (no session bus, the daemon not
    /// installed): icloud-session's `Service`. A failure the daemon itself
    /// reported (icloud-session's `Daemon`) is `Other`.
    #[error("icloud-sessiond: {0}")]
    SessionService(String),
    /// The keyring holding the session's cookies would not give them
    /// (no Secret Service, locked, prompt dismissed): not a sign-in.
    #[error("the keyring holding the iCloud session cannot be read: {0}")]
    KeyringUnavailable(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl CkError {
    /// [`CkError::RequestFailed`] without an HTTP status (exit 5).
    pub fn request_failed(message: impl Into<String>) -> CkError {
        CkError::RequestFailed {
            message: message.into(),
            status: None,
        }
    }
}

impl From<icloud_session::Error> for CkError {
    fn from(e: icloud_session::Error) -> CkError {
        match e {
            icloud_session::Error::SignInRequired => CkError::SignInRequired,
            icloud_session::Error::Http { status, body } => CkError::Http { status, body },
            icloud_session::Error::Network(m) => CkError::Network(m),
            icloud_session::Error::BadResponse(m) => CkError::UnexpectedResponse(m),
            icloud_session::Error::Offline(m) => CkError::Offline(m),
            icloud_session::Error::Io(e) => CkError::Io(e),
            icloud_session::Error::Service(m) => CkError::SessionService(m),
            icloud_session::Error::KeyringUnavailable(m) => CkError::KeyringUnavailable(m),
            other => CkError::Other(other.to_string()),
        }
    }
}
