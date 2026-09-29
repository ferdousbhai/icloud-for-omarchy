//! Upload: file chooser (or drag and drop), a progress dialog, then a sync
//! once iCloud has indexed the new assets.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use icloud_photos::transport::Error;
use icloud_photos::upload::{BatchEvent, BatchSummary, Step, Uploaded, is_supported, upload_batch};

use super::window::{App, Msg};

pub enum UploadMsg {
    Step { index: usize, total: usize, name: String, step: Step },
    FileDone { name: String, result: Result<Uploaded, Error> },
    Finished { uploaded: usize, duplicates: usize, failed: usize, stopped: bool },
}

pub struct UploadUi {
    dialog: adw::Dialog,
    progress: gtk::ProgressBar,
    detail: gtk::Label,
    button: gtk::Button,
    stop: Arc<AtomicBool>,
    total: usize,
    errors: Vec<String>,
}

pub fn choose(app: &Rc<App>) {
    if app.upload.borrow().is_some() {
        return;
    }
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Photos and videos"));
    filter.add_mime_type("image/*");
    filter.add_mime_type("video/*");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Upload to iCloud Photos").modal(true).filters(&filters).build();
    let a = app.clone();
    dialog.open_multiple(Some(&app.window), gio::Cancellable::NONE, move |result| {
        let Ok(files) = result else { return };
        let paths: Vec<PathBuf> = (0..files.n_items())
            .filter_map(|i| files.item(i).and_downcast::<gio::File>())
            .filter_map(|f| f.path())
            .collect();
        start(&a, paths);
    });
}

/// Dropping files on the window uploads them too.
pub fn connect_drop(app: &Rc<App>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let a = app.clone();
    target.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        start(&a, paths);
        true
    });
    app.window.add_controller(target);
}

fn start(app: &Rc<App>, paths: Vec<PathBuf>) {
    let (paths, skipped): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| p.is_file() && is_supported(p));
    if !skipped.is_empty() {
        app.toast(&format!("Skipped {} file(s) iCloud Photos does not take", skipped.len()));
    }
    if paths.is_empty() || app.upload.borrow().is_some() {
        return;
    }
    let Some(t) = app.transport() else {
        app.toast("Not connected to iCloud yet");
        return;
    };

    let progress = gtk::ProgressBar::builder().show_text(true).build();
    let detail = gtk::Label::builder().xalign(0.0).wrap(true).build();
    detail.add_css_class("dim-label");
    let button = gtk::Button::builder().label("Stop After This File").halign(gtk::Align::End).build();
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(12).margin_bottom(18).margin_start(18).margin_end(18).build();
    content.append(&progress);
    content.append(&detail);
    content.append(&button);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::builder().show_end_title_buttons(false).build());
    tv.set_content(Some(&content));
    let dialog = adw::Dialog::builder().title("Uploading to iCloud").content_width(440).child(&tv).can_close(false).build();

    let stop = Arc::new(AtomicBool::new(false));
    let (a, s) = (app.clone(), stop.clone());
    button.connect_clicked(move |b| {
        if a.upload.borrow().as_ref().is_some_and(|u| u.dialog.can_close()) {
            // Release the borrow first: force_close runs the closed handler,
            // which borrows `upload` again.
            let ui = a.upload.borrow_mut().take();
            if let Some(u) = ui {
                u.dialog.force_close();
            }
        } else {
            s.store(true, Ordering::Relaxed);
            b.set_sensitive(false);
            b.set_label("Stopping…");
        }
    });
    dialog.present(Some(&app.window));
    *app.upload.borrow_mut() = Some(UploadUi { dialog, progress, detail, button, stop: stop.clone(), total: paths.len(), errors: Vec::new() });

    let tx = app.tx.clone();
    std::thread::spawn(move || {
        let send = |m: UploadMsg| {
            let _ = tx.send_blocking(Msg::Upload(m));
        };
        let summary = upload_batch(&*t, &paths, Duration::from_secs(60), &|| stop.load(Ordering::Relaxed), &|event| match event {
            BatchEvent::Step { index, total, name, step } => send(UploadMsg::Step { index, total, name, step }),
            BatchEvent::FileDone { name, result, .. } => send(UploadMsg::FileDone { name, result }),
        });
        let BatchSummary { uploaded, duplicates, failed, stopped } = summary;
        send(UploadMsg::Finished { uploaded, duplicates, failed, stopped });
    });
}

pub fn on_msg(app: &Rc<App>, msg: UploadMsg) {
    let mut guard = app.upload.borrow_mut();
    let Some(ui) = guard.as_mut() else { return };
    match msg {
        UploadMsg::Step { index, total, name, step } => {
            let (frac_in_file, text) = match step {
                Step::Reserving => (0.05, format!("Preparing {name}…")),
                Step::Sending { bytes } => (0.2, format!("Sending {name} ({})…", glib::format_size(bytes))),
                Step::Registering => (0.85, format!("Adding {name} to your library…")),
                Step::Ingesting { progress } => (1.0, format!("iCloud is processing the upload ({progress}%)…")),
            };
            let done = (index as f64 + frac_in_file) / total.max(1) as f64;
            ui.progress.set_fraction(done.min(1.0));
            ui.progress.set_text(Some(&format!("{} of {total}", (index + 1).min(total))));
            ui.detail.set_label(&text);
        }
        UploadMsg::FileDone { name, result: Err(e) } => {
            if e.is_sign_in() {
                drop(guard);
                app.fail("Upload failed", &e);
                return;
            }
            ui.errors.push(if name.is_empty() { e.to_string() } else { format!("{name}: {e}") });
        }
        UploadMsg::FileDone { .. } => {}
        UploadMsg::Finished { uploaded, duplicates, failed, stopped } => {
            ui.progress.set_fraction(1.0);
            let mut lines = vec![match uploaded {
                1 => "1 item uploaded.".to_owned(),
                n => format!("{n} items uploaded."),
            }];
            if duplicates > 0 {
                lines.push(format!("{duplicates} already in iCloud."));
            }
            if failed > 0 {
                lines.push(format!("{failed} failed."));
            }
            if stopped {
                lines.push(format!("Stopped; {} not sent.", ui.total - uploaded - duplicates - failed));
            }
            lines.extend(ui.errors.iter().take(5).cloned());
            ui.detail.set_label(&lines.join("\n"));
            ui.dialog.set_can_close(true);
            ui.button.set_label("Close");
            ui.button.set_sensitive(true);
            let _ = ui.stop.load(Ordering::Relaxed);
            let (d, a) = (ui.dialog.clone(), app.clone());
            d.connect_closed(move |_| {
                a.upload.borrow_mut().take();
            });
            drop(guard);
            if uploaded > 0 {
                // CloudKit takes ~15-20 s to make new assets queryable.
                app.sync();
                let a = app.clone();
                glib::timeout_add_seconds_local_once(20, move || a.sync());
            }
        }
    }
}
