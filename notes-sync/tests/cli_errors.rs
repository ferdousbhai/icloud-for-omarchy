//! Known errors (`src/cmd/errors.rs`): messages, hints, codes and exit
//! codes. Sign-in failures are all `SignInRequired` (icloud-session owns
//! sign-in). Originally derived from icloud-md's tests.

use icloud_notes_sync::cloudkit::CkError;
use icloud_notes_sync::cmd::Error;

fn not_cloned() -> Error {
    Error::NotClonedDirectory {
        target_dir: "/tmp/not-a-vault".into(),
    }
}

/// Every known error: its class name, code, whether it has a hint, and its
/// exit code (the shared table: 1 error, 2 usage, 3 not signed in / not set
/// up, 4 not found, 5 a service unavailable).
#[test]
fn every_known_error_has_its_class_name_code_hint_and_exit_code() {
    let dir = || "/some/dir".to_string();
    let zone_failed = |code: &str| {
        Error::CloudKit(CkError::ZoneFetchFailed {
            server_error_code: code.into(),
            reason: "r".into(),
        })
    };
    let errors: Vec<(Error, &str, &str, bool, u8)> = vec![
        (not_cloned(), "NotClonedDirectoryError", "not_cloned_directory", true, 3),
        (
            Error::NotesUnavailable,
            "NotesUnavailableError",
            "notes_unavailable",
            true,
            3,
        ),
        (
            Error::SignInRequired,
            "SignInRequiredError",
            "sign_in_required",
            true,
            3,
        ),
        (
            Error::UnboundAccount { target_dir: dir() },
            "UnboundAccountError",
            "unbound_account",
            true,
            3,
        ),
        (
            Error::AccountMismatch {
                target_dir: dir(),
                expected: "me@example.com".into(),
                actual: "someone-else@example.com".into(),
            },
            "AccountMismatchError",
            "account_mismatch",
            true,
            3,
        ),
        (
            Error::RequestedAccountMismatch {
                requested: "me@example.com".into(),
                actual: "someone-else@example.com".into(),
            },
            "RequestedAccountMismatchError",
            "requested_account_mismatch",
            false,
            3,
        ),
        (
            Error::UntrackedFile {
                file: "missing-note.md".into(),
                target_dir: dir(),
            },
            "UntrackedFileError",
            "untracked_file",
            true,
            4,
        ),
        (
            Error::UnknownVersionSnapshot {
                id: "abc".into(),
                file: "a.md".into(),
            },
            "UnknownVersionSnapshotError",
            "unknown_version_snapshot",
            true,
            4,
        ),
        (
            zone_failed("ZONE_NOT_FOUND"),
            "CloudKitZoneFetchFailedError",
            "cloudkit_zone_fetch_failed",
            true,
            4,
        ),
        (
            zone_failed("INTERNAL_ERROR"),
            "CloudKitZoneFetchFailedError",
            "cloudkit_zone_fetch_failed",
            true,
            5,
        ),
        (
            Error::AmbiguousTrackedFile {
                base_name: "a.md".into(),
                candidates: vec!["x/a.md".into(), "y/a.md".into()],
            },
            "AmbiguousTrackedFileError",
            "ambiguous_tracked_file",
            true,
            2,
        ),
        (
            Error::AlreadyClonedDirectory { target_dir: dir() },
            "AlreadyClonedDirectoryError",
            "already_cloned_directory",
            true,
            2,
        ),
        (Error::Usage("bad".into()), "UsageError", "usage", false, 2),
        (
            Error::CloudKit(CkError::RequestFailed {
                message: "records/lookup request failed (private db): HTTP 500".into(),
                status: Some(500),
            }),
            "CloudKitRequestFailedError",
            "cloudkit_request_failed",
            true,
            5,
        ),
        (
            Error::CloudKit(CkError::request_failed("changes/zone reported moreComing")),
            "CloudKitRequestFailedError",
            "cloudkit_request_failed",
            true,
            5,
        ),
        // iCloud refused the request: not a service outage.
        (
            Error::CloudKit(CkError::RequestFailed {
                message: "Attachment download failed: HTTP 410".into(),
                status: Some(410),
            }),
            "CloudKitRequestFailedError",
            "cloudkit_request_failed",
            true,
            1,
        ),
        // Persisting 401/421 while icloud-sessiond holds the sign-in.
        (
            Error::CloudKit(CkError::SessionRejected { status: 421 }),
            "SessionRejectedError",
            "session_rejected",
            true,
            3,
        ),
        (
            Error::CloudKit(CkError::Offline("no route".into())),
            "OfflineError",
            "offline",
            true,
            5,
        ),
        (
            Error::CloudKit(CkError::Network("reset".into())),
            "NetworkError",
            "network",
            true,
            5,
        ),
        (
            Error::from(CkError::from(icloud_session::Error::Service("no bus".into()))),
            "SessionServiceError",
            "session_service",
            true,
            5,
        ),
        (
            Error::from(CkError::from(icloud_session::Error::KeyringUnavailable(
                "locked".into(),
            ))),
            "KeyringUnavailableError",
            "keyring_unavailable",
            true,
            5,
        ),
        (
            Error::CorruptStateFile("state.json has a malformed replicaId".into()),
            "CorruptStateFileError",
            "corrupt_state_file",
            true,
            1,
        ),
        (
            Error::UnsupportedVaultLayout { target_dir: dir() },
            "UnsupportedVaultLayoutError",
            "unsupported_vault_layout",
            true,
            1,
        ),
        (
            Error::VaultFromNewerTool {
                target_dir: dir(),
                vault_version: 9,
                supported_version: 4,
            },
            "VaultFromNewerToolError",
            "vault_from_newer_tool",
            true,
            1,
        ),
        (
            Error::VaultNeedsUpdate {
                target_dir: dir(),
                vault_version: 2,
            },
            "VaultNeedsUpdateError",
            "vault_needs_update",
            true,
            1,
        ),
        (
            Error::VersionContentUnavailable("x".into()),
            "VersionContentUnavailableError",
            "version_content_unavailable",
            true,
            1,
        ),
        (
            Error::VaultBusy {
                holder: "Notes".into(),
                app: true,
            },
            "VaultBusyError",
            "vault_busy",
            true,
            1,
        ),
        (
            Error::VaultLock {
                path: "/l".into(),
                reason: "r".into(),
            },
            "VaultLockError",
            "vault_lock",
            false,
            1,
        ),
        (
            Error::CloudKit(CkError::UnexpectedResponse("odd".into())),
            "BadResponseError",
            "bad_response",
            true,
            5,
        ),
        (
            Error::from(CkError::from(icloud_session::Error::BadResponse("html".into()))),
            "BadResponseError",
            "bad_response",
            true,
            5,
        ),
        (Error::Internal("bug".into()), "InternalError", "internal", false, 1),
    ];
    for (error, name, code, has_hint, exit) in errors {
        assert_eq!(error.name(), name);
        assert_eq!(error.code(), code, "{name}");
        assert_eq!(error.hint().is_some(), has_hint, "{name} hint");
        assert_eq!(error.exit_code(), exit, "{name} ({code})");
        // Every exit-5 error tells what to do.
        assert!(exit != 5 || error.hint().is_some(), "{name}");
    }
}

