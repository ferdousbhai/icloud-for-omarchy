//! What the toolbar's buttons, the menus, the rows' right clicks, a drag and
//! the keys do: new, rename, move and delete notes and folders, save, and
//! format the note's text. Each is a window action (`win.<name>`), which is
//! also what the preview script drives over D-Bus.

use std::rc::Rc;

use gtk::{gdk, gio, glib, prelude::*};
use icloud_notes::backend::Refusal;
use icloud_notes::syncmodel;
use omui::tr;

use crate::lists::{self, Item};
use crate::window::{Ui, as_is};

/// What an action does to the window, with its parameter.
type Activate = Box<dyn Fn(&Rc<Ui>, Option<&glib::Variant>)>;

/// A step of the editor's font zoom.
const ZOOM_STEP: f64 = 1.2;

pub fn add(ui: &Rc<Ui>) {
    let action = |name: &str, parameter: Option<&glib::VariantTy>, f: Activate| {
        let action = gio::SimpleAction::new(name, parameter);
        let weak = Rc::downgrade(ui);
        action.connect_activate(move |_, parameter| {
            if let Some(ui) = weak.upgrade() {
                f(&ui, parameter);
            }
        });
        ui.window.add_action(&action);
    };
    let text = |parameter: Option<&glib::Variant>| parameter.and_then(|p| p.get::<String>()).unwrap_or_default();
    // A vault-relative note path, as `icloud-notes` names notes.
    action(
        "open-note",
        Some(glib::VariantTy::STRING),
        Box::new(move |ui, parameter| {
            let path = text(parameter);
            let (folder, file) = path.rsplit_once('/').unwrap_or(("", &path));
            if ui.open_note(folder, file) && ui.split.is_collapsed() {
                ui.split.set_show_content(true);
            }
        }),
    );
    action(
        "refresh",
        None,
        Box::new(|ui, _| {
            ui.backend.borrow_mut().refresh();
            ui.show_vault();
        }),
    );
    action("new-note", None, Box::new(|ui, _| new_note(ui)));
    action("new-folder", None, Box::new(|ui, _| new_folder(ui)));
    action("delete-note", None, Box::new(|ui, _| delete_note(ui)));
    action("rename-note", None, Box::new(|ui, _| rename_note(ui)));
    action("move-note", None, Box::new(|ui, _| move_note(ui)));
    action("rename-folder", None, Box::new(|ui, _| rename_folder(ui)));
    action("delete-folder", None, Box::new(|ui, _| delete_folder(ui)));
    action("save", None, Box::new(|ui, _| save(ui)));
    action("find", None, Box::new(|ui, _| ui.focus_search()));
    action(
        "export-pdf",
        None,
        Box::new(|ui, _| {
            if !ui.flush_edits() {
                return;
            }
            let result = ui.backend.borrow_mut().export_pdf(&crate::pdf::render);
            match result {
                Ok(pdf) => {
                    let name = pdf.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    ui.set_status(move || tr!("Exported {} next to the note.", name));
                }
                Err(refusal) => ui.set_notice(as_is(refusal.message)),
            }
        }),
    );
    action(
        "conflict-as-text",
        None,
        Box::new(|ui, _| {
            ui.conflicts.as_text.set(true);
            ui.show_vault();
        }),
    );
    action(
        "recover-conflict",
        Some(glib::VariantTy::STRING),
        Box::new(move |ui, parameter| {
            let how = text(parameter);
            let result = ui.backend.borrow_mut().recover_conflicted_note(&how);
            match result {
                Ok(recovered) => {
                    ui.set_notice(as_is(recovered.message));
                    ui.load_editor();
                    ui.show_vault();
                }
                Err(refusal) => ui.set_notice(as_is(refusal.message)),
            }
        }),
    );
    action(
        "resolve-conflicts",
        None,
        Box::new(|ui, _| {
            let choices = ui.conflicts.choices();
            let result = ui.backend.borrow_mut().resolve_conflicts(&choices);
            if result.is_ok() {
                ui.clear_notice();
                ui.load_editor();
            }
            shown(ui, result);
        }),
    );
    action("sync-status", None, Box::new(|ui, _| crate::dialogs::sync_status(ui)));
    action("sync-log", None, Box::new(|ui, _| crate::dialogs::sync_log(ui)));
    action("history", None, Box::new(|ui, _| crate::dialogs::history(ui)));
    action("checklist", None, Box::new(|ui, _| toggle_task(ui)));
    action("bold", None, Box::new(|ui, _| wrap_selection(ui, "**", "**")));
    action("italic", None, Box::new(|ui, _| wrap_selection(ui, "*", "*")));
    action("link", None, Box::new(|ui, _| insert_link(ui)));
    action("zoom-in", None, Box::new(|ui, _| ui.zoom(ui.zoom.get() * ZOOM_STEP)));
    action("zoom-out", None, Box::new(|ui, _| ui.zoom(ui.zoom.get() / ZOOM_STEP)));
    action("zoom-reset", None, Box::new(|ui, _| ui.zoom(1.0)));

    let app = ui.window.application().expect("the window is the application's");
    for (name, keys) in [
        ("win.new-note", &["<Ctrl>n"][..]),
        ("win.save", &["<Ctrl>s"]),
        ("win.find", &["<Ctrl>f"]),
        ("win.bold", &["<Ctrl>b"]),
        ("win.italic", &["<Ctrl>i"]),
        ("win.link", &["<Ctrl>k"]),
        ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
        ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
        ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
        // Closing and quitting, Ctrl+W and Ctrl+Q, are omui's (keys.rs).
    ] {
        app.set_accels_for_action(name, keys);
    }
    // Ctrl+Enter toggles the checkbox only from the editor, where Enter is text.
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        if ctrl && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) {
            toggle_task(&ui);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    ui.editor.view.add_controller(keys);

    let weak = Rc::downgrade(ui);
    ui.editor.title.connect_activate(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.editor.view.grab_focus();
        }
    });
    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(ui);
    focus.connect_leave(move |_| {
        if let Some(ui) = weak.upgrade() {
            commit_title(&ui);
        }
    });
    ui.editor.title.add_controller(focus);
}

