//! Markdown spelling versions: how a note file says what it says.
//!
//! Spelling 1 (up to 0.11.1) read every line of a Markdown paragraph as a
//! line of the note at its own quote markers: a hard-wrapped line below a
//! list item (a lazy continuation line, `- item` then `text`) was a line of
//! body text below the item, and a hard line break (`\` or two spaces before
//! the line ending) ended a line. Spelling 2 reads them as CommonMark does: a
//! wrapped line is more of the item (a line break, U+2028, inside it), a lazy
//! line below a quoted line is more of the quote, and a hard line break is a
//! line break inside the line. So it writes a line of text below a list item
//! (or below a quoted line, quoted less) a blank line below it, and reads that
//! blank line as syntax (`md::parse`).
//!
//! A file written under spelling 1 is rewritten once, before anything reads
//! it, so that spelling 2 reads it as spelling 1 did ([`upgrade_spelling`]):
//! the vault's own record of its spelling is in `vault::spelling`.

use super::parse::{LineOutput, ParseRefusal, ParsedNoteMarkdown, read_note_lines, split_quote_markers};
use super::table::find_markdown_table_blocks;
use crate::doc::format::{FormatParagraph, ParagraphKind, formats_round_trip_equal};

/// The spelling this build reads and writes.
pub const MARKDOWN_SPELLING: u32 = 2;

/// `markdown` (a note file's body written under spelling 1) rewritten so
/// that spelling 2 reads it as spelling 1 did: `None` when it already does.
///
/// Line by line, from the top, until both readings agree:
/// - a blank line that is now a separator line is doubled (the empty line
///   stays one);
/// - a line now read as more of the line above: a hard line break ending
///   that line becomes a plain line ending; if that isn't enough (a list
///   item's paragraph, a lazy quoted line) a separator line goes above it
///   (with its own quote markers), its indentation goes, and a list marker
///   starting it is escaped;
/// - a line now read at another quote level gets a separator line above it,
///   and a numbered item that started a list below text (counting from 1)
///   and is now more of that text is numbered 1.
///
/// A line no rewrite makes read alike is left as it is (a numbered list
/// counting from other than 1 right below text, which spelling 2 has no
/// spelling for either).
///
/// A Markdown table is read as lines of text for this (it is one where it
/// stands either way).
pub fn upgrade_spelling(markdown: &str) -> Option<String> {
    if markdown.contains('\r') {
        return None;
    }
    let mut real: Vec<String> = markdown.split('\n').map(str::to_owned).collect();
    let mut analysis = real.clone();
    for block in find_markdown_table_blocks(markdown) {
        for line in &mut analysis[block.start_line..block.end_line.min(real.len())] {
            *line = crate::doc::embeds::OBJECT_REPLACEMENT_CHARACTER.to_string();
        }
    }
    let (wanted, legacy_lines) = read_note_lines(&analysis.join("\n"), true).ok()?;
    let (now, _) = read_note_lines(&analysis.join("\n"), false).ok()?;
    if same_note(&wanted, &now) {
        return None;
    }
    let original = real.clone();
    // Which line of the original each line is (`None`: one put in), and how
    // often each original line was rewritten.
    let mut origin: Vec<Option<usize>> = (0..real.len()).map(Some).collect();
    let mut attempts = vec![0u32; real.len()];
    loop {
        let (_, now_lines) = read_note_lines(&analysis.join("\n"), false).ok()?;
        let Some(at) = (0..analysis.len()).find(|&at| {
            origin[at].is_some_and(|o| attempts[o] < MOST_ATTEMPTS && !same_line(&legacy_lines[o], &now_lines[at]))
        }) else {
            break;
        };
        let o = origin[at]?;
        attempts[o] += 1;
        let before = (real.clone(), analysis.clone(), origin.clone());
        let mut tentative = true;
        match (&legacy_lines[o], &now_lines[at]) {
            (LineOutput::Untouched, LineOutput::Syntax) => {
                for lines in [&mut real, &mut analysis] {
                    let line = lines[at].clone();
                    lines.insert(at + 1, line);
                }
                origin.insert(at + 1, None);
                tentative = false;
            }
            (LineOutput::Paragraphs(was), LineOutput::Syntax) if at > 0 => {
                if strip_hard_break(&mut analysis[at - 1]) {
                    strip_hard_break(&mut real[at - 1]);
                    tentative = false;
                } else {
                    separate(&mut real, &mut analysis, &mut origin, at, was[0].block_quote_level);
                }
            }
            (LineOutput::Paragraphs(was), LineOutput::Paragraphs(_)) if at > 0 => {
                // A numbered item that started a list below text, now read
                // as more of the text: it counted from 1, which Markdown can
                // say there.
                let renumbered = was[0].kind == ParagraphKind::NumberedList
                    && was[0].start_number <= 1
                    && renumber_to_one(&mut analysis[at])
                    && renumber_to_one(&mut real[at]);
                if !renumbered {
                    separate(&mut real, &mut analysis, &mut origin, at, was[0].block_quote_level);
                }
            }
            _ => {
                attempts[o] = MOST_ATTEMPTS;
                continue;
            }
        }
        // A separator or a number that leaves the line read otherwise all
        // the same is taken back, and the line given up on.
        if tentative {
            let (_, now_lines) = read_note_lines(&analysis.join("\n"), false).ok()?;
            let moved = origin.iter().position(|&line| line == Some(o))?;
            if !same_line(&legacy_lines[o], &now_lines[moved]) {
                (real, analysis, origin) = before;
                attempts[o] = MOST_ATTEMPTS;
            }
        }
    }
    (real != original).then(|| real.join("\n"))
}

