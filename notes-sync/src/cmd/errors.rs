//! Known failures and their exit codes. Originally derived from icloud-md's
//! error classes.
//!
//! Exit codes, shared with the other iCloud tools (icloud-session,
//! icloud-notes, icloud-photos, icloud-findmy; docs/CLI.md):
//!
//! - 0 ok. `status`, `push --dry-run` and `diff` report their changes in the
//!   result and exit 0.
//! - 1 error: anything not below (`internal`, `io`, `error` (a failure
//!   icloud-sessiond reported), `vault_busy`,
//!   `vault_lock`, `vault_needs_update`, `vault_from_newer_tool`,
//!   `unsupported_vault_layout`, `corrupt_state_file`, a
//!   `cloudkit_request_failed` that is an HTTP 4xx, …).
//! - 2 usage: `usage`, `ambiguous_tracked_file`, `already_cloned_directory`.
//! - 3 not signed in / not set up for this account: `sign_in_required`,
//!   `not_cloned_directory`, `unbound_account`, `account_mismatch`,
//!   `requested_account_mismatch`, `notes_unavailable`, `session_rejected`
//!   (ckdatabasews keeps answering 401/421 although icloud-sessiond still
//!   holds the sign-in). The JSON `code` tells them apart; only
//!   `sign_in_required` and `session_rejected` are fixed by signing in.
//! - 4 not found: `untracked_file`, `unknown_version_snapshot`, a
//!   `cloudkit_zone_fetch_failed` whose zone is `ZONE_NOT_FOUND`.
//! - 5 a service unavailable: `network`, `offline`, `session_service`
//!   (icloud-sessiond unreachable), `keyring_unavailable`, `bad_response`
//!   (an answer that cannot be read), and iCloud failing a request
//!   (`cloudkit_request_failed` other than a 4xx,
//!   `cloudkit_zone_fetch_failed`).
//!
//! Hints for situations every iCloud tool shares (sign-in, network,
//! icloud-sessiond, keyring, iCloud trouble) are icloud-session's
//! (`icloud_session::output::*_HINT`). An icloud-session failure with no
//! notes-sync meaning of its own (`CkError::Session`, e.g. the daemon's
//! `Daemon`) is reported through `icloud_session::output::session_failure`,
//! exactly as the other tools report it.
//!
//! Messages keep icloud-md's wording (with `icloud-notes-sync` in hints).
//! `code()` is the machine-readable `error.code` of the contract error
//! object (`cmd::output` turns an `Error` into icloud-session's `CliError`).

use crate::cloudkit::CkError;
use crate::vault::state::{STATE_DIR_NAME, STATE_FILE_NAME};
use icloud_session::output::{
    CliError, KEYRING_HINT, NETWORK_HINT, SERVICE_TROUBLE_HINT, SESSION_SERVICE_HINT, SIGN_IN_HINT, session_failure,
};

