//! Format model → Markdown. Ports icloud-md `src/notes/renderNoteMarkdown.ts`
//! (remark-stringify + remark-gfm, `unsafe` escaping, `tablePipeAlign:false`).
//! Owner: workstream C.
#![allow(unused_variables)]

use crate::doc::format::FormatParagraph;

/// `RawSpelling`: optional escaping relaxations tried nicest-first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RawSpelling {
    /// Obsidian's own notation (`[[Note]]` rather than `\[\[Note]]`).
    pub obsidian: bool,
    /// Punctuation escaped only against GFM autolink literals.
    pub autolink: bool,
}

/// `CONSERVATIVE_SPELLING`.
pub const CONSERVATIVE_SPELLING: RawSpelling = RawSpelling {
    obsidian: false,
    autolink: false,
};

/// `renderNoteMarkdown`.
pub fn render_note_markdown(paragraphs: &[FormatParagraph]) -> String {
    todo!()
}

/// `spellingCandidates`.
pub fn spelling_candidates<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<RawSpelling> {
    todo!()
}
