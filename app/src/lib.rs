//! What the iCloud GTK apps share (Photos, Find My, Reminders, Contacts):
//! the helpers each of them used to carry a copy of. Internal: it builds no
//! binary and ships in no package of its own.
//!
//! - [`cli`]: the command lines' small helpers on top of
//!   `icloud_session::output` (confirm, the app's sign-in hint, warnings,
//!   name folding) and the launcher that turns `icloud-<app>` with no
//!   command into `icloud-<app>-app`.
//! - [`store`]: an app's data directory, its `flock`, and JSON files
//!   replaced whole.
//! - [`SessionTransport`]: one lazily connected `icloud_session::Session`.
//! - [`testing`] (feature `testing`): what the apps' CLI tests share.
//!
//! No GTK here, not even behind a feature: the command lines link this
//! crate and must not load the toolkit (each app's `tests/cli.rs` checks),
//! and a debug build shares generic code between the modules of one crate.
//! What the windows share (the sign-in banner, `background`, the `*-app`
//! binaries' main) is the sibling crate `icloud-app-ui` (`ui/`), which only
//! the `*-app` binaries use. What every iCloud tool shares, GTK app or not,
//! belongs in `icloud-session`; what only one app needs stays in that app.

pub mod cli;
pub mod store;
#[cfg(feature = "testing")]
pub mod testing;
mod transport;

pub use transport::SessionTransport;
