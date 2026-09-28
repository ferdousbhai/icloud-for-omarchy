//! Find My devices for Omarchy. The core ([`findme`], [`history`],
//! [`models`]) has no GTK dependency and is tested on its own; the widgets
//! live in [`ui`] behind the `gtk` / `ui` features.

pub mod findme;
pub mod history;
pub mod models;

#[cfg(feature = "gtk")]
pub mod ui;