/// Which actions can run now, as the Qt window's buttons were enabled.
pub fn update(ui: &Ui) {
    let b = ui.backend.borrow();
    let note = !b.current_note().is_empty();
    let unlocked = note && b.read_only_reason().is_empty();
    let syncing = b.sync_running();
    let folder = !b.current_folder().is_empty();
    // Not while conflict blocks are being picked from.
    let formatting = unlocked && ui.editor.view.is_editable() && !ui.conflicts.picker.is_visible();
    let has_targets = !move_targets(b.folders(), b.current_folder()).is_empty();
    let cloned = b.vault().cloned();
    for (name, on) in [
        ("new-note", !syncing),
        ("new-folder", !syncing),
        ("delete-note", note && !syncing),
        ("rename-note", unlocked && !syncing),
        ("move-note", unlocked && !syncing && has_targets),
        ("rename-folder", folder),
        ("sync-status", cloned && !syncing),
        ("history", note && cloned && !syncing),
        ("export-pdf", note),
        ("delete-folder", folder),
        ("checklist", formatting),
        ("bold", formatting),
        ("italic", formatting),
        ("link", formatting),
        ("attach", formatting),
    ] {
        enable(ui, name, on);
    }
}

/// Lets the window's action `name` run, or not, and so its buttons.
pub fn enable(ui: &Ui, name: &str, on: bool) {
    if let Some(action) = ui.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
        action.set_enabled(on);
    }
}

/// The folders the open note can move to: every other folder but the vault itself.
fn move_targets(folders: &[String], current: &str) -> Vec<String> {
    folders
        .iter()
        .filter(|f| !f.is_empty() && *f != current)
        .cloned()
        .collect()
}

