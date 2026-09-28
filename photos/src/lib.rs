//! iCloud Photos for Omarchy: the non-UI half, testable without GTK or Apple.

pub mod catalog;
pub mod cloudkit;
pub mod config;
#[cfg(feature = "session")]
pub mod session;
pub mod sync;
pub mod thumbs;
pub mod transport;
pub mod upload;
