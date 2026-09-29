//! Preferences: where originals go, and whether to download them all.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use icloud_photos::config::DownloadMode;

use super::window::App;

pub fn show(app: &Rc<App>) {
    let settings = app.settings.borrow().clone();

    let folder = adw::ActionRow::builder()
        .title("Originals folder")
        .subtitle(settings.library_dir.display().to_string())
        .build();
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    folder.add_suffix(&choose);

    let modes = gtk::StringList::new(&["On demand", "All"]);
    let mode = adw::ComboRow::builder()
        .title("Download originals")
        .subtitle("On demand: when you open or download one. All: every original, in the background.")
        .model(&modes)
        .selected(if settings.download == DownloadMode::All { 1 } else { 0 })
        .build();

    let group = adw::PreferencesGroup::builder().title("Library").build();
    group.add(&folder);
    group.add(&mode);
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);

    let (a, row) = (app.clone(), folder.clone());
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
    mode.connect_selected_notify(move |row| {
        let all = row.selected() == 1;
        a.settings.borrow_mut().download = if all { DownloadMode::All } else { DownloadMode::OnDemand };
        save(&a);
        if all {
            a.queue_all_originals();
        } else if let Some(d) = a.downloader.borrow().as_ref() {
            d.clear_background();
        }
    });

    dialog.present(Some(&app.window));
}

fn save(app: &App) {
    if let Err(e) = app.settings.borrow().save(&app.dirs) {
        app.toast(&format!("Could not save preferences: {e}"));
    }
}
