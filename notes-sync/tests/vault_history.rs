//! Ports icloud-md `src/notes/versionHistory.test.ts`, `noteEpoch.test.ts`
//! and `trackedFile.test.ts`, plus what the port adds: latest-only reading,
//! retention and previews that record nothing.

use std::path::Path;

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::vault::epoch::{find_epoch_by_id, list_epochs, record_epoch};
use icloud_notes_sync::vault::history::{
    HISTORY_KEEP_RECENT, HISTORY_RETENTION_DAYS, VersionSnapshotInput, find_snapshot_by_id, find_version,
    history_record_names, latest_version, list_versions, match_tracked_file, record_version, resolve_tracked_note_from,
    retention_keeps, without_recording,
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

// --- latest-only reading, retention, previews (not in icloud-md) -------------

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn capture_name(ms: i64, seq: usize, id: &str) -> String {
    let short: String = id.chars().filter(|c| *c != '-').take(8).collect();
    format!("{ms}-{seq:06}-{short}.json")
}

fn snapshot_id(n: usize) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// `days_ago` days before `now`, less an hour: day 30 falls inside the
/// retention window whatever the clock reads by the time it is pruned.
fn days_back(now: i64, days_ago: i64) -> i64 {
    now - days_ago * DAY_MS + HOUR_MS
}

/// A snapshot file as `record_version` writes it, [`days_back`]; returns
/// its id.
fn write_snapshot(dir: &Path, record_name: &str, now: i64, days_ago: i64, seq: usize) -> String {
    // Ids (and so short ids) distinct across records.
    let tag: u32 = record_name.bytes().map(u32::from).sum();
    let id = format!("{tag:04x}{:04x}-0000-4000-8000-000000000000", seq + 1);
    let record_dir = dir.join(".icloud-md/history").join(record_name);
    std::fs::create_dir_all(&record_dir).unwrap();
    let json = serde_json::json!({
        "recordName": record_name, "recordType": "Note", "field": "TextDataEncrypted",
        "recordChangeTag": format!("tag-{seq}"), "valueBase64": format!("V{seq}"),
        "id": id, "timestamp": "2026-01-01T00:00:00.000Z",
    });
    let name = capture_name(days_back(now, days_ago), seq, &id);
    std::fs::write(record_dir.join(name), json.to_string()).unwrap();
    id
}

fn write_epoch(dir: &Path, note: &str, ms: i64, seq: usize, snapshots: serde_json::Value) {
    let id = format!("e{seq:07x}-0000-4000-8000-000000000000");
    let epochs = dir.join(".icloud-md/history").join(note).join("epochs");
    std::fs::create_dir_all(&epochs).unwrap();
    let json = serde_json::json!({
        "id": id, "timestamp": "2026-01-01T00:00:00.000Z", "noteRecordName": note, "snapshots": snapshots,
    });
    std::fs::write(epochs.join(capture_name(ms, seq, &id)), json.to_string()).unwrap();
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    names
}

#[test]
fn record_version_and_record_epoch_parse_only_the_latest_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
    record_version(dir.path(), &note_input("REC-1", "tag-2", "BBBB")).unwrap();
    let record_dir = dir.path().join(".icloud-md/history/REC-1");
    let older = file_names(&record_dir).remove(0);
    std::fs::write(record_dir.join(&older), "not json").unwrap();

    assert!(!record_version(dir.path(), &note_input("REC-1", "tag-2", "BBBB")).unwrap());
    assert!(record_version(dir.path(), &note_input("REC-1", "tag-3", "CCCC")).unwrap());
    let latest = latest_version(dir.path(), "REC-1").unwrap().unwrap();
    assert_eq!(latest.value_base64, "CCCC");
    record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
    let epochs = list_epochs(dir.path(), "REC-1").unwrap();
    assert_eq!(epochs[0].snapshots["REC-1"], Some(latest.id.clone()));
    assert_eq!(find_version(dir.path(), "REC-1", &latest.id).unwrap(), Some(latest));
    assert!(
        list_versions(dir.path(), "REC-1").is_err(),
        "the full listing still parses every file"
    );
}

#[test]
fn sequence_numbers_continue_past_pruned_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(dir.path(), "REC-1", now_ms(), 1, 41);
    record_version(dir.path(), &note_input("REC-1", "tag-2", "BBBB")).unwrap();
    let names = file_names(&dir.path().join(".icloud-md/history/REC-1"));
    assert_eq!(names.len(), 2);
    assert_eq!(names[1].split('-').nth(1), Some("000042"));
}

const NOW: i64 = 1_790_000_000_000;

#[test]
fn retention_keeps_everything_up_to_the_recent_count() {
    let names: Vec<String> = (0..HISTORY_KEEP_RECENT)
        .map(|seq| capture_name(NOW - (100 - seq as i64) * DAY_MS, seq, &snapshot_id(seq)))
        .collect();
    assert!(retention_keeps(&names, NOW).iter().all(|k| *k));
}

#[test]
fn retention_keeps_the_newest_twenty_of_a_busy_day() {
    let names: Vec<String> = (0..50)
        .map(|seq| capture_name(NOW - HOUR_MS + seq as i64 * 1000, seq, &snapshot_id(seq)))
        .collect();
    let keeps = retention_keeps(&names, NOW);
    assert_eq!(keeps.iter().filter(|k| **k).count(), HISTORY_KEEP_RECENT);
    assert!(keeps[50 - HISTORY_KEEP_RECENT..].iter().all(|k| *k));
}

