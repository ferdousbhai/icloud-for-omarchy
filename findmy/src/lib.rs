//! Find My devices for Omarchy. This library is the core ([`findme`],
//! [`history`], [`models`]) and the command line ([`cli`]), with no GTK
//! anywhere; the window is the `icloud-findmy-app` binary (`src/bin/app/`),
//! behind the `ui` feature.

pub mod cli;
pub mod findme;
pub mod history;
pub mod models;
