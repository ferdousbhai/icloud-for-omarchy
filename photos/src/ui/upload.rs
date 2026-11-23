//! Upload: file chooser (or drag and drop), a progress dialog, then a sync
//! once iCloud has indexed the new assets.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use icloud_photos::upload::{BatchEvent, BatchSummary, Step, is_supported, upload_batch};
use omui::{FileFilter, FilePicker, PickError, PickOptions, i18n, tr, trn};

use super::window::{App, Msg};
use super::{Relabel, relabel};

pub enum UploadMsg {
    Event(BatchEvent),
    /// The batch is over; an error is the upload service not reached at all.
    Finished(icloud_photos::transport::Result<BatchSummary>),
}

pub struct UploadUi {
    dialog: omui::Dialog,
    progress: gtk::ProgressBar,
    /// "2 of 5" on the bar, and the step or the summary under it, each built again after a switch of language.
    count: Relabel<gtk::ProgressBar>,
    detail: Relabel<gtk::Label>,
    button: gtk::Button,
    total: usize,
    /// Each file that failed, with why (the library's words, in English).
    errors: Vec<String>,
    /// The application, held from the first file until the batch is over: an upload goes on, windowless, after
    /// the last window has closed, and the app ends once it is done.
    held: Option<omui::Hold>,
}

pub fn choose(app: &Rc<App>) {
    if app.upload.borrow().is_some() {
        return;
    }
    let (a, failed) = (app.clone(), app.clone());
    ask_files(
        &*app.picker,
        move |paths| start(&a, paths),
        move |message| failed.toast(&message),
    );
}

/// Asks `picker` for photos and videos to upload. `chosen` gets the files;
/// `failed` the message, in the language in use, of a file chooser that
/// could not be shown, for the window to show. Closing the chooser is no
/// answer and no error.
pub fn ask_files(
    picker: &dyn FilePicker,
    chosen: impl FnOnce(Vec<PathBuf>) + 'static,
    failed: impl FnOnce(String) + 'static,
) {
    let options = PickOptions {
        multiple: true,
        filters: vec![FileFilter::new(&tr("Photos and videos"), &[]).mime_types(&["image/*", "video/*"])],
        ..PickOptions::new(&tr("Upload to iCloud Photos"))
    };
    picker.open(
        options,
        Box::new(move |picked| match picked {
            Ok(files) => chosen(files),
            Err(PickError::Cancelled) => {}
            Err(e) => failed(e.to_string()),
        }),
    );
}

/// Dropping files on the window uploads them too.
pub fn connect_drop(app: &Rc<App>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let a = app.clone();
    target.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        start(&a, paths);
        true
    });
    app.window.add_controller(target);
}

/// Uploads `paths` (those iCloud Photos takes), with the progress dialog.
pub fn start(app: &Rc<App>, paths: Vec<PathBuf>) {
    let (paths, skipped): (Vec<PathBuf>, Vec<PathBuf>) =
        paths.into_iter().partition(|p| p.is_file() && is_supported(p));
    if !skipped.is_empty() {
        app.toast(&trn(
            "Skipped {n} files iCloud Photos does not take",
            skipped.len() as i64,
        ));
    }
    if paths.is_empty() || app.upload.borrow().is_some() {
        return;
    }
    let Some(t) = app.transport() else {
        app.toast(&tr("Not connected to iCloud yet"));
        return;
    };

    let progress = gtk::ProgressBar::builder().show_text(true).build();
    let count = Relabel::new(&progress, |bar, text| bar.set_text(Some(text)));
    let label = gtk::Label::builder().xalign(0.0).wrap(true).build();
    label.add_css_class("dim-label");
    let detail = relabel(&label);
    let button = gtk::Button::builder().halign(gtk::Align::End).build();
    i18n::bind_label(&button, "Stop After This File");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    content.append(&progress);
    content.append(&label);
    content.append(&button);
    // Neither Escape nor anything else closes it while files are going up:
    // the batch would carry on unseen. It gets its Close once it is over.
    let dialog = omui::Dialog::new(Some(""), None);
    dialog.set_heading_translated("Uploading to iCloud");
    dialog.set_extra_child(Some(content.upcast_ref()));
    dialog.set_content_size(440, -1);
    dialog.set_can_close(false);

    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    button.connect_clicked(move |b| {
        s.store(true, Ordering::Relaxed);
        b.set_sensitive(false);
        i18n::bind_label(b, "Stopping…");
    });
    dialog.present(&app.window);
    *app.upload.borrow_mut() = Some(UploadUi {
        dialog,
        progress,
        count,
        detail,
        button,
        total: paths.len(),
        errors: Vec::new(),
        held: Some(omui::hold()),
    });

    let tx = app.tx.clone();
    std::thread::spawn(move || {
        let send = |m: UploadMsg| {
            let _ = tx.send_blocking(Msg::Upload(m));
        };
        let summary = upload_batch(
            &*t,
            &paths,
            Duration::from_secs(60),
            &|| stop.load(Ordering::Relaxed),
            &|event| send(UploadMsg::Event(event)),
        );
        send(UploadMsg::Finished(summary));
    });
}

