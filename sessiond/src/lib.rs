//! The daemon's modules, shared by its two binaries: `icloud-sessiond`
//! (src/main.rs, which is also the `icloud-session` CLI) and the sign-in
//! window.

pub mod apple;
pub mod cli;
pub mod cookies;
pub mod daemon;
pub mod files;
pub mod secrets;
