//! What the windows of the iCloud GTK apps share (Find My, Reminders, and
//! Cloud Contacts by git): the sign-in banner ([`SignInBanner`]), drawn by
//! [omui](https://github.com/ferdousbhai/omui), whose `main`, `application`
//! and `background` the apps' windows use. Internal: it builds no binary and
//! ships in no package of its own.
//!
//! This is the GTK half of `icloud-app` (`app/`), and a crate of its own
//! rather than a feature of that one for a reason: the apps' command lines
//! link `icloud-app` and must not load GTK (each app's `tests/cli.rs`
//! checks). A debug build shares generic code between the modules of one
//! crate, so GTK code anywhere in a crate a command line links ends up in
//! that command line. Only the `*-app` binaries may use this crate; nothing
//! an app's library uses belongs here.

mod banner;

pub use banner::SignInBanner;
