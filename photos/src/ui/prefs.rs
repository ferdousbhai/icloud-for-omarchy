//! Preferences: where originals go, and whether to download them all.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use icloud_photos::config::DownloadMode;
use omui::{FilePicker, PickError, PickOptions, tr};

use super::window::App;

pub fn show(app: &Rc<App>) {
    let settings = app.settings.borrow().clone();

    // The folder's path is the user's own, shown as it is.
    let folder = omui::Row::new("", &settings.library_dir.display().to_string());
    let choose = gtk::Button::builder().valign(gtk::Align::Center).build();
    folder.add_suffix(&choose);

    let modes = gtk::StringList::new(&[&tr("On demand"), &tr("All")]);
    let selected = if settings.download == DownloadMode::All { 1 } else { 0 };
    let (mode_row, mode) = omui::combo_row("", "", &modes, selected);

    // The titles, the button and the heading follow a switch of language while it is open.
    folder.set_title_translated("Originals folder");
    omui::i18n::bind_label(&choose, "Choose…");
    mode_row.set_title_translated("Download originals");
    mode_row
        .set_subtitle_translated("On demand: when you open or download one. All: every original, in the background.");
    let group = omui::Group::new("", "");
    group.set_title_translated("Library");
    group.add(&folder.widget);
    group.add(&mode_row.widget);
    let dialog = omui::Dialog::new(Some(""), None);
    dialog.set_heading_translated("Preferences");
    dialog.set_extra_child(Some(group.widget.upcast_ref()));
    dialog.set_content_size(560, -1);
    dialog.add_response("close", "");
    dialog.set_response_translated("close", "Close");
    // Each choice is saved as it is made, so closing it any way loses nothing.
    dialog.set_close_response("close");

    let (a, row) = (app.clone(), folder);
    choose.connect_clicked(move |_| {
        let current = a.settings.borrow().library_dir.clone();
        let (chosen, failed) = (a.clone(), a.clone());
        let row = row.clone();
        ask_folder(
            &*a.picker,
            &current,
            move |path| {
                chosen.settings.borrow_mut().library_dir = path.clone();
                save(&chosen);
                row.set_subtitle(&path.display().to_string());
                if let Some(d) = chosen.downloader.borrow().as_ref() {
                    d.set_library(path);
                }
            },
            move |message| failed.toast(&message),
        );
    });

    let a = app.clone();
    mode.connect_selected_notify(move |mode| {
        let all = mode.selected() == 1;
        a.settings.borrow_mut().download = if all { DownloadMode::All } else { DownloadMode::OnDemand };
        save(&a);
        if all {
            a.queue_all_originals();
        } else if let Some(d) = a.downloader.borrow().as_ref() {
            d.clear_background();
        }
    });

    dialog.present(&app.window);
}

/// Asks `picker` for the folder originals go to, starting in `current` when
/// it is there. `chosen` gets the folder; `failed` the message, in the
/// language in use, of a file chooser that could not be shown, for the
/// window to show. Closing the chooser is no answer and no error.
pub fn ask_folder(
    picker: &dyn FilePicker,
    current: &Path,
    chosen: impl FnOnce(PathBuf) + 'static,
    failed: impl FnOnce(String) + 'static,
) {
    let options = PickOptions {
        directory: true,
        folder: current.exists().then(|| current.to_owned()),
        ..PickOptions::new(&tr("Folder for Originals"))
    };
    picker.open(
        options,
        Box::new(move |picked| match picked {
            Ok(folders) => {
                if let Some(folder) = folders.into_iter().next() {
                    chosen(folder);
                }
            }
            Err(PickError::Cancelled) => {}
            Err(e) => failed(e.to_string()),
        }),
    );
}

fn save(app: &App) {
    if let Err(e) = app.settings.borrow().save(&app.dirs) {
        app.toast(&omui::tr!("Could not save preferences: {}", e));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use omui::{FakeFilePicker, PickError};

    /// The folder chooser asks for a folder, starting at the one in use; the folder chosen is taken, closing it is
    /// nothing, and a chooser that failed is said.
    #[test]
    fn the_originals_folder_is_asked_of_the_file_chooser() {
        let _language = omui::i18n::lock_language();
        let picker = FakeFilePicker::new();
        let current = tempfile::tempdir().unwrap();
        let got: Rc<RefCell<Vec<String>>> = Rc::default();
        let ask = |picker: &FakeFilePicker| {
            let (chosen, failed) = (got.clone(), got.clone());
            super::ask_folder(
                picker,
                current.path(),
                move |path| chosen.borrow_mut().push(format!("chosen {}", path.display())),
                move |message| failed.borrow_mut().push(format!("failed {message}")),
            );
        };
        picker.answer(Ok(vec![PathBuf::from("/pictures/originals")]));
        ask(&picker);
        ask(&picker);
        picker.answer(Err(PickError::NoAnswer));
        ask(&picker);
        assert_eq!(
            *got.borrow(),
            [
                "chosen /pictures/originals",
                "failed The desktop's file chooser did not answer."
            ]
        );
        let asked = picker.asked();
        assert_eq!(asked.len(), 3);
        let (saving, options) = &asked[0];
        assert!(!saving && options.directory && !options.multiple);
        assert_eq!(options.folder.as_deref(), Some(current.path()));
        assert_eq!(options.title, "Folder for Originals");
    }
}
