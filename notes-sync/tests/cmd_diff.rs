//! Ports icloud-md `src/commands/diff.test.ts`.

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::diff::{decode_snapshot_text, render_diff, run_diff_with};
use icloud_notes_sync::cmd::remote::{FnConnector, Remote};
use icloud_notes_sync::vault::epoch::{list_epochs, record_epoch};
use icloud_notes_sync::vault::history::{VersionSnapshot, VersionSnapshotInput, record_version};
use icloud_notes_sync::vault::state::{CloneState, NoteEntry, write_clone_state};

/// `REAL_PLAIN_NOTE` from `tests/fixtures/real/`.
fn real_plain_note() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real/real_plain_note.json");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    json["base64"].as_str().unwrap().to_owned()
}

// Same real capture attachmentSync.test.ts uses for "Test Table Note (2)".
const TABLE_MERGEABLE_DATA_BASE64: &str = "eJzt1E9oFFccwPGZyWZ39iWNr5OahofQdgxJjHZdB5tDxUOTqKTEoJtNemghxHW0u2x2ZXdFI3opiBcPihRLyUULObVNm4stNFQwSslhD+k/Dy0UNNAiEimoB7H27SbVbLKhh1JP32GH3/x+7/d5895jd23D+fq2ZRvSUF/etoQtgvrZbDZUX+ptO6BabcNx3Vej/3KpoG06VtTUsUbHGh1rdbR1DOr4gqoXQgRKM+vMUq+nNtumarMtZ6P7WncsPnIg7Xdn00dHMz3JnJ8oJLOZPv9QIZ6NJQ+/X1A/mx+Y35tiwhQnRJ8TXPj2G/1RsjxhaeHl2G6WK6auWKoc2y3VJGw99kRf63Tf0+cOyzZLt/OS3p7cOhuev3dhr3F5z0yxMXq+S1dNR16fbL6/M198PD98LRG5dHXKaReOsKKlbQXU02Mq10K6FnxWW9ZZW6Uz9Kym6lPCtvQhBRxLmhWZVZHVVGSB//tE7vRfnPbnHtxtGZ+9NTbUsLB4Ilc2dY7sOntj/5mBybbQw9S2pX0Kvafwin3W61rdqhMpdYoqnfVrnEhtRRasyEIVma06vMV31On5Gla8I6xrLy57xz+9pa/muhW9eraoXNa7O9bjWG9F/+PZWioS2+IYVWZZw1Ss0amyxsaVa+x67mt07eRBP1NIFsbcpkSu2o/YDeT99CE3mMjFssfybnhwsLenN3PQP+6GE7nF3rxbl/DT6aWk4+VEdjQycuRI2o90x3rikf6B/qOjB/xclYGBQi6ZOdyxftVA6S3L+zPZgp+PLP3NrB7o7V5jAIFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAVApPnmycaYlt/u6jYzt+/OlU17vvePLED198dfOT7WO/zQ21npuO/uHJ65PN93fmi4/nh68lIpeuTnnSP9V2OvDhGxOfnpt69PFfG+Ke3Dobnr93Ya9xec9MsTF6vsuTEyd794nxz4Y7fy/KP9XufZ68sqlzZNfZG/vPDEy2hR6mtnnyvdZXZFPdo19v/mLEPo8PbvTknf6L0/7cg7st47O3xoYaFt7cIJRYtUrHsi19m38DuMWS1w==";

fn no_network() -> FnConnector<impl Fn() -> Result<Remote, Error>> {
    FnConnector(|| -> Result<Remote, Error> { panic!("network") })
}

#[test]
#[ignore = "needs A/B/C"]
fn decode_snapshot_text_decodes_a_note_snapshot() {
    let snapshot = VersionSnapshot {
        id: "id-1".into(),
        timestamp: "2026-01-01T00:00:00.000Z".into(),
        record_name: "REC1".into(),
        record_type: "Note".into(),
        field: "TextDataEncrypted".into(),
        record_change_tag: "tag".into(),
        value_base64: real_plain_note(),
        note_record_name: None,
    };
    assert_eq!(
        decode_snapshot_text(&snapshot).unwrap(),
        "Test Note\nThis is a test note used for testing out `icloud-notes-sync`\n"
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn decode_snapshot_text_decodes_a_table_snapshot_as_markdown() {
    let snapshot = VersionSnapshot {
        id: "id-2".into(),
        timestamp: "2026-01-01T00:00:00.000Z".into(),
        record_name: "ATT-1".into(),
        record_type: "Attachment".into(),
        field: "MergeableDataEncrypted".into(),
        record_change_tag: "tag".into(),
        value_base64: TABLE_MERGEABLE_DATA_BASE64.into(),
        note_record_name: Some("REC1".into()),
    };
    assert_eq!(
        decode_snapshot_text(&snapshot).unwrap(),
        "| A0 | B0 |\n| - | - |\n| | |"
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn render_diff_shows_no_differences_for_identical_text() {
    let rendered = render_diff("a\nb\nc", "a\nb\nc", "old", "new");
    assert_eq!(rendered.text, "--- old\n+++ new\n  a\n  b\n  c\n(no differences)");
    assert!(!rendered.has_differences);
}

#[test]
#[ignore = "needs A/B/C"]
fn render_diff_shows_added_and_removed_lines() {
    let rendered = render_diff("a\nb\nc", "a\nx\nc", "old", "new");
    let lines: Vec<&str> = rendered.text.split('\n').collect();
    assert_eq!(lines[0], "--- old");
    assert_eq!(lines[1], "+++ new");
    assert!(lines.contains(&"- b"));
    assert!(lines.contains(&"+ x"));
    assert!(!rendered.text.contains("no differences"));
    assert!(rendered.has_differences);
}

#[test]
#[ignore = "needs A/B/C"]
fn render_diff_treats_empty_from_as_everything_added() {
    let rendered = render_diff("", "new line", "old", "new");
    assert!(rendered.text.split('\n').any(|l| l == "+ new line"));
    assert!(rendered.has_differences);
}

fn state() -> CloneState {
    CloneState {
        sync_token: Some("token".into()),
        notes: [("REC1".to_owned(), NoteEntry::new("Test Note.md", "1a", 100))]
            .into_iter()
            .collect(),
        ..Default::default()
    }
}

#[test]
fn rejects_a_ref_matching_neither_snapshot_nor_epoch() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    let err = run_diff_with(
        &no_network(),
        dir.path(),
        "Test Note.md",
        "missing-id",
        None,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("No version snapshot with id \"missing-id\" found")
    );
}

#[test]
fn refuses_epoch_from_to_form_without_network() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    record_version(
        dir.path(),
        &VersionSnapshotInput::note("REC1", "tag-1", &real_plain_note()),
    )
    .unwrap();
    record_epoch(dir.path(), "REC1", &["REC1".to_owned()]).unwrap();
    let epoch = list_epochs(dir.path(), "REC1").unwrap().remove(0);
    let err = run_diff_with(
        &no_network(),
        dir.path(),
        "Test Note.md",
        &epoch.id,
        Some("some-other-id"),
        &mut |_| {},
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("epoch-vs-epoch diff"), "{message}");
    assert!(message.contains("isn't supported yet"), "{message}");
}
