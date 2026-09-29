//! Markdown → format model. Ports icloud-md `src/notes/parseNoteMarkdown.ts`
//! (remark-parse + remark-gfm; here `markdown` (markdown-rs) to mdast with
//! GFM and positions). Owner: workstream C.
#![allow(unused_variables)]

use crate::doc::format::FormatParagraph;

/// `{status: "ok", paragraphs, text}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNoteMarkdown {
    pub paragraphs: Vec<FormatParagraph>,
    /// The paragraphs' texts joined with `\n`.
    pub text: String,
}

/// `{status: "unsupported", reason}`: markdown Apple Notes can't represent.
/// `reason` is shown verbatim in push refusals.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct ParseRefusal {
    pub reason: String,
}

/// `parseNoteMarkdown`.
pub fn parse_note_markdown(markdown: &str) -> Result<ParsedNoteMarkdown, ParseRefusal> {
    todo!()
}

/// `countQuoteMarkers`.
pub fn count_quote_markers(raw_line: &str) -> usize {
    todo!()
}
