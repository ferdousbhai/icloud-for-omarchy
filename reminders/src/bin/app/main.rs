//! `icloud-reminders-app` opens the Reminders window (GTK4, libadwaita);
//! the desktop entry runs it, and so does `icloud-reminders` with no
//! command. Given a command it runs the command line, like
//! `icloud-reminders`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, and the background timer runs it every minute,
//! so no GTK code may reach it (not even through generic code a debug
//! build shares between modules of one crate).

mod banner;
mod window;

use gtk::{gio, glib};

pub const APP_ID: &str = "com.ferdousbhai.IcloudReminders";

fn main() {
    if std::env::args_os().len() > 1 {
        std::process::exit(icloud_reminders::cli::run().into());
    }
    std::process::exit(i32::from(run().get()));
}

/// Runs `f` on a worker thread and hands its result back on the main loop.
/// Every network and file call goes through here, never the main loop.
pub fn background<T, F, C>(f: F, done: C)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    C: FnOnce(Result<T, String>) + 'static,
{
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(f)
            .await
            .map_err(|_| "the background task crashed".to_string());
        done(result);
    });
}

fn run() -> glib::ExitCode {
    use adw::prelude::*;

    // The name the CLI has (X11 WM_CLASS, logs), not the
    // `icloud-reminders-app` binary's.
    glib::set_prgname(Some("icloud-reminders"));
    glib::set_application_name("Reminders");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        window::Window::new(app).present();
    });
    app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
    app.set_accels_for_action("win.new", &["<Ctrl>n"]);
    app.set_accels_for_action("window.close", &["<Ctrl>w", "<Ctrl>q"]);
    app.run()
}
