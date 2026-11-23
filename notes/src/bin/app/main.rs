//! `icloud-notes-app`: the Notes window (GTK 4 with omui's look and widgets
//! and Omarchy's current theme), on top of the `icloud_notes` library. The
//! desktop entry runs `icloud-notes open`, which runs this binary in its
//! place, as it does when given no command, and for `export-pdf`, which lays
//! out a page.
//!
//! The window's code lives in this binary, not in the library: the command
//! line and the background timer link the library, and no GTK code may
//! reach them.

mod actions;
mod attach;
mod banners;
mod conflicts;
mod dialogs;
mod editor;
mod highlight;
mod keys;
mod lists;
mod pdf;
mod search;
mod settings;
mod syncing;
mod window;

use gtk::{gio, prelude::*};
use icloud_notes::lock::{Taken, VaultLock};

/// The app's one name: its command, package, desktop entry and icon, and the
/// Wayland app id of its window, which Omarchy (Hyprland's window rules, the
/// dock) knows it by.
pub const NAME: &str = "icloud-notes";

/// The GApplication id, which needs a dot and so cannot be [`NAME`]. It stays
/// what it was before the one name, as do the per-vault ids made from it: it
/// is the window's name on the session bus, what makes a second launch on a
/// vault show the first window.
pub const APP_ID: &str = "com.ferdousbhai.IcloudNotes";

/// The window's strings in English, the one language it ships for now; omui
/// follows Omarchy's language and falls back to these.
const CATALOGS: &[(&str, &str)] = &[("en", include_str!("../../i18n/en.json"))];

fn main() -> gtk::glib::ExitCode {
    // `icloud-notes export-pdf` runs here, for the page it lays out: the
    // whole command line, with no window and no display.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| icloud_notes::cli::argument(&arg))
        .collect();
    if args
        .first()
        .is_some_and(|first| icloud_notes::cli::is_cli_invocation(first))
    {
        std::process::exit(icloud_notes::cli::run_in_app(&args, &pdf::render).into());
    }
    let app = icloud_app_ui::application(NAME, &application_id(), "Notes", CATALOGS, window::CSS, |app| {
        window::Window::new(app).present();
    });
    // GApplication is one per id only on a session bus; without one, the
    // vault's own app lock still keeps a second window off it. (A launch
    // that reaches the first window through the bus shows it and ends.)
    let _ = app.register(gio::Cancellable::NONE);
    let path = icloud_notes::vault::Vault::locate(None).lock_path();
    let mut instance = VaultLock::new(format!("{}-app.lock", path.display()).into());
    if !app.is_remote() && instance.try_lock(&format!("Notes (pid {})", std::process::id())) == Taken::Busy {
        eprintln!("icloud-notes: Notes is open on this vault already.");
        return gtk::glib::ExitCode::SUCCESS;
    }
    // A word that is no command opens the window as no word does
    // (`icloud-notes bogus`): the arguments are not GApplication's.
    app.run_with_args::<&str>(&[])
}

/// One window per vault, as the vault has one lock: a second launch on the
/// same vault shows the first window and ends. The usual vault is the app's
/// own id (the desktop entry's, which the window is known by); one named by
/// `ICLOUD_NOTES_VAULT` gets an id of its own. Without a session bus,
/// `main`'s app lock keeps it to one.
fn application_id() -> String {
    match std::env::var_os("ICLOUD_NOTES_VAULT").filter(|dir| !dir.is_empty()) {
        None => APP_ID.to_string(),
        Some(dir) => {
            let root = icloud_notes::vault::Vault::at(dir.to_string_lossy()).absolute();
            let hash = root
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .fold(0xcbf2_9ce4_8422_2325u64, |hash, &b| {
                    (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
                });
            format!("{APP_ID}.Vault{hash:016x}")
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalogs_match_the_sources() {
        // omui's `confirm` translates its heading and its button itself, given in English. The command line's
        // own `confirm` (cli.rs) shares the name and translates nothing, and the check knows a function by its
        // name alone, so that file, which has no translated text, is left out.
        icloud_app_ui::catalog_check(env!("CARGO_MANIFEST_DIR"))
            .arguments("confirm", &[1, 3], false)
            .skip("cli.rs")
            .assert_ok();
    }

    /// Every file that carries the name: the desktop entry is named by it,
    /// opens the window through it, and tells Hyprland the class the window
    /// has. The GApplication id is another, valid one.
    #[test]
    fn the_desktop_entry_is_the_one_name() {
        let entry = include_str!("../../../data/icloud-notes.desktop");
        for line in [
            "Exec=icloud-notes open",
            "Icon=icloud-notes",
            "StartupWMClass=icloud-notes",
        ] {
            assert!(entry.lines().any(|l| l == line), "{line} is missing");
        }
        assert_eq!(super::NAME, "icloud-notes");
        assert!(gtk::gio::Application::id_is_valid(super::APP_ID));
    }

    /// A vault of its own has an id of its own, made from the old one, so a
    /// window already open on it is still found after an upgrade.
    #[test]
    fn a_named_vault_keeps_its_application_id() {
        // SAFETY: no other test of this binary reads the environment.
        unsafe { std::env::set_var("ICLOUD_NOTES_VAULT", "/tmp/some vault") };
        let id = super::application_id();
        unsafe { std::env::remove_var("ICLOUD_NOTES_VAULT") };
        assert!(id.starts_with("com.ferdousbhai.IcloudNotes.Vault"), "{id}");
        assert!(gtk::gio::Application::id_is_valid(&id));
        assert_eq!(super::application_id(), super::APP_ID);
    }
}
