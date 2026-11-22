//! The window's dialogs about syncing: linking Apple Notes, Sync status,
//! the one place a sync is told to delete most of the vault, Note history
//! and the Sync log. They are omui's, drawn inside the window.

use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use icloud_notes::sync::Kind;
use icloud_notes::syncmodel;
use omui::{tr, trn};
use serde_json::{Map, Value};

use crate::window::{Ui, as_is};

fn text_of<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A monospace, read-only text in a scrolled box.
fn readout(text: &str) -> (gtk::ScrolledWindow, gtk::TextView) {
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    view.buffer().set_text(text);
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    (scroller, view)
}

/// "Link your Apple Notes": clone the account signed in on this computer,
/// or sign in first in Apple's window.
pub fn onboarding(ui: &Rc<Ui>) {
    let (signed_in, apple_id) = {
        let b = ui.backend.borrow();
        (b.signed_in(), b.apple_id().to_string())
    };
    let body = move || {
        if signed_in {
            tr!(
                "This downloads all your Apple Notes into ~/Documents/Notes as Markdown, one file per note with the title as its first line, like in Notes. It uses the iCloud account signed in on this computer, {}. Apple Notes must not use Advanced Data Protection, because icloud-notes-sync cannot decrypt it.",
                apple_id
            )
        } else {
            tr(
                "This downloads all your Apple Notes into ~/Documents/Notes as Markdown, one file per note with the title as its first line, like in Notes. Apple's own sign-in window opens first (password and 2FA stay on Apple's pages); the sign-in is shared with the other iCloud apps. Apple Notes must not use Advanced Data Protection, because icloud-notes-sync cannot decrypt it.",
            )
        }
    };
    let dialog = omui::Dialog::new(Some(""), Some(""));
    dialog.set_heading_translated("Link your Apple Notes");
    // In the language of the moment, as long as it is open.
    dialog.bind(move |dialog| dialog.set_body(&body()));
    dialog.add_response("cancel", "");
    dialog.set_response_translated("cancel", "_Cancel");
    dialog.add_response("clone", "");
    dialog.set_response_translated("clone", "Clone my notes");
    dialog.set_response_appearance("clone", omui::Appearance::Suggested);
    dialog.set_default_response("clone");
    dialog.set_close_response("cancel");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |_, id| {
        if let (Some(ui), "clone") = (weak.upgrade(), id) {
            clone(&ui);
        }
    });
    dialog.present(&ui.window);
}

/// Clones the account icloud-session is signed in to, signing in first.
pub fn clone(ui: &Rc<Ui>) {
    if ui.backend.borrow().signed_in() {
        ui.run(Kind::Clone);
    } else {
        ui.clone_wanted.set(true);
        ui.sign_in();
    }
}

/// "Sync status": what the next push would do, note by note, and Sync now.
pub fn sync_status(ui: &Rc<Ui>) {
    if !ui.flush_edits() {
        return;
    }
    let running = match ui.backend.borrow_mut().start_read(&["--quiet", "status"]) {
        Ok(running) => running,
        Err(why) => return ui.set_notice(as_is(why)),
    };
    ui.set_busy_status(|| tr("Push preview…"));
    let weak = Rc::downgrade(ui);
    omui::background(
        move || running.wait(),
        move |finished| {
            let Some(ui) = weak.upgrade() else { return };
            let finished = finished.unwrap_or_default();
            let parsed = syncmodel::parse_status_json(&finished.stdout);
            let error = match parsed.get("error") {
                Some(_) if finished.exit_code != 0 => match engine_message(&finished.stderr) {
                    Some(message) => message,
                    None => text_of(&parsed, "error").to_string(),
                },
                Some(_) => text_of(&parsed, "error").to_string(),
                None => String::new(),
            };
            ui.backend.borrow_mut().log(&format!("(exit {})", finished.exit_code));
            if !error.is_empty() {
                ui.set_status(|| tr("Push preview failed. See log."));
                ui.set_notice(as_is(error));
                return;
            }
            ui.set_status(|| tr("Push preview done."));
            show_status(&ui, &parsed);
        },
    );
}

