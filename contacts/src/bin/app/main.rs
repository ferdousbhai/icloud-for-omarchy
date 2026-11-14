//! `icloud-contacts-app` opens the Contacts window (GTK4, libadwaita); the
//! desktop entry runs it, and so does `icloud-contacts` with no command.
//! Given a command it runs the command line, like `icloud-contacts`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, so no GTK code may reach it (not even through
//! generic code a debug build shares between modules of one crate).

mod window;

use adw::prelude::*;

pub const APP_ID: &str = "com.ferdousbhai.IcloudContacts";

fn main() {
    icloud_app_ui::main(icloud_contacts::cli::run, || {
        let app = icloud_app_ui::application(APP_ID, "icloud-contacts", "Contacts", |app| {
            window::Window::new(app).present()
        });
        app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
        app.set_accels_for_action("win.new", &["<Ctrl>n"]);
        app.set_accels_for_action("win.find", &["<Ctrl>f"]);
        app.set_accels_for_action("win.edit", &["<Ctrl>e"]);
        app.set_accels_for_action("window.close", &["<Ctrl>w", "<Ctrl>q"]);
        app
    })
}
