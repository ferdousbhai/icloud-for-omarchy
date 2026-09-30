//! Ports icloud-md `src/commands/history.test.ts`.

mod common;

use icloud_notes_sync::cmd::history::{HistoryOptions, HistoryResult, run_history};
use icloud_notes_sync::vault::epoch::record_epoch;
use icloud_notes_sync::vault::history::{VersionSnapshotInput, record_version};
use icloud_notes_sync::vault::state::{CloneState, TableAttachmentEntry, write_clone_state};

// "Test Note.md" resolves by unique basename, whatever the process cwd is.
fn state() -> CloneState {
    CloneState {
        table_attachments: Some(
            [(
                "ATT-1".to_owned(),
                TableAttachmentEntry {
                    note_record_name: "REC1".into(),
                },
            )]
            .into_iter()
            .collect(),
        ),
        ..common::state()
    }
}

fn note(tag: &str, value: &str) -> VersionSnapshotInput {
    VersionSnapshotInput::note("REC1", tag, value)
}

fn table(tag: &str, value: &str) -> VersionSnapshotInput {
    VersionSnapshotInput::table("ATT-1", tag, value, "REC1")
}

#[test]
fn reports_no_history_when_nothing_recorded() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    let result = run_history(dir.path(), "Test Note.md", &HistoryOptions::default()).unwrap();
    assert_eq!(result, HistoryResult::Epochs { epochs: vec![] });
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"mode":"epochs","epochs":[]}"#
    );
}

#[test]
fn records_lists_note_and_table_snapshots_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    record_version(dir.path(), &note("tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &note("tag-2", "BBBB")).unwrap();
    record_version(dir.path(), &table("tag-1", "CCCC")).unwrap();

    let HistoryResult::Records { records } =
        run_history(dir.path(), "Test Note.md", &HistoryOptions { records: true }).unwrap()
    else {
        panic!("expected records mode");
    };
    assert_eq!(records.len(), 3);
    assert!(records.iter().any(|r| r.label == "table ATT-1"));
    let tag1 = records
        .iter()
        .position(|r| r.label == "note" && r.record_change_tag == "tag-1")
        .unwrap();
    let tag2 = records
        .iter()
        .position(|r| r.label == "note" && r.record_change_tag == "tag-2")
        .unwrap();
    assert!(
        tag2 < tag1,
        "the more recently captured note snapshot should list first"
    );
}

#[test]
fn defaults_to_epoch_timeline_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    let names = ["REC1".to_owned(), "ATT-1".to_owned()];
    record_version(dir.path(), &note("tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &table("tag-1", "CCCC")).unwrap();
    record_epoch(dir.path(), "REC1", &names).unwrap();
    record_version(dir.path(), &note("tag-2", "BBBB")).unwrap();
    record_epoch(dir.path(), "REC1", &names).unwrap();

    let HistoryResult::Epochs { epochs } = run_history(dir.path(), "Test Note.md", &HistoryOptions::default()).unwrap()
    else {
        panic!("expected epochs mode");
    };
    assert_eq!(epochs.len(), 2);
    assert_eq!(epochs[0].changed, vec!["note"]);
    assert_eq!(epochs[0].carried_over, vec!["table ATT-1"]);
    assert_eq!(epochs[1].changed, vec!["note", "table ATT-1"]);
    assert!(epochs[1].carried_over.is_empty());
}

#[test]
fn refuses_a_file_that_isnt_tracked() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    let err = run_history(dir.path(), "Nonexistent.md", &HistoryOptions::default()).unwrap_err();
    assert!(err.to_string().contains("isn't a tracked note"));
}

#[test]
fn refuses_when_theres_no_cloned_state() {
    let dir = tempfile::tempdir().unwrap();
    let err = run_history(dir.path(), "Test Note.md", &HistoryOptions::default()).unwrap_err();
    assert!(err.to_string().contains("doesn't look like a cloned notes directory"));
}
