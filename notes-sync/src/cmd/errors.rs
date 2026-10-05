//! Known failures and their exit codes. Originally derived from icloud-md's
//! error classes.
//!
//! Exit codes, shared with the other iCloud tools (icloud-session,
//! icloud-notes, icloud-photos, icloud-findmy; docs/CLI.md): 0 ok, 1 known
//! error (`IcloudNotesSyncError`), 2 sign-in required (icloud-md said
//! `Run "icloud-md reauthenticate"` with 1), 3 status has entries / diff has
//! differences (not an error), 64 usage (`EX_USAGE`; icloud-md used 2),
//! 70 internal (`EX_SOFTWARE`, anything unexpected).
//!
//! Messages keep icloud-md's wording (with `icloud-notes-sync` in hints).
//! `name()` is the error's class name; `code()` is the
//! machine-readable `error.code` of the `--json` error object.

use crate::cloudkit::CkError;
use crate::vault::state::{STATE_DIR_NAME, STATE_FILE_NAME};

/// `status`/`push --dry-run` has entries, `diff` found differences.
pub use icloud_session::cli::EXIT_CHANGES as EXIT_HAS_ENTRIES;
pub use icloud_session::cli::{EXIT_ERROR, EXIT_INTERNAL, EXIT_OK, EXIT_SIGN_IN, EXIT_USAGE};

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
    /// Sign-in required: exit 2.
    #[error("Not signed in to iCloud.")]
    SignInRequired,
    /// The vault's lock is held (by the Notes window when `app`).
    #[error("{}", if *app { format!("{holder} is open and owns the notes vault while it is open.") } else { format!("{holder} holds the notes vault.") })]
    VaultBusy { holder: String, app: bool },
    /// The vault's lock file can't be opened or locked.
    #[error("could not open the vault's lock {path}: {reason}")]
    VaultLock { path: String, reason: String },
    #[error(transparent)]
    CloudKit(CkError),
    #[error("{0}")]
    Usage(String),
    /// Anything else (a plain `Error` in icloud-md): exit 70.
    #[error("{0}")]
    Internal(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<CkError> for Error {
    fn from(e: CkError) -> Error {
        match e {
            CkError::SignInRequired => Error::SignInRequired,
            CkError::NotesUnavailable => Error::NotesUnavailable,
            other => Error::CloudKit(other),
        }
    }
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::SignInRequired => EXIT_SIGN_IN,
            Error::Usage(_) => EXIT_USAGE,
            Error::Internal(_) | Error::Io(_) => EXIT_INTERNAL,
            Error::CloudKit(
                CkError::ZoneFetchFailed { .. } | CkError::RequestFailed(_) | CkError::Network(_) | CkError::Offline(_),
            ) => EXIT_ERROR,
            Error::CloudKit(_) => EXIT_INTERNAL,
            _ => EXIT_ERROR,
        }
    }

    /// The `--json` `error` field.
    pub fn name(&self) -> &'static str {
        match self {
            Error::UntrackedFile { .. } => "UntrackedFileError",
            Error::NotClonedDirectory { .. } => "NotClonedDirectoryError",
            Error::AmbiguousTrackedFile { .. } => "AmbiguousTrackedFileError",
            Error::UnsupportedVaultLayout { .. } => "UnsupportedVaultLayoutError",
            Error::VaultFromNewerTool { .. } => "VaultFromNewerToolError",
            Error::VaultNeedsUpdate { .. } => "VaultNeedsUpdateError",
            Error::NotesUnavailable => "NotesUnavailableError",
            Error::CorruptStateFile(_) => "CorruptStateFileError",
            Error::AlreadyClonedDirectory { .. } => "AlreadyClonedDirectoryError",
            Error::UnboundAccount { .. } => "UnboundAccountError",
            Error::AccountMismatch { .. } => "AccountMismatchError",
            Error::RequestedAccountMismatch { .. } => "RequestedAccountMismatchError",
            Error::UnknownVersionSnapshot { .. } => "UnknownVersionSnapshotError",
            Error::VersionContentUnavailable(_) => "VersionContentUnavailableError",
            Error::SignInRequired => "SignInRequiredError",
            Error::VaultBusy { .. } => "VaultBusyError",
            Error::VaultLock { .. } => "VaultLockError",
            Error::CloudKit(CkError::ZoneFetchFailed { .. }) => "CloudKitZoneFetchFailedError",
            Error::CloudKit(CkError::RequestFailed(_)) => "CloudKitRequestFailedError",
            Error::CloudKit(CkError::Network(_)) => "NetworkError",
            Error::CloudKit(CkError::Offline(_)) => "OfflineError",
            Error::CloudKit(_) | Error::Internal(_) | Error::Io(_) => "InternalError",
            Error::Usage(_) => "UsageError",
        }
    }

    /// iCloud couldn't be reached (no network, or the connection failed):
    /// a retry once the network is back is all there is to do.
    pub fn is_network(&self) -> bool {
        matches!(self, Error::CloudKit(CkError::Network(_) | CkError::Offline(_)))
    }

    /// The `--json` error object, as `emit_error` reports it:
    /// `{"code","message","exit_code","hint"?}`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut error = serde_json::json!({
            "code": self.code(),
            "message": self.to_string(),
            "exit_code": self.exit_code(),
        });
        if let Some(hint) = self.hint() {
            error["hint"] = hint.into();
        }
        error
    }

    /// The `--json` error object's `code`: the class name in snake case
    /// without its `Error` suffix (`UntrackedFileError` → `untracked_file`),
    /// so `sign_in_required`, `usage` and `internal` read as in the other
    /// iCloud tools.
    pub fn code(&self) -> String {
        snake_code(self.name())
    }

    /// The hint line icloud-md prints under the message, if any.
    pub fn hint(&self) -> Option<String> {
        match self {
            Error::UntrackedFile { .. } => Some("Check the file name (it's case-sensitive) and try again.".into()),
            Error::NotClonedDirectory { .. } => Some("Run \"icloud-notes-sync clone <directory>\" first.".into()),
            Error::AmbiguousTrackedFile { .. } => {
                Some("Qualify it with its folder (or run the command from inside that folder).".into())
            }
            Error::UnsupportedVaultLayout { .. } => Some(
                "Re-clone into a fresh directory: \"icloud-notes-sync clone <new-directory>\". (This tool made no changes.)"
                    .into(),
            ),
            Error::VaultFromNewerTool { .. } => {
                Some("Upgrade icloud-notes-sync to the latest release. (This tool made no changes.)".into())
            }
            Error::VaultNeedsUpdate { .. } => Some(
                "Run \"icloud-notes-sync pull\" once to update it, then try again. (This command made no changes.)".into(),
            ),
            Error::NotesUnavailable => {
                Some("Check that Notes is enabled for this Apple ID (icloud.com → Notes) and try again.".into())
            }
            Error::CorruptStateFile(_) => Some(
                "This usually means state.json was hand-edited or written by an incompatible version. If you don't have \
                 local edits worth preserving, remove .icloud-notes/ and run \"icloud-notes-sync clone\" again into a fresh directory."
                    .into(),
            ),
            Error::AlreadyClonedDirectory { .. } => {
                Some("Run \"icloud-notes-sync pull\" instead to fetch changes into an existing clone.".into())
            }
            Error::UnboundAccount { .. } => Some(
                "This folder may predate per-folder account binding. If you don't have local edits worth preserving, \
                 remove .icloud-notes/ and run \"icloud-notes-sync clone\" again into a fresh directory."
                    .into(),
            ),
            Error::AccountMismatch { expected, .. } => {
                Some(format!("Sign in as {expected} to continue working with this folder."))
            }
            Error::RequestedAccountMismatch { .. } => None,
            Error::UnknownVersionSnapshot { file, .. } => {
                Some(format!("Run \"icloud-notes-sync history {file}\" to see available snapshot ids."))
            }
            Error::VersionContentUnavailable(_) => {
                Some("Run \"icloud-notes-sync pull\" to refresh local state, then try again.".into())
            }
            Error::SignInRequired => Some(
                "Sign in to iCloud again with icloud-session, then retry."
                    .into(),
            ),
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
            Error::CloudKit(CkError::ZoneFetchFailed { .. } | CkError::RequestFailed(_)) => Some(
                "This may be a transient network or iCloud-service issue - wait a moment and try again.".into(),
            ),
            Error::CloudKit(CkError::Network(_) | CkError::Offline(_)) => {
                Some("Couldn't reach iCloud - check the network connection, then try again.".into())
            }
            Error::CloudKit(_) | Error::Usage(_) | Error::Internal(_) | Error::Io(_) => None,
        }
    }
}

/// `UntrackedFileError` → `untracked_file`; `CloudKitZoneFetchFailedError` →
/// `cloudkit_zone_fetch_failed`.
fn snake_code(name: &str) -> String {
    let base = name
        .strip_suffix("Error")
        .unwrap_or(name)
        .replace("CloudKit", "Cloudkit");
    let mut out = String::new();
    for (i, c) in base.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