/// A dialog asking for a name: `heading`, a field, an optional hint, and
/// OK, which runs `ok` with what was typed (trimmed, never empty).
fn prompt(
    ui: &Rc<Ui>,
    heading: &str,
    placeholder: &str,
    initial: &str,
    hint: &str,
    ok: impl Fn(&Rc<Ui>, &str) + 'static,
) {
    let dialog = omui::Dialog::new(Some(heading), None);
    let field = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .text(initial)
        .activates_default(true)
        .width_request(300)
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    column.append(&field);
    if !hint.is_empty() {
        let hint = omui::label(hint, "dim-label");
        hint.set_max_width_chars(40);
        column.append(&hint);
    }
    dialog.set_extra_child(Some(column.upcast_ref()));
    dialog.add_response("cancel", "");
    dialog.set_response_translated("cancel", "_Cancel");
    dialog.add_response("ok", "");
    dialog.set_response_translated("ok", "_OK");
    dialog.set_response_appearance("ok", omui::Appearance::Suggested);
    dialog.set_default_response("ok");
    dialog.set_close_response("cancel");
    let weak = Rc::downgrade(ui);
    let entry = field.clone();
    dialog.connect_response(move |_, id| {
        let value = entry.text().trim().to_string();
        if let Some(ui) = weak.upgrade()
            && id == "ok"
            && !value.is_empty()
        {
            ok(&ui, &value);
        }
    });
    dialog.present(&ui.window);
    field.grab_focus();
}

/// A refusal's message in the status line, or the vault shown again.
fn shown(ui: &Ui, result: Result<(), Refusal>) {
    match result {
        Err(refusal) => ui.set_notice(as_is(refusal.message)),
        Ok(()) => ui.show_vault(),
    }
}

fn new_note(ui: &Rc<Ui>) {
    if ui.backend.borrow().sync_running() {
        return;
    }
    prompt(ui, &tr("New note"), &tr("Note title"), "", "", |ui, title| {
        if !ui.flush_edits() {
            return;
        }
        ui.backend.borrow_mut().new_note(title);
        ui.load_editor();
        ui.show_vault();
        if ui.split.is_collapsed() {
            ui.split.set_show_content(true);
        }
        ui.editor.view.grab_focus();
    });
}

fn new_folder(ui: &Rc<Ui>) {
    prompt(ui, &tr("New folder"), &tr("Folder name"), "", "", |ui, name| {
        ui.backend.borrow_mut().new_folder(name);
        ui.show_vault();
    });
}

fn delete_note(ui: &Rc<Ui>) {
    let file = ui.backend.borrow().current_note().to_string();
    if file.is_empty() {
        return;
    }
    let body = tr!(
        "Move \"{}\" to the trash? The next Push moves it to Recently Deleted in iCloud.",
        lists::note_label(&file)
    );
    let weak = Rc::downgrade(ui);
    omui::confirm(
        &ui.window,
        "Delete note?",
        &body,
        "_Delete",
        omui::Appearance::Destructive,
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let result = ui.backend.borrow_mut().delete_current_note();
            if result.is_ok() {
                ui.load_editor(); // unsaved edits go with the note
            }
            shown(&ui, result);
        },
    );
}

/// Rename: the title selected, in its field or on the note's first line.
fn rename_note(ui: &Rc<Ui>) {
    if ui.split.is_collapsed() {
        ui.split.set_show_content(true); // the title is in the editor
    }
    if WidgetExt::is_visible(&ui.editor.title) {
        ui.editor.title.grab_focus();
        ui.editor.title.select_region(0, -1);
        return;
    }
    let buffer = &ui.editor.buffer;
    let start = buffer.start_iter();
    let mut end = start;
    end.forward_to_line_end();
    let line: String = buffer.text(&start, &end, true).into();
    let hashes = line.chars().take_while(|c| *c == '#').count();
    let marks = if (1..=6).contains(&hashes) && line[hashes..].starts_with(char::is_whitespace) {
        hashes + line[hashes..].chars().take_while(|c| c.is_whitespace()).count()
    } else {
        0
    };
    ui.editor.view.grab_focus();
    buffer.select_range(&buffer.iter_at_offset(marks as i32), &end);
}