pub use icloud_session::cli::{
    EXIT_ERROR, EXIT_INTERNAL, EXIT_NOT_FOUND, EXIT_OK, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("\"{file}\" isn't a tracked note in {target_dir}.")]
    UntrackedFile { file: String, target_dir: String },
    #[error("{target_dir} doesn't look like a cloned notes directory (no {STATE_DIR_NAME}/{STATE_FILE_NAME}).")]
    NotClonedDirectory { target_dir: String },
    #[error("\"{base_name}\" matches more than one tracked note: {}.", candidates.join(", "))]
    AmbiguousTrackedFile { base_name: String, candidates: Vec<String> },
    #[error(
        "{target_dir} was cloned before folder support and uses the old flat layout, which this version no longer reads."
    )]
    UnsupportedVaultLayout { target_dir: String },
    #[error(
        "{target_dir} was written by a newer version of icloud-notes (vault layout {vault_version}; this build understands {supported_version})."
    )]
    VaultFromNewerTool {
        target_dir: String,
        vault_version: u64,
        supported_version: u32,
    },
    /// A layout older than 3, which only a command that takes the lock
    /// updates.
    #[error("{target_dir} uses an older vault layout ({vault_version}) that has to be updated first.")]
    VaultNeedsUpdate { target_dir: String, vault_version: u64 },
    #[error("Authenticated, but the account reported no ckdatabasews host - can't reach Notes.")]
    NotesUnavailable,
    #[error("{0}")]
    CorruptStateFile(String),
    #[error("{target_dir} is already a cloned notes directory ({STATE_DIR_NAME}/{STATE_FILE_NAME} exists).")]
    AlreadyClonedDirectory { target_dir: String },
    #[error("{target_dir} has no account bound to it (missing \"account\" in {STATE_DIR_NAME}/{STATE_FILE_NAME}).")]
    UnboundAccount { target_dir: String },
    /// `--account` (clone) or the vault's bound account doesn't match the
    /// icloud-session account.
    #[error("{target_dir} was cloned for {expected}, but the session just authenticated is for {actual}.")]
    AccountMismatch {
        target_dir: String,
        expected: String,
        actual: String,
    },
    #[error("--account asked for {requested}, but the signed-in account is {actual}.")]
    RequestedAccountMismatch { requested: String, actual: String },
    #[error("No version snapshot with id \"{id}\" found for \"{file}\".")]
    UnknownVersionSnapshot { id: String, file: String },
    #[error("Can't complete this operation: {0}.")]
    VersionContentUnavailable(String),
    /// Sign-in required: exit 3. The command line adds why, when
    /// icloud-sessiond knows (`cmd::output`).
    #[error("sign in to iCloud required")]
    SignInRequired,
    /// The vault's lock is held (by the Notes window when `app`).
    #[error("{}", if *app { format!("{holder} is open and owns the notes vault while it is open.") } else { format!("{holder} holds the notes vault.") })]
    VaultBusy { holder: String, app: bool },
    /// The vault's lock file can't be opened or locked.
    #[error("could not open the vault's lock {path}: {reason}")]
    VaultLock { path: String, reason: String },
    #[error(transparent)]
    CloudKit(CkError),
    /// Anything else (a plain `Error` in icloud-md): exit 1.
    #[error("{0}")]
    Internal(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<CkError> for Error {
    fn from(e: CkError) -> Error {
        match e {
            CkError::SignInRequired | CkError::Session(icloud_session::Error::SignInRequired) => Error::SignInRequired,
            CkError::NotesUnavailable => Error::NotesUnavailable,
            other => Error::CloudKit(other),
        }
    }
}

impl Error {
    /// The shared contract error of an icloud-session failure carried as
    /// it is (`CkError::Session`). Never a sign-in (that is
    /// [`Error::SignInRequired`]), so no D-Bus round trip.
    pub fn session_failure(&self) -> Option<CliError> {
        match self {
            Error::CloudKit(CkError::Session(e)) => Some(session_failure(e)),
            _ => None,
        }
    }

    pub fn exit_code(&self) -> u8 {
        if let Some(failure) = self.session_failure() {
            return failure.exit;
        }
        match self {
            Error::SignInRequired
            | Error::NotClonedDirectory { .. }
            | Error::UnboundAccount { .. }
            | Error::AccountMismatch { .. }
            | Error::RequestedAccountMismatch { .. }
            | Error::NotesUnavailable => EXIT_SIGN_IN,
            Error::AmbiguousTrackedFile { .. } | Error::AlreadyClonedDirectory { .. } => EXIT_USAGE,
            Error::UntrackedFile { .. } | Error::UnknownVersionSnapshot { .. } => EXIT_NOT_FOUND,
            Error::CloudKit(CkError::ZoneFetchFailed { server_error_code, .. })
                if server_error_code == "ZONE_NOT_FOUND" =>
            {
                EXIT_NOT_FOUND
            }
            Error::CloudKit(CkError::SessionRejected { .. }) => EXIT_SIGN_IN,
            // iCloud refused this request (an expired asset URL, say): not
            // a service outage.
            Error::CloudKit(CkError::RequestFailed {
                status: Some(400..=499),
                ..
            }) => EXIT_ERROR,
            Error::CloudKit(
                CkError::Network(_)
                | CkError::Offline(_)
                | CkError::SessionService(_)
                | CkError::KeyringUnavailable(_)
                | CkError::UnexpectedResponse(_)
                | CkError::ZoneFetchFailed { .. }
                | CkError::RequestFailed { .. },
            ) => EXIT_UNAVAILABLE,
            Error::Io(_) | Error::CloudKit(CkError::Io(_)) => EXIT_ERROR,
            Error::Internal(_) | Error::CloudKit(_) => EXIT_INTERNAL,
            Error::UnsupportedVaultLayout { .. }
            | Error::VaultFromNewerTool { .. }
            | Error::VaultNeedsUpdate { .. }
            | Error::CorruptStateFile(_)
            | Error::VersionContentUnavailable(_)
            | Error::VaultBusy { .. }
            | Error::VaultLock { .. } => EXIT_ERROR,
        }
    }

    /// iCloud couldn't be reached (no network, or the connection failed):
    /// a retry once the network is back is all there is to do.
    pub fn is_network(&self) -> bool {
        matches!(self, Error::CloudKit(CkError::Network(_) | CkError::Offline(_)))
    }

    /// The contract error object's `code` (`untracked_file`,
    /// `sign_in_required`, `usage` and `internal` read as in the other
    /// iCloud tools). An icloud-session failure carried as it is takes
    /// icloud-session's code (`Daemon` → `error`).
    pub fn code(&self) -> String {
        if let Some(failure) = self.session_failure() {
            return failure.code;
        }
        match self {
            Error::UntrackedFile { .. } => "untracked_file",
            Error::NotClonedDirectory { .. } => "not_cloned_directory",
            Error::AmbiguousTrackedFile { .. } => "ambiguous_tracked_file",
            Error::UnsupportedVaultLayout { .. } => "unsupported_vault_layout",
            Error::VaultFromNewerTool { .. } => "vault_from_newer_tool",
            Error::VaultNeedsUpdate { .. } => "vault_needs_update",
            Error::NotesUnavailable => "notes_unavailable",
            Error::CorruptStateFile(_) => "corrupt_state_file",
            Error::AlreadyClonedDirectory { .. } => "already_cloned_directory",
            Error::UnboundAccount { .. } => "unbound_account",
            Error::AccountMismatch { .. } => "account_mismatch",
            Error::RequestedAccountMismatch { .. } => "requested_account_mismatch",
            Error::UnknownVersionSnapshot { .. } => "unknown_version_snapshot",
            Error::VersionContentUnavailable(_) => "version_content_unavailable",
            Error::SignInRequired => "sign_in_required",
            Error::VaultBusy { .. } => "vault_busy",
            Error::VaultLock { .. } => "vault_lock",
            Error::CloudKit(CkError::ZoneFetchFailed { .. }) => "cloudkit_zone_fetch_failed",
            Error::CloudKit(CkError::RequestFailed { .. }) => "cloudkit_request_failed",
            Error::CloudKit(CkError::SessionRejected { .. }) => "session_rejected",
            Error::CloudKit(CkError::UnexpectedResponse(_)) => "bad_response",
            Error::CloudKit(CkError::Network(_)) => "network",
            Error::CloudKit(CkError::Offline(_)) => "offline",
            Error::CloudKit(CkError::SessionService(_)) => "session_service",
            Error::CloudKit(CkError::KeyringUnavailable(_)) => "keyring_unavailable",
            Error::Io(_) | Error::CloudKit(CkError::Io(_)) => "io",
            Error::CloudKit(_) | Error::Internal(_) => "internal",
        }
        .to_owned()
    }

    /// The hint line icloud-md prints under the message, if any.
    pub fn hint(&self) -> Option<String> {
        if let Some(failure) = self.session_failure() {
            return failure.hint;
        }
        match self {
            Error::UntrackedFile { .. } => Some("Check the file name (it's case-sensitive) and try again.".into()),
            Error::NotClonedDirectory { .. } => Some("Run \"icloud-notes clone\" first.".into()),
            Error::AmbiguousTrackedFile { .. } => {
                Some("Qualify it with its folder (or run the command from inside that folder).".into())
            }
            Error::UnsupportedVaultLayout { .. } => Some(
                "Move this folder aside, then run \"icloud-notes clone\". (This tool made no changes.)"
                    .into(),
            ),
            Error::VaultFromNewerTool { .. } => {
                Some("Upgrade icloud-notes to the latest release. (This tool made no changes.)".into())
            }
            Error::VaultNeedsUpdate { .. } => Some(
                "Run \"icloud-notes pull\" once to update it, then try again. (This command made no changes.)".into(),
            ),
            Error::NotesUnavailable => {
                Some("Check that Notes is enabled for this Apple ID (icloud.com → Notes) and try again.".into())
            }
            Error::CorruptStateFile(_) => Some(
                "This usually means state.json was hand-edited or written by an incompatible version. If you don't have \
                 local edits worth preserving, move this folder aside and run \"icloud-notes clone\" again."
                    .into(),
            ),
            Error::AlreadyClonedDirectory { .. } => {
                Some("Run \"icloud-notes pull\" instead to fetch changes into an existing clone.".into())
            }
            Error::UnboundAccount { .. } => Some(
                "This folder may predate per-folder account binding. If you don't have local edits worth preserving, \
                 move this folder aside and run \"icloud-notes clone\" again."
                    .into(),
            ),
            Error::AccountMismatch { expected, .. } => {
                Some(format!("Sign in as {expected} to continue working with this folder."))
            }
            Error::RequestedAccountMismatch { .. } => None,
            Error::UnknownVersionSnapshot { file, .. } => {
                Some(format!("Run \"icloud-notes history {file}\" to see available snapshot ids."))
            }
            Error::VersionContentUnavailable(_) => {
                Some("Run \"icloud-notes pull\" to refresh local state, then try again.".into())
            }
            Error::SignInRequired => Some(SIGN_IN_HINT.into()),
            Error::VaultBusy { app: true, .. } => Some(
                "Notes syncs this vault itself while it is open. Quit it and retry, or pass --wait SECS to wait for it to close."
                    .into(),
            ),
            Error::VaultBusy { app: false, .. } => Some("Retry later, or pass --wait SECS.".into()),
            Error::VaultLock { .. } => None,
            Error::CloudKit(CkError::ZoneFetchFailed { server_error_code, .. }) if server_error_code == "ZONE_NOT_FOUND" => {
                Some(
                    "The server no longer has this zone - most likely a share that was revoked or deleted. Retrying won't \
                     change that."
                        .into(),
                )
            }
            Error::CloudKit(CkError::RequestFailed {
                status: Some(400..=499),
                ..
            }) => Some("iCloud refused the request; try again, and report it if it keeps failing.".into()),
            Error::CloudKit(
                CkError::ZoneFetchFailed { .. } | CkError::RequestFailed { .. } | CkError::UnexpectedResponse(_),
            ) => Some(SERVICE_TROUBLE_HINT.into()),
            Error::CloudKit(CkError::SessionRejected { .. }) => Some(SIGN_IN_HINT.into()),
            Error::CloudKit(CkError::Network(_) | CkError::Offline(_)) => Some(NETWORK_HINT.into()),
            Error::CloudKit(CkError::SessionService(_)) => Some(SESSION_SERVICE_HINT.into()),
            Error::CloudKit(CkError::KeyringUnavailable(_)) => Some(KEYRING_HINT.into()),
            Error::CloudKit(_) | Error::Internal(_) | Error::Io(_) => None,
        }
    }
}