pub fn on_msg(app: &Rc<App>, msg: UploadMsg) {
    let mut guard = app.upload.borrow_mut();
    let Some(ui) = guard.as_mut() else { return };
    match msg {
        UploadMsg::Event(BatchEvent::Step {
            index,
            total,
            name,
            step,
        }) => {
            let frac_in_file = match step {
                Step::Reserving => 0.05,
                Step::Sending { .. } => 0.2,
                Step::Registering => 0.85,
                Step::Ingesting { .. } => 1.0,
            };
            // The file's name is the user's, as it is.
            ui.detail.set(move || match step {
                Step::Reserving => omui::tr!("Preparing {}…", name),
                Step::Sending { bytes } => omui::tr!("Sending {} ({})…", name, super::format_size(bytes)),
                Step::Registering => omui::tr!("Adding {} to your library…", name),
                Step::Ingesting { progress } => {
                    omui::tr!("iCloud is processing the upload ({}%)…", i18n::format_int(progress))
                }
            });
            let done = (index as f64 + frac_in_file) / total.max(1) as f64;
            ui.progress.set_fraction(done.min(1.0));
            let at = (index + 1).min(total);
            ui.count
                .set(move || omui::tr!("{} of {}", i18n::format_int(at as i64), i18n::format_int(total as i64)));
        }
        UploadMsg::Event(BatchEvent::FileDone {
            name, result: Err(e), ..
        }) => {
            if e.is_sign_in() {
                drop(guard);
                app.fail(&tr("Upload failed"), &e);
                return;
            }
            ui.errors.push(format!("{name}: {e}"));
        }
        UploadMsg::Event(BatchEvent::FileDone { .. }) => {}
        UploadMsg::Finished(result) => {
            let mut sign_in = None;
            let BatchSummary {
                uploaded,
                duplicates,
                failed,
                stopped,
            } = result.unwrap_or_else(|e| {
                if e.is_sign_in() {
                    sign_in = Some(e);
                } else {
                    ui.errors.push(e.to_string());
                }
                BatchSummary {
                    failed: ui.total,
                    ..Default::default()
                }
            });
            ui.progress.set_fraction(1.0);
            let not_sent = stopped.then(|| ui.total - uploaded - duplicates - failed);
            let errors: Vec<String> = ui.errors.iter().take(5).cloned().collect();
            ui.detail
                .set(move || summary(uploaded, duplicates, failed, not_sent, &errors));
            ui.button.set_visible(false);
            // The batch is over: nothing more to wait for, with a window or without.
            ui.held.take();
            ui.dialog.add_response("close", "");
            ui.dialog.set_response_translated("close", "Close");
            ui.dialog.set_default_response("close");
            ui.dialog.set_close_response("close");
            ui.dialog.set_can_close(true);
            let a = app.clone();
            ui.dialog.connect_response(move |_, _| {
                a.upload.borrow_mut().take();
            });
            drop(guard);
            if let Some(e) = sign_in {
                app.fail(&tr("Upload failed"), &e);
            }
            if uploaded > 0 {
                // CloudKit takes ~15-20 s to make new assets queryable.
                app.sync();
                let a = app.clone();
                glib::timeout_add_seconds_local_once(20, move || a.sync());
            }
        }
    }
}

/// What a finished batch says, one line each, in the language in use: how
/// many went up, were there already, failed, or were not sent after Stop,
/// then the first few errors.
fn summary(uploaded: usize, duplicates: usize, failed: usize, not_sent: Option<usize>, errors: &[String]) -> String {
    let mut lines = vec![trn("{n} items uploaded.", uploaded as i64)];
    if duplicates > 0 {
        lines.push(trn("{n} already in iCloud.", duplicates as i64));
    }
    if failed > 0 {
        lines.push(trn("{n} failed.", failed as i64));
    }
    if let Some(n) = not_sent {
        lines.push(trn("Stopped; {n} not sent.", n as i64));
    }
    lines.extend(errors.iter().cloned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use omui::{FakeFilePicker, PickError};

    /// Uploading asks the file chooser for any number of photos and videos; what is chosen goes up, closing it
    /// is nothing, and a chooser that could not be shown says so in the window.
    #[test]
    fn uploads_are_chosen_in_the_file_chooser() {
        let _language = omui::i18n::lock_language();
        let picker = FakeFilePicker::new();
        let got: Rc<RefCell<Vec<String>>> = Rc::default();
        let ask = |picker: &FakeFilePicker| {
            let (chosen, failed) = (got.clone(), got.clone());
            super::ask_files(
                picker,
                move |paths| chosen.borrow_mut().push(format!("{paths:?}")),
                move |message| failed.borrow_mut().push(format!("failed {message}")),
            );
        };
        picker.answer(Ok(vec![PathBuf::from("/a.jpg"), PathBuf::from("/b.mov")]));
        ask(&picker);
        ask(&picker);
        // The window is told omui's words for a chooser that would not open, not the portal's.
        picker.answer(Err(PickError::Refused(omui::PickDetail::new(
            "Portal operation not allowed",
        ))));
        ask(&picker);
        assert_eq!(
            *got.borrow(),
            [
                r#"["/a.jpg", "/b.mov"]"#,
                "failed The desktop's file chooser would not open."
            ]
        );
        let (saving, options) = &picker.asked()[0];
        assert!(!saving && options.multiple && !options.directory);
        assert_eq!(options.title, "Upload to iCloud Photos");
        assert_eq!(options.filters[0].mime_types, ["image/*", "video/*"]);
    }

    #[test]
    fn a_finished_batch_counts_each_outcome() {
        // The plural forms are the catalog's.
        omui::i18n::register("en", crate::ENGLISH);
        assert_eq!(super::summary(1, 0, 0, None, &[]), "1 item uploaded.");
        assert_eq!(
            super::summary(2, 1, 1, Some(3), &["x.jpg: too large".into()]),
            "2 items uploaded.\n1 already in iCloud.\n1 failed.\nStopped; 3 not sent.\nx.jpg: too large"
        );
    }
}
