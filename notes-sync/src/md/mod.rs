//! Markdown rendering/parsing, frontmatter, titles and file names. Owner:
//! workstream C (`src/md/**`, `src/diff3.rs`).
//!
//! Ports icloud-md renderNoteMarkdown, parseNoteMarkdown, frontmatter,
//! noteIdFrontmatter, noteTitleParagraph, titleFilename, filename and
//! markdownTable. The contract with the codec is `crate::doc::format`.
//!
//! icloud-md edits the frontmatter envelope with the `yaml` package's
//! Document API (keys set/deleted in place, the rest of the YAML kept as
//! written); no YAML crate is in Cargo.toml yet - pick one that preserves
//! formatting, or hand-roll the narrow subset, and freeze it with goldens.

pub mod filename;
pub mod frontmatter;
pub mod parse;
pub mod render;
pub mod table;
pub mod title;
