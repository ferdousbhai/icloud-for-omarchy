//! `icloud-photos-app`: the Photos window (`src/ui/`, GTK 4 and
//! libadwaita). `icloud-photos` with no arguments runs this; the desktop
//! entry starts it directly. Commands live in `icloud-photos` (`src/cli.rs`).

mod recall_client;
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
        // Left iCloud: the photos are files now, and the window would only
        // talk to iCloud.
        match icloud_app::leave::left("icloud-photos").ok().flatten() {
            Some(left) => left_window(app, &left).present(),
            None => ui::window::build(app).start(),
        }
    });
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let a = app.clone();
    quit.connect_activate(move |_, _| a.quit());
    app.add_action(&quit);
    // Arguments are icloud-photos' business (commands), not GApplication's.
    app.run_with_args::<&str>(&[])
}

/// The window once Photos left iCloud on this computer: where the photos
/// are now, and a button that opens the folder.
fn left_window(app: &adw::Application, left: &icloud_app::leave::Left) -> adw::ApplicationWindow {
    let folder = left.folder.clone();
    let open = gtk::Button::builder()
        .label("Open Folder")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    let page = adw::StatusPage::builder()
        .icon_name("image-x-generic-symbolic")
        .title("Your Photos Are Files Now")
        .description(format!(
            "Photos left iCloud on this computer. Every original is in {}, which Omacloud keeps in sync on your computers; each album is a list of files in its Albums folder.",
            folder.display()
        ))
        .child(&open)
        .build();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Photos")
        .icon_name(APP_ID)
        .default_width(560)
        .default_height(480)
        .content(&tv)
        .build();
    let parent = window.clone();
    open.connect_clicked(move |_| {
        let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(&folder)));
        launcher.launch(Some(&parent), gtk::gio::Cancellable::NONE, |_| {});
    });
    window
}