#[test]
fn retention_keeps_one_a_day_for_the_retention_window() {
    // `now` is 13:00 UTC; a capture every 12 hours (13:00 and 01:00) for 60
    // days, oldest first. `h` counts 12-hour steps back from now.
    let now = NOW.div_euclid(DAY_MS) * DAY_MS + 13 * HOUR_MS;
    let steps = 120;
    let names: Vec<String> = (0..steps)
        .map(|seq| capture_name(now - (steps - 1 - seq) as i64 * 12 * HOUR_MS, seq, &snapshot_id(seq)))
        .collect();
    let keeps = retention_keeps(&names, now);
    for (seq, keep) in keeps.iter().enumerate() {
        let h = (steps - 1 - seq) as i64;
        // The newest 20, then each day's 13:00 capture back to 30 days.
        let expected = h < HISTORY_KEEP_RECENT as i64 || (h % 2 == 0 && h <= HISTORY_RETENTION_DAYS * 2);
        assert_eq!(*keep, expected, "{h} half-days back");
    }
}

#[test]
fn retention_keeps_files_that_are_not_capture_names() {
    let mut names: Vec<String> = (0..40)
        .map(|seq| capture_name(NOW - (80 - seq as i64) * DAY_MS, seq, &snapshot_id(seq)))
        .collect();
    names.insert(0, "notes.json".to_owned());
    let keeps = retention_keeps(&names, NOW);
    assert!(keeps[0]);
    assert_eq!(keeps.iter().filter(|k| **k).count(), HISTORY_KEEP_RECENT + 1);
}

#[test]
fn record_version_prunes_the_record_but_keeps_what_kept_epochs_point_at() {
    let dir = tempfile::tempdir().unwrap();
    let now = now_ms();
    // One snapshot a day, 60 days back to yesterday.
    let ids: Vec<String> = (0..60)
        .map(|seq| write_snapshot(dir.path(), "REC-1", now, 60 - seq as i64, seq))
        .collect();
    // 25 epochs from 50 days ago: the newest (kept) points at the oldest
    // snapshot, the five oldest (pruned) at the second oldest.
    for seq in 0..25 {
        let pointed = match seq {
            24 => &ids[0],
            0..5 => &ids[1],
            _ => &ids[59],
        };
        let ms = now - 50 * DAY_MS + seq as i64;
        write_epoch(dir.path(), "REC-1", ms, seq, serde_json::json!({ "REC-1": pointed }));
    }
    // A table of the note: pruned by its own captures, not the note's.
    for seq in 0..31 {
        write_snapshot(dir.path(), "ATT-1", now, 70 - seq as i64, seq);
    }

    assert!(record_version(dir.path(), &note_input("REC-1", "tag-new", "NEW")).unwrap());

    let history = dir.path().join(".icloud-md/history");
    let kept = list_versions(dir.path(), "REC-1").unwrap();
    let kept_ids: Vec<&str> = kept.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(kept.last().unwrap().value_base64, "NEW");
    assert!(kept_ids.contains(&ids[0].as_str()), "pinned by a kept epoch");
    assert!(!kept_ids.contains(&ids[1].as_str()), "only pruned epochs pointed at it");
    // Days 1..=30 (one a day), the pinned one and the new one.
    assert_eq!(kept.len(), 30 + 1 + 1, "{kept_ids:?}");
    assert_eq!(file_names(&history.join("REC-1/epochs")).len(), HISTORY_KEEP_RECENT);
    assert_eq!(file_names(&history.join("ATT-1")).len(), 31);

    // A table capture prunes the table, against its note's epochs.
    let table = VersionSnapshotInput::table("ATT-1", "tag-x", "T-NEW", "REC-1");
    assert!(record_version(dir.path(), &table).unwrap());
    assert_eq!(file_names(&history.join("ATT-1")).len(), HISTORY_KEEP_RECENT);
}

#[test]
fn record_epoch_prunes_epochs_and_every_kept_epoch_still_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let now = now_ms();
    for seq in 0..40 {
        let days_ago = 45 - seq as i64;
        let id = write_snapshot(dir.path(), "REC-1", now, days_ago, seq);
        write_epoch(
            dir.path(),
            "REC-1",
            days_back(now, days_ago),
            seq,
            serde_json::json!({ "REC-1": id }),
        );
    }
    record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
    let epochs = list_epochs(dir.path(), "REC-1").unwrap();
    // The newest 20 epochs: the new one and days 6..=24; then days 25..=30.
    assert_eq!(epochs.len(), 20 + 6);
    for epoch in &epochs {
        let id = epoch.snapshots["REC-1"].as_deref().unwrap();
        assert!(find_version(dir.path(), "REC-1", id).unwrap().is_some(), "{id} is gone");
    }
    // The newest 20 snapshots (days 6..=25), then days 26..=30.
    assert_eq!(list_versions(dir.path(), "REC-1").unwrap().len(), 20 + 5);
    assert_eq!(
        find_epoch_by_id(dir.path(), "REC-1", &epochs[0].id).unwrap().as_ref(),
        Some(&epochs[0])
    );
}

#[test]
fn without_recording_writes_no_history_and_then_records_again() {
    let dir = tempfile::tempdir().unwrap();
    let wrote = without_recording(|| {
        let wrote = record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap();
        record_epoch(dir.path(), "REC-1", &names(&["REC-1"])).unwrap();
        wrote
    });
    assert!(!wrote);
    assert!(!dir.path().join(".icloud-md").exists());
    assert!(record_version(dir.path(), &note_input("REC-1", "tag-1", "AAAA")).unwrap());
}
