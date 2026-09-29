//! Ports icloud-md `src/notes/noteDocument.test.ts`.

use icloud_notes_sync::doc::document::{
    ApplyTextEditOptions, NoteDocument, ReplicaEntry, RunCoord, Splice, TextRun, apply_formatting_op, apply_text_edit,
    build_initial_note_document, compute_splice, compute_splices, encode_note_document, note_document_round_trips,
    parse_note_document, run_is_sentinel, validate_document_invariants,
};
use icloud_notes_sync::doc::js::{slice16, utf16_len};
use icloud_notes_sync::doc::proto::topotext::{self, AttachmentInfo, AttributeRun, ParagraphStyle};
use icloud_notes_sync::doc::proto::{Message, versioned_document};
use icloud_notes_sync::doc::text::{compress_note_document, decode_note_body_text};

const REPLICA_A: [u8; 16] = [0xaa; 16];
const REPLICA_B: [u8; 16] = [0xbb; 16];

fn run(replica: u32, clock: u32, length: u32, anchor: (u32, u32), sequence: &[u32]) -> TextRun {
    TextRun {
        coord: RunCoord { replica, clock },
        length,
        anchor: RunCoord {
            replica: anchor.0,
            clock: anchor.1,
        },
        tombstone: false,
        sequence: sequence.to_vec(),
    }
}

fn sentinel() -> TextRun {
    run(0, 0xffff_ffff, 0, (0, 0xffff_ffff), &[])
}

fn opts(replica_id: [u8; 16]) -> ApplyTextEditOptions {
    ApplyTextEditOptions { replica_id }
}

fn make_document(text: &str, runs: Vec<TextRun>, replica_clocks: &[u32]) -> NoteDocument {
    let mut all = vec![run(0, 0, 0, (0, 0), &[1])];
    all.extend(runs);
    all.push(sentinel());
    let mut replicas = vec![ReplicaEntry {
        id: REPLICA_A.to_vec(),
        counters: vec![replica_clocks.first().copied().unwrap_or(0), 1],
    }];
    for &clock in &replica_clocks[1..] {
        replicas.push(ReplicaEntry {
            id: REPLICA_B.to_vec(),
            counters: vec![clock, 1],
        });
    }
    NoteDocument {
        root_serialization_version: 0,
        version_serialization_version: 0,
        minimum_supported_version: 0,
        text: text.into(),
        runs: all,
        replicas,
        attribute_runs: vec![AttributeRun::with_length(utf16_len(text) as u32)],
    }
}

fn simple_document(text: &str) -> NoteDocument {
    let len = utf16_len(text) as u32;
    make_document(text, vec![run(1, 0, len, (1, 0), &[2])], &[len])
}

fn visible_text(doc: &NoteDocument) -> String {
    let mut position = 0usize;
    let mut out = String::new();
    for r in &doc.runs {
        if r.tombstone || r.coord.clock == 0xffff_ffff {
            continue;
        }
        out += &slice16(&doc.text, position, position + r.length as usize);
        position += r.length as usize;
    }
    out
}

fn reencode_and_decode(doc: &NoteDocument) -> String {
    decode_note_body_text(&compress_note_document(&encode_note_document(doc).unwrap())).unwrap()
}

fn splice(start: usize, delete_length: usize, insert_text: &str) -> Splice {
    Splice {
        start,
        delete_length,
        insert_text: insert_text.into(),
    }
}

fn sequences_of(doc: &NoteDocument) -> Vec<Vec<u32>> {
    doc.runs.iter().map(|r| r.sequence.clone()).collect()
}

#[test]
fn parse_encode_round_trips_a_synthetic_document_byte_for_byte() {
    let original = encode_note_document(&simple_document("Grocery list\nEggs\n")).unwrap();
    assert!(note_document_round_trips(&original));
    let reparsed = parse_note_document(&original).unwrap();
    assert_eq!(reparsed.text, "Grocery list\nEggs\n");
    assert_eq!(reparsed.runs.len(), 3);
    assert_eq!(reparsed.replicas.len(), 1);
    assert_eq!(reparsed.attribute_runs.len(), 1);
    assert_eq!(reparsed.attribute_runs[0].len(), 18);
}

#[test]
fn parsed_document_decodes_to_the_same_text_note_text_sees() {
    assert_eq!(reencode_and_decode(&simple_document("Hello\nWorld")), "Hello\nWorld");
}

