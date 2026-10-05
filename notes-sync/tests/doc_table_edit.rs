//! Table edits: rows, columns and cells. (A malformed replica id is
//! unrepresentable here: replica ids are `[u8; 16]`.) Originally derived from icloud-md's tests.

mod common;

use icloud_notes_sync::doc::proto::crdt::VectorTimestamp;
use icloud_notes_sync::doc::proto::crdt::vector_timestamp::Element;
use icloud_notes_sync::doc::proto::topotext::{self, Substring};
use icloud_notes_sync::doc::table_edit::{
    CellEdit, TableEditPlan, apply_table_edit_with, diff_table_grid, validate_table_document_invariants,
};
use icloud_notes_sync::doc::tables::{
    TableDocument, encode_table_document, grid_from_table_document, key_index_of, parse_ordered_set,
    parse_table_document, resolve_ref, resolve_table, table_document_round_trips, uuid_index_of_ref,
};
use icloud_notes_sync::js::base64_decode;
use serde_json::Value;

fn our_replica() -> [u8; 16] {
    std::array::from_fn(|i| 0xa0 + i as u8)
}
const SMALL_REPLICA: [u8; 16] = [0x01; 16];

fn parse_b64(b64: &str) -> TableDocument {
    parse_table_document(&base64_decode(b64)).unwrap()
}

fn apply(doc: &mut TableDocument, grid: &[Vec<String>], replica: &[u8; 16]) -> icloud_notes_sync::doc::Result<bool> {
    apply_table_edit_with(doc, grid, replica, &mut common::counting_uuids())
}

fn edit_and_verify(doc: &mut TableDocument, desired: &[Vec<String>]) -> TableDocument {
    assert!(apply(doc, desired, &our_replica()).unwrap());
    assert_eq!(grid_from_table_document(doc).unwrap(), desired);
    validate_table_document_invariants(doc).unwrap();
    let encoded = encode_table_document(doc).unwrap();
    assert!(table_document_round_trips(&encoded));
    let reparsed = parse_table_document(&encoded).unwrap();
    assert_eq!(grid_from_table_document(&reparsed).unwrap(), desired);
    reparsed
}

struct Revision {
    label: String,
    base64: String,
    grid: Option<Vec<Vec<String>>>,
}

fn revisions(file: &str) -> Vec<Revision> {
    common::fixture(file)["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r: &Value| Revision {
            label: format!(
                "{} {}",
                r.get("tag").or(r.get("seq")).unwrap(),
                r.get("op").and_then(Value::as_str).unwrap_or("")
            ),
            base64: r["base64"].as_str().unwrap().to_string(),
            grid: r.get("grid").map(common::grid),
        })
        .collect()
}

fn evolution(i: usize) -> TableDocument {
    parse_b64(&revisions("table_evolution_revisions.json")[i].base64)
}

fn restyle(i: usize) -> TableDocument {
    parse_b64(&revisions("table_restyle_revisions.json")[i].base64)
}

fn g(rows: &[&[&str]]) -> Vec<Vec<String>> {
    common::strings(rows)
}

fn all_fixtures() -> Vec<(String, String)> {
    let mut out = vec![
        ("first".to_string(), common::payload_base64("table_first_revision.json")),
        ("final".to_string(), common::payload_base64("table_final_revision.json")),
    ];
    for file in [
        "table_write_path_revisions.json",
        "table_long_lived_snapshots.json",
        "table_evolution_revisions.json",
        "table_restyle_revisions.json",
    ] {
        out.extend(
            revisions(file)
                .into_iter()
                .map(|r| (format!("{file} {}", r.label), r.base64)),
        );
    }
    out
}

#[test]
fn validate_table_document_invariants_accepts_every_real_fixture_untouched() {
    for (label, b64) in all_fixtures() {
        validate_table_document_invariants(&parse_b64(&b64)).unwrap_or_else(|e| panic!("{label}: {e}"));
    }
}

// --- ground truth: replay Apple's own scripted evolution -------------------

#[derive(Debug, PartialEq)]
struct AxisSummary {
    attachments: usize,
    set_pairs: usize,
    redirects: usize,
    mirror_text: String,
    mirror_tombstoned_units: u32,
}

fn axis_summary(doc: &TableDocument, pool_ref: u32) -> AxisSummary {
    let ordered_set = doc.objects()[pool_ref as usize].ts_ordered_set.as_ref().unwrap();
    let array = ordered_set.array.as_ref().unwrap();
    let string_array = array.array.as_ref().unwrap();
    let mirror = string_array.contents.as_ref().unwrap();
    AxisSummary {
        attachments: string_array.attachments.len(),
        set_pairs: ordered_set.set.as_ref().unwrap().element.len(),
        redirects: array.dictionary.as_ref().unwrap().element.len(),
        mirror_text: mirror.string().to_string(),
        mirror_tombstoned_units: mirror
            .substring
            .iter()
            .filter(|s| s.tombstone == Some(1))
            .map(|s| s.length.unwrap_or(0))
            .sum(),
    }
}

