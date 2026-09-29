//! Ports icloud-md `src/notes/versionHistory.test.ts`, `noteEpoch.test.ts`
//! and `trackedFile.test.ts`.

use std::path::Path;

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::vault::epoch::{find_epoch_by_id, list_epochs, record_epoch};
use icloud_notes_sync::vault::history::{
    VersionSnapshotInput, find_snapshot_by_id, history_record_names, list_versions, match_tracked_file, record_version,
    resolve_tracked_note_from,
};
use icloud_notes_sync::vault::state::{CloneState, NoteEntry, TableAttachmentEntry};
use indexmap::IndexMap;

fn note_input(record_name: &str, tag: &str, value: &str) -> VersionSnapshotInput {
    VersionSnapshotInput::note(record_name, tag, value)
}

fn att_input(value: &str) -> VersionSnapshotInput {
    VersionSnapshotInput::table("ATT-1", "tag-1", value, "REC-1")
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

// --- versionHistory.test.ts ---------------------------------------------------

#[test]
fn record_version_and_list_versions_round_trip_a_single_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    let versions = list_versions(dir.path(), "REC-1").unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].record_name, "REC-1");
    assert_eq!(versions[0].value_base64, "AAAA");
    assert!(!versions[0].id.is_empty());
    assert!(!versions[0].timestamp.is_empty());
}

#[test]
fn list_versions_is_empty_when_nothing_recorded() {
    let dir = tempfile::tempdir().unwrap();
    assert!(list_versions(dir.path(), "REC-NONE").unwrap().is_empty());
}

#[test]
fn record_version_appends_when_content_changed() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-2", "BBBB")).unwrap();
    let values: Vec<String> = list_versions(dir.path(), "REC-1")
        .unwrap()
        .into_iter()
        .map(|v| v.value_base64)
        .collect();
    assert_eq!(values, ["AAAA", "BBBB"]);
}

#[test]
fn record_version_is_a_no_op_for_identical_content() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-unchanged-content", "AAAA")).unwrap();
    assert_eq!(list_versions(dir.path(), "REC-1").unwrap().len(), 1);
}

#[test]
fn record_version_records_both_edges_of_a_revert_and_forward() {
    let dir = tempfile::tempdir().unwrap();
    for v in ["AAAA", "BBBB", "AAAA"] {
        record_version(dir.path(), &note_input("REC-1", "tag-1", v)).unwrap();
    }
    let values: Vec<String> = list_versions(dir.path(), "REC-1")
        .unwrap()
        .into_iter()
        .map(|v| v.value_base64)
        .collect();
    assert_eq!(values, ["AAAA", "BBBB", "AAAA"]);
}

#[test]
fn record_version_keeps_records_separate() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &note_input("REC-2", "tag-1", "BBBB")).unwrap();
    assert_eq!(list_versions(dir.path(), "REC-1").unwrap().len(), 1);
    assert_eq!(list_versions(dir.path(), "REC-2").unwrap().len(), 1);
}

#[test]
fn record_version_reports_whether_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    assert!(record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap());
    assert!(!record_version(dir.path(), &note_input("REC-1", "tag-unchanged", "AAAA")).unwrap());
    assert!(record_version(dir.path(), &note_input("REC-1", "tag-1", "BBBB")).unwrap());
}

#[test]
fn record_version_tracks_a_table_attachment_with_its_note() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &att_input("CCCC")).unwrap();
    let versions = list_versions(dir.path(), "ATT-1").unwrap();
    assert_eq!(versions[0].note_record_name.as_deref(), Some("REC-1"));
    assert_eq!(versions[0].record_type, "Attachment");
}

#[test]
fn snapshot_file_is_written_in_icloud_md_key_order() {
    // `{...input, id, timestamp}`, 2-space JSON with a trailing newline, in
    // `<ms>-<seq>-<shortId>.json`.
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &att_input("CCCC")).unwrap();
    let history = dir.path().join(".icloud-md/history/ATT-1");
    let files: Vec<_> = std::fs::read_dir(&history)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files.len(), 1);
    let name = files[0].to_string_lossy().into_owned();
    let v = &list_versions(dir.path(), "ATT-1").unwrap()[0];
    let short: String = v.id.replace('-', "").chars().take(8).collect();
    assert!(name.ends_with(&format!("-000000-{short}.json")), "{name}");
    let text = std::fs::read_to_string(history.join(&name)).unwrap();
    let expected = format!(
        "{{\n  \"recordName\": \"ATT-1\",\n  \"recordType\": \"Attachment\",\n  \"field\": \"MergeableDataEncrypted\",\n  \
         \"recordChangeTag\": \"tag-1\",\n  \"valueBase64\": \"CCCC\",\n  \"noteRecordName\": \"REC-1\",\n  \
         \"id\": \"{}\",\n  \"timestamp\": \"{}\"\n}}\n",
        v.id, v.timestamp
    );
    assert_eq!(text, expected);
}

