//! The window's dialogs about syncing: linking Apple Notes, Sync status,
//! the one place a sync is told to delete most of the vault, Note history
//! and the Sync log. They are omui's, drawn inside the window.

use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use icloud_notes::sync::Kind;
use icloud_notes::syncmodel;
use serde_json::{Map, Value};

use crate::window::Ui;

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
    let account = if signed_in {
        format!("It uses the iCloud account signed in on this computer, {apple_id}. ")
    } else {
        "Apple's own sign-in window opens first (password and 2FA stay on Apple's pages); the sign-in is shared with the other iCloud apps. ".to_string()
    };
    let body = format!(
        "This downloads all your Apple Notes into ~/Documents/Notes as Markdown, one file per note with the title as its first line, like in Notes. {account}Apple Notes must not use Advanced Data Protection, because icloud-notes-sync cannot decrypt it."
    );
    let dialog = omui::Dialog::new(Some("Link your Apple Notes"), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("clone", "Clone my notes");
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
        Err(why) => return ui.set_notice(&why),
    };
    ui.set_status("Push preview…");
    let weak = Rc::downgrade(ui);
    omui::background(
        move || running.wait(),
        move |finished| {
            let Some(ui) = weak.upgrade() else { return };
            let finished = finished.unwrap_or_default();
            let parsed = syncmodel::parse_status_json(&finished.stdout);
            let error = match parsed.get("error") {
                Some(_) if finished.exit_code != 0 => {
                    let document: Value = serde_json::from_slice(&finished.stdout).unwrap_or_default();
                    let message = document["error"]["message"].as_str().unwrap_or_default().to_string();
                    if message.is_empty() {
                        text_of(&parsed, "error").to_string()
                    } else {
                        message
                    }
                }
                Some(_) => text_of(&parsed, "error").to_string(),
                None => String::new(),
            };
            ui.backend.borrow_mut().log(&format!("(exit {})", finished.exit_code));
            if !error.is_empty() {
                ui.set_status("Push preview failed. See log.");
                ui.set_notice(&error);
                return;
            }
            ui.set_status("Push preview done.");
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
    let summary = if entries.is_empty() {
        "Nothing to sync: everything matches iCloud.".to_string()
    } else if unchanged > 0 {
        format!("{unchanged} note(s) already match iCloud.")
    } else {
        "Every tracked note has a pending change.".to_string()
    };
    let dialog = omui::Dialog::new(Some("Sync status"), Some(&summary));
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
        let kind = match text_of(entry, "kind") {
            "createFolder" => "new folder",
            "renameFolder" => "rename folder",
            "deleteFolder" => "delete folder",
            other => other,
        };
        line.append(&omui::badge(kind, ""));
        let resolution = text_of(entry, "resolution");
        let class = match resolution {
            "ready" => "success",
            "refused" => "error",
            _ => "warning",
        };
        line.append(&omui::badge(resolution, class));
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
    dialog.add_response("close", "Close");
    dialog.add_response("sync", "Sync now");
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
    let body = format!(
        "{refused}\n\nDeleting moves every note whose file is gone to Recently Deleted in iCloud, on all your devices (recoverable there for about 30 days). Sync status lists them."
    );
    let dialog = omui::Dialog::new(Some("Delete these notes from iCloud?"), Some(&body));
    dialog.add_response("delete", "Delete from iCloud");
    dialog.set_response_appearance("delete", omui::Appearance::Destructive);
    dialog.add_response("keep", "Keep them");
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
    let dialog = omui::Dialog::new(Some("Sync log"), None);
    dialog.set_extra_child(Some(scroller.upcast_ref()));
    dialog.set_content_size(600, 400);
    // Clear empties the log and leaves it open.
    dialog.add_response("clear", "Clear");
    dialog.set_response_closes("clear", false);
    dialog.set_response_enabled("clear", !log.is_empty());
    dialog.add_response("close", "Close");
    dialog.set_default_response("close");
    dialog.set_close_response("close");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |dialog, id| {
        if let (Some(ui), "clear") = (weak.upgrade(), id) {
            ui.backend.borrow_mut().clear_log();
            view.buffer().set_text("");
            dialog.set_response_enabled("clear", false);
        }
    });
    dialog.present(&ui.window);
}

/// The engine's answer to a read, once it ends: its stdout, or why not.
fn read(ui: &Rc<Ui>, args: Vec<String>, done: impl FnOnce(&Rc<Ui>, Vec<u8>, i32) + 'static) {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let running = match ui.backend.borrow_mut().start_read(&refs) {
        Ok(running) => running,
        Err(why) => return ui.set_notice(&why),
    };
    let weak = Rc::downgrade(ui);
    omui::background(
        move || running.wait(),
        move |finished| {
            if let Some(ui) = weak.upgrade() {
                let finished = finished.unwrap_or_default();
                done(&ui, finished.stdout, finished.exit_code);
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
    ui.set_status("History…");
    let args = vec!["--quiet".into(), "history".into(), note.clone()];
    read(ui, args, move |ui, stdout, code| {
        let parsed = syncmodel::parse_history_json(&stdout);
        if let Some(error) = parsed
            .get("error")
            .and_then(Value::as_str)
            .filter(|_| code != 0 || stdout.is_empty())
        {
            ui.set_status("History failed. See log.");
            ui.set_notice(error);
            return;
        }
        ui.set_status("History done.");
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
        let when = match text_of(epoch, "timestamp") {
            "" => text_of(epoch, "id"),
            timestamp => timestamp,
        };
        let changed: Vec<&str> = epoch
            .get("changed")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let row = omui::Row::new(when, &changed.join(", "));
        list.append(&row.widget);
    }
    let lists = gtk::ScrolledWindow::builder().width_request(220).child(&list).build();
    let (diff, view) = readout("Pick a snapshot to diff it against the current iCloud copy.");
    let panes = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    panes.append(&lists);
    panes.append(&diff);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    column.append(&omui::label(
        "Snapshots from past pulls and pushes, newest first. Read-only here; to discard a note's local edits, run `icloud-notes restore <note> --yes` in a terminal.",
        "dim-label",
    ));
    column.append(&panes);
    let dialog = omui::Dialog::new(Some("Note history"), None);
    dialog.set_extra_child(Some(column.upcast_ref()));
    dialog.set_content_size(620, 420);
    dialog.add_response("close", "Close");
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
        read(&ui, args, move |ui, stdout, _| {
            let document: Value = serde_json::from_slice(&stdout).unwrap_or_default();
            let text = match document["meta"]["text"].as_str() {
                Some(text) => text.to_string(),
                None => {
                    let message = document["error"]["message"]
                        .as_str()
                        .unwrap_or("The diff failed. See the log.");
                    ui.set_notice(message);
                    String::new()
                }
            };
            view.buffer().set_text(&text);
        });
    });
    dialog.present(&ui.window);
}
