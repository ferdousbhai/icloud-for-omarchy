//! iCloud Notes without a window: the vault and its notes as the app works
//! on them ([`backend`]), the pure rules behind safe sync ([`syncmodel`]),
//! the sidebar tree ([`notetree`]), the vault's lock ([`lock`]), runs of the
//! sync engine ([`sync`], [`engine`]) and the command line ([`cli`]).
//!
//! Nothing here links a toolkit: the `icloud-notes` binary is the command
//! line and the timer's `--sync`, and the window is another binary on top
//! of this crate (`src/bin/app/`, behind the `ui` feature).

pub mod attachments;
pub mod backend;
pub mod cli;
pub mod engine;
mod files;
mod help;
mod json;
pub mod lock;
pub mod notetree;
pub mod session;
pub mod sync;
pub mod syncmodel;
mod text;
mod trash;
pub mod vault;
