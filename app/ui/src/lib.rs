//! What the windows of the iCloud GTK apps share (Photos, Notes, Find My,
//! Reminders, and Cloud Contacts by git): the application each window runs
//! in, under the app's one short name ([`application`], [`name_window`]),
//! the strings of this crate in every language ([`register_catalogs`],
//! [`catalog_check`]), and the sign-in banner ([`SignInBanner`]), drawn by
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

use std::path::Path;

use gtk::prelude::*;

pub use banner::{SignInBanner, sign_in_ask};

/// This crate's own strings (the banner's), in every language it has a
/// catalog for. [`application`] registers them; an app that builds its
/// application another way calls this before it does.
pub fn register_catalogs() {
    omui::i18n::register_all(&[("en", ENGLISH)]);
}

/// This crate's `en.json`.
const ENGLISH: &str = include_str!("i18n/en.json");

/// The application of the app whose one name is `name` (`icloud-photos`:
/// its command, package, desktop entry, icon, and the Wayland app id of each
/// of its windows), with the GApplication id `id`, which needs a dot and so
/// cannot be the name (`com.ferdousbhai.IcloudPhotos`: what makes a second
/// launch show the first window, and what the window is known by on the
/// session bus). `catalogs` are the app's own, registered with this crate's
/// before omui's application starts; the rest is `omui::application`'s.
pub fn application(
    name: &'static str,
    id: &str,
    title: &str,
    catalogs: &[(&str, &str)],
    app_css: &'static str,
    open: impl Fn(&gtk::Application) + 'static,
) -> gtk::Application {
    register_catalogs();
    omui::i18n::register_all(catalogs);
    // omui gives every window added to the application `name` as its
    // Wayland app id, on every map, and makes it the program's name.
    omui::application(id, name, title, app_css, open)
}

/// Gives `window`, one the app keeps outside its application, the app's
/// short name as its Wayland app id: `omui::name_window`, which names it by
/// the program's name, the `name` [`application`] set. A window of the
/// application needs no call; omui names those as they join it. `name` is
/// kept so that apps calling this still build, and must be that same name.
pub fn name_window(window: &impl IsA<gtk::Window>, name: &'static str) {
    debug_assert_eq!(gtk::glib::prgname().as_deref(), Some(name));
    omui::name_window(window);
}

/// The catalog test of an app whose crate is at `manifest_dir` (its
/// `src/` and `src/i18n/`), knowing this crate's functions that take an
/// app's text ([`sign_in_ask`]):
///
/// ```ignore
/// icloud_app_ui::catalog_check(env!("CARGO_MANIFEST_DIR")).assert_ok();
/// ```
pub fn catalog_check(manifest_dir: impl AsRef<Path>) -> omui::i18n::CatalogCheck {
    let root = manifest_dir.as_ref();
    omui::i18n::CatalogCheck::new(root.join("src"), root.join("src/i18n")).function("sign_in_ask", 0, false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalogs_match_the_sources() {
        super::catalog_check(env!("CARGO_MANIFEST_DIR")).assert_ok();
    }

    /// An app's own text handed to `sign_in_ask` is the app's to list.
    #[test]
    fn an_app_lists_the_ask_it_hands_the_banner() {
        let app = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(app.path().join("src/i18n")).unwrap();
        std::fs::write(
            app.path().join("src/main.rs"),
            "fn main() { let _ = icloud_app_ui::sign_in_ask(\"Sign in to see your things\"); }",
        )
        .unwrap();
        std::fs::write(app.path().join("src/i18n/en.json"), "{}").unwrap();
        let problems = super::catalog_check(app.path()).problems();
        assert_eq!(problems, ["missing from en.json: \"Sign in to see your things\""]);
        std::fs::write(
            app.path().join("src/i18n/en.json"),
            r#"{"Sign in to see your things": "Sign in to see your things"}"#,
        )
        .unwrap();
        assert!(super::catalog_check(app.path()).problems().is_empty());
    }
}
