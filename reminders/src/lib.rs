//! iCloud Reminders for Omarchy. This library is the core ([`cloudkit`],
//! [`model`], [`topotext`], [`due`], [`store`], [`service`], [`notify`])
//! and the command line ([`cli`]), with no GTK anywhere; the window is the
//! `icloud-reminders-app` binary (`src/bin/app/`), behind the `ui` feature.

pub mod cli;
pub mod cloudkit;
pub mod due;
pub mod model;
pub mod notify;
pub mod service;
pub mod store;
pub mod topotext;