#[test]
fn sign_in_required_exits_3_with_the_reauthenticate_marker() {
    assert_eq!(Error::SignInRequired.exit_code(), 3);
    assert_eq!(Error::SignInRequired.code(), "sign_in_required");
    // The hint every iCloud tool gives.
    assert_eq!(
        Error::SignInRequired.hint().as_deref(),
        Some(icloud_session::output::SIGN_IN_HINT)
    );
    assert!(matches!(Error::from(CkError::SignInRequired), Error::SignInRequired));
}

#[test]
fn not_cloned_names_the_directory_and_points_at_clone() {
    let error = not_cloned();
    assert!(error.to_string().contains("/tmp/not-a-vault"));
    assert!(error.hint().unwrap().contains("clone"));
}

#[test]
fn notes_unavailable_mentions_ckdatabasews() {
    assert!(Error::NotesUnavailable.to_string().contains("ckdatabasews"));
}

#[test]
fn corrupt_state_file_points_at_recloning() {
    let error = Error::CorruptStateFile("state.json has a malformed replicaId".into());
    assert_eq!(error.to_string(), "state.json has a malformed replicaId");
    assert!(error.hint().unwrap().contains("clone"));
}

#[test]
fn cloudkit_request_failed_keeps_detail_and_gives_retry_hint() {
    let error = Error::CloudKit(CkError::RequestFailed {
        message: "records/lookup request failed (private db): HTTP 500".into(),
        status: Some(500),
    });
    assert_eq!(
        error.to_string(),
        "records/lookup request failed (private db): HTTP 500"
    );
    assert_eq!(
        error.hint().as_deref(),
        Some(icloud_session::output::SERVICE_TROUBLE_HINT)
    );
}

#[test]
fn session_rejected_says_so_and_points_at_sign_in() {
    let error = Error::from(CkError::SessionRejected { status: 401 });
    assert!(error.to_string().contains("HTTP 401"), "{error}");
    assert_eq!(error.hint().as_deref(), Some(icloud_session::output::SIGN_IN_HINT));
    // Not a plain sign-in: the Notes app keeps `sign_in_required` for that.
    assert!(!matches!(error, Error::SignInRequired));
}

#[test]
fn untracked_file_names_file_and_directory() {
    let error = Error::UntrackedFile {
        file: "missing-note.md".into(),
        target_dir: "/tmp/vault".into(),
    };
    let message = error.to_string();
    assert!(message.contains("missing-note.md"));
    assert!(message.contains("/tmp/vault"));
}

#[test]
fn already_cloned_names_directory_and_points_at_pull() {
    let error = Error::AlreadyClonedDirectory {
        target_dir: "/tmp/vault".into(),
    };
    let message = error.to_string();
    assert!(message.contains("/tmp/vault"));
    assert!(message.contains("already a cloned notes directory"));
    assert!(error.hint().unwrap().contains("pull"));
}

#[test]
fn unbound_account_names_directory_and_points_at_recloning() {
    let error = Error::UnboundAccount {
        target_dir: "/tmp/vault".into(),
    };
    let message = error.to_string();
    assert!(message.contains("/tmp/vault"));
    assert!(message.contains("no account bound"));
    assert!(error.hint().unwrap().contains("clone"));
}

#[test]
fn account_mismatch_names_directory_and_both_accounts() {
    let error = Error::AccountMismatch {
        target_dir: "/tmp/vault".into(),
        expected: "me@example.com".into(),
        actual: "someone-else@example.com".into(),
    };
    let message = error.to_string();
    assert!(message.contains("/tmp/vault"));
    assert!(message.contains("me@example.com"));
    assert!(message.contains("someone-else@example.com"));
    assert!(error.hint().unwrap().contains("me@example.com"));
}