#[derive(Debug, PartialEq)]
struct StructuralSummary {
    grid: Option<Vec<Vec<String>>>,
    rows: AxisSummary,
    columns: AxisSummary,
    cell_columns_entries: usize,
    row_map_sizes: Vec<i64>,
    identity_objects: usize,
}

fn structural_summary(doc: &TableDocument) -> StructuralSummary {
    let key = key_index_of(doc, "UUIDIndex");
    let identity_objects = doc
        .objects()
        .iter()
        .filter(|e| {
            e.custom
                .as_ref()
                .is_some_and(|c| c.map_entry.len() == 1 && i64::from(c.map_entry[0].key.unwrap_or(0)) == key)
        })
        .count();
    let cell_columns = doc.objects()[doc.cell_columns_ref as usize]
        .dictionary
        .as_ref()
        .unwrap();
    let mut row_map_sizes: Vec<i64> = cell_columns
        .element
        .iter()
        .map(|el| {
            el.value
                .as_ref()
                .and_then(|v| doc.objects().get(v.object_index.unwrap_or(0) as usize))
                .and_then(|o| o.dictionary.as_ref())
                .map_or(-1, |d| d.element.len() as i64)
        })
        .collect();
    row_map_sizes.sort();
    StructuralSummary {
        grid: Some(grid_from_table_document(doc).unwrap()),
        rows: axis_summary(doc, doc.cr_rows_ref),
        columns: axis_summary(doc, doc.cr_columns_ref),
        cell_columns_entries: cell_columns.element.len(),
        row_map_sizes,
        identity_objects,
    }
}

#[test]
fn evolution_replay_each_edit_is_structurally_equivalent_to_apples_own_save() {
    let revs = revisions("table_evolution_revisions.json");
    for pair in revs.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        let mut doc = parse_b64(&before.base64);
        let reparsed = edit_and_verify(&mut doc, after.grid.as_ref().unwrap());
        assert_eq!(
            structural_summary(&reparsed),
            structural_summary(&parse_b64(&after.base64)),
            "{} -> {}",
            before.label,
            after.label
        );
    }
}

#[test]
fn write_path_replay_the_same_logical_edit_applies_cleanly() {
    let revs = revisions("table_write_path_revisions.json");
    for pair in revs.windows(2) {
        let mut doc = parse_b64(&pair[0].base64);
        let desired = grid_from_table_document(&parse_b64(&pair[1].base64)).unwrap();
        edit_and_verify(&mut doc, &desired);
    }
}

// --- replica registration, in both clock systems --------------------------

fn identity_uuid_indexes(doc: &TableDocument) -> Vec<u64> {
    let key = key_index_of(doc, "UUIDIndex");
    doc.objects()
        .iter()
        .filter_map(|e| {
            let c = e.custom.as_ref()?;
            if c.map_entry.len() != 1 || i64::from(c.map_entry[0].key.unwrap_or(0)) != key {
                return None;
            }
            Some(c.map_entry[0].value.as_ref()?.unsigned_integer_value.unwrap_or(0))
        })
        .collect()
}

