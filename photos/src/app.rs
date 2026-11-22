//! `icloud-photos-app`: the Photos window (`src/ui/`, GTK 4 with omui's look
//! and widgets and Omarchy's current theme and language). `icloud-photos`
//! with no arguments, or `icloud-photos open`, runs this, and so does the
//! desktop entry. Commands live in `icloud-photos` (`src/cli.rs`).

mod recall_client;
mod ui;

use gtk::prelude::*;
use omui::i18n;
use omui::icons::name;
use omui::tr;

/// The app's one name: its command, package, desktop entry and icon, and the
/// Wayland app id of its windows, which Hyprland's rules and the desktop
/// entry's `StartupWMClass` match.
const NAME: &str = "icloud-photos";

/// The GApplication id, which needs a dot and so cannot be [`NAME`]: what
/// makes a second launch show the first window, and the app's name on the
/// session bus. Kept from before the one name, so nothing keyed by it moves.
const APP_ID: &str = "com.ferdousbhai.IcloudPhotos";

/// Every string of the window, in English, listed for the catalog test.
const ENGLISH: &str = include_str!("i18n/en.json");

fn main() -> gtk::glib::ExitCode {
    let app = icloud_app_ui::application(NAME, APP_ID, "Photos", &[("en", ENGLISH)], ui::CSS, |app| {
        // Left iCloud: the photos are files now, and the window would only
        // talk to iCloud.
        match icloud_app::leave::left(NAME).ok().flatten() {
            Some(left) => left_window(app, &left).present(),
            None => ui::window::build(app).start(),
        }
    });
    // Arguments are icloud-photos' business (commands), not GApplication's.
    app.run_with_args::<&str>(&[])
}

/// The window once Photos left iCloud on this computer: where the photos
/// are now, and a button that opens the folder.
fn left_window(app: &gtk::Application, left: &icloud_app::leave::Left) -> gtk::ApplicationWindow {
    let folder = left.folder.clone();
    let open = gtk::Button::builder().css_classes(["pill", "suggested-action"]).build();
    i18n::bind_label(&open, "Open Folder");
    let page = std::rc::Rc::new(omui::EmptyState::new(Some(name::IMAGE), "", None));
    page.set_child(Some(open.upcast_ref()));
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .icon_name(NAME)
        .default_width(560)
        .default_height(480)
        .child(&page.widget)
        .build();
    window.set_titlebar(Some(&omui::header(None)));
    // The folder's name is the user's own; the sentences around it follow the language.
    let (text, shown) = (page.clone(), folder.display().to_string());
    i18n::bind(&window, move |window| {
        window.set_title(Some(&tr("Photos")));
        text.set_title(&tr("Your Photos Are Files Now"));
        text.set_description(&omui::tr!(
            "Photos left iCloud on this computer. Every original is in {}, which Omacloud keeps in sync on your computers; each album is a list of files in its Albums folder.",
            shown
        ));
    });
    omui::quit_keys(&window);
    let parent = window.clone();
    open.connect_clicked(move |_| {
        let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(&folder)));
        launcher.launch(Some(&parent), gtk::gio::Cancellable::NONE, |_| {});
    });
    window
}

#[cfg(test)]
mod tests {
    /// The window's sources translate exactly the strings `en.json` lists.
    #[test]
    fn the_catalog_lists_every_string_of_the_window() {
        // omui's `confirm` translates its heading and its button itself, given in English. The command line's
        // `leave::confirm` (cli.rs) shares the name and translates nothing, and the check knows a function by its
        // name alone, so that file, which has no translated text, is left out.
        icloud_app_ui::catalog_check(env!("CARGO_MANIFEST_DIR"))
            .arguments("confirm", &[1, 3], false)
            .skip("cli.rs")
            .assert_ok();
    }

    /// Every file that carries the name: the desktop entry is named by it,
    /// opens the window through it, and tells Hyprland the class the window
    /// has; the icon is named by it too.
    #[test]
    fn the_desktop_entry_is_the_one_name() {
        let entry = include_str!("../data/icloud-photos.desktop");
        for line in [
            "Exec=icloud-photos open",
            "Icon=icloud-photos",
            "StartupWMClass=icloud-photos",
        ] {
            assert!(entry.lines().any(|l| l == line), "{line} is missing");
        }
        assert!(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("data/icloud-photos.svg")
                .is_file()
        );
        assert_eq!(super::NAME, "icloud-photos");
        assert!(gtk::gio::Application::id_is_valid(super::APP_ID));
    }
}