fn show_status(ui: &Rc<Ui>, parsed: &Map<String, Value>) {
    let entries: Vec<Map<String, Value>> = parsed
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_object().cloned())
        .collect();
    // The badges "push refused" and "remote changed" come from here.
    let refused: HashMap<String, String> = entries
        .iter()
        .filter(|e| !matches!(text_of(e, "resolution"), "ready" | "noop"))
        .map(|e| (text_of(e, "file").to_string(), text_of(e, "resolution").to_string()))
        .collect();
    ui.statuses.replace(refused);
    ui.show_vault();

    let unchanged = parsed.get("unchanged").and_then(Value::as_i64).unwrap_or(0);
    let nothing = entries.is_empty();
    let summary = move || {
        if nothing {
            tr("Nothing to sync: everything matches iCloud.")
        } else if unchanged > 0 {
            trn!("{n} notes already match iCloud.", unchanged)
        } else {
            tr("Every tracked note has a pending change.")
        }
    };
    let dialog = omui::Dialog::new(Some(""), Some(""));
    dialog.set_heading_translated("Sync status");
    // In the language of the moment, as long as it is open.
    dialog.bind(move |dialog| dialog.set_body(&summary()));
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    let notices: Vec<&str> = parsed
        .get("notices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !notices.is_empty() {
        column.append(&omui::label(&notices.join("\n"), "warning"));
    }
    for entry in &entries {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // The engine's words for a change, translated where the window knows them.
        let kind = match text_of(entry, "kind") {
            "createFolder" => tr("new folder"),
            "renameFolder" => tr("rename folder"),
            "deleteFolder" => tr("delete folder"),
            other => other.to_string(),
        };
        line.append(&omui::badge(&kind, ""));
        let resolution = text_of(entry, "resolution");
        let class = match resolution {
            "ready" => "success",
            "refused" => "error",
            _ => "warning",
        };
        let resolution = match resolution {
            "ready" => tr("ready"),
            "refused" => tr("refused"),
            "conflict" => tr("conflict"),
            other => other.to_string(),
        };
        line.append(&omui::badge(&resolution, class));
        line.append(
            &gtk::Label::builder()
                .label(text_of(entry, "file"))
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build(),
        );
        column.append(&line);
        let reason = text_of(entry, "reason");
        let remark = text_of(entry, "remark");
        if !reason.is_empty() {
            column.append(&omui::label(reason, "error"));
        } else if !remark.is_empty() {
            column.append(&omui::label(remark, "dim-label"));
        }
    }
    dialog.set_extra_child(Some(column.upcast_ref()));
    dialog.set_content_size(560, -1);
    dialog.add_response("close", "");
    dialog.set_response_translated("close", "_Close");
    dialog.add_response("sync", "");
    dialog.set_response_translated("sync", "Sync now");
    dialog.set_response_appearance("sync", omui::Appearance::Suggested);
    dialog.set_response_enabled("sync", !ui.backend.borrow().sync_running());
    dialog.set_close_response("close");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |_, id| {
        if let (Some(ui), "sync") = (weak.upgrade(), id) {
            ui.sync_now(false);
        }
    });
    dialog.present(&ui.window);
}

/// The one place a sync is told to delete most of the vault (`sync --yes`).
/// Keeping them is the default: Enter and Escape both leave everything as it is.
pub fn delete_missing(ui: &Rc<Ui>) {
    let refused = ui.deletions_refused.borrow().clone();
    // What the engine refused, in its English, then the window's own words.
    let body = move || {
        tr!(
            "{}\n\nDeleting moves every note whose file is gone to Recently Deleted in iCloud, on all your devices (recoverable there for about 30 days). Sync status lists them.",
            refused
        )
    };
    let dialog = omui::Dialog::new(Some(""), Some(""));
    dialog.set_heading_translated("Delete these notes from iCloud?");
    // In the language of the moment, as long as it is open.
    dialog.bind(move |dialog| dialog.set_body(&body()));
    dialog.add_response("delete", "");
    dialog.set_response_translated("delete", "Delete from iCloud");
    dialog.set_response_appearance("delete", omui::Appearance::Destructive);
    dialog.add_response("keep", "");
    dialog.set_response_translated("keep", "Keep them");
    dialog.set_response_appearance("keep", omui::Appearance::Suggested);
    dialog.set_default_response("keep");
    dialog.set_close_response("keep");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |_, id| {
        if let (Some(ui), "delete") = (weak.upgrade(), id) {
            ui.sync_now(true);
        }
    });
    dialog.present(&ui.window);
}

/// "Sync log": what the runs since the window opened (or the log was
/// cleared) did.
pub fn sync_log(ui: &Rc<Ui>) {
    let log = ui.backend.borrow().sync_log().to_string();
    let (scroller, view) = readout(&log);
    // The log is the engine's runs, in English as it wrote them.
    let dialog = omui::Dialog::new(Some(""), None);
    dialog.set_heading_translated("Sync log");
    dialog.set_extra_child(Some(scroller.upcast_ref()));
    dialog.set_content_size(600, 400);
    // Clear empties the log and leaves it open.
    dialog.add_response("clear", "");
    dialog.set_response_translated("clear", "Clear");
    dialog.set_response_closes("clear", false);
    dialog.set_response_enabled("clear", !log.is_empty());
    dialog.add_response("close", "");
    dialog.set_response_translated("close", "_Close");
    dialog.set_default_response("close");
    dialog.set_close_response("close");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |dialog, id| {
        if let (Some(ui), "clear") = (weak.upgrade(), id) {
            ui.backend.borrow_mut().clear_log();
            view.buffer().set_text("");
            // To the log before Clear is disabled with the keyboard on it: GTK then leaves the keyboard nowhere, and
            // omui's dialog brings it back only from an idle, until when Escape reaches no one.
            view.grab_focus();
            dialog.set_response_enabled("clear", false);
        }
    });
    dialog.present(&ui.window);
}

