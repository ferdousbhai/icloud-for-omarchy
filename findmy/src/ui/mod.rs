//! GTK4 + libadwaita + libshumate front end (the `ui` feature).

pub mod banner;
pub mod devices;
pub mod map;
pub mod window;

use gtk::{gio, glib};

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

pub fn run() -> glib::ExitCode {
    use adw::prelude::*;

    glib::set_application_name("Find My");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(map::CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
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
