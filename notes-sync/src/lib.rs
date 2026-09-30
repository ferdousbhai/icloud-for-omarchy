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

pub use cloudkit::{CloudKitRecord, Database};

/// `generator` written into state.json.
pub const GENERATOR: &str = concat!("icloud-notes-sync ", env!("CARGO_PKG_VERSION"));
