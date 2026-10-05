//! icloud-notes-sync: iCloud Notes ⇄ a folder of Markdown files. Originally
//! derived from icloud-md 0.6.2 (MIT, Adam Coddington; see NOTICE); design
//! notes in `docs/DESIGN.md`.
//!
//! | module | what |
//! |---|---|
//! | `cloudkit` | the CloudKit database client (over icloud-session's transport) |
//! | `doc` | the note/table codec |
//! | `md`, `diff3` | Markdown, frontmatter, names, three-way merge |
//! | `vault`, `cmd` | vault state and the commands |
//! | `js` | JavaScript/Node semantics the derived code relies on |

pub mod cloudkit;
pub mod cmd;
pub mod diff3;
pub mod doc;
pub mod js;
pub mod md;
pub mod vault;

pub use cloudkit::{CloudKitRecord, Database};

/// `generator` written into state.json.
pub const GENERATOR: &str = concat!("icloud-notes-sync ", env!("CARGO_PKG_VERSION"));