#[test]
fn appending_with_our_own_replica_extends_our_trailing_run_without_adding_a_run() {
    let mut doc = simple_document("Hello");
    let before = doc.runs.len();
    assert!(apply_text_edit(&mut doc, "Hello there", &opts(REPLICA_A)).unwrap());
    assert_eq!(doc.text, "Hello there");
    assert_eq!(doc.runs.len(), before);
    assert_eq!(doc.replicas.len(), 1);
    assert_eq!(doc.replicas[0].counters[0], 11);
    assert_eq!(doc.replicas[0].counters[1], 1);
    assert_eq!(doc.attribute_runs.len(), 1);
    assert_eq!(doc.attribute_runs[0].len(), 11);
    assert_eq!(visible_text(&doc), "Hello there");
    assert_eq!(reencode_and_decode(&doc), "Hello there");
}

#[test]
fn appending_as_a_new_replica_adds_a_replica_entry_and_a_new_run() {
    let mut doc = simple_document("Hello");
    assert!(apply_text_edit(&mut doc, "Hello!", &opts(REPLICA_B)).unwrap());
    assert_eq!(doc.replicas.len(), 2);
    assert_eq!(doc.replicas[1].counters, vec![6, 1]);
    let inserted = &doc.runs[doc.runs.len() - 2];
    assert_eq!(inserted.coord, RunCoord { replica: 2, clock: 5 });
    assert_eq!(inserted.length, 1);
    assert_eq!(visible_text(&doc), "Hello!");
    assert_eq!(reencode_and_decode(&doc), "Hello!");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn mid_text_insertion_splits_the_containing_run() {
    let mut doc = simple_document("Hello world");
    assert!(apply_text_edit(&mut doc, "Hello brave world", &opts(REPLICA_B)).unwrap());
    assert_eq!(doc.text, "Hello brave world");
    assert_eq!(visible_text(&doc), "Hello brave world");
    let content: Vec<(u32, u32, u32)> = doc
        .runs
        .iter()
        .filter(|r| r.length > 0)
        .map(|r| (r.coord.replica, r.coord.clock, r.length))
        .collect();
    assert_eq!(content, vec![(1, 0, 6), (2, 11, 6), (1, 6, 5)]);
    assert_eq!(reencode_and_decode(&doc), "Hello brave world");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn deletion_tombstones_the_removed_range_instead_of_dropping_it() {
    let mut doc = simple_document("Hello brave world");
    assert!(apply_text_edit(&mut doc, "Hello world", &opts(REPLICA_B)).unwrap());
    assert_eq!(doc.text, "Hello world");
    assert_eq!(visible_text(&doc), "Hello world");
    let tombstones: Vec<&TextRun> = doc.runs.iter().filter(|r| r.tombstone).collect();
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].length, 6);
    assert_eq!(tombstones[0].coord.clock, 6);
    assert_eq!(tombstones[0].anchor, RunCoord { replica: 2, clock: 8 });
    assert_eq!(doc.replicas.len(), 2);
    assert_eq!(doc.replicas[1].counters, vec![17, 9]);
    assert_eq!(reencode_and_decode(&doc), "Hello world");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn deletion_spanning_multiple_runs_tombstones_each_covered_piece() {
    let mut doc = make_document(
        "aaabbbccc",
        vec![
            run(1, 0, 3, (1, 0), &[2]),
            run(2, 0, 3, (2, 0), &[3]),
            run(1, 3, 3, (1, 0), &[4]),
        ],
        &[6, 3],
    );
    assert!(apply_text_edit(&mut doc, "aacc", &opts(REPLICA_A)).unwrap());
    assert_eq!(visible_text(&doc), "aacc");
    let tombstoned: Vec<&TextRun> = doc.runs.iter().filter(|r| r.tombstone).collect();
    assert_eq!(tombstoned.len(), 3);
    assert_eq!(tombstoned.iter().map(|r| r.length).sum::<u32>(), 5);
    assert_eq!(reencode_and_decode(&doc), "aacc");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn replacing_text_mid_note_tombstones_the_old_range_and_inserts_at_the_same_spot() {
    let mut doc = simple_document("The quick brown fox");
    assert!(apply_text_edit(&mut doc, "The slow brown fox", &opts(REPLICA_B)).unwrap());
    assert_eq!(visible_text(&doc), "The slow brown fox");
    assert_eq!(reencode_and_decode(&doc), "The slow brown fox");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn edits_never_split_a_surrogate_pair() {
    let mut doc = simple_document("ab\u{1f600}cd");
    assert!(apply_text_edit(&mut doc, "ab\u{1f601}cd", &opts(REPLICA_B)).unwrap());
    assert_eq!(doc.text, "ab\u{1f601}cd");
    assert_eq!(visible_text(&doc), "ab\u{1f601}cd");
    assert_eq!(reencode_and_decode(&doc), "ab\u{1f601}cd");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn unchanged_text_is_a_no_op() {
    let mut doc = simple_document("same");
    let before = encode_note_document(&doc).unwrap();
    assert!(!apply_text_edit(&mut doc, "same", &opts(REPLICA_B)).unwrap());
    assert_eq!(encode_note_document(&doc).unwrap(), before);
}

#[test]
fn consecutive_pushes_from_the_same_replica_keep_extending_the_same_run() {
    let mut doc = simple_document("v1");
    apply_text_edit(&mut doc, "v1 v2", &opts(REPLICA_B)).unwrap();
    let after_first = doc.runs.len();
    apply_text_edit(&mut doc, "v1 v2 v3", &opts(REPLICA_B)).unwrap();
    assert_eq!(doc.runs.len(), after_first);
    assert_eq!(visible_text(&doc), "v1 v2 v3");
    assert_eq!(doc.replicas.len(), 2);
    assert_eq!(reencode_and_decode(&doc), "v1 v2 v3");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn a_document_missing_its_replica_clock_table_is_refused() {
    let inner = topotext::String {
        string: Some("hi".into()),
        ..Default::default()
    };
    let raw = versioned_document::Document {
        version: vec![versioned_document::Version {
            minimum_supported_version: Some(0),
            data: Some(inner.encode().unwrap()),
            ..Default::default()
        }],
        ..Default::default()
    }
    .encode()
    .unwrap();
    let err = parse_note_document(&raw).unwrap_err().to_string();
    assert!(err.contains("missing its replica clock table"), "{err}");
    assert!(!note_document_round_trips(&raw));
}

#[test]
fn a_note_with_only_known_fields_round_trips() {
    let doc = simple_document("hi");
    assert!(note_document_round_trips(&encode_note_document(&doc).unwrap()));
}

#[test]
fn invariant_validation_rejects_run_lengths_that_disagree_with_the_text() {
    let mut doc = simple_document("hello");
    doc.text = "hello!".into();
    let err = validate_document_invariants(&doc).unwrap_err().to_string();
    assert!(err.contains("do not match note text length"), "{err}");
}

#[test]
fn invariant_validation_rejects_clocks_past_the_replica_counter() {
    let mut doc = simple_document("hello");
    doc.replicas[0].counters[0] = 3;
    let err = validate_document_invariants(&doc).unwrap_err().to_string();
    assert!(err.contains("exceed replica"), "{err}");
}

#[test]
fn editing_a_linear_document_keeps_its_child_edges_a_chain() {
    let mut doc = simple_document("Hello world");
    apply_text_edit(&mut doc, "Hello brave world", &opts(REPLICA_B)).unwrap();
    let sequences: Vec<Vec<u32>> = doc
        .runs
        .iter()
        .filter(|r| r.coord.clock != 0xffff_ffff)
        .map(|r| r.sequence.clone())
        .collect();
    let chain: Vec<Vec<u32>> = (0..sequences.len()).map(|i| vec![i as u32 + 1]).collect();
    assert_eq!(sequences, chain);
}

fn branched_document() -> NoteDocument {
    make_document(
        "aaabbbccc",
        vec![
            run(1, 0, 3, (1, 0), &[2, 3]),
            run(2, 0, 3, (2, 0), &[4]),
            run(1, 3, 3, (1, 0), &[4]),
        ],
        &[6, 3],
    )
}

#[test]
fn a_deletion_inside_one_branch_preserves_the_other_branch_edges() {
    let mut doc = branched_document();
    assert!(apply_text_edit(&mut doc, "aaabbccc", &opts(REPLICA_A)).unwrap());
    assert_eq!(visible_text(&doc), "aaabbccc");
    assert_eq!(reencode_and_decode(&doc), "aaabbccc");
    validate_document_invariants(&doc).unwrap();
    assert_eq!(
        sequences_of(&doc),
        vec![vec![1], vec![2, 4], vec![3], vec![5], vec![5], vec![]]
    );
    assert_eq!(
        doc.runs.iter().map(|r| r.tombstone).collect::<Vec<_>>(),
        vec![false, false, false, true, false, false]
    );
    assert_eq!(doc.runs[3].anchor, RunCoord { replica: 1, clock: 8 });
}

#[test]
fn an_insert_between_branches_follows_apples_edge_splice_rule() {
    let mut doc = branched_document();
    assert!(apply_text_edit(&mut doc, "aaabbbXXccc", &opts(REPLICA_A)).unwrap());
    assert_eq!(visible_text(&doc), "aaabbbXXccc");
    assert_eq!(reencode_and_decode(&doc), "aaabbbXXccc");
    validate_document_invariants(&doc).unwrap();
    assert_eq!(
        sequences_of(&doc),
        vec![vec![1], vec![2, 4], vec![3], vec![5], vec![5], vec![]]
    );
    assert_eq!(doc.runs[3].coord, RunCoord { replica: 1, clock: 6 });
    assert_eq!(doc.runs[3].length, 2);
}

#[test]
fn a_formatting_op_that_splits_a_branch_node_keeps_both_branch_edges_on_the_tail_piece() {
    let mut doc = branched_document();
    apply_formatting_op(&mut doc, &[(1, 2)], &REPLICA_A).unwrap();
    validate_document_invariants(&doc).unwrap();
    assert_eq!(
        sequences_of(&doc),
        vec![vec![1], vec![2], vec![3], vec![4, 5], vec![6], vec![6], vec![]]
    );
    assert_eq!(doc.runs[2].anchor, RunCoord { replica: 1, clock: 1 });
    assert_eq!(reencode_and_decode(&doc), "aaabbbccc");
}

#[test]
fn validate_document_invariants_rejects_graphs_the_surgery_could_never_produce() {
    let mut backward = simple_document("hello");
    backward.runs[1].sequence = vec![0];
    assert!(
        validate_document_invariants(&backward)
            .unwrap_err()
            .to_string()
            .contains("child edge to 0")
    );

    let mut out_of_range = simple_document("hello");
    out_of_range.runs[1].sequence = vec![9];
    assert!(
        validate_document_invariants(&out_of_range)
            .unwrap_err()
            .to_string()
            .contains("child edge to 9")
    );

    let mut dangling = simple_document("hello");
    dangling.runs[1].sequence = vec![];
    assert!(
        validate_document_invariants(&dangling)
            .unwrap_err()
            .to_string()
            .contains("no child edge")
    );
}

#[test]
fn structural_edits_advance_the_event_counter_and_the_sentinel_never_gets_a_sequence() {
    let mut doc = simple_document("Hello brave world");
    assert_eq!(doc.replicas[0].counters[1], 1);
    apply_text_edit(&mut doc, "Hello world", &opts(REPLICA_A)).unwrap();
    assert_eq!(doc.replicas[0].counters[1], 9);
    assert!(doc.runs.last().unwrap().sequence.is_empty());
}

#[test]
fn compute_splice_finds_minimal_edits() {
    assert_eq!(compute_splice("abc", "abXc"), splice(2, 0, "X"));
    assert_eq!(compute_splice("abc", "ac"), splice(1, 1, ""));
    assert_eq!(compute_splice("abc", "aXc"), splice(1, 1, "X"));
    assert_eq!(compute_splice("abc", "abc def"), splice(3, 0, " def"));
    assert_eq!(compute_splice("", "new"), splice(0, 0, "new"));
}

#[test]
fn compute_splices_keeps_separated_edits_as_separate_hunks() {
    assert_eq!(compute_splices("same", "same"), vec![]);
    assert_eq!(compute_splices("abc def", "abc XX def"), vec![splice(4, 0, "XX ")]);
    assert_eq!(
        compute_splices("one\ntwo\nthree\n", "one EDIT\ntwo\nthree, EDITED\n"),
        vec![splice(3, 0, " EDIT"), splice(13, 0, ", EDITED")]
    );
}

#[test]
fn compute_splices_mid_document_insert_plus_trailing_newline_stays_two_small_hunks() {
    let remote = "p2 bravo dev-E1\n\np3 charlie\ntyped-on-device tail";
    let local = "p2 bravo EDIT-E1 dev-E1\n\np3 charlie\ntyped-on-device tail\n";
    assert_eq!(
        compute_splices(remote, local),
        vec![splice(9, 0, "EDIT-E1 "), splice(utf16_len(remote), 0, "\n")]
    );
}

#[test]
fn a_multi_hunk_edit_never_re_authors_another_replicas_text_between_the_hunks() {
    let mut doc = make_document(
        "alpha\n\nmid\nbravo-device",
        vec![run(1, 0, 11, (1, 0), &[2]), run(2, 0, 12, (2, 0), &[3])],
        &[11, 12],
    );
    assert!(apply_text_edit(&mut doc, "alpha EDIT\n\nmid\nbravo-device\n", &opts(REPLICA_A)).unwrap());
    let foreign: Vec<&TextRun> = doc.runs.iter().filter(|r| r.coord.replica == 2).collect();
    assert_eq!(foreign.len(), 1);
    assert_eq!(foreign[0].coord, RunCoord { replica: 2, clock: 0 });
    assert_eq!(foreign[0].length, 12);
    assert_eq!(foreign[0].anchor, RunCoord { replica: 2, clock: 0 });
    assert!(!foreign[0].tombstone);
    assert!(!doc.runs.iter().any(|r| r.tombstone));
    let inserted: u32 = doc
        .runs
        .iter()
        .filter(|r| r.coord.replica == 1 && r.coord.clock >= 11)
        .map(|r| r.length)
        .sum();
    assert_eq!(inserted, 6);
    assert_eq!(visible_text(&doc), "alpha EDIT\n\nmid\nbravo-device\n");
    assert_eq!(reencode_and_decode(&doc), "alpha EDIT\n\nmid\nbravo-device\n");
}

#[test]
fn multi_hunk_deletions_consume_a_single_formatting_op() {
    let mut doc = simple_document("aa bb\nmid\ncc dd\n");
    assert!(apply_text_edit(&mut doc, "aa\nmid\ncc\n", &opts(REPLICA_A)).unwrap());
    let tombstones: Vec<&TextRun> = doc.runs.iter().filter(|r| r.tombstone).collect();
    assert_eq!(tombstones.len(), 2);
    for t in tombstones {
        assert_eq!(t.anchor, RunCoord { replica: 1, clock: 8 });
    }
    assert_eq!(doc.replicas[0].counters[1], 9);
    assert_eq!(visible_text(&doc), "aa\nmid\ncc\n");
}

fn restyled_document() -> NoteDocument {
    let mut doc = make_document(
        "Hellobrave ",
        vec![run(1, 0, 5, (1, 0), &[2]), run(2, 0, 6, (2, 80), &[3])],
        &[5, 6],
    );
    doc.replicas[0].counters[1] = 5;
    doc.replicas[1].counters[1] = 81;
    doc
}

#[test]
fn deleting_text_another_replica_restyled_stamps_past_that_replicas_higher_style_clock() {
    let mut doc = restyled_document();
    assert!(apply_text_edit(&mut doc, "Hello", &opts(REPLICA_A)).unwrap());
    let tombstones: Vec<&TextRun> = doc.runs.iter().filter(|r| r.tombstone).collect();
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].anchor, RunCoord { replica: 1, clock: 88 });
    assert_eq!(doc.replicas[0].counters[1], 89);
    assert_eq!(visible_text(&doc), "Hello");
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn a_formatting_op_restamps_past_another_replicas_higher_style_clock() {
    let mut doc = restyled_document();
    apply_formatting_op(&mut doc, &[(5, 11)], &REPLICA_A).unwrap();
    let restamped = doc.runs.iter().find(|r| r.coord.replica == 2).unwrap();
    assert_eq!(restamped.anchor, RunCoord { replica: 1, clock: 81 });
    assert_eq!(doc.replicas[0].counters[1], 82);
    validate_document_invariants(&doc).unwrap();
}

#[test]
fn build_initial_note_document_builds_a_first_save_document() {
    let doc = build_initial_note_document("Grocery list\nEggs\nMilk\n", &[7; 16]).unwrap();
    validate_document_invariants(&doc).unwrap();
    assert_eq!(doc.text, "Grocery list\nEggs\nMilk\n");
    assert_eq!(doc.replicas.len(), 1);
    assert_eq!(doc.replicas[0].counters, vec![utf16_len(&doc.text) as u32, 1]);
    assert_eq!(doc.runs.len(), 3);
    assert_eq!(doc.runs[1].coord.replica, 1);
    assert_eq!(doc.runs[1].length as usize, utf16_len(&doc.text));
    assert!(run_is_sentinel(&doc.runs[2]));
    let reparsed = parse_note_document(&encode_note_document(&doc).unwrap()).unwrap();
    assert_eq!(reparsed.text, "Grocery list\nEggs\nMilk\n");
}

#[test]
fn build_initial_note_document_output_survives_the_round_trip_gate() {
    let doc = build_initial_note_document("One line\n", &[3; 16]).unwrap();
    assert!(note_document_round_trips(&encode_note_document(&doc).unwrap()));
}

#[test]
fn build_initial_note_document_refuses_empty_text() {
    let err = build_initial_note_document("", &[0; 16]).unwrap_err().to_string();
    assert!(err.contains("refusing to create an empty document"), "{err}");
}

#[test]
fn a_built_document_accepts_a_follow_up_apply_text_edit() {
    let replica = [9u8; 16];
    let mut doc = build_initial_note_document("Title\nBody\n", &replica).unwrap();
    assert!(apply_text_edit(&mut doc, "Title\nBody with more\n", &opts(replica)).unwrap());
    validate_document_invariants(&doc).unwrap();
    assert_eq!(
        parse_note_document(&encode_note_document(&doc).unwrap()).unwrap().text,
        "Title\nBody with more\n"
    );
}

// --- the attachmentInfo-run insertion guard -----------------------------------

fn attachment_run(id: &str, uti: &str, style: Option<u32>) -> AttributeRun {
    AttributeRun {
        length: Some(1),
        paragraph_style: style.map(|s| ParagraphStyle {
            style: Some(s),
            ..Default::default()
        }),
        attachment_info: Some(AttachmentInfo {
            attachment_identifier: Some(id.into()),
            type_uti: Some(uti.into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn inserting_right_after_an_embed_never_grows_its_attachment_info_run() {
    let mut doc = simple_document("a\u{fffc}b");
    doc.attribute_runs = vec![
        AttributeRun::with_length(1),
        attachment_run("A-1", "public.jpeg", Some(3)),
        AttributeRun::with_length(1),
    ];
    assert!(apply_text_edit(&mut doc, "a\u{fffc}Xb", &opts(REPLICA_A)).unwrap());
    validate_document_invariants(&doc).unwrap();
    assert_eq!(doc.attribute_runs.len(), 4);
    assert_eq!(doc.attribute_runs[1].len(), 1);
    assert_eq!(
        doc.attribute_runs[1]
            .attachment_info
            .as_ref()
            .unwrap()
            .attachment_identifier
            .as_deref(),
        Some("A-1")
    );
    assert_eq!(doc.attribute_runs[2].len(), 1);
    assert!(doc.attribute_runs[2].attachment_info.is_none());
    assert_eq!(doc.attribute_runs[2].paragraph_style.as_ref().unwrap().style, Some(3));
    assert_eq!(reencode_and_decode(&doc), "a\u{fffc}Xb");
}

#[test]
fn inserting_at_position_0_before_a_leading_embed_keeps_its_run_first_class() {
    let mut doc = simple_document("\u{fffc}b");
    doc.attribute_runs = vec![
        attachment_run("A-2", "com.apple.notes.gallery", None),
        AttributeRun::with_length(1),
    ];
    assert!(apply_text_edit(&mut doc, "X\u{fffc}b", &opts(REPLICA_A)).unwrap());
    validate_document_invariants(&doc).unwrap();
    assert_eq!(doc.attribute_runs.len(), 3);
    assert_eq!(doc.attribute_runs[0].len(), 1);
    assert!(doc.attribute_runs[0].attachment_info.is_none());
    assert_eq!(doc.attribute_runs[1].len(), 1);
    assert_eq!(
        doc.attribute_runs[1]
            .attachment_info
            .as_ref()
            .unwrap()
            .attachment_identifier
            .as_deref(),
        Some("A-2")
    );
    assert_eq!(reencode_and_decode(&doc), "X\u{fffc}b");
}

#[test]
fn appending_after_a_trailing_embed_grows_a_fresh_run() {
    let mut doc = simple_document("a\u{fffc}");
    doc.attribute_runs = vec![
        AttributeRun::with_length(1),
        attachment_run("A-3", "com.apple.paper", None),
    ];
    assert!(apply_text_edit(&mut doc, "a\u{fffc} tail", &opts(REPLICA_A)).unwrap());
    validate_document_invariants(&doc).unwrap();
    assert_eq!(doc.attribute_runs[1].len(), 1);
    assert_eq!(
        doc.attribute_runs[1]
            .attachment_info
            .as_ref()
            .unwrap()
            .attachment_identifier
            .as_deref(),
        Some("A-3")
    );
    assert_eq!(doc.attribute_runs[2].len(), 5);
    assert!(doc.attribute_runs[2].attachment_info.is_none());
    assert_eq!(reencode_and_decode(&doc), "a\u{fffc} tail");
}
