//! Reconciling a document with a desired format model (originally derived
//! from icloud-md's tests). The desired
//! paragraphs are the recorded `parse_note_markdown` output for each test's
//! markdown (`tests/fixtures/parsed_markdown.json`), so the reconcile tests
//! don't move when the Markdown parser does; `recorded_parses_match_the_parser`
//! flags a drift (re-record with `ICLOUD_NOTES_SYNC_REGEN=1`).

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

/// A monospaced paragraph shows no inline styling, whatever its runs
/// carry. Made body text, it shows what they carry: that is reconciled
/// with the styling asked for, like any other.
#[test]
fn a_code_line_made_body_text_drops_the_styling_its_runs_carried_unseen() {
    use icloud_notes_sync::doc::format::formats_round_trip_equal;
    use icloud_notes_sync::md::parse::parse_note_markdown;
    let styled = |length: u32| AttributeRun {
        length: Some(length),
        paragraph_style: Some(style(4)),
        font_hints: Some(1),
        underline: Some(1),
        link: Some("https://example.com/".into()),
        ..Default::default()
    };
    for (markdown, text) in [("some code", "some code"), ("some **code**", "some code")] {
        let mut doc = doc_with(text, vec![styled(5), with_style(4, style(4))]);
        let desired = parse_note_markdown(markdown).unwrap().paragraphs;
        let result = reconcile_note_format_with(&mut doc, &desired, &REPLICA_B, &mut common::counting_uuids());
        assert_eq!(result.unwrap(), Ok(true));
        let written = decode_note_format(&doc.text, &doc.attribute_runs).unwrap();
        assert!(formats_round_trip_equal(&written, &desired), "{markdown}: {written:?}");
    }
}

/// Text typed at the start of a line joins the run of the line above
/// (`apply_text_edit`), which says that line's paragraph style. A paragraph
/// is what the run over its line break says, and its other runs are made to
/// say the same.
#[test]
fn text_typed_at_the_start_of_a_line_takes_that_lines_paragraph_style() {
    use icloud_notes_sync::md::parse::parse_note_markdown;
    // Per line, what its runs say it is: (style, quote level, todo uuid).
    type Says = (u32, u32, Option<Vec<u8>>);
    fn said_per_line(doc: &NoteDocument) -> Vec<Vec<Says>> {
        let mut lines: Vec<Vec<Says>> = vec![Vec::new()];
        let mut units = doc.text.encode_utf16();
        for run in &doc.attribute_runs {
            let style = run.paragraph_style.as_ref();
            let says = (
                style.and_then(|s| s.style).unwrap_or(3),
                style.and_then(|s| s.block_quote_level).unwrap_or(0),
                style.and_then(|s| s.todo.as_ref()).and_then(|t| t.todo_uuid.clone()),
            );
            for _ in 0..run.len() {
                let line = lines.last_mut().unwrap();
                if line.last() != Some(&says) {
                    line.push(says.clone());
                }
                if units.next() == Some(u16::from(b'\n')) {
                    lines.push(Vec::new());
                }
            }
        }
        lines
    }
    for (before, after, styles) in [
        ("# Title\nbody", "# Title\nmy body", vec![0, 3]),
        ("text\n> quoted", "text\n> more quoted", vec![3, 3]),
        ("## Head\n- one", "## Head\n- [ ] one more\n- two", vec![1, 103, 100]),
        (
            "# Title\nbody text\n- [x] one\n- [ ] two",
            "# Titles\nmy body text\n- [x] one!\n- [ ] a two",
            vec![0, 3, 103, 103],
        ),
    ] {
        let written = parse_note_markdown(before).unwrap();
        let mut doc = build_initial_note_document(&written.text, &REPLICA_A).unwrap();
        let mut mint = common::counting_uuids();
        reconcile_note_format_with(&mut doc, &written.paragraphs, &REPLICA_A, &mut mint)
            .unwrap()
            .unwrap();
        let edited = parse_note_markdown(after).unwrap();
        assert!(apply_text_edit(&mut doc, &edited.text, &ApplyTextEditOptions { replica_id: REPLICA_B }).unwrap());
        reconcile_note_format_with(&mut doc, &edited.paragraphs, &REPLICA_B, &mut mint)
            .unwrap()
            .unwrap();
        let said = said_per_line(&doc);
        assert!(said.iter().all(|line| line.len() == 1), "{after:?}: {said:?}");
        assert_eq!(
            said.iter().map(|line| line[0].0).collect::<Vec<_>>(),
            styles,
            "{after:?}"
        );
    }
    // The quote level is the quoted line's own, too.
    let mut doc = doc_with(
        "text\nmore quoted",
        vec![
            plain(10),
            with_style(
                6,
                ParagraphStyle {
                    block_quote_level: Some(1),
                    ..style(3)
                },
            ),
        ],
    );
    let quoted = parse_note_markdown("text\n> more quoted").unwrap().paragraphs;
    let result = reconcile_note_format_with(&mut doc, &quoted, &REPLICA_B, &mut common::counting_uuids());
    assert_eq!(result.unwrap(), Ok(true));
    assert_eq!(said_per_line(&doc), vec![vec![(3, 0, None)], vec![(3, 1, None)]]);
}

