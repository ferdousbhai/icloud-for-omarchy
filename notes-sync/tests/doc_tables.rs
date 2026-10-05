//! Decoding table records, and pushing table edits. Originally derived from icloud-md's tests.

mod common;

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
use icloud_notes_sync::doc::proto::crdt::VectorTimestamp;
use icloud_notes_sync::doc::proto::crdt::vector_timestamp::Element;
use icloud_notes_sync::doc::table_edit::{
    TableAttachmentUpdate, prepare_table_attachment_update_with, validate_table_document_invariants,
};
use icloud_notes_sync::doc::tables::{
    TableDocument, decode_table_markdown, encode_table_document, grid_from_table_document, parse_table_document,
    table_document_round_trips,
};
use icloud_notes_sync::js::{base64_decode, base64_encode};
use serde_json::json;

fn parse(file: &str) -> TableDocument {
    parse_table_document(&common::payload(file)).unwrap()
}

fn revisions(file: &str) -> Vec<(String, String)> {
    common::fixture(file)["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get("tag").or(r.get("seq")).unwrap().to_string(),
                r["base64"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

// --- decoding table records -----------------------------------------------------

#[test]
fn decode_table_markdown_renders_a_real_captured_2x2_grid() {
    assert_eq!(
        decode_table_markdown(&common::payload("table_first_revision.json")).unwrap(),
        "| A0 | B0 |\n| - | - |\n| | |"
    );
}

#[test]
fn decode_table_markdown_renders_a_real_captured_5x4_grid() {
    assert_eq!(
        decode_table_markdown(&common::payload("table_final_revision.json")).unwrap(),
        [
            "| A0 | B0 | B0-new | C0 |",
            "| - | - | - | - |",
            "| A1 | B1 | B1-new | C1 |",
            "| A2 | B2 | B2-new | C2 |",
            "| A3 | B3 | B3-new | C3-edited |",
            "| A4 | B4 | B4-new | C4 |",
        ]
        .join("\n")
    );
}

#[test]
fn decode_table_markdown_errors_on_bytes_that_arent_a_table_document() {
    assert!(decode_table_markdown(b"not a real table").is_err());
}

#[test]
fn table_document_round_trips_for_every_write_path_revision() {
    for (tag, b64) in revisions("table_write_path_revisions.json") {
        assert!(table_document_round_trips(&base64_decode(&b64)), "{tag}");
    }
}

#[test]
fn table_document_round_trips_for_every_long_lived_snapshot() {
    for (tag, b64) in revisions("table_long_lived_snapshots.json") {
        assert!(table_document_round_trips(&base64_decode(&b64)), "{tag}");
    }
}

#[test]
fn long_lived_revision_2ax_still_decodes_to_the_expected_grid() {
    assert_eq!(
        grid_from_table_document(&parse("table_long_lived_rev_2ax.json")).unwrap(),
        common::strings(&[
            &["A0", "B0", "B0-new", "C0"],
            &["A1", "B1", "B1-new", "C1"],
            &["A2", "B2", "B2-new", "C2"],
            &["A3", "B3", "B3-new", "C3-edited"],
            &["A4", "B4", "B4-new", "C4"],
        ])
    );
}

#[test]
fn real_revision_29z_decodes_to_the_expected_4x5_grid() {
    assert_eq!(
        grid_from_table_document(&parse("table_rev_baseline.json")).unwrap(),
        common::strings(&[
            &["R0C0", "R0C1", "R0C2", "R0C3", "R0C4"],
            &["R2C0", "R2C1", "R2C2", "R2C3", "R2C4"],
            &["R3C0", "R3C1", "R3C2", "R3C3", "R3C4"],
            &["R4C0", "R4C1", "R4C2", "R4C3", "R4C4"],
        ])
    );
}

#[test]
fn real_revision_2ai_decodes_to_the_expected_grid() {
    assert_eq!(
        grid_from_table_document(&parse("table_rev_cell_edit_2.json")).unwrap(),
        common::strings(&[
            &["R2C2", "NEW COL", "R2C3"],
            &["NEW ROW", "NEW COL / NEW ROW", "NEW ROW (2)"],
            &["R3C2", "NEW COL", "R3C3"],
        ])
    );
}

// --- pushing table edits --------------------------------------------------------

fn our_replica() -> [u8; 16] {
    std::array::from_fn(|i| 0xb0 + i as u8)
}

fn attachment_record(value: Option<String>) -> CloudKitRecord {
    let mut record = CloudKitRecord {
        record_name: "attachment-1".into(),
        record_type: "Attachment".into(),
        record_change_tag: Some("tag-1".into()),
        ..Default::default()
    };
    if let Some(value) = value {
        record.fields.insert(
            "MergeableDataEncrypted".into(),
            FieldValue {
                value: json!(value),
                type_: "ENCRYPTED_BYTES".into(),
            },
        );
    }
    record
}

fn first_revision_record() -> CloudKitRecord {
    attachment_record(Some(common::payload_base64("table_first_revision.json")))
}

fn prepare(record: &CloudKitRecord, grid: &[Vec<String>]) -> Result<TableAttachmentUpdate, String> {
    prepare_table_attachment_update_with(record, grid, &our_replica(), &mut common::counting_uuids())
}

fn expect_written(result: Result<TableAttachmentUpdate, String>, desired: &[Vec<String>]) -> TableDocument {
    let update = result.unwrap();
    assert!(update.changed);
    let compressed = base64_decode(&update.mergeable_data_base64);
    assert!(table_document_round_trips(&compressed));
    let doc = parse_table_document(&compressed).unwrap();
    assert_eq!(grid_from_table_document(&doc).unwrap(), desired);
    validate_table_document_invariants(&doc).unwrap();
    doc
}

#[test]
fn prepare_unchanged_grid_resolves_to_a_no_op_with_unchanged_bytes() {
    let record = first_revision_record();
    let grid = grid_from_table_document(&parse("table_first_revision.json")).unwrap();
    let update = prepare(&record, &grid).unwrap();
    assert!(!update.changed);
    assert_eq!(
        update.mergeable_data_base64,
        common::payload_base64("table_first_revision.json")
    );
}

#[test]
fn prepare_a_cell_edit_produces_new_gate_passing_bytes() {
    let desired = common::strings(&[&["A0", "B0-edited"], &["", ""]]);
    expect_written(prepare(&first_revision_record(), &desired), &desired);
}

#[test]
fn prepare_a_row_insertion_is_written() {
    let desired = common::strings(&[&["A0", "B0"], &["", ""], &["NEW", "ROW"]]);
    expect_written(prepare(&first_revision_record(), &desired), &desired);
}

#[test]
fn prepare_a_row_deletion_is_written() {
    let desired = common::strings(&[&["A0", "B0"]]);
    expect_written(prepare(&first_revision_record(), &desired), &desired);
}

#[test]
fn prepare_a_column_insertion_is_written() {
    let desired = common::strings(&[&["A0", "B0", "NEW"], &["", "", "COL"]]);
    expect_written(prepare(&first_revision_record(), &desired), &desired);
}

#[test]
fn prepare_a_column_deletion_is_written() {
    let desired = common::strings(&[&["A0"], &[""]]);
    expect_written(prepare(&first_revision_record(), &desired), &desired);
}

#[test]
fn prepare_a_pure_reorder_is_refused() {
    let reason = prepare(&first_revision_record(), &common::strings(&[&["B0", "A0"], &["", ""]])).unwrap_err();
    assert_eq!(
        reason,
        "rows or columns were reordered without anything added or removed - not supported in one edit (split it into a delete push and an insert push)"
    );
}

#[test]
fn prepare_changing_both_axes_at_once_is_refused_with_the_diffs_reason() {
    let reason = prepare(&first_revision_record(), &common::strings(&[&["one", "two", "three"]])).unwrap_err();
    assert_eq!(
        reason,
        "both row and column counts changed in the same edit - can't safely resolve this as one structural operation"
    );
}

#[test]
fn prepare_refuses_a_record_with_no_readable_mergeable_data() {
    assert_eq!(
        prepare(&attachment_record(None), &common::strings(&[&["A"]])).unwrap_err(),
        "table attachment has no readable data"
    );
}

#[test]
fn prepare_refuses_a_record_whose_document_is_a_crdt_delta() {
    let mut doc = parse("table_first_revision.json");
    doc.document.start_version = Some(VectorTimestamp {
        element: vec![Element {
            replica_index: Some(0),
            clock: Some(1),
            subclock: Some(0),
            ..Default::default()
        }],
        ..Default::default()
    });
    let delta = encode_table_document(&doc).unwrap();
    assert!(table_document_round_trips(&delta));
    let reason = prepare(
        &attachment_record(Some(base64_encode(&delta))),
        &common::strings(&[&["A0", "B0-edited"], &["", ""]]),
    )
    .unwrap_err();
    assert_eq!(
        reason,
        "Table document carries a populated startVersion - it is a CRDT delta, not a full document, and this engine only edits full documents"
    );
}

#[test]
fn prepare_refuses_a_record_whose_bytes_dont_decompress_or_round_trip() {
    let reason = prepare(
        &attachment_record(Some(base64_encode(b"not a real table"))),
        &common::strings(&[&["A"]]),
    )
    .unwrap_err();
    assert_eq!(
        reason,
        "the table's document doesn't round-trip byte-for-byte through our model - refusing to edit"
    );
}
