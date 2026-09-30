//! The round-trip projection of the format model - icloud-md
//! `noteFormat.ts`'s `normalizeSpans`, `trimTrailingWhitespace`,
//! `paragraphProjectionsEqual` and `formatsRoundTripEqual`.
//!
//! `doc::format` declares them; the renderer needs them to choose between
//! spellings (`projectionSurvives`) and the parser trims with them, so the
//! port lives here and `doc::format` delegates. Offsets are UTF-16 units throughout.

use crate::doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind};
use crate::js;

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn unit_is_whitespace(unit: u16) -> bool {
    js::is_whitespace16(unit)
}

fn push_merged(out: &mut Vec<InlineSpan>, span: InlineSpan) {
    if let Some(previous) = out.last_mut()
        && previous.style == span.style
    {
        previous.length += span.length;
        return;
    }
    out.push(span);
}

/// `normalizeSpans`.
pub fn normalize_spans(paragraph: &FormatParagraph) -> Vec<InlineSpan> {
    let text = units(&paragraph.text);
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
    let covered = |start: usize, length: usize| -> String {
        let end = (start + length).min(text.len());
        let start = start.min(end);
        String::from_utf16_lossy(&text[start..end])
    };
    let mut out: Vec<InlineSpan> = Vec::new();
    let mut at = 0;
    for span in &paragraph.spans {
        let covered_text = covered(at, span.length);
        at += span.length;
        let mut normalized = span.clone();
        if normalized.style.link == covered_text {
            normalized.style.link = String::new();
        }
        push_merged(&mut out, normalized);
    }
    let mut start = 0;
    for span in &mut out {
        if !span.style.link.is_empty() && covered(start, span.length) == span.style.link {
            span.style.link = String::new();
        }
        start += span.length;
    }
    let trimmed = trim_delimiter_styles_off_whitespace(&text, out);
    let mut merged = Vec::new();
    for span in trimmed {
        push_merged(&mut merged, span);
    }
    merged
}

/// `trimDelimiterStylesOffWhitespace`.
fn trim_delimiter_styles_off_whitespace(text: &[u16], spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    if spans.is_empty()
        || !spans
            .iter()
            .any(|s| s.style.bold || s.style.italic || s.style.strikethrough)
    {
        return spans;
    }
    let mut styles: Vec<InlineStyle> = Vec::new();
    for span in &spans {
        for _ in 0..span.length {
            styles.push(span.style.clone());
        }
    }
    let is_ws = |index: usize| text.get(index).is_some_and(|&u| unit_is_whitespace(u));
    type Get = fn(&mut InlineStyle) -> &mut bool;
    let dimensions: [Get; 3] = [|s| &mut s.bold, |s| &mut s.italic, |s| &mut s.strikethrough];
    for get in dimensions {
        let mut i = 0;
        while i < styles.len() {
            if !*get(&mut styles[i]) {
                i += 1;
                continue;
            }
            let mut end = i;
            while end < styles.len() && *get(&mut styles[end]) {
                end += 1;
            }
            let mut k = i;
            while k < end && is_ws(k) {
                *get(&mut styles[k]) = false;
                k += 1;
            }
            let mut k = end;
            while k > i && is_ws(k - 1) {
                *get(&mut styles[k - 1]) = false;
                k -= 1;
            }
            i = end;
        }
    }
    let mut out = Vec::new();
    for style in styles {
        push_merged(&mut out, InlineSpan { style, length: 1 });
    }
    out
}

/// `trimTrailingWhitespace`: `text.replace(/[ \t]+$/, "")`, spans shrunk.
pub fn trim_trailing_whitespace(paragraph: &FormatParagraph) -> FormatParagraph {
    if paragraph.kind == ParagraphKind::Monospaced {
        return paragraph.clone();
    }
    let text = paragraph.text.trim_end_matches([' ', '\t']);
    if text.len() == paragraph.text.len() {
        return paragraph.clone();
    }
    let mut remaining = js::len16(text);
    let mut spans = Vec::new();
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

fn effective_start_number(paragraph: &FormatParagraph) -> u32 {
    if paragraph.start_number == 0 {
        1
    } else {
        paragraph.start_number
    }
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
        let starts = |p: &FormatParagraph, previous: Option<&FormatParagraph>| {
            previous.is_none_or(|prev| prev.kind != ParagraphKind::NumberedList || prev.indent != p.indent)
        };
        let starts_a = starts(&a, previous_a);
        let starts_b = starts(&b, previous_b);
        if starts_a != starts_b {
            return false;
        }
        if starts_a && effective_start_number(&a) != effective_start_number(&b) {
            return false;
        }
    }
    normalize_spans(&a) == normalize_spans(&b)
}

/// `formatsRoundTripEqual`.
pub fn formats_round_trip_equal(a: &[FormatParagraph], b: &[FormatParagraph]) -> bool {
    a.len() == b.len()
        && (0..a.len()).all(|i| {
            let (previous_a, previous_b) = if i == 0 {
                (None, None)
            } else {
                (a.get(i - 1), b.get(i - 1))
            };
            paragraph_projections_equal(&a[i], &b[i], previous_a, previous_b)
        })
}
