//! `icloud-photos-app`: the Photos window (`src/ui/`, GTK 4 with omui's look
//! and widgets and Omarchy's current theme). `icloud-photos` with no
//! arguments runs this; the desktop entry starts it directly. Commands live
//! in `icloud-photos` (`src/cli.rs`).

mod recall_client;
mod ui;

use gtk::prelude::*;
use omui::icons::name;

const APP_ID: &str = "com.ferdousbhai.IcloudPhotos";

fn main() -> gtk::glib::ExitCode {
    let app = omui::application(APP_ID, "icloud-photos", "Photos", ui::CSS, |app| {
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
fn left_window(app: &gtk::Application, left: &icloud_app::leave::Left) -> gtk::ApplicationWindow {
    let folder = left.folder.clone();
    let open = gtk::Button::builder()
        .label("Open Folder")
        .css_classes(["pill", "suggested-action"])
        .build();
    let page = omui::EmptyState::new(
        Some(name::IMAGE),
        "Your Photos Are Files Now",
        Some(&format!(
            "Photos left iCloud on this computer. Every original is in {}, which Omacloud keeps in sync on your computers; each album is a list of files in its Albums folder.",
            folder.display()
        )),
    );
    page.set_child(Some(open.upcast_ref()));
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Photos")
        .icon_name(APP_ID)
        .default_width(560)
        .default_height(480)
        .child(&page.widget)
        .build();
    window.set_titlebar(Some(&omui::header(None)));
    let parent = window.clone();
    open.connect_clicked(move |_| {
        let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(&folder)));
        launcher.launch(Some(&parent), gtk::gio::Cancellable::NONE, |_| {});
    });
    window
}
