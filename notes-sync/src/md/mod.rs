//! Markdown rendering/parsing, frontmatter, titles and file names. Owner:
//! workstream C (`src/md/**`, `src/diff3.rs`).
//!
//! Ports icloud-md renderNoteMarkdown, parseNoteMarkdown, frontmatter,
//! noteIdFrontmatter, noteTitleParagraph, titleFilename, filename and
//! markdownTable. The contract with the codec is `crate::doc::format`.
//!
//! | module | ports |
//! |---|---|
//! | `render`, `to_markdown` | renderNoteMarkdown + the remark-stringify / mdast-util-to-markdown / mdast-util-gfm subset it runs |
//! | `parse`, `mdast_fix` | parseNoteMarkdown over markdown-rs, plus fixups where markdown-rs's tree differs from micromark's |
//! | `projection` | noteFormat's round-trip projection (normalizeSpans & co.; workstream B's `doc::format` declares them) |
//! | `table` | markdownTable |
//! | `frontmatter` | frontmatter + noteIdFrontmatter (stub) |
//! | `title`, `filename` | noteTitleParagraph, titleFilename, filename |
//! | `js` | JS string semantics (whitespace, UTF-16, `path.posix`) |
//!
//! Byte-exactness is frozen by golden corpora from icloud-md's own code
//! (`tests/golden/gen.mts`, `tests/md_*_golden.rs`).

pub mod filename;
pub mod frontmatter;
pub mod js;
mod mdast_fix;
pub mod parse;
pub mod projection;
pub mod render;
pub mod table;
pub mod title;
pub mod to_markdown;
