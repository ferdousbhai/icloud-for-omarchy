//! The semantic formatting model: the B↔C contract. Ports icloud-md
//! `src/notes/noteFormat.ts`. Owner: workstream B (types are frozen here so
//! workstream C's renderer/parser can be written against them).
//!
//! Units: every `length`, `start` and offset is in UTF-16 code units, exactly
//! as in icloud-md (JS string indices) and on the wire (attribute-run
//! lengths). `text` is a Rust `String`; convert with
//! `text.encode_utf16().count()` and friends, never byte lengths.
//!
//! Wire values (topotext `ParagraphStyle.style`): 0=Title 1=Heading
//! 2=Subheading 3=Body (absent style also means Body) 4=Monospaced
//! 100=bullet 101=dash 102=numbered 103=checklist (`todo{uuid, done}`);
//! `indent` is list nesting; `fontHints` bit 1=bold, bit 2=italic;
//! `underline`/`strikethrough` are 0/1 flags; `link` covers exactly the
//! linked range.
//!
//! serde: camelCase like the TS objects, so golden JSON dumped from
//! icloud-md (`tests/fixtures/`) deserializes directly. `InlineSpan` is
//! flattened (`{bold, italic, strikethrough, underline, link, length}`).

use serde::{Deserialize, Serialize};

use super::js::{from_utf16, is_js_whitespace, utf16};
use super::proto::topotext::{AttributeRun, ParagraphStyle};

/// `ParagraphKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParagraphKind {
    Title,
    Heading,
    Subheading,
    Body,
    Monospaced,
    BulletList,
    DashList,
    NumberedList,
    TodoList,
}

impl ParagraphKind {
    /// `STYLE_TO_KIND`: the wire `ParagraphStyle.style` value → kind;
    /// `None` for a style icloud-md doesn't understand.
    pub fn from_style(style: u32) -> Option<ParagraphKind> {
        Some(match style {
            0 => ParagraphKind::Title,
            1 => ParagraphKind::Heading,
            2 => ParagraphKind::Subheading,
            3 => ParagraphKind::Body,
            4 => ParagraphKind::Monospaced,
            100 => ParagraphKind::BulletList,
            101 => ParagraphKind::DashList,
            102 => ParagraphKind::NumberedList,
            103 => ParagraphKind::TodoList,
            _ => return None,
        })
    }

    /// The inverse of `from_style`.
    pub fn style(self) -> u32 {
        match self {
            ParagraphKind::Title => 0,
            ParagraphKind::Heading => 1,
            ParagraphKind::Subheading => 2,
            ParagraphKind::Body => 3,
            ParagraphKind::Monospaced => 4,
            ParagraphKind::BulletList => 100,
            ParagraphKind::DashList => 101,
            ParagraphKind::NumberedList => 102,
            ParagraphKind::TodoList => 103,
        }
    }

    /// `isListKind`.
    pub fn is_list(self) -> bool {
        matches!(
            self,
            ParagraphKind::BulletList | ParagraphKind::DashList | ParagraphKind::NumberedList | ParagraphKind::TodoList
        )
    }

    /// `projectedKind`: dash lists render (and so compare) as bullet lists.
    pub fn projected(self) -> ParagraphKind {
        if self == ParagraphKind::DashList {
            ParagraphKind::BulletList
        } else {
            self
        }
    }
}

/// `InlineStyle`. Equality is `inlineStylesEqual`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
    /// Link target URL; empty string means "not a link".
    pub link: String,
}

impl InlineStyle {
    /// `PLAIN_STYLE`.
    pub const PLAIN: InlineStyle = InlineStyle {
        bold: false,
        italic: false,
        strikethrough: false,
        underline: false,
        link: String::new(),
    };
}

/// `InlineSpan`: a style over `length` UTF-16 units of a paragraph's text.
/// A paragraph's spans cover its text exactly, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InlineSpan {
    #[serde(flatten)]
    pub style: InlineStyle,
    /// UTF-16 length of the span.
    pub length: usize,
}