/// The note's last paragraph has no line break to carry its style: empty,
/// it has no character at all, and nothing is written for the style asked.
#[test]
fn an_empty_last_paragraph_is_given_no_style() {
    use icloud_notes_sync::doc::format::{formats_round_trip_equal, note_holds_format};
    use icloud_notes_sync::md::parse::parse_note_markdown;
    for markdown in ["- one\n-", "- one\n- [ ]", "- one\n##", "- one\n>"] {
        let wanted = parse_note_markdown(markdown).unwrap();
        let last = wanted.paragraphs.last().unwrap();
        assert!(
            last.kind != ParagraphKind::Body || last.block_quote_level == 1,
            "{markdown:?}"
        );
        let length = len16(&wanted.text) as u32;
        let mut doc = doc_with(&wanted.text, vec![with_style(length, style(100))]);
        let before = doc.clone();
        let result =
            reconcile_note_format_with(&mut doc, &wanted.paragraphs, &REPLICA_B, &mut common::counting_uuids());
        assert_eq!(result.unwrap(), Ok(false), "{markdown:?}");
        assert_eq!(doc, before, "{markdown:?}");
        let written = decode_note_format(&doc.text, &doc.attribute_runs).unwrap();
        assert!(note_holds_format(&written, &wanted.paragraphs), "{markdown:?}");
        assert!(!formats_round_trip_equal(&written, &wanted.paragraphs), "{markdown:?}");
    }
    // A last paragraph with text is held to what was asked, as is an empty
    // one that isn't the last.
    let wanted = parse_note_markdown("- one\n- two").unwrap().paragraphs;
    let plain_last = parse_note_markdown("- one\ntwo").unwrap().paragraphs;
    assert!(!note_holds_format(&plain_last, &wanted));
    let wanted = parse_note_markdown("-\n- two").unwrap().paragraphs;
    let plain_first = parse_note_markdown("\n- two").unwrap().paragraphs;
    assert!(!note_holds_format(&plain_first, &wanted));
}

#[test]
fn misaligned_desired_paragraphs_refuse_rather_than_guess() {
    let mut doc = doc_with("one line", vec![plain(8)]);
    assert_eq!(
        reconcile(&mut doc, "different text", &REPLICA_A),
        Err("the note's paragraphs don't line up with the edited text - refusing to guess".into())
    );
}

/// `tests/fixtures/parsed_markdown.json` is what `parse_note_markdown` gives
/// now; `ICLOUD_NOTES_SYNC_REGEN=1` rewrites it.
#[test]
fn recorded_parses_match_the_parser() {
    use serde::Serialize;
    use serde_json::{Value, json};
    let text = std::fs::read_to_string(common::PARSED_MARKDOWN).unwrap();
    let recorded: Value = serde_json::from_str(&text).unwrap();
    let mut now = serde_json::Map::new();
    for markdown in recorded.as_object().unwrap().keys() {
        let parsed = icloud_notes_sync::md::parse::parse_note_markdown(markdown).unwrap();
        now.insert(
            markdown.clone(),
            json!({ "status": "ok", "paragraphs": parsed.paragraphs, "text": parsed.text }),
        );
    }
    let now = Value::Object(now);
    if common::regen() {
        let mut out = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        now.serialize(&mut serde_json::Serializer::with_formatter(&mut out, formatter))
            .unwrap();
        std::fs::write(common::PARSED_MARKDOWN, out).unwrap();
    } else {
        assert_eq!(recorded, now, "re-record with ICLOUD_NOTES_SYNC_REGEN=1 if intended");
    }
}

/// Bolding or unbolding text changes its font's name only: a size the font
/// carries, which the Markdown doesn't show, stays. Found by
/// `roundtrip_property` (hidden attributes on generated notes).
#[test]
fn bolding_text_keeps_its_font_size() {
    use icloud_notes_sync::doc::proto::topotext::Font;
    let sized = |name: Option<&str>, hints: Option<u32>| AttributeRun {
        length: Some(5),
        font_hints: hints,
        font: Some(Font {
            name: name.map(str::to_string),
            point_size: Some(24.0f32.to_bits()),
            ..Default::default()
        }),
        ..Default::default()
    };
    for (run, markdown, name) in [
        (sized(None, None), "**Hello**", Some("SFUIText-Bold")),
        (sized(Some("SFUIText-Bold"), Some(1)), "Hello", None),
    ] {
        let mut doc = doc_with("Hello", vec![run]);
        let wanted = icloud_notes_sync::md::parse::parse_note_markdown(markdown)
            .unwrap()
            .paragraphs;
        assert_eq!(
            reconcile_note_format_with(&mut doc, &wanted, &REPLICA_A, &mut common::counting_uuids()).unwrap(),
            Ok(true)
        );
        let font = doc.attribute_runs[0].font.clone().unwrap();
        assert_eq!(font.point_size, Some(24.0f32.to_bits()), "{markdown}");
        assert_eq!(font.name.as_deref(), name, "{markdown}");
    }
}
