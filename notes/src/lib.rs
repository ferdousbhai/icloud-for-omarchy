//! iCloud Notes without a window: the vault and its notes as the app works
//! on them ([`backend`]), the pure rules behind safe sync ([`syncmodel`]),
//! the sidebar tree ([`notetree`]), the vault's lock ([`lock`]), runs of the
//! sync engine ([`sync`], [`engine`]) and the command line ([`cli`]).
//!
//! Nothing here links a toolkit: the `icloud-notes` binary is the command
//! line and the timer's `--sync`, and a window is another binary on top of
//! this crate. Until that one exists the Qt app (the C++ beside these
//! files) is the window, and still carries its own copy of all this.

pub mod attachments;
pub mod backend;
pub mod cli;
pub mod engine;
mod files;
mod help;
mod json;
pub mod lock;
pub mod notetree;
mod session;
pub mod sync;
pub mod syncmodel;
mod text;
mod trash;
pub mod vault;
