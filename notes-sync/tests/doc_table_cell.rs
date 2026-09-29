//! Ports icloud-md `src/notes/tableCellEdit.test.ts`.

use icloud_notes_sync::doc::document::{RunCoord, TextRun};
use icloud_notes_sync::doc::js::base64_decode;
use icloud_notes_sync::doc::proto::Message;
use icloud_notes_sync::doc::proto::topotext::{self, AttributeRun};
use icloud_notes_sync::doc::table_edit::{
    TableCellDocument, TopotextClockSource, apply_cell_text_edit, encode_cell_document, new_cell_document,
    parse_cell_document, validate_cell_invariants,
};

const REAL_MULTI_TOMBSTONE_A1_CELL: &str = "EgJBMRoQCgQIABAAEAAaBAgAEAAoARoQCgQIARAuEAEaBAgBEAAoAhoSCgQIARAvEAEaBAgBEAAgASgDGhIKBAgBEAIQAhoECAEQCSABKAQaEgoECAEQMBADGgQIARAAIAEoBRoQCgQIARAzEAEaBAgBEAAoBhoWCggIABD/////DxAAGggIABD/////DyoCCAI=";

fn load_real_cell() -> TableCellDocument {
    parse_cell_document(&topotext::String::decode(&base64_decode(REAL_MULTI_TOMBSTONE_A1_CELL)).unwrap()).unwrap()
}

/// The test clock of the TS suite: replica 1, text clock from `start`, style
/// clock from `style_start` with a per-save floor.
struct TestClock {
    counter: u32,
    style: u32,
    floor: u32,
}

impl TestClock {
    fn new(start: u32, style_start: u32) -> Self {
        TestClock {
            counter: start,
            style: style_start,
            floor: style_start,
        }
    }
}

impl TopotextClockSource for TestClock {
    fn replica_index(&self) -> u32 {
        1
    }
    fn take(&mut self, units: u32) -> u32 {
        let value = self.counter;
        self.counter += units;
        value
    }
    fn take_tombstone_anchor(&mut self, previous_clock: u32) -> icloud_notes_sync::doc::Result<u32> {
        let assigned = (previous_clock + 8).max(self.floor);
        self.style = self.style.max(assigned + 1);
        Ok(assigned)
    }
}

fn filled_cell(text: &str, clock: &mut TestClock) -> TableCellDocument {
    let mut cell = new_cell_document();
    if !text.is_empty() {
        apply_cell_text_edit(&mut cell, text, clock).unwrap();
    }
    cell
}

fn run(replica: u32, clock: u32, length: u32, anchor: u32, sequence: &[u32]) -> TextRun {
    TextRun {
        coord: RunCoord { replica, clock },
        length,
        anchor: RunCoord { replica, clock: anchor },
        tombstone: false,
        sequence: sequence.to_vec(),
    }
}

fn sentinel() -> TextRun {
    run(0, 0xffff_ffff, 0, 0xffff_ffff, &[])
}

#[test]
fn parse_encode_cell_round_trips_the_real_multi_tombstone_cell_byte_for_byte() {
    let raw = base64_decode(REAL_MULTI_TOMBSTONE_A1_CELL);
    let cell = parse_cell_document(&topotext::String::decode(&raw).unwrap()).unwrap();
    assert_eq!(cell.text, "A1");
    assert_eq!(cell.runs.len(), 7);
    assert_eq!(cell.runs.iter().filter(|r| r.tombstone).count(), 3);
    assert_eq!(encode_cell_document(&cell).encode().unwrap(), raw);
}

#[test]
fn new_cell_document_matches_the_captured_brand_new_empty_cell_shape() {
    let cell = new_cell_document();
    assert_eq!(cell.text, "");
    assert_eq!(cell.runs.len(), 2);
    assert_eq!(cell.runs[0].sequence, vec![1]);
    assert_eq!(cell.runs[0].length, 0);
    assert_eq!(cell.runs[1].coord.clock, 0xffff_ffff);
    assert!(cell.attribute_runs.is_empty());
    validate_cell_invariants(&cell).unwrap();
}

