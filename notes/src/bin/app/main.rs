//! `icloud-notes-app`: the Notes window (GTK 4 with omui's look and widgets
//! and Omarchy's current theme), on top of the `icloud_notes` library. The
//! desktop entry runs `icloud-notes`, which runs this binary in its place
//! when it is given no command.
//!
//! The window's code lives in this binary, not in the library: the command
//! line and the background timer link the library, and no GTK code may
//! reach them.

mod icons;
mod window;

use gtk::prelude::*;

pub const APP_ID: &str = "com.ferdousbhai.IcloudNotes";

fn main() -> gtk::glib::ExitCode {
    let app = omui::application(APP_ID, "icloud-notes", "Notes", window::CSS, |app| {
        window::Window::new(app).present();
    });
    // A word that is no command opens the window as no word does
    // (`icloud-notes bogus`): the arguments are not GApplication's.
    app.run_with_args::<&str>(&[])
}
