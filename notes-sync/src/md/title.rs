//! Title paragraphs and title-carrying file names. Originally derived from
//! icloud-md.

use crate::doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind};
use crate::js;

/// `MAX_TITLE_LENGTH` (UTF-16 units, like JS `.length`).
pub const MAX_TITLE_LENGTH: usize = 60;

/// `SplitTitleParagraph`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitTitle {
    pub title: Option<FormatParagraph>,
    pub body: Vec<FormatParagraph>,
}

/// `{text, paragraphs}` as `restoreTitleParagraphText` returns it (same
/// shape as `parse::ParsedNoteMarkdown`).
pub use super::parse::ParsedNoteMarkdown as NoteText;

/// `splitTitleParagraph`.
pub fn split_title_paragraph(paragraphs: &[FormatParagraph]) -> SplitTitle {
    match paragraphs.split_first() {
        Some((title, body)) => SplitTitle {
            title: Some(title.clone()),
            body: body.to_vec(),
        },
        None => SplitTitle {
            title: None,
            body: Vec::new(),
        },
    }
}

/// `restoreTitleParagraph`: recomputes every `start`.
pub fn restore_title_paragraph(title: &FormatParagraph, body: &[FormatParagraph]) -> Vec<FormatParagraph> {
    let mut paragraphs: Vec<FormatParagraph> = std::iter::once(title).chain(body).cloned().collect();
    let mut offset = 0;
    for paragraph in &mut paragraphs {
        paragraph.start = offset;
        offset += js::len16(&paragraph.text) + 1;
    }
    paragraphs
}

/// `restoreTitleParagraphText`.
pub fn restore_title_paragraph_text(title: &FormatParagraph, body: &[FormatParagraph]) -> NoteText {
    let paragraphs = restore_title_paragraph(title, body);
    let text = paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    NoteText { paragraphs, text }
}

/// `titleFromNoteFileName`.
pub fn title_from_note_file_name(file: &str) -> String {
    decode_title_stem(&js::posix::basename_suffix(file, ".md"))
}

/// `titleParagraphFromFilename`: plain Title-style paragraph.
pub fn title_paragraph_from_filename(title: &str) -> FormatParagraph {
    let length = js::len16(title);
    FormatParagraph {
        kind: ParagraphKind::Title,
        indent: 0,
        block_quote_level: 0,
        done: None,
        start_number: 0,
        text: title.to_string(),
        spans: if length > 0 {
            vec![InlineSpan {
                style: InlineStyle::PLAIN,
                length,
            }]
        } else {
            Vec::new()
        },
        start: 0,
    }
}

/// `HOMOGLYPHS`: illegal (or Obsidian-hostile) character → its stand-in.
const HOMOGLYPHS: [(char, char); 13] = [
    ('/', '\u{2044}'),  // FRACTION SLASH
    ('\\', '\u{29F5}'), // REVERSE SOLIDUS OPERATOR
    (':', '\u{A789}'),  // MODIFIER LETTER COLON
    ('*', '\u{2217}'),  // ASTERISK OPERATOR
    ('?', '\u{FF1F}'),  // FULLWIDTH QUESTION MARK
    ('"', '\u{201D}'),  // RIGHT DOUBLE QUOTATION MARK
    ('<', '\u{2039}'),  // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
    ('>', '\u{203A}'),  // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
    ('|', '\u{2758}'),  // LIGHT VERTICAL BAR
    ('#', '\u{FF03}'),  // FULLWIDTH NUMBER SIGN
    ('^', '\u{FF3E}'),  // FULLWIDTH CIRCUMFLEX ACCENT
    ('[', '\u{FF3B}'),  // FULLWIDTH LEFT SQUARE BRACKET
    (']', '\u{FF3D}'),  // FULLWIDTH RIGHT SQUARE BRACKET
];

/// `ESCAPE`: U+2060 WORD JOINER, prefixed to a title character that is
/// already a homoglyph.
const ESCAPE: char = '\u{2060}';

const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn homoglyph_of(c: char) -> Option<char> {
    HOMOGLYPHS
        .iter()
        .find(|(literal, _)| *literal == c)
        .map(|(_, glyph)| *glyph)
}

fn literal_of(c: char) -> Option<char> {
    HOMOGLYPHS
        .iter()
        .find(|(_, glyph)| *glyph == c)
        .map(|(literal, _)| *literal)
}

/// `encodeTitleStem` (homoglyph substitution + escapes).
pub fn encode_title_stem(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.chars() {
        if literal_of(c).is_some() {
            out.push(ESCAPE);
            out.push(c);
        } else {
            out.push(homoglyph_of(c).unwrap_or(c));
        }
    }
    out
}

/// `decodeTitleStem`.
pub fn decode_title_stem(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut escaped = false;
    for c in stem.chars() {
        if c == ESCAPE {
            escaped = true;
            continue;
        }
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        out.push(literal_of(c).unwrap_or(c));
    }
    out
}

/// `titleIsRepresentable`.
pub fn title_is_representable(title: &str) -> bool {
    representability_problem(title).is_none()
}

/// `representabilityProblem`: why a file name can't carry `title`.
pub fn representability_problem(title: &str) -> Option<String> {
    if js::trim(title).is_empty() {
        return Some("the title is empty".into());
    }
    let carried = carried_title_spelling(title);
    if js::len16(&carried) > MAX_TITLE_LENGTH {
        return Some(format!("the title is longer than {MAX_TITLE_LENGTH} characters"));
    }
    if carried.starts_with('.') {
        return Some("the title starts with a dot, which would make it a hidden file".into());
    }
    if carried.ends_with('.') {
        return Some("the title ends with a dot, which Windows silently strips".into());
    }
    if RESERVED_DEVICE_NAMES.contains(&carried.to_uppercase().as_str()) {
        return Some("the title is a reserved device name on Windows".into());
    }
    None
}

/// `carriedTitleSpelling`: `title.trimEnd()` (JS whitespace set).
pub fn carried_title_spelling(title: &str) -> String {
    js::trim_end(title).to_string()
}