/// The title field lost the keyboard: a changed title renames the note's file.
fn commit_title(ui: &Rc<Ui>) {
    let title = ui.editor.title.text().trim().to_string();
    let current = ui.note_title();
    if title.is_empty() || title == current || !ui.flush_edits() {
        ui.editor.title.set_text(&current);
        return;
    }
    if ui.backend.borrow().sync_running() {
        ui.set_notice(|| tr("Wait for the sync to finish, then rename the note."));
        ui.editor.title.set_text(&current);
        return;
    }
    let result = ui.backend.borrow_mut().rename_current_note(&title);
    if result.is_err() {
        ui.editor.title.set_text(&current);
    }
    shown(ui, result);
}

fn move_note(ui: &Rc<Ui>) {
    let (title, targets) = {
        let b = ui.backend.borrow();
        let file = b.current_note().to_string();
        let details = b.note_details(&file);
        let title = if details.title.is_empty() {
            lists::note_label(&file).to_string()
        } else {
            details.title
        };
        (title, move_targets(b.folders(), b.current_folder()))
    };
    if targets.is_empty() {
        return;
    }
    let dialog = omui::Dialog::new(Some(""), Some(""));
    dialog.set_heading_translated("Move note");
    dialog.bind(move |dialog| dialog.set_body(&tr!("Move \"{}\" to:", title)));
    let names: Vec<&str> = targets.iter().map(String::as_str).collect();
    let choice = gtk::DropDown::from_strings(&names);
    choice.set_width_request(300);
    dialog.set_extra_child(Some(choice.upcast_ref()));
    dialog.add_response("cancel", "");
    dialog.set_response_translated("cancel", "_Cancel");
    dialog.add_response("ok", "");
    dialog.set_response_translated("ok", "_Move");
    dialog.set_response_appearance("ok", omui::Appearance::Suggested);
    dialog.set_default_response("ok");
    dialog.set_close_response("cancel");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |_, id| {
        if let (Some(ui), "ok") = (weak.upgrade(), id)
            && let Some(folder) = targets.get(choice.selected() as usize)
        {
            move_open_note(&ui, folder);
        }
    });
    dialog.present(&ui.window);
}

/// Moves the open note to `folder`, as a mv does: the next push moves it in iCloud.
pub fn move_open_note(ui: &Rc<Ui>, folder: &str) {
    if !ui.flush_edits() {
        return;
    }
    if ui.backend.borrow().sync_running() {
        ui.set_notice(|| tr("Wait for the sync to finish, then move the note."));
        return;
    }
    let result = ui.backend.borrow_mut().move_current_note(folder);
    if result.is_ok() {
        ui.handle_events();
    }
    shown(ui, result);
}

/// A note dropped on a folder, in either layout.
pub fn drop_note(ui: &Rc<Ui>, note: &Item, folder: &str) {
    if let Item::Note { folder: from, file } = note
        && from != folder
        && ui.select_note(from, file)
    {
        move_open_note(ui, folder);
    }
}

fn rename_folder(ui: &Rc<Ui>) {
    let current = ui.backend.borrow().current_folder().to_string();
    if current.is_empty() {
        return;
    }
    prompt(
        ui,
        &tr("Rename folder"),
        &tr("Folder name"),
        &lists::folder_label(&current),
        &tr("The next Push renames the folder in iCloud too."),
        |ui, name| {
            if !ui.flush_edits() {
                return;
            }
            let result = ui.backend.borrow_mut().rename_current_folder(name);
            shown(ui, result);
        },
    );
}

fn delete_folder(ui: &Rc<Ui>) {
    let (folder, count) = {
        let b = ui.backend.borrow();
        let folder = b.current_folder().to_string();
        let count = b.folder_note_counts().get(&folder).copied().unwrap_or(0);
        (folder, count)
    };
    if folder.is_empty() {
        return;
    }
    let body = omui::trn!(
        "Move \"{}\" and its {n} notes to the trash? The next Push moves the notes to Recently Deleted in iCloud and deletes the folder there.",
        count,
        lists::folder_label(&folder)
    );
    let weak = Rc::downgrade(ui);
    omui::confirm(
        &ui.window,
        "Delete folder?",
        &body,
        "_Delete",
        omui::Appearance::Destructive,
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let result = ui.backend.borrow_mut().delete_current_folder();
            if result.is_ok() {
                ui.load_editor();
            }
            shown(&ui, result);
        },
    );
}