// --- noteEpoch.test.ts --------------------------------------------------------

#[test]
fn list_epochs_is_empty_when_nothing_recorded() {
    let dir = tempfile::tempdir().unwrap();
    assert!(list_epochs(dir.path(), "REC-1").unwrap().is_empty());
}

#[test]
fn record_epoch_indexes_the_current_snapshot_for_every_record() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &att_input("BBBB")).unwrap();
    let note = list_versions(dir.path(), "REC-1").unwrap().remove(0);
    let att = list_versions(dir.path(), "ATT-1").unwrap().remove(0);
    record_epoch(dir.path(), "REC-1", &names(&["REC-1", "ATT-1"])).unwrap();
    let epoch = list_epochs(dir.path(), "REC-1").unwrap().remove(0);
    assert_eq!(epoch.note_record_name, "REC-1");
    let expected: IndexMap<String, Option<String>> =
        [("REC-1".to_owned(), Some(note.id)), ("ATT-1".to_owned(), Some(att.id))]
            .into_iter()
            .collect();
    assert_eq!(epoch.snapshots, expected);
}

#[test]
fn record_epoch_records_null_for_an_uncaptured_record() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_epoch(dir.path(), "REC-1", &names(&["REC-1", "ATT-NEVER-CAPTURED"])).unwrap();
    let epoch = list_epochs(dir.path(), "REC-1").unwrap().remove(0);
    assert_eq!(epoch.snapshots["ATT-NEVER-CAPTURED"], None);
    let raw = std::fs::read_dir(dir.path().join(".icloud-md/history/REC-1/epochs"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .next()
        .unwrap();
    assert!(raw.contains("\"ATT-NEVER-CAPTURED\": null"));
}

#[test]
fn record_epoch_appends_rather_than_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "BBBB")).unwrap();
    record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
    let epochs = list_epochs(dir.path(), "REC-1").unwrap();
    assert_eq!(epochs.len(), 2);
    assert_ne!(epochs[0].id, epochs[1].id);
}

#[test]
fn find_epoch_by_id_locates_a_recorded_epoch() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
    let expected = list_epochs(dir.path(), "REC-1").unwrap().remove(0);
    let found = find_epoch_by_id(dir.path(), "REC-1", &expected.id).unwrap().unwrap();
    assert_eq!(found.id, expected.id);
}

#[test]
fn find_epoch_by_id_returns_none_when_nothing_matches() {
    let dir = tempfile::tempdir().unwrap();
    assert!(find_epoch_by_id(dir.path(), "REC-1", "missing-id").unwrap().is_none());
}

#[test]
fn record_epoch_keeps_notes_separate() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &note_input("REC-2", "tag-1", "AAAA")).unwrap();
    record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
    record_epoch(dir.path(), "REC-2", &names(&["REC-2"])).unwrap();
    assert_eq!(list_epochs(dir.path(), "REC-1").unwrap().len(), 1);
    assert_eq!(list_epochs(dir.path(), "REC-2").unwrap().len(), 1);
}

// --- trackedFile.test.ts ------------------------------------------------------

fn notes(entries: &[(&str, &str)]) -> IndexMap<String, NoteEntry> {
    entries
        .iter()
        .map(|(rn, file)| ((*rn).to_owned(), NoteEntry::new(*file, "1a", 100)))
        .collect()
}

fn state() -> CloneState {
    CloneState {
        sync_token: Some("token".into()),
        notes: notes(&[("REC1", "Test Note.md")]),
        table_attachments: Some(
            [("ATT-1", "REC1"), ("ATT-2", "REC-OTHER")]
                .into_iter()
                .map(|(k, n)| {
                    (
                        k.to_owned(),
                        TableAttachmentEntry {
                            note_record_name: n.into(),
                        },
                    )
                })
                .collect(),
        ),
        ..Default::default()
    }
}