/// `FormatParagraph`: one line of a note (split on `\n`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatParagraph {
    pub kind: ParagraphKind,
    /// List nesting depth (0 = top level); wire `ParagraphStyle.indent` is
    /// int32. Carried for every kind, only compared on list kinds.
    pub indent: i32,
    pub block_quote_level: u32,
    /// Checklist state; `Some` exactly on `TodoList` (TS omits the key
    /// otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    /// `startingListItemNumber`; only meaningful on `NumberedList` (0 = default).
    pub start_number: u32,
    /// Paragraph text, without its trailing newline.
    pub text: String,
    pub spans: Vec<InlineSpan>,
    /// UTF-16 offset of `text` within the note's full text.
    pub start: usize,
}

/// `DecodeNoteFormatResult`: `Err(reason)` is `{status: "unsupported", reason}`.
pub type DecodeNoteFormatResult = Result<Vec<FormatParagraph>, String>;

/// `inlineStyleOfRun`.
pub fn inline_style_of_run(run: &AttributeRun) -> InlineStyle {
    let hints = run.font_hints.unwrap_or(0);
    InlineStyle {
        bold: hints & 1 != 0,
        italic: hints & 2 != 0,
        strikethrough: run.strikethrough == Some(1),
        underline: run.underline == Some(1),
        link: run.link.clone().unwrap_or_default(),
    }
}

/// `paragraphKindOf`: absent style means Body; `None` for a style icloud-md
/// doesn't understand.
fn paragraph_kind_of(ps: Option<&ParagraphStyle>) -> Option<ParagraphKind> {
    match ps.and_then(|ps| ps.style) {
        None => Some(ParagraphKind::Body),
        Some(style) => ParagraphKind::from_style(style),
    }
}

/// `decodeNoteFormat`: split `text` into per-line paragraphs and derive each
/// one's kind and spans from the attribute runs (paragraph attributes from the
/// run covering the line's newline). Runs overshooting the text, or an unknown
/// paragraph style, are `Err` with icloud-md's reason string.
pub fn decode_note_format(text: &str, attribute_runs: &[AttributeRun]) -> DecodeNoteFormatResult {
    let units = utf16(text);
    let covered: u64 = attribute_runs.iter().map(|r| u64::from(r.len())).sum();
    if covered > units.len() as u64 {
        return Err("the note's formatting runs overshoot its text".into());
    }
    for run in attribute_runs {
        if let Some(ps) = &run.paragraph_style
            && paragraph_kind_of(Some(ps)).is_none()
        {
            return Err(format!(
                "the note uses a paragraph style ({}) this tool doesn't understand",
                ps.style.unwrap_or(0)
            ));
        }
    }

    // Absolute [start, end) intervals of the non-empty runs.
    let mut intervals: Vec<(&AttributeRun, usize, usize)> = Vec::new();
    let mut run_offset = 0usize;
    for run in attribute_runs {
        let len = run.len() as usize;
        if len > 0 {
            intervals.push((run, run_offset, run_offset + len));
        }
        run_offset += len;
    }
    let run_at = |index: usize| intervals.iter().find(|(_, start, end)| index >= *start && index < *end);

    let lines: Vec<&[u16]> = units.split(|&u| u == u16::from(b'\n')).collect();
    let mut paragraphs = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for (line_index, line) in lines.iter().enumerate() {
        let line_start = offset;
        let line_end = line_start + line.len();
        let has_newline = line_index < lines.len() - 1;
        offset = line_end + usize::from(has_newline);

        let anchor_index = if has_newline {
            line_end as i64
        } else {
            line_end as i64 - 1
        };
        let anchor_run = if anchor_index >= line_start as i64 {
            run_at(anchor_index as usize).map(|(run, _, _)| *run)
        } else {
            None
        };
        let ps = anchor_run.and_then(|run| run.paragraph_style.as_ref());
        let kind = paragraph_kind_of(ps).unwrap_or(ParagraphKind::Body);

        let mut spans: Vec<InlineSpan> = Vec::new();
        let mut at = line_start;
        while at < line_end {
            let interval = run_at(at);
            let span_end = interval.map_or(line_end, |(_, _, end)| line_end.min(*end));
            let style = interval.map_or(InlineStyle::PLAIN, |(run, _, _)| inline_style_of_run(run));
            match spans.last_mut() {
                Some(previous) if previous.style == style => previous.length += span_end - at,
                _ => spans.push(InlineSpan {
                    style,
                    length: span_end - at,
                }),
            }
            at = span_end;
        }

        paragraphs.push(FormatParagraph {
            kind,
            indent: ps.and_then(|ps| ps.indent).unwrap_or(0),
            block_quote_level: ps.and_then(|ps| ps.block_quote_level).unwrap_or(0),
            done: (kind == ParagraphKind::TodoList)
                .then(|| ps.and_then(|ps| ps.todo.as_ref()).and_then(|todo| todo.done) == Some(1)),
            start_number: ps.and_then(|ps| ps.starting_list_item_number).unwrap_or(0),
            text: from_utf16(line),
            spans,
            start: line_start,
        });
    }
    Ok(paragraphs)
}

