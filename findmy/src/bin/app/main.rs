//! `icloud-findmy-app` opens the Find My window (GTK 4 with omui's look and
//! widgets and Omarchy's current theme, and libshumate's map); the desktop
//! entry runs it, and so does `icloud-findmy` with no command. Given a
//! command it runs the command line, like `icloud-findmy`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, so no GTK code may reach it (not even through
//! generic code a debug build shares between modules of one crate).

mod devices;
mod map;
mod window;

use gtk::prelude::*;

pub const APP_ID: &str = "com.ferdousbhai.IcloudFindMy";

fn main() {
    omui::main(icloud_findmy::cli::run, || {
        let app = omui::application(APP_ID, "icloud-findmy", "Find My", window::CSS, |app| {
            window::Window::new(app).present()
        });
        app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
        app.set_accels_for_action("window.close", &["<Ctrl>w", "<Ctrl>q"]);
        app
    })
}