fn nested() -> IndexMap<String, NoteEntry> {
    notes(&[
        ("REC-PIE", "Recipes/Pie.md"),
        ("REC-STANDUP", "Work/Standup.md"),
        ("REC-NOTES-1", "Recipes/Shared.md"),
        ("REC-NOTES-2", "Work/Shared.md"),
    ])
}

const ELSEWHERE: &str = "/somewhere/else";

#[test]
fn resolve_tracked_note_finds_a_note_by_exact_file_name() {
    let tracked =
        resolve_tracked_note_from(&state(), "Test Note.md", Path::new("/vault"), Path::new(ELSEWHERE)).unwrap();
    assert_eq!(tracked.record_name, "REC1");
    assert_eq!(tracked.entry.file, "Test Note.md");
}

#[test]
fn resolve_tracked_note_matches_by_base_name_when_given_a_path() {
    let tracked = resolve_tracked_note_from(
        &state(),
        "/some/path/Test Note.md",
        Path::new("/vault"),
        Path::new(ELSEWHERE),
    )
    .unwrap();
    assert_eq!(tracked.record_name, "REC1");
}

#[test]
fn resolve_tracked_note_errors_for_an_unknown_file() {
    let err =
        resolve_tracked_note_from(&state(), "Nonexistent.md", Path::new("/vault"), Path::new(ELSEWHERE)).unwrap_err();
    assert!(matches!(err, Error::UntrackedFile { .. }));
}

fn matched(file_arg: &str, cwd: &str) -> Result<Option<String>, Error> {
    let entries = nested();
    Ok(match_tracked_file(&entries, file_arg, Path::new("/vault"), Path::new(cwd))?.map(|(rn, _)| rn.clone()))
}

#[test]
fn match_tracked_file_resolves_a_bare_name_against_the_cwd() {
    assert_eq!(matched("Pie.md", "/vault/Recipes").unwrap().as_deref(), Some("REC-PIE"));
}

#[test]
fn match_tracked_file_resolves_a_dotdot_path_from_a_sibling() {
    assert_eq!(
        matched("../Work/Standup.md", "/vault/Recipes").unwrap().as_deref(),
        Some("REC-STANDUP")
    );
}

#[test]
fn match_tracked_file_prefers_the_cwd_exact_match() {
    assert_eq!(
        matched("Shared.md", "/vault/Work").unwrap().as_deref(),
        Some("REC-NOTES-2")
    );
}

#[test]
fn match_tracked_file_falls_back_to_a_unique_basename() {
    assert_eq!(
        matched("Standup.md", ELSEWHERE).unwrap().as_deref(),
        Some("REC-STANDUP")
    );
}

#[test]
fn match_tracked_file_errors_when_a_bare_name_is_ambiguous() {
    assert!(matches!(
        matched("Shared.md", ELSEWHERE),
        Err(Error::AmbiguousTrackedFile { .. })
    ));
}

#[test]
fn history_record_names_includes_the_note_plus_its_tables_only() {
    assert_eq!(history_record_names(&state(), "REC1"), ["REC1", "ATT-1"]);
}

#[test]
fn history_record_names_is_just_the_note_without_tables() {
    assert_eq!(history_record_names(&state(), "REC-NO-TABLES"), ["REC-NO-TABLES"]);
}

#[test]
fn find_snapshot_by_id_searches_across_record_names() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &VersionSnapshotInput::table("ATT-1", "tag", "AAAA", "REC1")).unwrap();
    let expected = list_versions(dir.path(), "ATT-1").unwrap().remove(0);
    let found = find_snapshot_by_id(dir.path(), &names(&["REC1", "ATT-1"]), &expected.id, "Test Note.md").unwrap();
    assert_eq!(found.record_name, "ATT-1");
    assert_eq!(found.value_base64, "AAAA");
}

#[test]
fn find_snapshot_by_id_errors_when_nothing_matches() {
    let dir = tempfile::tempdir().unwrap();
    let err = find_snapshot_by_id(dir.path(), &names(&["REC1", "ATT-1"]), "missing-id", "Test Note.md").unwrap_err();
    assert!(matches!(err, Error::UnknownVersionSnapshot { .. }));
}
