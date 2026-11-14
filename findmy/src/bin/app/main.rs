//! `icloud-findmy-app` opens the Find My window (GTK4, libadwaita,
//! libshumate); the desktop entry runs it, and so does `icloud-findmy` with
//! no command. Given a command it runs the command line, like
//! `icloud-findmy`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, so no GTK code may reach it (not even through
//! generic code a debug build shares between modules of one crate).

mod devices;
mod map;
mod window;

#[path = "../../../../native_theme.rs"]
mod theme;

use adw::prelude::*;

pub const APP_ID: &str = "com.ferdousbhai.IcloudFindMy";

fn main() {
    icloud_app_ui::main(icloud_findmy::cli::run, || {
        let app = icloud_app_ui::application(APP_ID, "icloud-findmy", "Find My", |app| {
            window::Window::new(app).present()
        });
        app.connect_startup(|_| theme::install(map::CSS));
        app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
        app.set_accels_for_action("window.close", &["<Ctrl>w", "<Ctrl>q"]);
        app
    })
}
