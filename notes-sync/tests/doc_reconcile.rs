//! Ports icloud-md `src/notes/formatReconcile.test.ts`. The desired
//! paragraphs are icloud-md's own `parseNoteMarkdown` output for each test's
//! markdown (`tests/doc_node/parsed_markdown.json`), so this doesn't depend
//! on the Markdown parser.

mod common;

use icloud_notes_sync::doc::document::{
    ApplyTextEditOptions, NoteDocument, ReplicaEntry, RunCoord, TextRun, apply_text_edit, build_initial_note_document,
};
use icloud_notes_sync::doc::format::{FormatParagraph, ParagraphKind, decode_note_format};
use icloud_notes_sync::doc::proto::topotext::{AttachmentInfo, AttributeRun, Color, ParagraphStyle, Todo};
use icloud_notes_sync::doc::reconcile::{ReconcileResult, reconcile_note_format_with};
use icloud_notes_sync::js::len16;

const REPLICA_A: [u8; 16] = [0xaa; 16];
const REPLICA_B: [u8; 16] = [0xbb; 16];
const TODO_UUID: [u8; 16] = [0x77; 16];

fn run(replica: u32, clock: u32, length: u32, sequence: &[u32]) -> TextRun {
    TextRun {
        coord: RunCoord { replica, clock },
        length,
        anchor: RunCoord {
            replica,
            clock: if clock == 0xffff_ffff { clock } else { 0 },
        },
        tombstone: false,
        sequence: sequence.to_vec(),
    }
}

fn doc_with(text: &str, attribute_runs: Vec<AttributeRun>) -> NoteDocument {
    let len = len16(text) as u32;
    NoteDocument {
        root_serialization_version: 0,
        version_serialization_version: 0,
        minimum_supported_version: 0,
        text: text.into(),
        runs: vec![run(0, 0, 0, &[1]), run(1, 0, len, &[2]), run(0, 0xffff_ffff, 0, &[])],
        replicas: vec![ReplicaEntry {
            id: REPLICA_A.to_vec(),
            counters: vec![len, 3],
        }],
        attribute_runs,
    }
}

fn desired(markdown: &str) -> Vec<FormatParagraph> {
    common::parsed_markdown(markdown).1
}

fn reconcile(doc: &mut NoteDocument, markdown: &str, replica: &[u8; 16]) -> ReconcileResult {
    reconcile_note_format_with(doc, &desired(markdown), replica, &mut common::counting_uuids()).unwrap()
}

fn plain(length: u32) -> AttributeRun {
    AttributeRun::with_length(length)
}

fn with_style(length: u32, style: ParagraphStyle) -> AttributeRun {
    AttributeRun {
        length: Some(length),
        paragraph_style: Some(style),
        ..Default::default()
    }
}

