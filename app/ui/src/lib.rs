//! What the windows of the iCloud GTK apps share (Find My, Reminders,
//! Contacts): [`main`] and [`application`] for a `*-app` binary,
//! [`background`], and the sign-in banner ([`SignInBanner`]). Internal: it
//! builds no binary and ships in no package of its own.
//!
//! This is the GTK half of `icloud-app` (`app/`), and a crate of its own
//! rather than a feature of that one for a reason: the apps' command lines
//! link `icloud-app` and must not load GTK (each app's `tests/cli.rs`
//! checks). A debug build shares generic code between the modules of one
//! crate, so GTK code anywhere in a crate a command line links ends up in
//! that command line. Only the `*-app` binaries may use this crate; nothing
//! an app's library uses belongs here.

mod banner;

use adw::prelude::*;
use gtk::{gio, glib};

pub use banner::SignInBanner;

/// `main` of an app's `*-app` binary: the desktop entry runs it with no
/// arguments, and so does the command line with no command, to open the
/// window (`app` builds the application, see [`application`]). Given a
/// command it runs the command line (`cli`), like the app's own.
pub fn main(cli: fn() -> u8, app: impl FnOnce() -> adw::Application) -> ! {
    if std::env::args_os().len() > 1 {
        std::process::exit(cli().into());
    }
    std::process::exit(i32::from(app().run().get()));
}

/// The application `id`, named `name` as the command line is (X11 WM_CLASS,
/// logs; not the `*-app` binary's name) and shown as `title`. It has one
/// window: `open` builds and presents it on the first activation, and a
/// later one presents it again.
pub fn application(id: &str, name: &str, title: &str, open: impl Fn(&adw::Application) + 'static) -> adw::Application {
    glib::set_prgname(Some(name));
    glib::set_application_name(title);
    let app = adw::Application::builder().application_id(id).build();
    app.connect_activate(move |app| match app.active_window() {
        Some(win) => win.present(),
        None => open(app),
    });
    app
}

/// Runs `f` on a worker thread and hands its result back on the main loop.
/// Every network, file and SQLite call goes through here, never the main
/// loop.
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
