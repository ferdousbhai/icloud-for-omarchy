//! icloud-notes-sync: iCloud Notes ⇄ a folder of Markdown files. A Rust
//! port of icloud-md 0.6.2 (MIT, Adam Coddington); see `docs/PORT_PLAN.md`.
//!
//! | module | ports |
//! |---|---|
//! | `cloudkit` | `cloudkit/databaseClient.ts` (+ icloud-session transport) |
//! | `doc` | the note/table codec (`notes/noteDocument.ts` & co.) |
//! | `md`, `diff3` | Markdown, frontmatter, names, node-diff3 |
//! | `vault`, `cmd` | vault state and the commands |
//! | `js` | the JavaScript/Node semantics the ports rely on |

pub mod cloudkit;
pub mod cmd;
pub mod diff3;
pub mod doc;
pub mod js;
pub mod md;
pub mod vault;

pub use cloudkit::{CkError, CloudKitRecord, Database, Transport};
pub use cmd::Error;
pub use diff3::{CommHunk, MergeOutcome, diff_comm, merge_note_versions};
pub use doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
pub use doc::document::{
    ApplyTextEditOptions, NoteDocument, apply_text_edit, encode_note_document, parse_note_document,
};
pub use doc::encode::{UpdateFieldValue, build_note_create_fields, build_note_update_fields};
pub use doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind};
pub use md::frontmatter::{Envelope, split_frontmatter};
pub use md::parse::{ParseRefusal, parse_note_markdown};
pub use md::render::render_note_markdown;
pub use vault::state::{CloneState, NoteEntry, TitleMode, read_clone_state, write_clone_state};

/// `generator` written into state.json.
pub const GENERATOR: &str = concat!("icloud-notes-sync ", env!("CARGO_PKG_VERSION"));