fn todo_style(done: u32) -> ParagraphStyle {
    ParagraphStyle {
        style: Some(103),
        alignment: Some(4),
        todo: Some(Todo {
            todo_uuid: Some(TODO_UUID.to_vec()),
            done: Some(done),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn style(style: u32) -> ParagraphStyle {
    ParagraphStyle {
        style: Some(style),
        alignment: Some(4),
        ..Default::default()
    }
}

fn content_anchors(doc: &NoteDocument) -> Vec<(u32, RunCoord)> {
    doc.runs
        .iter()
        .filter(|r| r.length > 0 && !r.tombstone)
        .map(|r| (r.length, r.anchor))
        .collect()
}

#[test]
fn matching_formatting_is_a_no_op_that_leaves_the_document_untouched() {
    let mut doc = doc_with("plain line", vec![plain(10)]);
    let before = doc.clone();
    assert_eq!(reconcile(&mut doc, "plain line", &REPLICA_A), Ok(false));
    assert_eq!(doc, before);
}

#[test]
fn a_checklist_done_toggle_keeps_the_todo_uuid_and_bumps_only_the_op_clock() {
    let mut doc = doc_with("buy milk", vec![with_style(8, todo_style(0))]);
    assert_eq!(reconcile(&mut doc, "- [x] buy milk", &REPLICA_A), Ok(true));
    assert_eq!(doc.attribute_runs.len(), 1);
    let todo = doc.attribute_runs[0]
        .paragraph_style
        .as_ref()
        .unwrap()
        .todo
        .as_ref()
        .unwrap();
    assert_eq!(todo.done, Some(1));
    assert_eq!(todo.todo_uuid.as_deref(), Some(&TODO_UUID[..]));
    assert_eq!(doc.replicas[0].counters, vec![8, 4]);
    assert_eq!(content_anchors(&doc)[0].1, RunCoord { replica: 1, clock: 3 });
}

#[test]
fn styling_a_paragraph_clones_the_underlying_run_so_opaque_fields_ride_along() {
    let color = Color {
        red: Some(1f32.to_bits()),
        green: Some(0f32.to_bits()),
        blue: Some(0f32.to_bits()),
        alpha: Some(1f32.to_bits()),
        ..Default::default()
    };
    let mut doc = doc_with(
        "make me a heading",
        vec![AttributeRun {
            length: Some(17),
            color: Some(color.clone()),
            timestamp: Some(42),
            ..Default::default()
        }],
    );
    assert_eq!(reconcile(&mut doc, "## make me a heading", &REPLICA_A), Ok(true));
    let r = &doc.attribute_runs[0];
    assert_eq!(doc.attribute_runs.len(), 1);
    assert_eq!(r.paragraph_style.as_ref().unwrap().style, Some(1));
    assert_eq!(r.paragraph_style.as_ref().unwrap().alignment, Some(4));
    assert_eq!(r.color, Some(color));
    assert_eq!(r.timestamp, Some(42));
}

#[test]
fn bolding_a_word_splits_runs_writes_font_hints_and_font_and_restamps_only_that_paragraph() {
    let mut doc = doc_with("first line\nbold me here", vec![plain(23)]);
    assert_eq!(
        reconcile(&mut doc, "first line\n**bold** me here", &REPLICA_A),
        Ok(true)
    );
    let shape: Vec<(u32, u32, Option<String>)> = doc
        .attribute_runs
        .iter()
        .map(|r| {
            (
                r.len(),
                r.font_hints.unwrap_or(0),
                r.font.as_ref().and_then(|f| f.name.clone()),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![(11, 0, None), (4, 1, Some("SFUIText-Bold".into())), (8, 0, None)]
    );
    assert!(doc.attribute_runs[1].font_hints.is_some());
    assert_eq!(
        content_anchors(&doc),
        vec![
            (11, RunCoord { replica: 1, clock: 0 }),
            (12, RunCoord { replica: 1, clock: 3 })
        ]
    );
}

#[test]
fn a_dash_list_paragraph_edited_only_elsewhere_keeps_its_101_style_verbatim() {
    let mut doc = doc_with("dash item\nplain", vec![with_style(10, style(101)), plain(5)]);
    assert_eq!(reconcile(&mut doc, "- dash item\n## plain", &REPLICA_A), Ok(true));
    assert_eq!(doc.attribute_runs[0].paragraph_style.as_ref().unwrap().style, Some(101));
    assert_eq!(doc.attribute_runs[1].paragraph_style.as_ref().unwrap().style, Some(1));
}

#[test]
fn a_new_checklist_paragraph_gets_a_fresh_web_client_shape_style_with_a_minted_uuid() {
    let mut doc = doc_with("todo item", vec![plain(9)]);
    assert_eq!(reconcile(&mut doc, "- [ ] todo item", &REPLICA_A), Ok(true));
    let ps = doc.attribute_runs[0].paragraph_style.as_ref().unwrap();
    assert_eq!(ps.style, Some(103));
    assert_eq!(ps.alignment, Some(4));
    assert_eq!(ps.todo.as_ref().unwrap().done, Some(0));
    assert_eq!(ps.todo.as_ref().unwrap().todo_uuid.as_ref().unwrap().len(), 16);
    assert_eq!(ps.uuid.as_ref().unwrap().len(), 0);
}

#[test]
fn an_attachment_info_run_keeps_its_linkage_through_a_paragraph_style_change() {
    let mut doc = doc_with(
        "a\n\u{fffc}",
        vec![
            plain(2),
            AttributeRun {
                length: Some(1),
                attachment_info: Some(AttachmentInfo {
                    attachment_identifier: Some("A-1".into()),
                    type_uti: Some("public.jpeg".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
    );
    assert_eq!(reconcile(&mut doc, "> a\n> \u{fffc}", &REPLICA_A), Ok(true));
    let attachment = doc.attribute_runs.iter().find(|r| r.attachment_info.is_some()).unwrap();
    assert_eq!(attachment.len(), 1);
    assert_eq!(
        attachment
            .attachment_info
            .as_ref()
            .unwrap()
            .attachment_identifier
            .as_deref(),
        Some("A-1")
    );
    assert_eq!(attachment.paragraph_style.as_ref().unwrap().block_quote_level, Some(1));
}

#[test]
fn a_different_replica_reconciling_joins_the_table_at_the_observed_clock_maxima() {
    let mut doc = doc_with("check me", vec![with_style(8, todo_style(0))]);
    assert_eq!(reconcile(&mut doc, "- [x] check me", &REPLICA_B), Ok(true));
    assert_eq!(doc.replicas[1].counters, vec![8, 4]);
    assert_eq!(content_anchors(&doc)[0].1, RunCoord { replica: 2, clock: 3 });
}

#[test]
fn create_path_flow_text_edit_into_an_empty_skeleton_then_formatting_reconcile() {
    let markdown = "# Title\n\n- [ ] first todo\n- [x] second";
    let (text, paragraphs) = common::parsed_markdown(markdown);
    let mut doc = build_initial_note_document(&text, &REPLICA_A).unwrap();
    assert_eq!(
        reconcile_note_format_with(&mut doc, &paragraphs, &REPLICA_A, &mut common::counting_uuids()).unwrap(),
        Ok(true)
    );
    let decoded = decode_note_format(&doc.text, &doc.attribute_runs).unwrap();
    use ParagraphKind::*;
    assert_eq!(
        decoded.iter().map(|p| (p.kind, p.done)).collect::<Vec<_>>(),
        vec![
            (Title, None),
            (Body, None),
            (TodoList, Some(false)),
            (TodoList, Some(true))
        ]
    );
}

#[test]
fn formatting_only_reconcile_after_a_text_edit_composes_with_apply_text_edit() {
    let mut doc = doc_with("old text", vec![plain(8)]);
    let (text, _) = common::parsed_markdown("## new heading");
    assert!(apply_text_edit(&mut doc, &text, &ApplyTextEditOptions { replica_id: REPLICA_A }).unwrap());
    assert_eq!(reconcile(&mut doc, "## new heading", &REPLICA_A), Ok(true));
    let decoded = decode_note_format(&doc.text, &doc.attribute_runs).unwrap();
    assert_eq!(decoded[0].kind, ParagraphKind::Heading);
}

#[test]
fn two_checklist_items_sharing_an_inherited_todo_uuid_get_the_later_one_re_minted() {
    let mut doc = doc_with(
        "todo two\nstep2 verify line",
        vec![with_style(9, todo_style(0)), with_style(17, todo_style(0))],
    );
    assert_eq!(
        reconcile(&mut doc, "- [ ] todo two\n- [ ] step2 verify line", &REPLICA_A),
        Ok(true)
    );
    let first = doc.attribute_runs[0]
        .paragraph_style
        .as_ref()
        .unwrap()
        .todo
        .as_ref()
        .unwrap();
    let second = doc
        .attribute_runs
        .last()
        .unwrap()
        .paragraph_style
        .as_ref()
        .unwrap()
        .todo
        .as_ref()
        .unwrap();
    assert_eq!(first.todo_uuid.as_deref(), Some(&TODO_UUID[..]));
    assert_eq!(second.todo_uuid.as_ref().unwrap().len(), 16);
    assert_ne!(second.todo_uuid.as_deref(), Some(&TODO_UUID[..]));
}

#[test]
fn numbered_lists_omit_starting_list_item_number_at_the_default_start() {
    let mut doc = doc_with("num one\nnum two", vec![plain(15)]);
    assert_eq!(reconcile(&mut doc, "1. num one\n2. num two", &REPLICA_A), Ok(true));
    for r in &doc.attribute_runs {
        let ps = r.paragraph_style.as_ref().unwrap();
        assert_eq!(ps.style, Some(102));
        assert!(ps.starting_list_item_number.is_none());
    }
    let mut started = doc_with("five", vec![plain(4)]);
    assert_eq!(reconcile(&mut started, "5. five", &REPLICA_A), Ok(true));
    assert_eq!(
        started.attribute_runs[0]
            .paragraph_style
            .as_ref()
            .unwrap()
            .starting_list_item_number,
        Some(5)
    );
}

#[test]
fn an_explicit_starting_list_item_number_of_0_is_repaired_even_when_nothing_rendered_changed() {
    let zero = || ParagraphStyle {
        starting_list_item_number: Some(0),
        ..style(102)
    };
    let mut doc = doc_with("num one\nnum two", vec![with_style(8, zero()), with_style(7, zero())]);
    assert_eq!(reconcile(&mut doc, "1. num one\n2. num two", &REPLICA_A), Ok(true));
    for r in &doc.attribute_runs {
        let ps = r.paragraph_style.as_ref().unwrap();
        assert_eq!(ps.style, Some(102));
        assert!(ps.starting_list_item_number.is_none());
    }
}

#[test]
fn misaligned_desired_paragraphs_refuse_rather_than_guess() {
    let mut doc = doc_with("one line", vec![plain(8)]);
    assert_eq!(
        reconcile(&mut doc, "different text", &REPLICA_A),
        Err("the note's paragraphs don't line up with the edited text - refusing to guess".into())
    );
}
