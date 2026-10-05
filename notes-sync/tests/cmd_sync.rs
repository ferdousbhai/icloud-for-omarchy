//! `sync` (push, then pull, in one run): when the pull runs, the exit code,
//! and one connection for both halves. The recorded scenarios `tiny-sync`
//! and `tiny-sync-clean` (cli_differential.rs) cover the binary's output.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use icloud_notes_sync::cloudkit::transport::ReplayTransport;
use icloud_notes_sync::cloudkit::{CkError, Database};
use icloud_notes_sync::cmd::remote::{FnConnector, Remote};
use icloud_notes_sync::cmd::sync::run_sync_with;
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::vault::state::Account;

fn differential() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The tiny clone, with its note edited locally (so the push has to reach
/// iCloud).
fn edited_vault() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&differential().join("expected/tiny-clone/vault"), tmp.path());
    let note = tmp.path().join("Notes/Test Note.md");
    let mut text = std::fs::read_to_string(&note).unwrap();
    text.push_str("\nA line added locally.");
    std::fs::write(&note, text).unwrap();
    tmp
}

/// Runs a sync whose every connect fails with `error()`; returns the
/// outcome's exit code, whether the pull ran, and how many connects there
/// were.
fn sync_failing_with(error: fn() -> Error) -> (u8, bool, usize) {
    let vault = edited_vault();
    let connects = Cell::new(0);
    let connector = FnConnector(|| -> Result<Remote, Error> {
        connects.set(connects.get() + 1);
        Err(error())
    });
    let outcome = run_sync_with(&connector, vault.path(), &mut NoProgress, &mut |_| {});
    assert!(outcome.push.is_err());
    (outcome.exit_code(), outcome.pull.is_some(), connects.get())
}

#[test]
fn a_sign_in_required_on_push_skips_the_pull() {
    assert_eq!(sync_failing_with(|| Error::SignInRequired), (2, false, 1));
}

#[test]
fn no_network_on_push_skips_the_pull() {
    assert_eq!(
        sync_failing_with(|| Error::CloudKit(CkError::Offline("no route".into()))),
        (1, false, 1)
    );
    assert_eq!(
        sync_failing_with(|| Error::CloudKit(CkError::Network("reset".into()))),
        (1, false, 1)
    );
    let offline = Error::from(CkError::Offline("no route".into()));
    assert_eq!((offline.code().as_str(), offline.exit_code()), ("offline", 1));
    assert!(offline.is_network() && offline.hint().is_some());
    let network = Error::from(CkError::Network("reset".into()));
    assert_eq!((network.code().as_str(), network.exit_code()), ("network", 1));
}

/// Any other failure pulls anyway, as two separate runs did; a failed
/// connect is tried again rather than remembered.
#[test]
fn another_push_failure_still_pulls() {
    assert_eq!(sync_failing_with(|| Error::NotesUnavailable), (1, true, 2));
}

#[test]
fn both_halves_share_one_connection() {
    let vault = edited_vault();
    let connects = Cell::new(0);
    let connector = FnConnector(|| -> Result<Remote, Error> {
        connects.set(connects.get() + 1);
        let transport = ReplayTransport::open(&differential().join("cassettes/tiny-sync.json"), None)?;
        let account = Account {
            apple_id: transport.account().apple_id.clone(),
            dsid: transport.account().dsid.clone(),
        };
        Ok(Remote {
            db: Database::new(Box::new(transport)),
            account,
        })
    });
    let outcome = run_sync_with(&connector, vault.path(), &mut NoProgress, &mut |_| {});
    assert_eq!(outcome.push.as_ref().unwrap().pushed, Some(1));
    assert!(outcome.pull.as_ref().unwrap().is_ok());
    assert_eq!((outcome.exit_code(), connects.get()), (0, 1));
}