#[test]
fn first_fill_matches_the_captured_freshly_filled_cell_shape() {
    let mut clock = TestClock::new(13, 1);
    let cell = filled_cell("hello", &mut clock);
    assert_eq!(cell.text, "hello");
    assert_eq!(cell.runs.len(), 3);
    assert_eq!(cell.runs[1].length, 5);
    assert_eq!(cell.runs[1].coord, RunCoord { replica: 1, clock: 13 });
    assert_eq!(cell.runs[1].anchor, RunCoord { replica: 1, clock: 0 });
    assert_eq!(cell.attribute_runs.iter().map(|r| r.len()).collect::<Vec<_>>(), vec![5]);
    assert_eq!(clock.counter, 18);
    validate_cell_invariants(&cell).unwrap();
}

#[test]
fn clocks_are_drawn_from_the_shared_document_global_sequence() {
    let mut clock = TestClock::new(0, 1);
    let first = filled_cell("abcde", &mut clock);
    let second = filled_cell("xyz", &mut clock);
    assert_eq!(first.runs[1].coord.clock, 0);
    assert_eq!(second.runs[1].coord.clock, 5);
    assert_eq!(clock.counter, 8);
}

#[test]
fn apply_cell_text_edit_returns_false_and_changes_nothing_when_the_text_is_unchanged() {
    let mut clock = TestClock::new(0, 1);
    let mut cell = filled_cell("same", &mut clock);
    let before = cell.clone();
    let counter = clock.counter;
    assert!(!apply_cell_text_edit(&mut cell, "same", &mut clock).unwrap());
    assert_eq!(cell, before);
    assert_eq!(clock.counter, counter);
}

#[test]
fn apply_cell_text_edit_gives_a_brand_new_empty_cell_its_first_text() {
    let mut cell = new_cell_document();
    assert!(apply_cell_text_edit(&mut cell, "hi", &mut TestClock::new(0, 1)).unwrap());
    assert_eq!(cell.text, "hi");
    validate_cell_invariants(&cell).unwrap();
}

#[test]
fn apply_cell_text_edit_splits_a_run_when_editing_in_the_middle() {
    let mut clock = TestClock::new(0, 1);
    let mut cell = filled_cell("abcdef", &mut clock);
    apply_cell_text_edit(&mut cell, "abcXYZdef", &mut clock).unwrap();
    assert_eq!(cell.text, "abcXYZdef");
    validate_cell_invariants(&cell).unwrap();
}

#[test]
fn apply_cell_text_edit_fully_deletes_a_cells_text_without_touching_the_text_clock() {
    let mut clock = TestClock::new(0, 1);
    let mut cell = filled_cell("gone", &mut clock);
    let counter = clock.counter;
    apply_cell_text_edit(&mut cell, "", &mut clock).unwrap();
    assert_eq!(cell.text, "");
    validate_cell_invariants(&cell).unwrap();
    assert!(cell.runs.iter().any(|r| r.tombstone));
    assert_eq!(clock.counter, counter);
}

#[test]
fn real_multi_tombstone_cell_appending_preserves_prior_tombstone_history() {
    let mut cell = load_real_cell();
    apply_cell_text_edit(&mut cell, "A1-edited", &mut TestClock::new(100, 1)).unwrap();
    assert_eq!(cell.text, "A1-edited");
    validate_cell_invariants(&cell).unwrap();
    assert_eq!(cell.runs.iter().filter(|r| r.tombstone).count(), 3);
}

#[test]
fn real_multi_tombstone_cell_deleting_everything_tombstones_the_visible_runs_too() {
    let mut cell = load_real_cell();
    apply_cell_text_edit(&mut cell, "", &mut TestClock::new(100, 1)).unwrap();
    assert_eq!(cell.text, "");
    validate_cell_invariants(&cell).unwrap();
    assert!(cell.runs.iter().filter(|r| r.tombstone).count() > 3);
}

#[test]
fn real_multi_tombstone_cell_mid_text_edit_splits_without_disturbing_tombstones() {
    let mut cell = load_real_cell();
    apply_cell_text_edit(&mut cell, "AX1", &mut TestClock::new(100, 1)).unwrap();
    assert_eq!(cell.text, "AX1");
    validate_cell_invariants(&cell).unwrap();
    assert_eq!(cell.runs.iter().filter(|r| r.tombstone).count(), 3);
}

#[test]
fn editing_a_linear_cell_keeps_its_child_edges_a_chain() {
    let mut clock = TestClock::new(0, 1);
    let mut cell = filled_cell("ab", &mut clock);
    apply_cell_text_edit(&mut cell, "abc", &mut clock).unwrap();
    let firsts: Vec<u32> = cell
        .runs
        .iter()
        .filter(|r| r.coord.clock != 0xffff_ffff)
        .map(|r| r.sequence[0])
        .collect();
    let chain: Vec<u32> = (1..=firsts.len() as u32).collect();
    assert_eq!(firsts, chain);
}