#[test]
fn first_edit_registers_our_replica_and_shifts_every_identity_uuid_index() {
    let original = evolution(4);
    let replica_count = original.version().element.len();
    let uuid_count = original.uuid_table().len();
    let mut before_indexes = identity_uuid_indexes(&original);
    before_indexes.sort();

    let mut doc = evolution(4);
    edit_and_verify(
        &mut doc,
        &g(&[&["apple-r1c1", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2-changed"]]),
    );
    assert_eq!(doc.version().element.len(), replica_count + 1);
    let ours = &doc.version().element[replica_count];
    assert_eq!(ours.replica_index, Some(replica_count as u64));
    assert_eq!(ours.clock, Some(1));
    assert_eq!(doc.uuid_table().len(), uuid_count + 1);
    assert_eq!(doc.uuid_table()[replica_count], our_replica().to_vec());
    let mut after = identity_uuid_indexes(&doc);
    after.sort();
    assert_eq!(after, before_indexes.iter().map(|i| i + 1).collect::<Vec<_>>());
}

fn mirror(doc: &TableDocument, pool_ref: u32) -> &topotext::String {
    doc.objects()[pool_ref as usize]
        .ts_ordered_set
        .as_ref()
        .unwrap()
        .array
        .as_ref()
        .unwrap()
        .array
        .as_ref()
        .unwrap()
        .contents
        .as_ref()
        .unwrap()
}

fn char_id(s: &Substring) -> (u32, u32) {
    let id = s.char_id.as_ref().unwrap();
    (id.replica_id.unwrap_or(0), id.clock.unwrap_or(0))
}

#[test]
fn registration_below_existing_entries_inserts_at_the_sorted_rank_and_remaps_every_char_id() {
    let original = evolution(4);
    let before: Vec<(u32, u32)> = mirror(&original, original.cr_rows_ref)
        .substring
        .iter()
        .map(char_id)
        .collect();
    let mut doc = evolution(4);
    let desired = g(&[&["apple-r1c1", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2-small"]]);
    assert!(apply(&mut doc, &desired, &SMALL_REPLICA).unwrap());
    validate_table_document_invariants(&doc).unwrap();
    assert_eq!(grid_from_table_document(&doc).unwrap(), desired);
    let tt = doc.document.tt_timestamp.as_ref().unwrap();
    assert_eq!(tt.clock[0].replica_uuid.as_deref(), Some(&SMALL_REPLICA[..]));
    let after: Vec<(u32, u32)> = mirror(&doc, doc.cr_rows_ref).substring.iter().map(char_id).collect();
    for (i, (replica, clock)) in before.iter().enumerate() {
        assert_eq!(after[i].0, if *replica == 0 { 0 } else { replica + 1 });
        assert_eq!(after[i].1, *clock);
    }
    let edited = doc
        .objects()
        .iter()
        .find(|o| o.string.as_ref().is_some_and(|s| s.string() == "delta-r2c2-small"))
        .unwrap();
    assert!(
        edited
            .string
            .as_ref()
            .unwrap()
            .substring
            .iter()
            .any(|s| char_id(s).0 == 1)
    );
    let encoded = encode_table_document(&doc).unwrap();
    assert!(table_document_round_trips(&encoded));
    assert_eq!(
        grid_from_table_document(&parse_table_document(&encoded).unwrap()).unwrap(),
        desired
    );
}

#[test]
fn the_direction_markers_register_latest_is_left_untouched() {
    let original = evolution(4);
    let before = original
        .objects()
        .iter()
        .find_map(|o| o.register_latest.clone())
        .unwrap();
    let mut doc = evolution(4);
    edit_and_verify(
        &mut doc,
        &g(&[&["apple-r1c1", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2-reg"]]),
    );
    let after = doc.objects().iter().find_map(|o| o.register_latest.clone()).unwrap();
    assert_eq!(after.timestamp, before.timestamp);
}

// --- the Stage-1 doubling incident's own artifact -------------------------

fn incident_replica() -> [u8; 16] {
    let hex = "44681ad5c726c5e57d0008df7530ae41";
    std::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
}

fn incident() -> TableDocument {
    parse_b64(&common::payload_base64("table_unsorted_tt_regression.json"))
}

#[test]
fn the_unsorted_tt_timestamp_incident_document_violates_the_invariants_as_parsed() {
    let err = validate_table_document_invariants(&incident()).unwrap_err().to_string();
    assert!(err.contains("sorted UUID order") || err.contains("clock"), "{err}");
}

#[test]
fn editing_the_incident_document_under_its_own_replica_heals_it() {
    let mut doc = incident();
    let desired = g(&[&["alpha", "bravo"], &["one", "two-edited-again"]]);
    assert!(apply(&mut doc, &desired, &incident_replica()).unwrap());
    assert_eq!(grid_from_table_document(&doc).unwrap(), desired);
    validate_table_document_invariants(&doc).unwrap();
    let tt = doc.document.tt_timestamp.as_ref().unwrap();
    assert_eq!(tt.clock[0].replica_uuid.as_deref(), Some(&incident_replica()[..]));
    let encoded = encode_table_document(&doc).unwrap();
    assert!(table_document_round_trips(&encoded));
    assert_eq!(
        grid_from_table_document(&parse_table_document(&encoded).unwrap()).unwrap(),
        desired
    );
}

#[test]
fn a_no_op_edit_against_the_incident_document_reports_no_change() {
    let mut doc = incident();
    let grid = grid_from_table_document(&doc).unwrap();
    assert!(!apply(&mut doc, &grid, &incident_replica()).unwrap());
}

#[test]
fn an_unsorted_document_that_is_not_our_own_residue_is_refused() {
    let mut doc = incident();
    let mut changed = grid_from_table_document(&doc).unwrap();
    changed[0][0] = "changed".into();
    let err = apply(&mut doc, &changed, &our_replica()).unwrap_err().to_string();
    assert_eq!(
        err,
        "Table's topotext clock table isn't in sorted UUID order and isn't this tool's own residue - refusing to edit"
    );
}

#[test]
fn first_edit_registers_our_topotext_clock() {
    let mut doc = evolution(4);
    let before = doc.document.tt_timestamp.as_ref().unwrap().clock.len();
    edit_and_verify(
        &mut doc,
        &g(&[&["apple-r1c1", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2-XY"]]),
    );
    let tt = doc.document.tt_timestamp.as_ref().unwrap();
    assert_eq!(tt.clock.len(), before + 1);
    let ours = tt.clock.last().unwrap();
    assert_eq!(ours.replica_uuid.as_deref(), Some(&our_replica()[..]));
    assert_eq!(ours.replica_clock.len(), 2);
    assert_eq!(ours.replica_clock[0].clock, Some(3));
    assert_eq!(ours.replica_clock[1].clock, Some(1));
}

#[test]
fn a_second_edit_reuses_the_registered_replica_and_keeps_both_clocks_monotonic() {
    let mut doc = evolution(4);
    edit_and_verify(
        &mut doc,
        &g(&[&["apple-r1c1", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2-more"]]),
    );
    let replica_count = doc.version().element.len();
    let tt_entries = doc.document.tt_timestamp.as_ref().unwrap().clock.len();
    let clock_after_first = doc.version().element[replica_count - 1].clock.unwrap();
    edit_and_verify(
        &mut doc,
        &g(&[&["apple-r1c1", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2-more-still"]]),
    );
    assert_eq!(doc.version().element.len(), replica_count);
    assert_eq!(doc.document.tt_timestamp.as_ref().unwrap().clock.len(), tt_entries);
    assert!(doc.version().element[replica_count - 1].clock.unwrap() > clock_after_first);
}

// --- the redirect/identity-pair rules, per edit type ----------------------

fn base_3x2() -> TableDocument {
    evolution(7)
}

fn redirect_counts(doc: &TableDocument) -> (usize, usize) {
    let redirects = |r: u32| {
        doc.objects()[r as usize]
            .ts_ordered_set
            .as_ref()
            .unwrap()
            .array
            .as_ref()
            .unwrap()
            .dictionary
            .as_ref()
            .unwrap()
            .element
            .len()
    };
    (redirects(doc.cr_rows_ref), redirects(doc.cr_columns_ref))
}

#[test]
fn row_insert_mints_a_real_identity_pair() {
    let mut doc = base_3x2();
    let before = redirect_counts(&doc);
    edit_and_verify(
        &mut doc,
        &g(&[
            &["apple-r1c1-edit1", "berry-r1c2"],
            &["new-r2c1", "new-r2c2"],
            &["cedar-r2c1", "delta-r2c2"],
            &["echo-r3c1", ""],
        ]),
    );
    assert_eq!(redirect_counts(&doc), (before.0 + 1, before.1));
    let row_set = doc.objects()[doc.cr_rows_ref as usize].ts_ordered_set.as_ref().unwrap();
    let redirect = &row_set.array.as_ref().unwrap().dictionary.as_ref().unwrap().element[before.0];
    let ordering = uuid_index_of_ref(
        &doc.document,
        resolve_ref(redirect.key.as_ref().unwrap(), "redirect key").unwrap(),
    )
    .unwrap();
    let content = uuid_index_of_ref(
        &doc.document,
        resolve_ref(redirect.value.as_ref().unwrap(), "redirect value").unwrap(),
    )
    .unwrap();
    assert_ne!(ordering, content);
}

#[test]
fn column_insert_mints_an_identity_pair_and_a_full_row_map() {
    let mut doc = base_3x2();
    let before = redirect_counts(&doc);
    edit_and_verify(
        &mut doc,
        &g(&[
            &["apple-r1c1-edit1", "mid-r1", "berry-r1c2"],
            &["cedar-r2c1", "mid-r2", "delta-r2c2"],
            &["echo-r3c1", "mid-r3", ""],
        ]),
    );
    assert_eq!(redirect_counts(&doc), (before.0, before.1 + 1));
}

fn tombstoned(s: &topotext::String) -> Vec<&Substring> {
    s.substring.iter().filter(|s| s.tombstone == Some(1)).collect()
}

fn anchor_clock(s: &Substring) -> u32 {
    s.timestamp.as_ref().unwrap().clock.unwrap_or(0)
}

fn anchor_replica(s: &Substring) -> u32 {
    s.timestamp.as_ref().unwrap().replica_id.unwrap_or(0)
}

#[test]
fn row_delete_retains_redirects_and_identities_and_tombstones_the_mirror_by_apples_rule() {
    let mut doc = base_3x2();
    let before = redirect_counts(&doc);
    let identities = identity_uuid_indexes(&doc).len();
    let set_pairs = doc.objects()[doc.cr_rows_ref as usize]
        .ts_ordered_set
        .as_ref()
        .unwrap()
        .set
        .as_ref()
        .unwrap()
        .element
        .len();
    edit_and_verify(&mut doc, &g(&[&["apple-r1c1-edit1", "berry-r1c2"], &["echo-r3c1", ""]]));
    assert_eq!(redirect_counts(&doc), before);
    assert_eq!(identity_uuid_indexes(&doc).len(), identities);
    assert_eq!(
        doc.objects()[doc.cr_rows_ref as usize]
            .ts_ordered_set
            .as_ref()
            .unwrap()
            .set
            .as_ref()
            .unwrap()
            .element
            .len(),
        set_pairs - 1
    );
    let row_mirror = mirror(&doc, doc.cr_rows_ref);
    let dead = tombstoned(row_mirror);
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].length, Some(1));

    let apple = evolution(8);
    let apple_dead = tombstoned(mirror(&apple, apple.cr_rows_ref))
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(apple_dead.len(), 1);
    assert_eq!(anchor_clock(&apple_dead[0]), 8);
    assert_eq!(
        apple.document.tt_timestamp.as_ref().unwrap().clock[0].replica_clock[1].clock,
        Some(9)
    );

    let tt = doc.document.tt_timestamp.as_ref().unwrap();
    assert_eq!(anchor_replica(dead[0]) as usize, tt.clock.len());
    assert_eq!(anchor_clock(dead[0]), 8);
    assert_eq!(tt.clock.last().unwrap().replica_clock[1].clock, Some(9));
}

#[test]
fn a_later_deletion_floors_on_the_already_advanced_style_clock() {
    let mut doc = base_3x2();
    edit_and_verify(&mut doc, &g(&[&["apple-r1c1-edit1", "berry-r1c2"], &["echo-r3c1", ""]]));
    edit_and_verify(&mut doc, &g(&[&["apple-r1c1-edit1", "berry-r1c2"]]));
    let apple = evolution(11);
    assert_eq!(
        tombstoned(mirror(&apple, apple.cr_columns_ref))
            .iter()
            .map(|s| anchor_clock(s))
            .collect::<Vec<_>>(),
        vec![9]
    );
    assert_eq!(
        apple.document.tt_timestamp.as_ref().unwrap().clock[0].replica_clock[1].clock,
        Some(10)
    );
    assert_eq!(
        tombstoned(mirror(&doc, doc.cr_rows_ref))
            .iter()
            .map(|s| anchor_clock(s))
            .collect::<Vec<_>>(),
        vec![8, 9]
    );
    assert_eq!(
        doc.document
            .tt_timestamp
            .as_ref()
            .unwrap()
            .clock
            .last()
            .unwrap()
            .replica_clock[1]
            .clock,
        Some(10)
    );
}

#[test]
fn a_cell_text_deletion_restamps_its_tombstone_and_advances_the_shared_style_clock() {
    let mut doc = base_3x2();
    edit_and_verify(
        &mut doc,
        &g(&[
            &["apple-r1c1-edit1", "berry"],
            &["cedar-r2c1", "delta-r2c2"],
            &["echo-r3c1", ""],
        ]),
    );
    let tt = doc.document.tt_timestamp.as_ref().unwrap();
    let cell = doc
        .objects()
        .iter()
        .find_map(|o| o.string.as_ref().filter(|s| s.string() == "berry"))
        .unwrap();
    let dead = tombstoned(cell);
    assert_eq!(dead.len(), 1);
    assert_eq!(anchor_replica(dead[0]) as usize, tt.clock.len());
    assert_eq!(anchor_clock(dead[0]), 8);
    assert_eq!(tt.clock.last().unwrap().replica_clock[1].clock, Some(9));
}

// --- Apple's own restyle sequence ------------------------------------------

fn cell_string(doc: &TableDocument, row: usize, column: usize) -> topotext::String {
    let resolved = resolve_table(doc).unwrap();
    let cell = resolved.cells.get(&(row, column)).unwrap();
    doc.objects()[cell.text_ref as usize].string.clone().unwrap()
}

fn apple_clocks(doc: &TableDocument) -> (u32, u32) {
    let entry = &doc.document.tt_timestamp.as_ref().unwrap().clock[0];
    (
        entry.replica_clock[0].clock.unwrap(),
        entry.replica_clock[1].clock.unwrap(),
    )
}

fn live(s: &topotext::String) -> Vec<Substring> {
    s.substring
        .iter()
        .filter(|r| r.tombstone != Some(1) && r.length.unwrap_or(0) > 0)
        .cloned()
        .collect()
}

fn dead(s: &topotext::String) -> Vec<Substring> {
    s.substring.iter().filter(|r| r.tombstone == Some(1)).cloned().collect()
}

#[test]
fn apple_restyling_a_cell_stamps_its_live_run_by_the_plus_1_rule() {
    let before = restyle(2);
    let after = restyle(3);
    let before_run = &live(&cell_string(&before, 0, 0))[0];
    assert_eq!(before_run.length, Some(5));
    assert_eq!(anchor_clock(before_run), 0);
    assert_eq!(cell_string(&before, 0, 0).attribute_run.len(), 1);
    assert!(cell_string(&before, 0, 0).attribute_run[0].font.is_none());
    assert_eq!(apple_clocks(&before), (9, 9));
    let after_run = &live(&cell_string(&after, 0, 0))[0];
    assert_eq!(char_id(after_run), char_id(before_run));
    assert_eq!(anchor_replica(after_run), 1);
    assert_eq!(anchor_clock(after_run), 9);
    assert_eq!(apple_clocks(&after), (9, 10));
    let run = &cell_string(&after, 0, 0).attribute_run[0];
    assert_eq!(run.font_hints, Some(1));
    assert_eq!(run.font.as_ref().unwrap().name.as_deref(), Some("SFUIText-Bold"));
}

#[test]
fn apple_deleting_a_restyled_cell_stamps_old_plus_8() {
    let before = restyle(3);
    let after = restyle(4);
    assert_eq!(anchor_clock(&live(&cell_string(&before, 0, 0))[0]), 9);
    assert_eq!(apple_clocks(&before), (9, 10));
    let tombstones = dead(&cell_string(&after, 0, 0));
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].length, Some(5));
    assert_eq!(anchor_clock(&tombstones[0]), 17);
    assert_eq!(anchor_replica(&tombstones[0]), 1);
    assert_eq!(apple_clocks(&after), (9, 18));
}

#[test]
fn apple_deleting_a_never_restyled_cell_stamps_the_floor_side() {
    let before = restyle(1);
    let after = restyle(2);
    assert_eq!(anchor_clock(&live(&cell_string(&before, 0, 1))[0]), 0);
    assert_eq!(apple_clocks(&before), (9, 1));
    let tombstones = dead(&cell_string(&after, 0, 1));
    assert_eq!(tombstones.len(), 1);
    assert_eq!(anchor_clock(&tombstones[0]), 8);
    assert_eq!(apple_clocks(&after), (9, 9));
}

#[test]
fn apple_deleting_a_run_it_created_in_the_same_save_takes_the_other_branch() {
    let before = restyle(4);
    let after = restyle(5);
    assert!(dead(&cell_string(&before, 1, 1)).is_empty());
    let tombstones = dead(&cell_string(&after, 1, 1));
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].length, Some(4));
    assert_eq!(anchor_clock(&tombstones[0]), 0);
    assert_eq!(anchor_replica(&tombstones[0]), 1);
    assert_eq!(apple_clocks(&before), (9, 18));
    assert_eq!(apple_clocks(&after), (13, 18));
}

fn our_tt_index(doc: &TableDocument) -> Option<usize> {
    doc.document
        .tt_timestamp
        .as_ref()?
        .clock
        .iter()
        .position(|c| c.replica_uuid.as_deref() == Some(&our_replica()[..]))
}

#[test]
fn our_engine_deleting_the_restyled_cell_reproduces_apples_own_17() {
    let mut doc = restyle(3);
    edit_and_verify(&mut doc, &g(&[&["", ""], &["charlie", ""]]));
    let ours = our_tt_index(&doc).unwrap();
    let tombstones = dead(&cell_string(&doc, 0, 0));
    assert_eq!(tombstones.len(), 1);
    assert_eq!(anchor_replica(&tombstones[0]) as usize, ours + 1);
    assert_eq!(anchor_clock(&tombstones[0]), 17);
    assert_eq!(
        doc.document.tt_timestamp.as_ref().unwrap().clock[ours].replica_clock[1].clock,
        Some(18)
    );
    assert_eq!(anchor_clock(&dead(&cell_string(&restyle(4), 0, 0))[0]), 17);
    assert!(cell_string(&doc, 0, 0).attribute_run.is_empty());
    assert!(cell_string(&restyle(4), 0, 0).attribute_run.is_empty());
}

#[test]
fn our_engine_deleting_the_never_restyled_cell_reproduces_apples_own_8() {
    let mut doc = restyle(1);
    edit_and_verify(&mut doc, &g(&[&["alpha", ""], &["charlie", ""]]));
    let ours = our_tt_index(&doc).unwrap();
    let tombstones = dead(&cell_string(&doc, 0, 1));
    assert_eq!(tombstones.len(), 1);
    assert_eq!(anchor_clock(&tombstones[0]), 8);
    assert_eq!(
        doc.document.tt_timestamp.as_ref().unwrap().clock[ours].replica_clock[1].clock,
        Some(9)
    );
    assert_eq!(anchor_clock(&dead(&cell_string(&restyle(2), 0, 1))[0]), 8);
}

fn every_mergeable_string(doc: &TableDocument) -> Vec<topotext::String> {
    let mut out: Vec<topotext::String> = doc.objects().iter().filter_map(|o| o.string.clone()).collect();
    for r in [doc.cr_rows_ref, doc.cr_columns_ref] {
        out.push(mirror(doc, r).clone());
    }
    out
}

fn our_topotext_state(doc: &TableDocument) -> (u32, u32) {
    match our_tt_index(doc) {
        None => (
            doc.document.tt_timestamp.as_ref().map_or(0, |t| t.clock.len()) as u32 + 1,
            0,
        ),
        Some(i) => (
            i as u32 + 1,
            doc.document.tt_timestamp.as_ref().unwrap().clock[i].replica_clock[0]
                .clock
                .unwrap(),
        ),
    }
}

fn assert_no_same_save_tombstone(doc: &TableDocument, before: (u32, u32), label: &str) {
    for s in every_mergeable_string(doc) {
        for run in dead(&s) {
            let (replica, clock) = char_id(&run);
            assert!(
                !(replica == before.0 && clock >= before.1),
                "{label}: run {replica}:{clock} was both created and tombstoned by one save"
            );
        }
    }
}

#[test]
fn no_supported_edit_creates_and_tombstones_a_run_in_one_save() {
    let edits: Vec<(&str, Vec<Vec<String>>)> = vec![
        (
            "cell text replaced outright",
            g(&[
                &["zulu", "berry-r1c2"],
                &["cedar-r2c1", "delta-r2c2"],
                &["echo-r3c1", ""],
            ]),
        ),
        (
            "row inserted",
            g(&[
                &["apple-r1c1-edit1", "berry-r1c2"],
                &["cedar-r2c1", "delta-r2c2"],
                &["echo-r3c1", ""],
                &["new", "row"],
            ]),
        ),
        (
            "row deleted",
            g(&[&["apple-r1c1-edit1", "berry-r1c2"], &["echo-r3c1", ""]]),
        ),
        (
            "column inserted",
            g(&[
                &["apple-r1c1-edit1", "berry-r1c2", "new"],
                &["cedar-r2c1", "delta-r2c2", ""],
                &["echo-r3c1", "", ""],
            ]),
        ),
        (
            "column deleted",
            g(&[&["apple-r1c1-edit1"], &["cedar-r2c1"], &["echo-r3c1"]]),
        ),
    ];
    for (label, grid) in edits {
        let mut doc = base_3x2();
        let before = our_topotext_state(&doc);
        edit_and_verify(&mut doc, &grid);
        assert_no_same_save_tombstone(&doc, before, label);
    }
    let mut doc = base_3x2();
    edit_and_verify(
        &mut doc,
        &g(&[
            &["ours", "berry-r1c2"],
            &["cedar-r2c1", "delta-r2c2"],
            &["echo-r3c1", ""],
        ]),
    );
    let between = our_topotext_state(&doc);
    edit_and_verify(
        &mut doc,
        &g(&[&["", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2"], &["echo-r3c1", ""]]),
    );
    assert_no_same_save_tombstone(&doc, between, "second save");
    let ours: Vec<Substring> = every_mergeable_string(&doc)
        .iter()
        .flat_map(dead)
        .filter(|r| char_id(r).0 == between.0)
        .collect();
    assert_eq!(ours.len(), 1);
    assert!(char_id(&ours[0]).1 < between.1);
}

#[test]
fn column_delete_physically_removes_the_row_map_and_its_cells() {
    let mut doc = base_3x2();
    let before = redirect_counts(&doc);
    let identities = identity_uuid_indexes(&doc).len();
    let pool = doc.objects().len();
    edit_and_verify(&mut doc, &g(&[&["apple-r1c1-edit1"], &["cedar-r2c1"], &["echo-r3c1"]]));
    assert_eq!(redirect_counts(&doc), before);
    assert_eq!(identity_uuid_indexes(&doc).len(), identities);
    assert_eq!(doc.objects().len(), pool - 4);
}

#[test]
fn cell_edits_change_nothing_structural() {
    let mut doc = base_3x2();
    let mut before = structural_summary(&doc);
    edit_and_verify(
        &mut doc,
        &g(&[
            &["apple-r1c1-edit1", "berry-r1c2-changed"],
            &["cedar-r2c1", "delta-r2c2"],
            &["echo-r3c1", "now filled"],
        ]),
    );
    let mut after = structural_summary(&doc);
    before.grid = None;
    after.grid = None;
    assert_eq!(after, before);
}

#[test]
fn sequential_edits_accumulate_cleanly() {
    let mut doc = evolution(4);
    let steps = vec![
        g(&[&["apple-x", "berry-r1c2"], &["cedar-r2c1", "delta-r2c2"]]),
        g(&[
            &["apple-x", "berry-r1c2"],
            &["mid-1", "mid-2"],
            &["cedar-r2c1", "delta-r2c2"],
        ]),
        g(&[
            &["apple-x", "berry-r1c2", "c3-1"],
            &["mid-1", "mid-2", "c3-2"],
            &["cedar-r2c1", "delta-r2c2", "c3-3"],
        ]),
        g(&[
            &["apple-x", "berry-r1c2", "c3-1"],
            &["cedar-r2c1", "delta-r2c2", "c3-3"],
        ]),
        g(&[&["apple-x", "c3-1"], &["cedar-r2c1", "c3-3"]]),
        g(&[&["apple-x", "c3-1"], &["cedar-r2c1", "done"]]),
    ];
    for step in steps {
        doc = edit_and_verify(&mut doc, &step);
    }
}

#[test]
fn long_lived_snapshots_take_a_cell_edit_and_a_row_insert() {
    for snapshot in revisions("table_long_lived_snapshots.json") {
        let mut doc = parse_b64(&snapshot.base64);
        let mut edited = grid_from_table_document(&doc).unwrap();
        edited[0][0] = format!("{}-ours", edited[0][0]);
        let mut reparsed = edit_and_verify(&mut doc, &edited);
        let mut with_row = grid_from_table_document(&reparsed).unwrap();
        let width = with_row[0].len();
        with_row.insert(1, (0..width).map(|i| format!("ours-{i}")).collect());
        edit_and_verify(&mut reparsed, &with_row);
    }
}

// --- delta documents are refused -------------------------------------------

fn populated_start_version() -> VectorTimestamp {
    VectorTimestamp {
        element: vec![Element {
            replica_index: Some(0),
            clock: Some(1),
            subclock: Some(0),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn real_fixtures_carry_start_version_only_as_the_present_but_empty_zero_vector() {
    let present: Vec<String> = all_fixtures()
        .into_iter()
        .filter(|(_, b64)| parse_b64(b64).document.start_version.is_some())
        .map(|(label, b64)| {
            let doc = parse_b64(&b64);
            assert_eq!(doc.document.start_version.as_ref().unwrap().element.len(), 0);
            validate_table_document_invariants(&doc).unwrap();
            label
        })
        .collect();
    assert_eq!(
        present,
        revisions("table_long_lived_snapshots.json")
            .iter()
            .map(|r| format!("table_long_lived_snapshots.json {}", r.label))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_document_carrying_a_populated_start_version_is_refused() {
    let mut doctored = parse_b64(&common::payload_base64("table_final_revision.json"));
    doctored.document.start_version = Some(populated_start_version());
    let err = validate_table_document_invariants(&doctored).unwrap_err().to_string();
    assert!(err.contains("populated startVersion"), "{err}");
    let mut reparsed = parse_table_document(&encode_table_document(&doctored).unwrap()).unwrap();
    assert_eq!(reparsed.document.start_version.as_ref().unwrap().element.len(), 1);
    let mut edited = grid_from_table_document(&reparsed).unwrap();
    edited[0][0] = "changed".into();
    let err = apply(&mut reparsed, &edited, &our_replica()).unwrap_err().to_string();
    assert!(err.contains("populated startVersion"), "{err}");
}

#[test]
fn clearing_a_doctored_start_version_restores_editability() {
    let mut doctored = parse_b64(&common::payload_base64("table_final_revision.json"));
    doctored.document.start_version = Some(populated_start_version());
    let mut reparsed = parse_table_document(&encode_table_document(&doctored).unwrap()).unwrap();
    reparsed.document.start_version = None;
    let mut edited = grid_from_table_document(&reparsed).unwrap();
    edited[0][0] = "changed".into();
    edit_and_verify(&mut reparsed, &edited);
}

// --- diffTableGrid plan shapes -----------------------------------------------

#[test]
fn diff_table_grid_plan_shapes() {
    assert_eq!(
        diff_table_grid(&g(&[&["a", "b"]]), &g(&[&["a", "b"]])),
        TableEditPlan::Noop
    );
    assert_eq!(
        diff_table_grid(&g(&[&["a", "b"], &["c", "d"]]), &g(&[&["a", "B"], &["C", "d"]])),
        TableEditPlan::CellEdits(vec![
            CellEdit {
                row: 0,
                column: 1,
                text: "B".into()
            },
            CellEdit {
                row: 1,
                column: 0,
                text: "C".into()
            },
        ])
    );
    assert_eq!(
        diff_table_grid(&g(&[&["a"], &["z"]]), &g(&[&["a"], &["m"], &["n"], &["z"]])),
        TableEditPlan::InsertRows {
            position: 1,
            rows: g(&[&["m"], &["n"]])
        }
    );
    assert_eq!(
        diff_table_grid(&g(&[&["a", "b", "c", "d"]]), &g(&[&["a", "d"]])),
        TableEditPlan::DeleteColumns { position: 1, count: 2 }
    );
    assert!(matches!(
        diff_table_grid(&g(&[&["a", "b"]]), &g(&[&["a"], &["x"]])),
        TableEditPlan::Unsupported(_)
    ));
    assert_eq!(
        diff_table_grid(
            &g(&[&["a", "b"], &["c", "d"]]),
            &g(&[&["a", "EDITED"], &["new", "row"], &["c", "d"]])
        ),
        TableEditPlan::Unsupported("row insertion/deletion couldn't be resolved to a single contiguous change".into())
    );
    assert!(matches!(
        diff_table_grid(&g(&[&["a", "b"], &["c", "d"]]), &g(&[&["c", "d"], &["a", "b"]])),
        TableEditPlan::Unsupported(_)
    ));
}

// --- applyTableEdit refusals --------------------------------------------------

#[test]
fn apply_table_edit_returns_false_for_an_unchanged_grid() {
    let mut doc = evolution(4);
    let grid = grid_from_table_document(&doc).unwrap();
    assert!(!apply(&mut doc, &grid, &our_replica()).unwrap());
}

#[test]
fn apply_table_edit_refusals_carry_icloud_mds_messages() {
    let mut doc = evolution(4);
    assert_eq!(
        apply(&mut doc, &[], &our_replica()).unwrap_err().to_string(),
        "Cannot edit a table down to no rows or no columns - delete the table from the note instead"
    );
    assert_eq!(
        apply(&mut doc, &g(&[&["a", "b"], &["c"]]), &our_replica())
            .unwrap_err()
            .to_string(),
        "Every row of an edited table must have the same number of columns"
    );
    assert_eq!(
        apply(&mut doc, &g(&[&["only", "one", "wider", "row"]]), &our_replica())
            .unwrap_err()
            .to_string(),
        "both row and column counts changed in the same edit - can't safely resolve this as one structural operation"
    );
}

#[test]
fn after_a_row_insert_the_mirror_is_one_placeholder_per_live_row() {
    let mut doc = base_3x2();
    edit_and_verify(
        &mut doc,
        &g(&[
            &["apple-r1c1-edit1", "berry-r1c2"],
            &["cedar-r2c1", "delta-r2c2"],
            &["echo-r3c1", ""],
            &["tail-1", "tail-2"],
        ]),
    );
    let m = mirror(&doc, doc.cr_rows_ref);
    assert_eq!(m.string(), "\u{fffc}".repeat(4));
    assert_eq!(
        m.attribute_run.iter().map(|r| r.len()).collect::<Vec<_>>(),
        vec![1, 1, 1, 1]
    );
    assert_eq!(
        parse_ordered_set(&doc.document, doc.cr_rows_ref)
            .unwrap()
            .array_uuid_indexes
            .len(),
        4
    );
}
