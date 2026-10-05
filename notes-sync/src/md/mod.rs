//! Markdown rendering/parsing, frontmatter, titles and file names.
//!
//! Ports icloud-md renderNoteMarkdown, parseNoteMarkdown, frontmatter,
//! noteIdFrontmatter, noteTitleParagraph, titleFilename, filename and
//! markdownTable. The contract with the codec is `crate::doc::format`, which
//! also holds noteFormat's round-trip projection (normalizeSpans & co.).
//!
//! | module | ports |
//! |---|---|
//! | `render`, `to_markdown` | renderNoteMarkdown + the remark-stringify / mdast-util-to-markdown / mdast-util-gfm subset it runs |
//! | `parse`, `mdast_fix` | parseNoteMarkdown over markdown-rs, plus fixups where markdown-rs's tree differs from micromark's |
//! | `table` | markdownTable |
//! | `frontmatter`, `yaml` | frontmatter + noteIdFrontmatter, and the slice of the `yaml` package they use |
//! | `title`, `filename` | noteTitleParagraph, titleFilename, filename |
//!
//! Output is frozen by golden corpora (`tests/golden/`, checked by
//! `tests/md_*_golden.rs`), first recorded from icloud-md's own code and now
//! from this crate.

pub mod filename;
pub mod frontmatter;
mod mdast_fix;
pub mod parse;
pub mod render;
pub mod table;
pub mod title;
pub mod to_markdown;
pub mod yaml;
