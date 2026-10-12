//! iCloud Contacts for Omarchy. This library is the core ([`api`],
//! [`model`], [`store`], [`service`], [`vcard`]) and the command line
//! ([`cli`]), with no GTK anywhere; the window is the `icloud-contacts-app`
//! binary (`src/bin/app/`), behind the `ui` feature.

pub mod api;
pub mod cli;
pub mod model;
pub mod service;
pub mod store;
pub mod vcard;
