//! iCloud Notes without a window: the vault and its notes as the app works
//! on them ([`backend`]), the pure rules behind safe sync ([`syncmodel`]),
//! the sidebar tree ([`notetree`]), the vault's lock ([`lock`]) and runs of
//! the sync engine ([`sync`], [`engine`]).
//!
//! Nothing here links a toolkit. The Qt app (the C++ beside these files) is
//! still the app, and carries its own copy of all this.

pub mod attachments;
pub mod backend;
pub mod engine;
mod files;
pub mod lock;
pub mod notetree;
mod session;
pub mod sync;
pub mod syncmodel;
mod text;
mod trash;
pub mod vault;
