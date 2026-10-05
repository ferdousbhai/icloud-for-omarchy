//! `status`. Originally derived from icloud-md's tests.

use std::path::Path;

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::plan::{PlanEntryKind, PlanResolution};
use icloud_notes_sync::cmd::remote::{FnConnector, Remote};
use icloud_notes_sync::cmd::status::{StatusResult, run_status_with};
use icloud_notes_sync::vault::state::{CloneState, FolderEntry, NoteEntry};

/// Writes the state, and every folder directory it names, as a pull leaves
/// them (a folder directory missing is one deleted here).
fn write_clone_state(dir: &Path, state: &CloneState) -> Result<(), Error> {
    icloud_notes_sync::vault::state::write_clone_state(dir, state)?;
    let index =
        icloud_notes_sync::vault::layout::state_dir_index(icloud_notes_sync::vault::layout::PreviousLayout::of(state));
    for folder_dir in index.keys() {
        std::fs::create_dir_all(dir.join(folder_dir)).unwrap();
    }
    Ok(())
}

fn status(dir: &Path) -> Result<StatusResult, Error> {
    let connector = FnConnector(|| -> Result<Remote, Error> { panic!("network") });
    run_status_with(&connector, dir, &mut |_| {})
}

fn notes_folder_state() -> CloneState {
    CloneState {
        sync_token: Some("token".into()),
        folders: Some(
            [("DefaultFolder-CloudKit".to_owned(), FolderEntry::new("Notes", "Notes"))]
                .into_iter()
                .collect(),
        ),
        ..Default::default()
    }
}

#[test]
fn refuses_when_theres_no_cloned_state() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(status(dir.path()), Err(Error::NotClonedDirectory { .. })));
}

#[test]
fn no_entries_for_a_clean_untracked_file_free_directory() {
    let dir = tempfile::tempdir().unwrap();
    let state = CloneState {
        sync_token: Some("token".into()),
        ..Default::default()
    };
    write_clone_state(dir.path(), &state).unwrap();
    let result = status(dir.path()).unwrap();
    assert!(result.entries.is_empty());
    assert_eq!(result.unchanged, 0);
}

#[test]
fn reports_an_untracked_files_local_refusal_without_a_bound_account() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &notes_folder_state()).unwrap();
    std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
    std::fs::write(dir.path().join("Notes/Empty Note.md"), "").unwrap();
    let result = status(dir.path()).unwrap();
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].kind, PlanEntryKind::Create);
    assert_eq!(result.entries[0].resolution, PlanResolution::Refused);
    assert!(result.entries[0].file.contains("Empty Note.md"));
    assert!(
        result.entries[0]
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("the file is empty - nothing to create")
    );
}

#[test]
fn creatable_untracked_file_needs_the_live_check() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &notes_folder_state()).unwrap();
    std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
    std::fs::write(dir.path().join("Notes/New Note.md"), "Hello").unwrap();
    assert!(matches!(status(dir.path()), Err(Error::UnboundAccount { .. })));
}

#[test]
fn missing_tracked_file_needs_the_live_check() {
    let dir = tempfile::tempdir().unwrap();
    let state = CloneState {
        sync_token: Some("token".into()),
        notes: [("REC1".to_owned(), NoteEntry::new("Tracked.md", "1a", 100))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    write_clone_state(dir.path(), &state).unwrap();
    assert!(matches!(status(dir.path()), Err(Error::UnboundAccount { .. })));
}
