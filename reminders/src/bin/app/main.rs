//! `icloud-reminders-app` opens the Reminders window (GTK 4, with omui's
//! look and widgets and Omarchy's current theme); `icloud-reminders open`
//! runs it, as the desktop entry does, and so does `icloud-reminders` with
//! no command. Given a command it runs the command line, like
//! `icloud-reminders`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, and the background timer runs it every minute,
//! so no GTK code may reach it (not even through generic code a debug
//! build shares between modules of one crate).

mod window;

use gtk::prelude::*;

/// The app's one name: its command, package, desktop entry, icon, and the
/// Wayland app id of its windows.
pub const NAME: &str = "icloud-reminders";
/// The GApplication id, which needs a dot and so cannot be [`NAME`]. It is
/// what makes a second launch show the first window and what the app is
/// known by on the session bus; it stays as it was before the one name.
pub const APP_ID: &str = "com.ferdousbhai.IcloudReminders";

fn main() {
    omui::main(icloud_reminders::cli::run, || {
        let local = icloud_reminders::due::local_zone().unwrap_or_else(|e| {
            eprintln!("icloud-reminders: {e}");
            std::process::exit(1);
        });
        let app = icloud_app_ui::application(
            NAME,
            APP_ID,
            "Reminders",
            &[("en", include_str!("../../i18n/en.json"))],
            window::CSS,
            move |app| {
                // Left iCloud: the files are the reminders, and the window
                // would only sync with iCloud.
                match icloud_app::leave::left("icloud-reminders").ok().flatten() {
                    Some(left) => window::left(app, &left).present(),
                    None => window::Window::new(app, local.clone()).present(),
                }
            },
        );
        // Keys with Ctrl, or a function key: they act in the text fields
        // too, so they stay accelerators. Ctrl+W and Ctrl+Q are each
        // window's `omui::quit_keys`, the overlay's `?` an `omui::Keys`.
        app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
        app.set_accels_for_action("win.new", &["<Ctrl>n"]);
        app
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalogs_match_the_sources() {
        icloud_app_ui::catalog_check(env!("CARGO_MANIFEST_DIR")).assert_ok();
    }

    /// Omarchy knows an app by one name: the desktop entry is named after
    /// it, starts it with `<name> open`, and matches its windows by it.
    #[test]
    fn the_desktop_entry_is_the_one_name() {
        let entry = include_str!("../../../data/icloud-reminders.desktop");
        let lines: Vec<&str> = entry.lines().collect();
        for line in [
            "Exec=icloud-reminders open",
            "Icon=icloud-reminders",
            "StartupWMClass=icloud-reminders",
        ] {
            assert!(lines.contains(&line), "{line} is not in the desktop entry:\n{entry}");
        }
        let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/icloud-reminders.svg");
        assert!(icon.is_file(), "{} is missing", icon.display());
        assert_eq!(super::NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn the_application_id_is_a_valid_gapplication_id() {
        assert!(gtk::gio::Application::id_is_valid(super::APP_ID));
        // The short name is not one, which is why the two differ.
        assert!(!gtk::gio::Application::id_is_valid(super::NAME));
    }
}
