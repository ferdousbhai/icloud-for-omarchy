//! `icloud-findmy-app` opens the Find My window (GTK 4 with omui's look and
//! widgets and Omarchy's current theme, and libshumate's map); the desktop
//! entry runs it through `icloud-findmy open`, and so does `icloud-findmy`
//! with no command. Given a command it runs the command line, like
//! `icloud-findmy`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, so no GTK code may reach it (not even through
//! generic code a debug build shares between modules of one crate).

mod devices;
mod map;
mod window;

use gtk::prelude::*;

/// The app's one name: its command, package, desktop entry, icon and the
/// Wayland app id of its window.
pub const NAME: &str = "icloud-findmy";
/// The GApplication id, which needs a dot and so cannot be [`NAME`]. It is
/// what makes a second launch show the first window, and what the window is
/// known by on the session bus (dev/preview.sh drives its actions there);
/// it stays as it was before the app had one name.
pub const APP_ID: &str = "com.ferdousbhai.IcloudFindMy";

fn main() {
    omui::main(icloud_findmy::cli::run, || {
        // "Find My" is Apple's name for the service: a brand, not translated.
        let app = icloud_app_ui::application(
            NAME,
            APP_ID,
            "Find My",
            &[("en", include_str!("../../i18n/en.json"))],
            window::CSS,
            |app| window::Window::new(app).present(),
        );
        app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
        app
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalogs_match_the_sources() {
        icloud_app_ui::catalog_check(env!("CARGO_MANIFEST_DIR")).assert_ok();
    }

    /// Every file that carries the name: the desktop entry is named by it,
    /// opens the window through it, and tells Hyprland the class the window
    /// has; the icon is named by it too.
    #[test]
    fn the_desktop_entry_is_the_one_name() {
        let entry = include_str!("../../../data/icloud-findmy.desktop");
        for line in [
            "Exec=icloud-findmy open",
            "Icon=icloud-findmy",
            "StartupWMClass=icloud-findmy",
        ] {
            assert!(entry.lines().any(|l| l == line), "{line} is missing");
        }
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        assert!(data.join("icloud-findmy.svg").is_file());
        assert_eq!(super::NAME, "icloud-findmy");
        assert!(gtk::gio::Application::id_is_valid(super::APP_ID));
    }
}