#[test]
fn editing_a_branched_cell_preserves_the_branch_edges() {
    let mut cell = TableCellDocument {
        text: "aabbcc".into(),
        runs: vec![
            run(0, 0, 0, 0, &[1]),
            run(1, 0, 2, 0, &[2, 3]),
            run(2, 0, 2, 0, &[4]),
            run(1, 2, 2, 0, &[4]),
            sentinel(),
        ],
        attribute_runs: vec![AttributeRun::with_length(6)],
    };
    assert!(apply_cell_text_edit(&mut cell, "z", &mut TestClock::new(100, 1)).unwrap());
    validate_cell_invariants(&cell).unwrap();
    assert_eq!(cell.text, "z");
    assert_eq!(
        cell.runs.iter().map(|r| r.sequence.clone()).collect::<Vec<_>>(),
        vec![vec![1], vec![2, 3], vec![5], vec![4], vec![5], vec![]]
    );
    assert_eq!(
        cell.runs.iter().map(|r| r.tombstone).collect::<Vec<_>>(),
        vec![false, true, true, true, false, false]
    );
}

#[test]
fn deleting_cell_text_restamps_the_tombstone_with_apples_deletion_bias() {
    let mut clock = TestClock::new(100, 1);
    let mut cell = filled_cell("hello world", &mut clock);
    apply_cell_text_edit(&mut cell, "hello", &mut clock).unwrap();
    let tombstoned: Vec<&TextRun> = cell.runs.iter().filter(|r| r.tombstone).collect();
    assert_eq!(tombstoned.len(), 1);
    assert_eq!(tombstoned[0].anchor, RunCoord { replica: 1, clock: 8 });
    assert_eq!(clock.style, 9);
    validate_cell_invariants(&cell).unwrap();
}

#[test]
fn a_deletion_out_ranks_a_high_pre_existing_anchor() {
    let mut cell = TableCellDocument {
        text: "abc".into(),
        runs: vec![run(0, 0, 0, 0, &[1]), run(2, 0, 3, 20, &[2]), sentinel()],
        attribute_runs: vec![AttributeRun::with_length(3)],
    };
    let mut clock = TestClock::new(100, 1);
    apply_cell_text_edit(&mut cell, "", &mut clock).unwrap();
    assert_eq!(cell.runs[1].anchor, RunCoord { replica: 1, clock: 28 });
    assert!(cell.runs[1].tombstone);
    assert_eq!(clock.style, 29);
}

#[test]
fn two_runs_tombstoned_in_one_save_share_the_saves_style_clock_floor() {
    let mut cell = TableCellDocument {
        text: "aabb".into(),
        runs: vec![
            run(0, 0, 0, 0, &[1]),
            run(2, 0, 2, 0, &[2]),
            run(2, 2, 2, 0, &[3]),
            sentinel(),
        ],
        attribute_runs: vec![AttributeRun::with_length(4)],
    };
    let mut clock = TestClock::new(100, 1);
    apply_cell_text_edit(&mut cell, "", &mut clock).unwrap();
    assert_eq!(
        cell.runs
            .iter()
            .filter(|r| r.tombstone)
            .map(|r| r.anchor)
            .collect::<Vec<_>>(),
        vec![RunCoord { replica: 1, clock: 8 }, RunCoord { replica: 1, clock: 8 }]
    );
    assert_eq!(clock.style, 9);
}

#[test]
fn validate_cell_invariants_rejects_visible_run_lengths_that_dont_match_the_text() {
    let mut cell = filled_cell("hello", &mut TestClock::new(0, 1));
    cell.text = "hello world".into();
    let err = validate_cell_invariants(&cell).unwrap_err().to_string();
    assert!(err.contains("visible run lengths"), "{err}");
}

#[test]
fn validate_cell_invariants_rejects_attribute_run_lengths_that_dont_match_the_text() {
    let mut cell = filled_cell("hello", &mut TestClock::new(0, 1));
    cell.attribute_runs.clear();
    let err = validate_cell_invariants(&cell).unwrap_err().to_string();
    assert!(err.contains("attribute run lengths"), "{err}");
}
