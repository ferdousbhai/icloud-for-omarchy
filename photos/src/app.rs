//! `icloud-photos-app`: the Photos window (`src/ui/`, GTK 4 and
//! libadwaita). `icloud-photos` with no arguments runs this; the desktop
//! entry starts it directly. Commands live in `icloud-photos` (`src/cli.rs`).

mod ui;

#[path = "../../native_theme.rs"]
mod theme;

use adw::prelude::*;

const APP_ID: &str = "com.ferdousbhai.IcloudPhotos";

fn main() -> gtk::glib::ExitCode {
    gtk::glib::set_prgname(Some("icloud-photos"));
    gtk::glib::set_application_name("Photos");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| theme::install(ui::CSS));
    app.connect_activate(|app| {
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        ui::window::build(app).start();
    });
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let a = app.clone();
    quit.connect_activate(move |_, _| a.quit());
    app.add_action(&quit);
    // Arguments are icloud-photos' business (commands), not GApplication's.
    app.run_with_args::<&str>(&[])
}
