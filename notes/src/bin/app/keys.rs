//! The window's keyboard as a whole: the overlay that lists every shortcut
//! (F1 or Ctrl+?, as Notes is typed into: `?` is a character here), and
//! closing and quitting on Ctrl+W and Ctrl+Q through the window's close
//! request, which asks first about edits it cannot save. The shortcuts
//! themselves are the window's actions' accelerators (actions.rs) and the
//! lists' and the editor's own keys (window.rs).

use std::rc::Rc;

use gtk::{gio, prelude::*};
use omui::tr;

use crate::window::Ui;

/// What the overlay lists, in the language of the moment: the README's
/// table, which says the same.
fn sections() -> Vec<omui::ShortcutSection> {
    vec![
        omui::ShortcutSection::new(
            &tr("Notes"),
            &[
                ("Ctrl+N", &tr("New note")),
                ("Ctrl+F", &tr("Search the notes")),
                (
                    "Ctrl+S",
                    &tr("Save now, asking first when a guardrail has flagged the edit"),
                ),
                ("Ctrl+W", &tr("Close the window")),
                ("Ctrl+Q", &tr("Quit")),
                ("F1", &tr("Show or hide the shortcuts")),
            ],
        ),
        omui::ShortcutSection::new(
            &tr("Editing"),
            &[
                ("Ctrl+Z", &tr("Undo")),
                ("Ctrl+Shift+Z", &tr("Redo")),
                ("Ctrl+B", &tr("Bold")),
                ("Ctrl+I", &tr("Italic")),
                ("Ctrl+K", &tr("Link")),
                ("Ctrl+Enter", &tr("Toggle the checkbox of the line")),
                ("Ctrl+V", &tr("Paste; a copied image or file becomes an attachment")),
                ("Ctrl++", &tr("Zoom the note's text in")),
                ("Ctrl+-", &tr("Zoom the note's text out")),
                ("Ctrl+0", &tr("Zoom the note's text back")),
            ],
        ),
        omui::ShortcutSection::new(
            &tr("Lists"),
            &[
                ("Up / Down", &tr("Pick a folder or note, opening it")),
                ("Right / Enter", &tr("Step deeper: the note list, then the note")),
                ("Left / Escape", &tr("Step back out")),
                ("Home / End", &tr("Jump to the start or the end of a list")),
            ],
        ),
    ]
}

/// The overlay, Ctrl+W and Ctrl+Q on the window, and `win.shortcuts`, which
/// More's menu opens the overlay with.
pub fn install(ui: &Rc<Ui>) {
    let overlay = omui::ShortcutOverlay::new(sections);
    overlay.install(&ui.window, true);
    // Closing goes through the window's close request either way: edits that
    // cannot be saved are asked about first.
    omui::quit_keys(&ui.window);
    let action = gio::SimpleAction::new("shortcuts", None);
    let window = ui.window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window.upgrade() {
            overlay.toggle(&window);
        }
    });
    ui.window.add_action(&action);
}
