//! iCloud Drive for Omarchy: keeps a local folder (`~/iCloud Drive`) and
//! iCloud Drive in step, both ways, over the shared `icloud-session` sign-in.
//!
//! - [`remote`]: iCloud Drive's web API behind the [`remote::Remote`] trait.
//! - [`local`]: the folder: scanning it, the trash, conflicted copies.
//! - [`state`]: what was in step after the last sync (SQLite).
//! - [`plan`]: the rules, as a pure function from the three views to operations.
//! - [`engine`]: lists, scans, plans and carries the plan out.
//! - [`cli`]: the `icloud-drive` command.

pub mod cli;
#[doc(hidden)]
pub mod crash;
pub mod engine;
pub mod local;
pub mod plan;
pub mod remote;
pub mod state;
