//! The frontmatter envelope and the keys this tool owns in it. Ports icloud-md
//! `src/notes/frontmatter.ts` and `noteIdFrontmatter.ts`. Owner: workstream C.
//!
//! Plan deviation: the plan's `with_note_id(body, id)` is icloud-md's
//! `composeNoteFile(frontmatter, body, recordName, unrepresentableTitle)`;
//! every vault's note files are exactly `---\napple-note-id: <ID>\n---\n\n`
//! + body when the file had no other frontmatter.
#![allow(unused_variables)]

pub const FENCE: &str = "---";
pub const NOTE_ID_KEY: &str = "apple-note-id";
pub const NOTE_TITLE_KEY: &str = "apple-note-title";

/// `SplitMarkdown`. Invariant: `frontmatter + body` is the original text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// The `---` fences, YAML, and following blank lines, verbatim; empty
    /// when the file has none.
    pub frontmatter: String,
    pub body: String,
}

/// `SplitFrontmatterOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SplitOptions {
    /// Filename-as-title vault: the body may itself open with `---` or blank
    /// lines.
    pub filename_as_title: bool,
}

/// `splitFrontmatter`.
pub fn split_frontmatter(text: &str, options: SplitOptions) -> Envelope {
    todo!()
}

/// `joinFrontmatter`.
pub fn join_frontmatter(frontmatter: &str, body: &str) -> String {
    format!("{frontmatter}{body}")
}

/// `isNoteId`: UUID-shaped, either case.
pub fn is_note_id(value: &str) -> bool {
    todo!()
}

/// `readNoteId`: total - broken YAML, missing key, non-UUID all read `None`.
pub fn read_note_id(frontmatter: &str) -> Option<String> {
    todo!()
}

/// `setNoteId`.
pub fn set_note_id(frontmatter: &str, id: &str) -> String {
    todo!()
}

/// `clearNoteId`.
pub fn clear_note_id(frontmatter: &str) -> String {
    todo!()
}

/// `readNoteTitle`.
pub fn read_note_title(frontmatter: &str) -> Option<String> {
    todo!()
}

/// `setNoteTitle`.
pub fn set_note_title(frontmatter: &str, title: &str) -> String {
    todo!()
}

/// `clearNoteTitle`.
pub fn clear_note_title(frontmatter: &str) -> String {
    todo!()
}

/// `composeNoteFile`: stamp the id, set or clear `apple-note-title`, join.
pub fn compose_note_file(
    frontmatter: &str,
    body: &str,
    record_name: &str,
    unrepresentable_title: Option<&str>,
) -> String {
    todo!()
}
