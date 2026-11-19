//! `icloud-notes-app`: the Notes window (GTK 4 with omui's look and widgets
//! and Omarchy's current theme), on top of the `icloud_notes` library. The
//! desktop entry runs `icloud-notes`, which runs this binary in its place
//! when it is given no command, and for `export-pdf`, which lays out a page.
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
mod icons;
mod lists;
mod pdf;
mod syncing;
mod window;

use gtk::prelude::*;

pub const APP_ID: &str = "com.ferdousbhai.IcloudNotes";

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
    let app = omui::application(&application_id(), "icloud-notes", "Notes", window::CSS, |app| {
        window::Window::new(app).present();
    });
    // A word that is no command opens the window as no word does
    // (`icloud-notes bogus`): the arguments are not GApplication's.
    app.run_with_args::<&str>(&[])
}

/// One window per vault, as the vault has one lock: a second launch on the
/// same vault shows the first window and ends. The usual vault is the app's
/// own id (the desktop entry's, which the window is known by); one named by
/// `ICLOUD_NOTES_VAULT` gets an id of its own.
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