/// `units[start..end]`, clamped like `String.prototype.slice`.
fn slice_units(units: &[u16], start: usize, end: usize) -> &[u16] {
    let end = end.min(units.len());
    &units[start.min(end)..end]
}

/// `normalizeSpans`: the canonical projection of a paragraph's spans
/// (monospaced drops styling, bare-URL links collapse, delimiter styles
/// retreat off edge whitespace, adjacent equal spans merge).
pub fn normalize_spans(paragraph: &FormatParagraph) -> Vec<InlineSpan> {
    let text = utf16(&paragraph.text);
    if paragraph.kind == ParagraphKind::Monospaced {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![InlineSpan {
                style: InlineStyle::PLAIN,
                length: text.len(),
            }]
        };
    }
    let mut out: Vec<InlineSpan> = Vec::new();
    let mut at = 0usize;
    for span in &paragraph.spans {
        let covered = slice_units(&text, at, at + span.length);
        at += span.length;
        let mut normalized = span.clone();
        if utf16(&span.style.link) == covered {
            normalized.style.link = String::new();
        }
        match out.last_mut() {
            Some(previous) if previous.style == normalized.style => previous.length += normalized.length,
            _ => out.push(normalized),
        }
    }
    let mut start = 0usize;
    for span in &mut out {
        if !span.style.link.is_empty() && slice_units(&text, start, start + span.length) == utf16(&span.style.link) {
            span.style.link = String::new();
        }
        start += span.length;
    }
    merge_adjacent_equal_spans(trim_delimiter_styles_off_whitespace(&text, out))
}

fn trim_delimiter_styles_off_whitespace(text: &[u16], spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    let any_delimited = spans.iter().any(|s| s.style.bold)
        || spans.iter().any(|s| s.style.italic)
        || spans.iter().any(|s| s.style.strikethrough);
    if spans.is_empty() || !any_delimited {
        return spans;
    }
    let mut styles: Vec<InlineStyle> = Vec::new();
    for span in &spans {
        for _ in 0..span.length {
            styles.push(span.style.clone());
        }
    }
    let is_whitespace = |index: usize| text.get(index).is_some_and(|&u| is_js_whitespace(u));
    type Dim = fn(&mut InlineStyle) -> &mut bool;
    let dimensions: [Dim; 3] = [|s| &mut s.bold, |s| &mut s.italic, |s| &mut s.strikethrough];
    for dim in dimensions {
        let mut i = 0usize;
        while i < styles.len() {
            if !*dim(&mut styles[i]) {
                i += 1;
                continue;
            }
            let mut end = i;
            while end < styles.len() && *dim(&mut styles[end]) {
                end += 1;
            }
            let mut k = i;
            while k < end && is_whitespace(k) {
                *dim(&mut styles[k]) = false;
                k += 1;
            }
            let mut k = end as i64 - 1;
            while k >= i as i64 && is_whitespace(k as usize) {
                *dim(&mut styles[k as usize]) = false;
                k -= 1;
            }
            i = end;
        }
    }
    let mut out: Vec<InlineSpan> = Vec::new();
    for style in styles {
        match out.last_mut() {
            Some(previous) if previous.style == style => previous.length += 1,
            _ => out.push(InlineSpan { style, length: 1 }),
        }
    }
    out
}

