//! CloudKit database client and transports.
//!
//! Ports icloud-md `src/cloudkit/databaseClient.ts` (request shapes, paging,
//! response parsing). Authentication and `/validate` are icloud-session's job:
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
/// `cmd::errors`: `SignInRequired` is exit 2; `ZoneFetchFailed` and
/// `RequestFailed` are icloud-md's known `CloudKit*Error`s (exit 1);
/// `UnexpectedResponse` is a plain `Error` in icloud-md (exit 70).
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
    /// `Attachment download failed: HTTP 403`.
    #[error("{0}")]
    RequestFailed(String),
    /// A non-2xx answer as the transport saw it, before the client wraps it
    /// into `RequestFailed` with the operation name.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// A response whose shape icloud-md rejects with a plain `Error`
    /// ("Unexpected response shape from ..."), message verbatim.
    #[error("{0}")]
    UnexpectedResponse(String),
    /// `NotesUnavailableError`: the session's webservices have no
    /// `ckdatabasews`. `cmd::errors` should map it to its own
    /// `Error::NotesUnavailable` (exit 1, with the hint).
    #[error("Authenticated, but the account reported no ckdatabasews host - can't reach Notes.")]
    NotesUnavailable,
    #[error("network: {0}")]
    Network(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl From<icloud_session::Error> for CkError {
    fn from(e: icloud_session::Error) -> CkError {
        match e {
            icloud_session::Error::SignInRequired => CkError::SignInRequired,
            icloud_session::Error::Http { status, body } => CkError::Http { status, body },
            icloud_session::Error::Network(m) => CkError::Network(m),
            icloud_session::Error::Io(e) => CkError::Io(e),
            other => CkError::Other(other.to_string()),
        }
    }
}
