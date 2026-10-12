//! `icloud-findmy-app` opens the Find My window (GTK4, libadwaita,
//! libshumate); the desktop entry runs it, and so does `icloud-findmy` with
//! no command. Given a command it runs the command line, like
//! `icloud-findmy`.
//!
//! The window's code lives in this binary, not in the library: the command
//! line links the library, so no GTK code may reach it (not even through
//! generic code a debug build shares between modules of one crate).

mod banner;
mod devices;
mod map;
mod window;

use gtk::{gio, glib};

fn main() {
    if std::env::args_os().len() > 1 {
        std::process::exit(icloud_findmy::cli::run().into());
    }
    std::process::exit(i32::from(run().get()));
}

pub const APP_ID: &str = "com.ferdousbhai.IcloudFindMy";

/// Runs `f` on a worker thread and hands its result back on the main loop.
/// Every network and SQLite call goes through here, never the main loop.
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

    // The name the app had as one binary (X11 WM_CLASS, logs), not the
    // `icloud-findmy-app` binary's.
    glib::set_prgname(Some("icloud-findmy"));
    glib::set_application_name("Find My");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(map::CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    app.connect_activate(|app| {
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        window::Window::new(app).present();
    });
    app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
    app.set_accels_for_action("window.close", &["<Ctrl>w", "<Ctrl>q"]);
    app.run()
}
