//! Ports icloud-md `src/errors.test.ts` for the error classes the port keeps
//! (`src/cmd/errors.rs`). Dropped with auth/session handling (icloud-session
//! owns sign-in): AuthenticationExpiredError, SilentReauthFailedError,
//! MissingSessionFileError, CorruptSessionFileError, ChromiumNotInstalledError,
//! SignInIncompleteError (all become `SignInRequired`, exit 4). The
//! IcloudNotesSyncError base-class/`cause` tests have no Rust counterpart
//! (an enum, not a class hierarchy).

use icloud_notes_sync::cloudkit::CkError;
use icloud_notes_sync::cmd::Error;

fn not_cloned() -> Error {
    Error::NotClonedDirectory {
        target_dir: "/tmp/not-a-vault".into(),
    }
}

#[test]
fn every_known_error_has_its_class_name_a_hint_and_exit_1() {
    let errors = vec![
        (not_cloned(), "NotClonedDirectoryError"),
        (Error::NotesUnavailable, "NotesUnavailableError"),
        (
            Error::CorruptStateFile("state.json has a malformed replicaId".into()),
            "CorruptStateFileError",
        ),
        (
            Error::CloudKit(CkError::RequestFailed(
                "records/lookup request failed (private db): HTTP 500".into(),
            )),
            "CloudKitRequestFailedError",
        ),
        (
            Error::UntrackedFile {
                file: "missing-note.md".into(),
                target_dir: "/some/dir".into(),
            },
            "UntrackedFileError",
        ),
        (
            Error::AlreadyClonedDirectory {
                target_dir: "/some/dir".into(),
            },
            "AlreadyClonedDirectoryError",
        ),
        (
            Error::UnboundAccount {
                target_dir: "/some/dir".into(),
            },
            "UnboundAccountError",
        ),
        (
            Error::AccountMismatch {
                target_dir: "/some/dir".into(),
                expected: "me@example.com".into(),
                actual: "someone-else@example.com".into(),
            },
            "AccountMismatchError",
        ),
    ];
    for (error, name) in errors {
        assert_eq!(error.name(), name);
        assert!(error.hint().is_some(), "{name} should have a hint");
        assert_eq!(error.exit_code(), 1, "{name}");
    }
}

#[test]
fn sign_in_required_exits_2_with_the_reauthenticate_marker() {
    assert_eq!(Error::SignInRequired.exit_code(), 2);
    assert_eq!(Error::SignInRequired.code(), "sign_in_required");
    assert!(
        Error::SignInRequired
            .hint()
            .unwrap()
            .contains("icloud-md reauthenticate")
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
    let error = Error::CloudKit(CkError::RequestFailed(
        "records/lookup request failed (private db): HTTP 500".into(),
    ));
    assert_eq!(
        error.to_string(),
        "records/lookup request failed (private db): HTTP 500"
    );
    assert!(error.hint().unwrap().contains("try again"));
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