/// Ctrl+S: what autosave holds back (a guardrail, removed attachments) asks first.
fn save(ui: &Rc<Ui>) {
    if ui.backend.borrow().current_note().is_empty() {
        return;
    }
    let warning = ui.backend.borrow().save_warning(&ui.editor.text());
    if warning.is_empty() {
        ui.save();
        return;
    }
    let weak = Rc::downgrade(ui);
    // The warning is the library's, in English.
    omui::confirm(
        &ui.window,
        "Save anyway?",
        &warning,
        "_Save",
        omui::Appearance::Destructive,
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.save();
            }
        },
    );
}

/// The checkbox on the cursor's line, flipped (a bare list item gains one).
fn toggle_task(ui: &Rc<Ui>) {
    if !ui.window.lookup_action("checklist").is_some_and(|a| a.is_enabled()) {
        return;
    }
    let buffer = &ui.editor.buffer;
    let before = ui.editor.text();
    let at = buffer.cursor_position();
    let line = buffer.iter_at_offset(at).line();
    let updated = syncmodel::toggle_checkbox(&before, i64::from(line));
    if updated == before {
        return;
    }
    let grown = updated.chars().count() as i32 - before.chars().count() as i32;
    buffer.set_text(&updated);
    buffer.place_cursor(&buffer.iter_at_offset((at + grown).max(0)));
}

/// The selection between `before` and `after`, still selected.
fn wrap_selection(ui: &Rc<Ui>, before: &str, after: &str) {
    let buffer = &ui.editor.buffer;
    let Some((mut start, mut end)) = buffer.selection_bounds() else {
        return;
    };
    if !ui.window.lookup_action("bold").is_some_and(|a| a.is_enabled()) {
        return;
    }
    let selected: String = buffer.text(&start, &end, true).into();
    let from = start.offset();
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &format!("{before}{selected}{after}"));
    buffer.end_user_action();
    let inner = from + before.chars().count() as i32;
    buffer.select_range(
        &buffer.iter_at_offset(inner),
        &buffer.iter_at_offset(inner + selected.chars().count() as i32),
    );
    ui.editor.view.grab_focus();
}

/// `[selection](https://)`, with `https://` selected to type the address over.
fn insert_link(ui: &Rc<Ui>) {
    if !ui.window.lookup_action("link").is_some_and(|a| a.is_enabled()) {
        return;
    }
    let buffer = &ui.editor.buffer;
    let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
        (
            buffer.iter_at_offset(buffer.cursor_position()),
            buffer.iter_at_offset(buffer.cursor_position()),
        )
    });
    let selected: String = buffer.text(&start, &end, true).into();
    let text = if selected.is_empty() {
        "text".to_string()
    } else {
        selected
    };
    let from = start.offset();
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &format!("[{text}](https://)"));
    buffer.end_user_action();
    let target = from + text.chars().count() as i32 + 3;
    buffer.select_range(&buffer.iter_at_offset(target), &buffer.iter_at_offset(target + 8));
    ui.editor.view.grab_focus();
}

/// A row's right-click menu, acting on that row once it is the open folder
/// or note (edits that cannot be saved yet keep the open one, and the menu
/// would act on it).
pub fn row_menu(ui: &Rc<Ui>, item: &Item, list: &gtk::Widget, x: f64, y: f64) {
    let menu = gio::Menu::new();
    match item {
        Item::Folder(folder) => {
            if folder.is_empty() {
                return;
            }
            ui.open_folder(folder);
            if ui.backend.borrow().current_folder() != folder {
                return;
            }
            menu.append(Some(&tr("Rename folder…")), Some("win.rename-folder"));
            menu.append(Some(&tr("Delete folder…")), Some("win.delete-folder"));
        }
        Item::Note { folder, file } => {
            if !ui.select_note(folder, file) {
                return;
            }
            let note = gio::Menu::new();
            note.append(Some(&tr("Rename note")), Some("win.rename-note"));
            note.append(Some(&tr("Move to…")), Some("win.move-note"));
            menu.append_section(None, &note);
            menu.append(Some(&tr("Delete note…")), Some("win.delete-note"));
        }
    }
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(list);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        // Unparented once its item's action has run.
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}