fn merge_adjacent_equal_spans(spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    let mut out: Vec<InlineSpan> = Vec::new();
    for span in spans {
        match out.last_mut() {
            Some(previous) if previous.style == span.style => previous.length += span.length,
            _ => out.push(span),
        }
    }
    out
}

/// `trimTrailingWhitespace`: non-monospaced paragraphs lose trailing spaces
/// and tabs; spans shrink to cover exactly the trimmed text.
pub fn trim_trailing_whitespace(paragraph: &FormatParagraph) -> FormatParagraph {
    if paragraph.kind == ParagraphKind::Monospaced {
        return paragraph.clone();
    }
    let text = paragraph.text.trim_end_matches([' ', '\t']);
    if text.len() == paragraph.text.len() {
        return paragraph.clone();
    }
    let mut spans = Vec::new();
    let mut remaining = utf16(text).len();
    for span in &paragraph.spans {
        if remaining == 0 {
            break;
        }
        let length = span.length.min(remaining);
        spans.push(InlineSpan {
            style: span.style.clone(),
            length,
        });
        remaining -= length;
    }
    FormatParagraph {
        text: text.to_string(),
        spans,
        ..paragraph.clone()
    }
}

fn effective_start_number(start_number: u32) -> u32 {
    if start_number == 0 { 1 } else { start_number }
}

/// `paragraphProjectionsEqual`.
pub fn paragraph_projections_equal(
    raw_a: &FormatParagraph,
    raw_b: &FormatParagraph,
    previous_a: Option<&FormatParagraph>,
    previous_b: Option<&FormatParagraph>,
) -> bool {
    let a = trim_trailing_whitespace(raw_a);
    let b = trim_trailing_whitespace(raw_b);
    if a.kind.projected() != b.kind.projected() || a.text != b.text {
        return false;
    }
    if a.block_quote_level != b.block_quote_level {
        return false;
    }
    if a.kind.is_list() && a.indent != b.indent {
        return false;
    }
    if a.kind == ParagraphKind::TodoList && a.done.unwrap_or(false) != b.done.unwrap_or(false) {
        return false;
    }
    if a.kind == ParagraphKind::NumberedList {
        let starts_a = previous_a.is_none_or(|p| p.kind != ParagraphKind::NumberedList || p.indent != a.indent);
        let starts_b = previous_b.is_none_or(|p| p.kind != ParagraphKind::NumberedList || p.indent != b.indent);
        if starts_a != starts_b {
            return false;
        }
        if starts_a && effective_start_number(a.start_number) != effective_start_number(b.start_number) {
            return false;
        }
    }
    let spans_a = normalize_spans(&a);
    let spans_b = normalize_spans(&b);
    spans_a.len() == spans_b.len()
        && spans_a
            .iter()
            .zip(&spans_b)
            .all(|(x, y)| x.length == y.length && x.style == y.style)
}

/// `formatsRoundTripEqual`.
pub fn formats_round_trip_equal(a: &[FormatParagraph], b: &[FormatParagraph]) -> bool {
    a.len() == b.len()
        && (0..a.len()).all(|i| {
            paragraph_projections_equal(
                &a[i],
                &b[i],
                i.checked_sub(1).map(|p| &a[p]),
                i.checked_sub(1).map(|p| &b[p]),
            )
        })
}
