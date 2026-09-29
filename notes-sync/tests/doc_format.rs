//! Ports icloud-md `src/notes/noteFormat.test.ts`.

use icloud_notes_sync::doc::format::{
    FormatParagraph, InlineSpan, InlineStyle, ParagraphKind, decode_note_format, formats_round_trip_equal,
    normalize_spans,
};
use icloud_notes_sync::doc::js::utf16_len;
use icloud_notes_sync::doc::proto::topotext::{AttributeRun, ParagraphStyle, Todo};

fn styled(length: u32, style: u32) -> AttributeRun {
    AttributeRun {
        length: Some(length),
        paragraph_style: Some(ParagraphStyle {
            style: Some(style),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn span(style: InlineStyle, length: usize) -> InlineSpan {
    InlineSpan { style, length }
}

fn plain(length: usize) -> InlineSpan {
    span(InlineStyle::PLAIN, length)
}

fn paragraph(text: &str) -> FormatParagraph {
    let len = utf16_len(text);
    FormatParagraph {
        kind: ParagraphKind::Body,
        indent: 0,
        block_quote_level: 0,
        done: None,
        start_number: 0,
        text: text.into(),
        spans: if len == 0 { vec![] } else { vec![plain(len)] },
        start: 0,
    }
}

fn with_kind(mut p: FormatParagraph, kind: ParagraphKind) -> FormatParagraph {
    p.kind = kind;
    p
}

#[test]
fn decode_maps_every_wire_style_value_to_its_paragraph_kind() {
    let mut todo = styled(1, 103);
    todo.paragraph_style.as_mut().unwrap().todo = Some(Todo {
        todo_uuid: Some(vec![0; 16]),
        done: Some(1),
        ..Default::default()
    });
    let result = decode_note_format(
        "t\nh\ns\nb\nm\nu\nd\nn\nc",
        &[
            styled(2, 0),
            styled(2, 1),
            styled(2, 2),
            styled(2, 3),
            styled(2, 4),
            styled(2, 100),
            styled(2, 101),
            styled(2, 102),
            todo,
        ],
    )
    .unwrap();
    use ParagraphKind::*;
    assert_eq!(
        result.iter().map(|p| p.kind).collect::<Vec<_>>(),
        vec![
            Title,
            Heading,
            Subheading,
            Body,
            Monospaced,
            BulletList,
            DashList,
            NumberedList,
            TodoList
        ]
    );
    assert_eq!(result[8].done, Some(true));
}

#[test]
fn decode_treats_absent_paragraph_style_and_explicit_style_3_identically_as_body() {
    let a = decode_note_format("x", &[styled(1, 3)]).unwrap();
    let b = decode_note_format("x", &[AttributeRun::with_length(1)]).unwrap();
    let c = decode_note_format(
        "x",
        &[AttributeRun {
            length: Some(1),
            paragraph_style: Some(ParagraphStyle {
                indent: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        }],
    )
    .unwrap();
    assert_eq!(a[0].kind, ParagraphKind::Body);
    assert_eq!(b[0].kind, ParagraphKind::Body);
    assert_eq!(c[0].kind, ParagraphKind::Body);
    assert_eq!(c[0].indent, 2);
    assert!(formats_round_trip_equal(&a, &b));
    assert!(formats_round_trip_equal(&a, &c));
}

#[test]
fn decode_refuses_an_unknown_paragraph_style_value() {
    let reason = decode_note_format("x", &[styled(1, 7)]).unwrap_err();
    assert_eq!(
        reason,
        "the note uses a paragraph style (7) this tool doesn't understand"
    );
}

#[test]
fn decode_tolerates_under_covering_runs_but_refuses_overshoot() {
    let under = decode_note_format(
        "covered and not",
        &[AttributeRun {
            length: Some(7),
            font_hints: Some(1),
            ..Default::default()
        }],
    )
    .unwrap();
    let bold = InlineStyle {
        bold: true,
        ..InlineStyle::PLAIN
    };
    assert_eq!(under[0].spans, vec![span(bold, 7), plain(8)]);
    assert_eq!(
        decode_note_format("ab", &[AttributeRun::with_length(5)]).unwrap_err(),
        "the note's formatting runs overshoot its text"
    );
}

#[test]
fn a_paragraphs_attributes_come_from_the_run_covering_its_newline() {
    let result = decode_note_format("one\ntwo\nthree", &[styled(8, 1), styled(5, 2)]).unwrap();
    assert_eq!(
        result.iter().map(|p| p.kind).collect::<Vec<_>>(),
        vec![
            ParagraphKind::Heading,
            ParagraphKind::Heading,
            ParagraphKind::Subheading
        ]
    );
}

#[test]
fn adjacent_equal_inline_runs_merge_into_one_span() {
    let result = decode_note_format(
        "abcdef",
        &[
            AttributeRun {
                length: Some(2),
                font_hints: Some(3),
                ..Default::default()
            },
            AttributeRun {
                length: Some(2),
                font_hints: Some(3),
                timestamp: Some(9),
                ..Default::default()
            },
            AttributeRun {
                length: Some(2),
                underline: Some(1),
                ..Default::default()
            },
        ],
    )
    .unwrap();
    let bold_italic = InlineStyle {
        bold: true,
        italic: true,
        ..InlineStyle::PLAIN
    };
    let underline = InlineStyle {
        underline: true,
        ..InlineStyle::PLAIN
    };
    assert_eq!(result[0].spans, vec![span(bold_italic, 4), span(underline, 2)]);
}

fn linked(link: &str) -> InlineStyle {
    InlineStyle {
        link: link.into(),
        ..InlineStyle::PLAIN
    }
}

#[test]
fn normalize_spans_collapses_a_bare_url_link_to_plain_text() {
    let url = "https://example.com/x";
    let len = utf16_len(url);
    let mut p = paragraph(url);
    p.spans = vec![span(linked(url), len)];
    assert_eq!(normalize_spans(&p), vec![plain(len)]);
    let mut split = paragraph(url);
    split.spans = vec![span(linked(url), 10), span(linked(url), len - 10)];
    assert_eq!(normalize_spans(&split), vec![plain(len)]);
}

#[test]
fn normalize_spans_keeps_an_explicit_link_whose_text_differs_from_its_target() {
    let mut p = paragraph("docs");
    p.spans = vec![span(linked("https://example.com/"), 4)];
    assert_eq!(normalize_spans(&p), vec![span(linked("https://example.com/"), 4)]);
}

#[test]
fn normalize_spans_drops_inline_styling_inside_monospaced_paragraphs() {
    let mut p = with_kind(paragraph("code"), ParagraphKind::Monospaced);
    p.spans = vec![span(
        InlineStyle {
            bold: true,
            ..InlineStyle::PLAIN
        },
        4,
    )];
    assert_eq!(normalize_spans(&p), vec![plain(4)]);
}

#[test]
fn normalize_spans_retreats_delimiter_styles_off_whitespace_keeping_underline() {
    let italic_underline = InlineStyle {
        italic: true,
        underline: true,
        ..InlineStyle::PLAIN
    };
    let underline = InlineStyle {
        underline: true,
        ..InlineStyle::PLAIN
    };
    let mut p = paragraph("ab cd");
    p.spans = vec![plain(2), span(italic_underline.clone(), 3)];
    assert_eq!(
        normalize_spans(&p),
        vec![plain(2), span(underline, 1), span(italic_underline, 2)]
    );
}

#[test]
fn projection_equality_dash_and_bullet_interchangeable_checklist_state_not() {
    assert!(formats_round_trip_equal(
        &[with_kind(paragraph("item"), ParagraphKind::DashList)],
        &[with_kind(paragraph("item"), ParagraphKind::BulletList)]
    ));
    let mut unchecked = with_kind(paragraph("todo"), ParagraphKind::TodoList);
    unchecked.done = Some(false);
    let mut checked = unchecked.clone();
    checked.done = Some(true);
    assert!(!formats_round_trip_equal(&[unchecked], &[checked]));
}

#[test]
fn projection_equality_numbered_start_matters_only_at_a_groups_first_item() {
    let numbered = |text: &str, start: u32| {
        let mut p = with_kind(paragraph(text), ParagraphKind::NumberedList);
        p.start_number = start;
        p
    };
    let a = [numbered("one", 5), numbered("two", 0)];
    let b = [numbered("one", 5), numbered("two", 9)];
    assert!(formats_round_trip_equal(&a, &b));
    let c = [numbered("one", 4), numbered("two", 0)];
    assert!(!formats_round_trip_equal(&a, &c));
}
