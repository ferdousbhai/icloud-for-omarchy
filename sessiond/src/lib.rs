//! The daemon's modules, shared by its three binaries: `icloud-sessiond`
//! (src/main.rs), the sign-in window and the `icloud-session` CLI.

pub mod apple;
pub mod cookies;
pub mod daemon;
pub mod files;
pub mod secrets;
