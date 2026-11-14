//! `icloud-reminders-app` opens the Reminders window (GTK4, libadwaita);
//! the desktop entry runs it, and so does `icloud-reminders` with no
//! command. Given a command it runs the command line, like
//! `icloud-reminders`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, and the background timer runs it every minute,
//! so no GTK code may reach it (not even through generic code a debug
//! build shares between modules of one crate).

mod window;

use adw::prelude::*;

pub const APP_ID: &str = "com.ferdousbhai.IcloudReminders";

fn main() {
    icloud_app_ui::main(icloud_reminders::cli::run, || {
        let local = icloud_reminders::due::local_zone().unwrap_or_else(|e| {
            eprintln!("icloud-reminders: {e}");
            std::process::exit(1);
        });
        let app = icloud_app_ui::application(APP_ID, "icloud-reminders", "Reminders", move |app| {
            window::Window::new(app, local.clone()).present()
        });
        app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
        app.set_accels_for_action("win.new", &["<Ctrl>n"]);
        app.set_accels_for_action("window.close", &["<Ctrl>w", "<Ctrl>q"]);
        app
    })
}