/// The message of the error the engine wrote on its stderr when a run
/// failed: in a machine mode its one-line error object, the last on stderr
/// (stdout holds only results). `None` when it wrote none (a crash).
fn engine_message(stderr: &[u8]) -> Option<String> {
    let document = icloud_session::output::error_line(stderr)?;
    let message = document["error"]["message"].as_str()?;
    (!message.is_empty()).then(|| message.to_string())
}

/// The engine's answer to a read, once it ends: its stdout, the message of
/// its error when it failed with one, and its exit code.
fn read(ui: &Rc<Ui>, args: Vec<String>, done: impl FnOnce(&Rc<Ui>, Vec<u8>, Option<String>, i32) + 'static) {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let running = match ui.backend.borrow_mut().start_read(&refs) {
        Ok(running) => running,
        Err(why) => return ui.set_notice(as_is(why)),
    };
    let weak = Rc::downgrade(ui);
    omui::background(
        move || running.wait(),
        move |finished| {
            if let Some(ui) = weak.upgrade() {
                let finished = finished.unwrap_or_default();
                let failure = engine_message(&finished.stderr);
                done(&ui, finished.stdout, failure, finished.exit_code);
            }
        },
    );
}

/// "Note history": snapshots from past pulls and pushes, newest first, each
/// diffed against the iCloud copy when picked.
pub fn history(ui: &Rc<Ui>) {
    let note = ui.backend.borrow().current_path();
    if ui.backend.borrow().current_note().is_empty() {
        return;
    }
    ui.set_busy_status(|| tr("History…"));
    let args = vec!["--quiet".into(), "history".into(), note.clone()];
    read(ui, args, move |ui, stdout, failure, code| {
        let parsed = syncmodel::parse_history_json(&stdout);
        if let Some(error) = parsed
            .get("error")
            .and_then(Value::as_str)
            .filter(|_| code != 0 || stdout.is_empty())
        {
            ui.set_status(|| tr("History failed. See log."));
            ui.set_notice(as_is(failure.as_deref().unwrap_or(error)));
            return;
        }
        ui.set_status(|| tr("History done."));
        let epochs: Vec<Map<String, Value>> = parsed
            .get("epochs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_object().cloned())
            .collect();
        show_history(ui, &note, epochs);
    });
}

fn show_history(ui: &Rc<Ui>, note: &str, epochs: Vec<Map<String, Value>>) {
    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
    for epoch in &epochs {
        // The engine's ISO 8601 time, as the language spells a moment out.
        let timestamp = text_of(epoch, "timestamp");
        let when = gtk::glib::DateTime::from_iso8601(timestamp, None)
            .and_then(|when| when.to_local())
            .map(|when| crate::window::long_when(&when))
            .unwrap_or_else(|_| match timestamp {
                "" => text_of(epoch, "id").to_string(),
                timestamp => timestamp.to_string(),
            });
        let changed: Vec<&str> = epoch
            .get("changed")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let row = omui::Row::new(&when, &changed.join(", "));
        list.append(&row.widget);
    }
    let lists = gtk::ScrolledWindow::builder().width_request(220).child(&list).build();
    let (diff, view) = readout(&tr("Pick a snapshot to diff it against the current iCloud copy."));
    let panes = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    panes.append(&lists);
    panes.append(&diff);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    column.append(&omui::label(
        &tr(
            "Snapshots from past pulls and pushes, newest first. Read-only here; to discard a note's local edits, run `icloud-notes restore <note> --yes` in a terminal.",
        ),
        "dim-label",
    ));
    column.append(&panes);
    let dialog = omui::Dialog::new(Some(""), None);
    dialog.set_heading_translated("Note history");
    dialog.set_extra_child(Some(column.upcast_ref()));
    dialog.set_content_size(620, 420);
    dialog.add_response("close", "");
    dialog.set_response_translated("close", "_Close");
    dialog.set_close_response("close");
    let weak = Rc::downgrade(ui);
    let note = note.to_string();
    list.connect_row_activated(move |_, row| {
        let (Some(ui), Some(epoch)) = (weak.upgrade(), epochs.get(row.index() as usize)) else {
            return;
        };
        let id = text_of(epoch, "id").to_string();
        let view = view.clone();
        let args = vec!["--json".into(), "diff".into(), note.clone(), id];
        read(&ui, args, move |ui, stdout, failure, _| {
            let document: Value = serde_json::from_slice(&stdout).unwrap_or_default();
            let text = match document["meta"]["text"].as_str() {
                Some(text) => text.to_string(),
                None => {
                    match failure {
                        Some(failure) => ui.set_notice(as_is(failure)),
                        None => ui.set_notice(|| tr("The diff failed. See the log.")),
                    }
                    String::new()
                }
            };
            view.buffer().set_text(&text);
        });
    });
    dialog.present(&ui.window);
}
