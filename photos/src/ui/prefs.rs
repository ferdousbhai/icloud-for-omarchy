//! Preferences: where originals go, and whether to download them all.

use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use icloud_photos::config::DownloadMode;

use super::window::App;

pub fn show(app: &Rc<App>) {
    let settings = app.settings.borrow().clone();

    let folder = omui::Row::new("Originals folder", &settings.library_dir.display().to_string());
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    folder.add_suffix(&choose);

    let modes = gtk::StringList::new(&["On demand", "All"]);
    let (mode_row, mode) = omui::combo_row(
        "Download originals",
        "On demand: when you open or download one. All: every original, in the background.",
        &modes,
        if settings.download == DownloadMode::All { 1 } else { 0 },
    );

    let group = omui::Group::new("Library", "");
    group.add(&folder.widget);
    group.add(&mode_row.widget);
    let dialog = omui::Dialog::new(Some("Preferences"), None);
    dialog.set_extra_child(Some(group.widget.upcast_ref()));
    dialog.set_content_size(560, -1);
    dialog.add_response("close", "Close");

    let (a, row) = (app.clone(), folder);
    choose.connect_clicked(move |_| {
        let picker = gtk::FileDialog::builder()
            .title("Folder for Originals")
            .modal(true)
            .build();
        let current = a.settings.borrow().library_dir.clone();
        if current.exists() {
            picker.set_initial_folder(Some(&gio::File::for_path(&current)));
        }
        let (a, row) = (a.clone(), row.clone());
        picker.select_folder(Some(&a.window.clone()), gio::Cancellable::NONE, move |r| {
            let Some(path) = r.ok().and_then(|f| f.path()) else {
                return;
            };
            a.settings.borrow_mut().library_dir = path.clone();
            save(&a);
            row.set_subtitle(&path.display().to_string());
            if let Some(d) = a.downloader.borrow().as_ref() {
                d.set_library(path);
            }
        });
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

fn save(app: &App) {
    if let Err(e) = app.settings.borrow().save(&app.dirs) {
        app.toast(&format!("Could not save preferences: {e}"));
    }
}
