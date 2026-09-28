//! iCloud Photos for Omarchy.

mod ui;

use adw::prelude::*;

const APP_ID: &str = "com.ferdousbhai.IcloudPhotos";

fn main() -> gtk::glib::ExitCode {
    gtk::glib::set_prgname(Some("icloud-photos"));
    gtk::glib::set_application_name("iCloud Photos");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(ui::CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
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
    app.run()
}
