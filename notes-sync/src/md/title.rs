//! Title paragraphs and title-carrying file names. Ports icloud-md
//! `src/notes/noteTitleParagraph.ts` and `titleFilename.ts`. Owner:
//! workstream C.
#![allow(unused_variables)]

use crate::doc::format::FormatParagraph;

/// `MAX_TITLE_LENGTH`.
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
    todo!()
}

/// `restoreTitleParagraph`: recomputes every `start`.
pub fn restore_title_paragraph(title: &FormatParagraph, body: &[FormatParagraph]) -> Vec<FormatParagraph> {
    todo!()
}

/// `restoreTitleParagraphText`.
pub fn restore_title_paragraph_text(title: &FormatParagraph, body: &[FormatParagraph]) -> NoteText {
    todo!()
}

/// `titleFromNoteFileName`.
pub fn title_from_note_file_name(file: &str) -> String {
    todo!()
}

/// `titleParagraphFromFilename`: plain Title-style paragraph.
pub fn title_paragraph_from_filename(title: &str) -> FormatParagraph {
    todo!()
}

/// `encodeTitleStem` (homoglyph substitution + escapes).
pub fn encode_title_stem(title: &str) -> String {
    todo!()
}

/// `decodeTitleStem`.
pub fn decode_title_stem(stem: &str) -> String {
    todo!()
}

/// `titleIsRepresentable`.
pub fn title_is_representable(title: &str) -> bool {
    representability_problem(title).is_none()
}

/// `representabilityProblem`: why a file name can't carry `title`.
pub fn representability_problem(title: &str) -> Option<String> {
    todo!()
}

/// `carriedTitleSpelling`: `title.trimEnd()` (JS whitespace set).
pub fn carried_title_spelling(title: &str) -> String {
    todo!()
}
