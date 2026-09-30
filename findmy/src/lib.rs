//! Find My devices for Omarchy. The core ([`findme`], [`history`],
//! [`models`]) has no GTK dependency and is tested on its own; the widgets
//! live in [`ui`] behind the `ui` feature, and the command line
//! ([`cli`]) runs the core without GTK.

pub mod cli;
pub mod findme;
pub mod history;
pub mod models;

#[cfg(feature = "ui")]
pub mod ui;