/// How many rewrites one line gets before it is given up on.
const MOST_ATTEMPTS: u32 = 3;

/// `line`'s numbered list marker (after its quote markers and
/// indentation) made `1`: whether it had one other than that.
fn renumber_to_one(line: &mut String) -> bool {
    let (_, rest) = split_quote_markers(line);
    let indented = rest.trim_start_matches([' ', '\t']);
    let digits = indented.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || !matches!(indented.as_bytes().get(digits), Some(b'.' | b')')) || &indented[..digits] == "1" {
        return false;
    }
    let at = line.len() - indented.len();
    line.replace_range(at..at + digits, "1");
    true
}

/// What spelling 1 read `markdown` as.
pub fn read_as_spelling_1(markdown: &str) -> Result<ParsedNoteMarkdown, ParseRefusal> {
    read_note_lines(markdown, true).map(|(parsed, _)| parsed)
}

/// Whether two readings of a note are the same note.
fn same_note(a: &ParsedNoteMarkdown, b: &ParsedNoteMarkdown) -> bool {
    a.text == b.text && formats_round_trip_equal(&a.paragraphs, &b.paragraphs)
}

/// Whether a source line gave the same lines of the note in both readings,
/// but for their text (which a line joined further down changes).
fn same_line(a: &LineOutput, b: &LineOutput) -> bool {
    let shape = |p: &FormatParagraph| {
        (
            p.kind.projected(),
            p.block_quote_level,
            if p.kind.is_list() { p.indent } else { 0 },
        )
    };
    match (a, b) {
        (LineOutput::Untouched, LineOutput::Untouched) | (LineOutput::Syntax, LineOutput::Syntax) => true,
        (LineOutput::Paragraphs(a), LineOutput::Paragraphs(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| shape(x) == shape(y))
        }
        _ => false,
    }
}

/// A separator line (`level` quote markers) above line `at`, whose
/// indentation goes. Text that was more of a paragraph is text: a list
/// marker starting it (which only a line after a blank one could read as a
/// list) is escaped.
fn separate(
    real: &mut Vec<String>,
    analysis: &mut Vec<String>,
    origin: &mut Vec<Option<usize>>,
    at: usize,
    level: u32,
) {
    for lines in [real, analysis] {
        let (_, rest) = split_quote_markers(&lines[at]);
        let prefix = &lines[at][..lines[at].len() - rest.len()];
        let text = rest.trim_start_matches([' ', '\t']);
        let marker_end = |at: usize| matches!(text.as_bytes().get(at), None | Some(b' ' | b'\t'));
        let digits = text.bytes().take_while(u8::is_ascii_digit).count();
        let escape_at = if (1..=9).contains(&digits)
            && matches!(text.as_bytes().get(digits), Some(b'.' | b')'))
            && marker_end(digits + 1)
        {
            Some(digits)
        } else if matches!(text.as_bytes().first(), Some(b'-' | b'+' | b'*')) && marker_end(1) {
            Some(0)
        } else {
            None
        };
        let text = match escape_at {
            Some(at) => format!("{}\\{}", &text[..at], &text[at..]),
            None => text.to_owned(),
        };
        lines[at] = format!("{prefix}{text}");
        lines.insert(at, ">".repeat(level as usize));
    }
    origin.insert(at, None);
}

/// `line` without the hard line break ending it (an unescaped `\`, or two
/// or more spaces or tabs): whether it had one.
fn strip_hard_break(line: &mut String) -> bool {
    let backslashes = line.len() - line.trim_end_matches('\\').len();
    if backslashes % 2 == 1 {
        line.pop();
        return true;
    }
    let trimmed = line.trim_end_matches([' ', '\t']).len();
    if line.len() - trimmed >= 2 {
        line.truncate(trimmed);
        return true;
    }
    false
}
