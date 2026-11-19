//! The window: the toolbar, the banners, the panes in their three layouts,
//! and the status line, and what choosing, opening and saving a note does.
//!
//! A wide window shows the folder list and the note list as two panes beside
//! the editor; a narrower one (a half-screen split) shows one sidebar
//! instead, the folders and their notes as a tree. The two panes give way to
//! the tree before the editor would get less than a comfortable width beside
//! them: about 60 characters of a note plus its margins (1062 px in all, so
//! half of a 1920 px screen has the tree). Narrower than the tree's and the
//! editor's least together, the window shows one of them at a time, and the
//! toolbar's sidebar button switches between them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::{gio, glib, prelude::*};
use icloud_notes::backend::{Backend, Event, Role};
use icloud_notes::lock::Taken;
use icloud_notes::syncmodel;
use icloud_notes::vault::Vault;
use omui::icons::name;

use crate::actions;
use crate::attach::Strip;
use crate::banners::Banners;
use crate::conflicts::Conflicts;
use crate::editor::Editor;
use crate::lists::{self, Handlers, Item, Lists};
use crate::settings::Settings;

/// The folder list's and the note list's widths beside the editor at
/// first, and the least each is dragged to: the split's divider sets the two
/// together, the handle between them the folder list's.
pub const FOLDER_PANE: i32 = 210;
const FOLDER_LEAST: i32 = 150;
pub const NOTE_PANE: i32 = 290;
const NOTE_LEAST: i32 = 200;
/// The tree's width at first, and the least and the most the divider drags it to.
pub const TREE_PANE: i32 = 260;
const TREE_LEAST: i32 = 200;
const TREE_MOST: i32 = 500;
/// The least the editor is given, and the width it is comfortable at.
const EDITOR_LEAST: i32 = 320;
const EDITOR_COMFORT: i32 = 560;
/// From this width the window has the two lists beside the editor.
const WIDE_FROM: i32 = FOLDER_PANE + NOTE_PANE + EDITOR_COMFORT + 2;
/// The most the two lists are dragged to together: the editor keeps its
/// least beside them at the narrowest wide window.
const LISTS_MOST: i32 = WIDE_FROM - EDITOR_LEAST - 2;
/// Below this width it shows the tree or the editor, one at a time.
const ONE_PANE_BELOW: i32 = TREE_LEAST + EDITOR_LEAST + 1;
/// Edits are written once typing has paused this long.
const AUTOSAVE: Duration = Duration::from_millis(1500);

/// Over omui's stylesheet: the panes, the note rows and badges, the editor,
/// the status line.
pub const CSS: &str = "
.note-pane { background: @om_bg; border-right: 1px solid alpha(@om_border, 0.6); }
.pane-title { font-size: 1.3em; font-weight: 700; margin: 12px 20px 0 20px; }
.pane-caption { margin: 14px 18px 6px 18px; font-size: 0.8em; font-weight: 700; }
.list-summary { margin: 0 20px 4px 20px; }
.folder-list, .note-tree { background: none; }
.note-list { background: none; padding: 0 8px; }
.note-list > row { padding: 9px 12px; border-bottom: 1px solid alpha(@om_border, 0.3); }
.note-list > row:selected { border-bottom-color: transparent; }
.tree-row { border-spacing: 6px; }
.note-tree > row.current { background: alpha(@om_accent, 0.18); }
button.chevron { min-width: 12px; min-height: 12px; padding: 0; }
button.chevron image { -gtk-icon-size: 12px; color: @om_dim_text; }
.badges { margin-top: 2px; }
.badge.muted { background: alpha(@om_text, 0.08); color: @om_dim_text; }
.drag-note { background: @om_surface; border: 1px solid @om_accent; border-radius: 6px; padding: 4px 10px; }
entry.note-title { font-size: 1.6em; font-weight: 700; background: none; border-color: transparent; }
entry.note-title:focus-within { border-color: alpha(@om_border, 0.6); }
.conflict-card { padding: 12px; border-radius: 10px; }
.warning-card { background: alpha(@om_warning, 0.12); }
.warning-card image.warning { color: @om_warning; -gtk-icon-size: 18px; }
button.version-card { padding: 12px; border-radius: 10px; background: @om_bg; border: 1px solid alpha(@om_border, 0.6); }
button.version-card:hover { background: alpha(@om_text, 0.04); }
button.version-card.picked { border: 2px solid @om_accent; background: alpha(@om_accent, 0.08); }
image.unpicked { opacity: 0.3; }
.conflict-line { padding: 2px 6px; border-radius: 3px; }
.changed-line { background: alpha(@om_accent, 0.14); box-shadow: inset 2px 0 @om_accent; }
.status-bar { border-top: 1px solid alpha(@om_border, 0.6); padding: 3px 14px; border-spacing: 8px; }
.status-bar label { font-size: 0.85em; }
.notice { color: @om_accent; }
.pane-grip { min-width: 5px; }
.unsaved { color: @om_warning; }
";

/// The window's parts that change after it is built, and its state.
pub struct Ui {
    me: Weak<Ui>,
    pub backend: RefCell<Backend>,
    pub window: gtk::ApplicationWindow,
    pub shell: Rc<omui::Shell>,
    pub split: omui::Split,
    /// The sidebar: the two lists ("panes"), or the tree ("tree").
    pub sidebar: gtk::Stack,
    pub sidebar_button: gtk::Button,
    /// The folder list, whose width the handle on its right edge drags.
    folder_pane: gtk::Box,
    /// The sidebar's width with the two lists and with the tree (the
    /// divider's), kept for the layout not on screen and the next start.
    sidebar_widths: Cell<[i32; 2]>,
    /// Shown only where there is room for them; their shortcuts stay.
    pub roomy: Vec<gtk::Widget>,
    pub narrow: Cell<bool>,
    /// The window has had its first layout.
    laid_out: Cell<bool>,
    pub lists: Lists,
    pub handlers: Rc<Handlers>,
    pub folder_title: gtk::Label,
    pub list_summary: gtk::Label,
    tree_summary: gtk::Label,
    /// The tree's open folders ("" is All Notes).
    pub expanded: RefCell<Vec<String>>,
    pub editor: Editor,
    pub conflicts: Conflicts,
    pub strip: Strip,
    /// Closing was stopped once for edits that cannot be saved: the next close discards them.
    close_warned: Cell<bool>,
    /// What the search boxes hold (both layouts' share it), and what it found.
    pub query: RefCell<String>,
    pub hits: RefCell<Option<Vec<lists::Hit>>>,
    /// The search read only the first notes of a larger vault.
    pub search_truncated: Cell<bool>,
    /// Recall's ranking by meaning: which search it is for, and its delay.
    pub meaning: crate::search::Meaning,
    search_boxes: [gtk::SearchEntry; 2],
    autosave: RefCell<Option<glib::SourceId>>,
    /// The editor's font zoom (Ctrl++, Ctrl+-, Ctrl+0), and the style that sets it.
    pub zoom: Cell<f64>,
    zoom_style: gtk::CssProvider,
    pub status: gtk::Label,
    pub notice: gtk::Label,
    pub unsaved: gtk::Label,
    pub spinner: gtk::Spinner,
    pub banners: Banners,
    // ---- syncing (syncing.rs)
    /// The first sync has been started (or decided against).
    pub started: Cell<bool>,
    /// icloud-session's sign-in window is open, or was asked for.
    pub signing_in: Cell<bool>,
    /// A clone waits for the sign-in it asked for.
    pub clone_wanted: Cell<bool>,
    /// Failed pulls in a row (at most 4), which slow the step down.
    pub poll_failures: Cell<u32>,
    /// When the window last synced or asked the engine to.
    pub last_sync: Cell<Option<Instant>>,
    /// The editor waits, read-only, for a pull to bring the latest copy.
    pub freshening: Cell<bool>,
    pub soon: RefCell<Option<glib::SourceId>>,
    /// What stops the engine's run under way, for a close that writes the
    /// save waiting for it.
    pub run_stopper: RefCell<Option<icloud_notes::engine::Stopper>>,
    /// The window looks again for the vault's lock another process holds,
    /// for the run that waits for it (if any).
    pub lock_retry: Cell<bool>,
    pub waiting_run: Cell<Option<icloud_notes::sync::Kind>>,
    /// The engine's message when it refused a push that would delete most of
    /// the vault; empty once a push goes through.
    pub deletions_refused: RefCell<String>,
    /// The last Sync status's notes it would not simply push, by
    /// vault-relative file: their resolution.
    pub statuses: RefCell<HashMap<String, String>>,
    /// What the vault is watched by, and for which folders and note.
    watching: RefCell<(String, Vec<gio::FileMonitor>)>,
    reread: RefCell<Option<glib::SourceId>>,
}

pub struct Window;

impl Window {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(app: &gtk::Application) -> gtk::ApplicationWindow {
        let saved = Settings::load();
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Notes")
            .icon_name(crate::APP_ID)
            .default_width(saved.width)
            .default_height(saved.height)
            .width_request(440)
            .build();

        // ---- the toolbar
        let header = omui::header(None);
        let sidebar_button = omui::icon_button(name::SIDEBAR, "Hide the sidebar");
        let new_folder = omui::icon_button(name::NEW_FOLDER, "New folder");
        new_folder.set_action_name(Some("win.new-folder"));
        let new_note = omui::icon_button(name::COMPOSE, "New note (Ctrl+N)");
        new_note.set_action_name(Some("win.new-note"));
        let delete = omui::icon_button(name::DELETE, "Delete note");
        delete.set_action_name(Some("win.delete-note"));
        let rename = omui::icon_button(name::EDIT, "Rename note");
        rename.set_action_name(Some("win.rename-note"));
        let separator = || gtk::Separator::new(gtk::Orientation::Vertical);
        let sidebar_separator = separator();
        for widget in [
            sidebar_button.upcast_ref::<gtk::Widget>(),
            sidebar_separator.upcast_ref(),
            new_folder.upcast_ref(),
            new_note.upcast_ref(),
            separator().upcast_ref(),
            delete.upcast_ref(),
            rename.upcast_ref(),
        ] {
            header.pack_start(widget);
        }
        let checklist = omui::icon_button(name::CHECKED, "Checklist (Ctrl+Enter)");
        checklist.set_action_name(Some("win.checklist"));
        let bold = omui::icon_button(name::BOLD, "Bold (Ctrl+B)");
        bold.set_action_name(Some("win.bold"));
        let italic = omui::icon_button(name::ITALIC, "Italic (Ctrl+I)");
        italic.set_action_name(Some("win.italic"));
        let link = omui::icon_button(name::LINK, "Link (Ctrl+K)");
        link.set_action_name(Some("win.link"));
        let attach = omui::icon_button(name::ATTACH, "Attach a file (or drop or paste one)");
        attach.set_action_name(Some("win.attach"));
        let sync_status = omui::icon_button(name::REFRESH, "Sync status");
        sync_status.set_action_name(Some("win.sync-status"));
        let more = omui::icon_menu_button(name::MORE, "More", &more_menu());
        // pack_end goes from the right edge inwards.
        for widget in [
            more.upcast_ref::<gtk::Widget>(),
            sync_status.upcast_ref(),
            separator().upcast_ref(),
            attach.upcast_ref(),
            link.upcast_ref(),
            italic.upcast_ref(),
            bold.upcast_ref(),
            checklist.upcast_ref(),
        ] {
            header.pack_end(widget);
        }
        window.set_titlebar(Some(&header));

        let ui = Rc::new_cyclic(|weak: &Weak<Ui>| {
            let handlers = Rc::new(handlers(weak));
            let lists = Lists::new(handlers.clone());
            let search_box = || {
                gtk::SearchEntry::builder()
                    .placeholder_text("Search notes")
                    .margin_start(12)
                    .margin_end(12)
                    .margin_top(8)
                    .margin_bottom(6)
                    .build()
            };
            let search_boxes = [search_box(), search_box()];

            // ---- the panes
            let lists_width = saved.folder_pane + saved.note_pane;
            let folder_pane = pane(
                "sidebar",
                saved
                    .folder_pane
                    .clamp(FOLDER_LEAST, lists_width.min(LISTS_MOST) - NOTE_LEAST),
            );
            let caption = omui::label("ICLOUD", "dim-label");
            caption.add_css_class("pane-caption");
            folder_pane.append(&caption);
            folder_pane.append(&lists.folder_pane);
            let note_pane = pane("note-pane", NOTE_LEAST);
            note_pane.set_hexpand(true);
            let folder_title = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["pane-title"])
                .build();
            let list_summary = gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["caption", "dim-label", "list-summary"])
                .build();
            note_pane.append(&folder_title);
            note_pane.append(&search_boxes[0]);
            note_pane.append(&list_summary);
            note_pane.append(&lists.note_pane);
            let panes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            // The note list keeps its least beside it.
            let w = weak.clone();
            panes.append(&resizable(&folder_pane, FOLDER_LEAST, move || {
                w.upgrade().map_or(FOLDER_LEAST, |ui| ui.sidebar.width() - NOTE_LEAST)
            }));
            panes.append(&note_pane);
            let tree_pane = pane("sidebar", TREE_LEAST);
            tree_pane.append(&search_boxes[1]);
            let tree_summary = gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["caption", "dim-label", "list-summary"])
                .visible(false)
                .build();
            tree_pane.append(&tree_summary);
            tree_pane.append(&lists.tree_pane);
            let sidebar = gtk::Stack::builder().hhomogeneous(false).build();
            sidebar.add_named(&panes, Some("panes"));
            sidebar.add_named(&tree_pane, Some("tree"));

            let editor = Editor::new();
            editor.widget.set_width_request(EDITOR_LEAST);
            let conflicts = Conflicts::new();
            let strip = Strip::new();
            editor.page.insert_child_after(&strip.widget, Some(&editor.read_only));
            editor
                .page
                .insert_child_after(&conflicts.unreadable, Some(&strip.widget));
            editor.page.append(&conflicts.picker);

            // The divider drags the sidebar within each layout's limits
            // (set_narrow), and no wider than leaves the editor its least.
            let split = omui::Split::new(&sidebar, &editor.widget, (TREE_LEAST, TREE_MOST), Some(ONE_PANE_BELOW));
            split.set_resizable(true);
            split.widget.set_vexpand(true);

            // ---- the status line
            let spinner = omui::spinner();
            spinner.set_visible(false);
            let status = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label"])
                .build();
            let notice = gtk::Label::builder()
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(60)
                .css_classes(["notice"])
                .visible(false)
                .build();
            let unsaved = gtk::Label::builder()
                .label("● unsaved")
                .css_classes(["unsaved"])
                .visible(false)
                .build();
            let status_bar = gtk::Box::builder().css_classes(["status-bar"]).build();
            status_bar.append(&spinner);
            status_bar.append(&status);
            status_bar.append(&notice);
            status_bar.append(&unsaved);

            let banners = Banners::new();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&banners.widget);
            content.append(&split.widget);
            content.append(&status_bar);
            let shell = omui::Shell::new(&window, &content);

            Ui {
                me: weak.clone(),
                backend: RefCell::new(Backend::new(Vault::locate(None), Role::App)),
                window: window.clone(),
                shell,
                split,
                sidebar,
                sidebar_button: sidebar_button.clone(),
                folder_pane,
                sidebar_widths: Cell::new([lists_width, saved.tree_pane]),
                roomy: vec![bold.upcast(), italic.upcast(), link.upcast()],
                narrow: Cell::new(false),
                laid_out: Cell::new(false),
                lists,
                handlers,
                folder_title,
                list_summary,
                expanded: RefCell::new(saved.expanded),
                editor,
                conflicts,
                strip,
                close_warned: Cell::new(false),
                query: RefCell::default(),
                hits: RefCell::default(),
                search_truncated: Cell::new(false),
                meaning: Default::default(),
                search_boxes,
                tree_summary,
                autosave: RefCell::default(),
                zoom: Cell::new(saved.zoom),
                zoom_style: gtk::CssProvider::new(),
                status,
                notice,
                unsaved,
                spinner,
                banners,
                started: Cell::new(false),
                signing_in: Cell::new(false),
                clone_wanted: Cell::new(false),
                poll_failures: Cell::new(0),
                last_sync: Cell::new(None),
                freshening: Cell::new(false),
                soon: RefCell::default(),
                run_stopper: RefCell::default(),
                lock_retry: Cell::new(false),
                waiting_run: Cell::new(None),
                deletions_refused: RefCell::default(),
                statuses: RefCell::default(),
                watching: RefCell::default(),
                reread: RefCell::default(),
            }
        });
        ui.banners.connect(&ui);

        // ---- the layouts
        let weak = Rc::downgrade(&ui);
        sidebar_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_sidebar();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.split.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.sync_sidebar_button();
            }
        });
        let weak = Rc::downgrade(&ui);
        omui::on_narrow(&ui.split.widget, WIDE_FROM, move |narrow| {
            if let Some(ui) = weak.upgrade() {
                ui.set_narrow(narrow);
            }
        });
        sidebar_button
            .bind_property("visible", &sidebar_separator, "visible")
            .sync_create()
            .build();

        // ---- the editor
        let weak = Rc::downgrade(&ui);
        ui.editor.buffer.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.text_changed();
            }
        });

        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &ui.zoom_style,
            omui::APP_PRIORITY,
        );
        ui.zoom(saved.zoom);
        actions::add(&ui);
        crate::attach::connect(&ui);
        ui.pane_keys();
        let weak = Rc::downgrade(&ui);
        window.connect_close_request(move |_| match weak.upgrade() {
            Some(ui) if !ui.may_close() => glib::Propagation::Stop,
            Some(ui) => {
                ui.settings().save();
                glib::Propagation::Proceed
            }
            None => glib::Propagation::Proceed,
        });
        // Editing the latest copy: focusing the editor, or the first key typed
        // into an untouched note, pulls first when the last pull is a minute
        // old; that key is dropped rather than typed into a copy about to change.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&ui);
        focus.connect_enter(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.pull_if_stale();
            }
        });
        ui.editor.view.add_controller(focus);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let types = key.to_unicode().is_some_and(|c| !c.is_control())
                && !modifiers.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK);
            let edits = types || matches!(key, gtk::gdk::Key::BackSpace | gtk::gdk::Key::Delete);
            if edits && !ui.editor.dirty() && ui.pull_if_stale() {
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        ui.editor.view.add_controller(keys);
        for (at, entry) in ui.search_boxes.iter().enumerate() {
            let weak = Rc::downgrade(&ui);
            entry.connect_search_changed(move |entry| {
                if let Some(ui) = weak.upgrade() {
                    ui.search(&entry.text(), at);
                }
            });
            // Escape clears the search and goes back to the note.
            let weak = Rc::downgrade(&ui);
            entry.connect_stop_search(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.search("", 2);
                    ui.editor.view.grab_focus();
                }
            });
            // Down goes on to the results.
            let keys = gtk::EventControllerKey::new();
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if key != gtk::gdk::Key::Down {
                    return glib::Propagation::Proceed;
                }
                let list = if ui.narrow.get() {
                    &ui.lists.tree
                } else {
                    &ui.lists.notes
                };
                list.child_focus(gtk::DirectionType::TabForward);
                glib::Propagation::Stop
            });
            entry.add_controller(keys);
        }
        ui.start();
        ui.show_banners();
        ui.start_syncing();
        // Everything else holds the state weakly: the window keeps it for as
        // long as it is open.
        let keep = ui.clone();
        window.connect_destroy(move |_| {
            let _ = &keep;
        });
        window
    }
}

/// What the lists' rows do, for a window that may be gone.
fn handlers(weak: &Weak<Ui>) -> Handlers {
    let w = weak.clone();
    let chosen = move |item: &Item| {
        if let Some(ui) = w.upgrade() {
            ui.choose(item);
        }
    };
    let w = weak.clone();
    let activated = move |item: &Item| {
        if let Some(ui) = w.upgrade() {
            ui.activate(item);
        }
    };
    let w = weak.clone();
    let menu = move |item: &Item, list: &gtk::Widget, x: f64, y: f64| {
        if let Some(ui) = w.upgrade() {
            actions::row_menu(&ui, item, list, x, y);
        }
    };
    let w = weak.clone();
    let dropped = move |note: &Item, folder: &str| {
        if let Some(ui) = w.upgrade() {
            actions::drop_note(&ui, note, folder);
        }
    };
    Handlers {
        chosen: Box::new(chosen),
        menu: Box::new(menu),
        activated: Box::new(activated),
        dropped: Box::new(dropped),
    }
}

/// `pane` with a handle on its right edge that drags its width, from
/// `least` to what `most` says then.
fn resizable(pane: &gtk::Box, least: i32, most: impl Fn() -> i32 + 'static) -> gtk::Overlay {
    let overlay = gtk::Overlay::builder().child(pane).build();
    let grip = gtk::Box::builder()
        .halign(gtk::Align::End)
        .css_classes(["pane-grip"])
        .build();
    grip.set_cursor_from_name(Some("col-resize"));
    let drag = gtk::GestureDrag::new();
    let from = Rc::new(Cell::new(0));
    let (pane_at, from_at) = (pane.downgrade(), from.clone());
    drag.connect_drag_begin(move |_, _, _| {
        if let Some(pane) = pane_at.upgrade() {
            from_at.set(pane.width_request());
        }
    });
    let pane_at = pane.downgrade();
    drag.connect_drag_update(move |_, dx, _| {
        if let Some(pane) = pane_at.upgrade() {
            pane.set_width_request((from.get() + dx as i32).clamp(least, most().max(least)));
        }
    });
    grip.add_controller(drag);
    overlay.add_overlay(&grip);
    overlay
}

/// A pane of the sidebar: a column `width` wide at the least, with `class`.
fn pane(class: &str, width: i32) -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(width)
        .css_classes([class])
        .build()
}

fn more_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let note = gio::Menu::new();
    note.append(Some("Note history"), Some("win.history"));
    note.append(Some("Export PDF"), Some("win.export-pdf"));
    menu.append_section(None, &note);
    let app = gio::Menu::new();
    app.append(Some("Refresh"), Some("win.refresh"));
    app.append(Some("Sync log"), Some("win.sync-log"));
    menu.append_section(None, &app);
    menu
}

impl Ui {
    /// The vault's lock, held for as long as the window is open so no
    /// background sync runs the engine beside it, and the first status.
    fn start(self: &Rc<Self>) {
        let taken = self.backend.borrow_mut().lock_vault();
        let line = match taken {
            Taken::Locked => None,
            Taken::Busy => Some(format!(
                "{} has the notes; syncing here waits for it.",
                self.backend.borrow().lock_holder()
            )),
            Taken::Failed => Some(format!(
                "Could not open the sync lock {}; nothing syncs until it opens.",
                self.backend.borrow().vault().lock_path().display()
            )),
        };
        if let Some(line) = line {
            self.backend.borrow_mut().log(&line);
        }
        if taken == Taken::Busy {
            self.wait_for_lock();
        }
        {
            let b = self.backend.borrow();
            let vault = b.vault();
            self.set_status(if vault.left_icloud() {
                "Omacloud syncs this folder; iCloud is no longer used here."
            } else if b.engine().is_none() {
                icloud_notes::sync::NOT_INSTALLED
            } else if vault.cloned() {
                "Ready."
            } else {
                "Not linked to iCloud yet. Press Clone."
            });
        }
        self.show_vault();
    }

    // ---- the layouts ---------------------------------------------------------------

    /// Wide: the two lists. Narrow: the tree, and the button that puts the
    /// sidebar away. Either way the sidebar comes back.
    fn set_narrow(&self, narrow: bool) {
        let first = !self.laid_out.replace(true);
        let mut widths = self.sidebar_widths.get();
        if !first {
            widths[usize::from(self.narrow.get())] = self.split.sidebar_width();
            self.sidebar_widths.set(widths);
        }
        self.narrow.set(narrow);
        self.sidebar
            .set_visible_child_name(if narrow { "tree" } else { "panes" });
        self.split.set_sidebar_limits(if narrow {
            (TREE_LEAST, TREE_MOST)
        } else {
            (FOLDER_LEAST + NOTE_LEAST, LISTS_MOST)
        });
        self.split.set_sidebar_width(widths[usize::from(narrow)]);
        self.sidebar_button.set_visible(narrow);
        self.split.set_show_sidebar(true);
        self.split.set_show_content(false);
        if narrow {
            self.reveal_current();
        }
        self.show_lists();
        self.sync_sidebar_button();
        // At the start the arrows work the list (the folders, or the tree)
        // before anything is clicked.
        if first {
            focus_list(if narrow { &self.lists.tree } else { &self.lists.folders });
        }
    }

    /// Whether the sidebar is on screen.
    fn sidebar_shown(&self) -> bool {
        self.split.shows_sidebar() && !(self.split.is_collapsed() && self.split.shows_content())
    }

    /// The toolbar's sidebar button: one pane at a time, the tree or the
    /// editor; else the tree beside the editor, or not.
    fn toggle_sidebar(&self) {
        if !self.sidebar_shown() {
            self.split.set_show_sidebar(true);
            self.split.set_show_content(false);
        } else if self.split.is_collapsed() {
            self.split.set_show_content(true);
        } else {
            self.split.set_show_sidebar(false);
        }
    }

    fn sync_sidebar_button(&self) {
        let shown = self.sidebar_shown();
        let tip = if shown { "Hide the sidebar" } else { "Show the sidebar" };
        self.sidebar_button.set_tooltip_text(Some(tip));
        self.sidebar_button
            .update_property(&[gtk::accessible::Property::Label(tip)]);
        omui::set_class(&self.sidebar_button, "active", shown);
        // One pane at a time, the formatting buttons keep only their
        // shortcuts.
        for widget in &self.roomy {
            widget.set_visible(!self.split.is_collapsed());
        }
    }

    /// Unfolds what it takes for the open note (or, with none, the selected
    /// folder) to show in the tree.
    fn reveal_current(&self) {
        let b = self.backend.borrow();
        let mut expanded = self.expanded.borrow_mut();
        let mut add = |folder: &str| {
            if !expanded.iter().any(|f| f == folder) {
                expanded.push(folder.to_string());
            }
        };
        let folder = b.current_folder();
        if b.current_note().is_empty() {
            let mut parent = icloud_notes::notetree::parent_of(folder);
            while !parent.is_empty() {
                add(parent);
                parent = icloud_notes::notetree::parent_of(parent);
            }
        } else if folder.is_empty() {
            add(""); // a note at the top of the vault is only under All Notes
        } else {
            let mut at = folder;
            while !at.is_empty() {
                add(at);
                at = icloud_notes::notetree::parent_of(at);
            }
        }
    }

    // ---- what is on screen ---------------------------------------------------------

    /// Everything the vault shows: the lists, the open note, the title.
    pub fn show_vault(&self) {
        self.show_lists();
        self.show_note_header();
        let b = self.backend.borrow();
        let title = match b.current_note() {
            "" => "Notes".to_string(),
            file => format!("{} - Notes", lists::note_label(file)),
        };
        self.window.set_title(Some(&title));
        drop(b);
        actions::update(self);
        self.watch_vault();
    }

    /// The open note's title, as its list row shows it.
    pub fn note_title(&self) -> String {
        let b = self.backend.borrow();
        let file = b.current_note();
        let details = b.note_details(file);
        if details.title.is_empty() {
            lists::note_label(file).to_string()
        } else {
            details.title
        }
    }

    /// The editor's text at `zoom` times its size, within limits.
    pub fn zoom(&self, zoom: f64) {
        let zoom = zoom.clamp(0.5, 4.0);
        self.zoom.set(zoom);
        self.zoom_style
            .load_from_string(&format!("textview.note-text {{ font-size: {zoom:.3}em; }}"));
    }

    /// The lists of the layout on screen.
    fn show_lists(&self) {
        if self.narrow.get() {
            let expanded = self.expanded.borrow().clone();
            let weak = self.weak();
            let toggle: Rc<dyn Fn(&str)> = Rc::new(move |folder: &str| {
                if let Some(ui) = weak.upgrade() {
                    ui.toggle_folder(folder);
                }
            });
            let hits = self.hits.borrow();
            let mut b = self.backend.borrow_mut();
            lists::fill_tree(
                &self.lists.tree,
                &mut b,
                &expanded,
                hits.as_deref(),
                toggle,
                &self.handlers,
            );
            self.tree_summary.set_visible(hits.is_some());
            self.tree_summary.set_label(&match hits.as_deref() {
                Some([]) => "No matching notes".to_string(),
                Some(found) => summary(found, self.search_truncated.get()),
                None => String::new(),
            });
        } else {
            let b = self.backend.borrow();
            lists::fill_folders(&self.lists.folders, &b, &self.handlers);
            let statuses = self.statuses.borrow();
            let folder = b.current_folder().to_string();
            let pushed = |file: &str| match statuses.get(&icloud_notes_path(&folder, file)).map(String::as_str) {
                Some("refused") => vec![("push refused", "error")],
                Some("conflict") => vec![("remote changed", "warning")],
                _ => Vec::new(),
            };
            let hits = self.hits.borrow();
            lists::fill_notes(&self.lists.notes, &b, hits.as_deref(), &pushed);
            self.folder_title.set_label(lists::folder_label(b.current_folder()));
            let n = b.notes().len();
            self.list_summary.set_label(&match hits.as_deref() {
                Some(found) => summary(found, self.search_truncated.get()),
                None => format!("{n} note{}", if n == 1 { "" } else { "s" }),
            });
        }
    }

    /// The editor's header: the date, the title of a file-titled note, and
    /// why it cannot be edited.
    pub fn show_note_header(&self) {
        let mut b = self.backend.borrow_mut();
        let file = b.current_note().to_string();
        if file.is_empty() {
            self.editor.close();
            return;
        }
        self.editor.widget.set_visible_child_name("note");
        let details = b.note_details(&file);
        self.editor.date.set_label(&if self.freshening.get() {
            "Getting the latest from iCloud…".to_string()
        } else {
            long_date(details.modified_ms)
        });
        let by_file_name = b.vault_title_mode() == "filename";
        self.editor.title.set_visible(by_file_name);
        if by_file_name {
            let title = if details.title.is_empty() {
                lists::note_label(&file)
            } else {
                &details.title
            };
            self.editor.title.set_text(title);
        }
        let reason = b.read_only_reason().to_string();
        self.editor.read_only.set_visible(!reason.is_empty());
        self.editor.read_only.set_label(&format!(
            "Read-only here: this note {reason}. Edit it in Apple Notes, and the changes still sync here."
        ));
        self.editor.title.set_editable(reason.is_empty());
        self.editor
            .view
            .set_editable(reason.is_empty() && !self.freshening.get());
        self.editor
            .conflict_copy
            .set_visible(b.note_flags(&file).contains(&"conflict-copy"));
        drop(b);
        if let Some(ui) = self.me.upgrade() {
            let resolving = ui.show_conflicts();
            self.editor.scroller.set_visible(!resolving);
            if self.conflicts.unreadable.is_visible() {
                self.editor.view.set_editable(false);
            }
        }
        self.show_attachments();
    }

    /// Loads the open note into the editor.
    pub fn load_editor(&self) {
        let body = syncmodel::editor_form(self.backend.borrow().note_body());
        self.editor.load(&body);
        self.unsaved.set_visible(false);
    }

    /// The status line's message.
    pub fn set_status(&self, text: &str) {
        self.status.set_label(text);
    }

    /// The window's own message beside it ("" hides it).
    pub fn set_notice(&self, text: &str) {
        self.notice.set_label(text);
        self.notice.set_tooltip_text(Some(text).filter(|t| !t.is_empty()));
        self.notice.set_visible(!text.is_empty());
    }

    /// The search boxes' query, typed into box `from` (2: neither): both
    /// boxes hold it, and the lists show what it finds.
    fn search(&self, query: &str, from: usize) {
        for (at, entry) in self.search_boxes.iter().enumerate() {
            if at != from && entry.text() != query {
                entry.set_text(query);
            }
        }
        if *self.query.borrow() == query {
            return;
        }
        self.query.replace(query.to_string());
        self.refresh_search();
        self.show_lists();
    }

    /// Ctrl+F: the sidebar shown, and its search box focused.
    pub fn focus_search(&self) {
        if !self.sidebar_shown() {
            self.toggle_sidebar();
        }
        let entry = &self.search_boxes[usize::from(self.narrow.get())];
        entry.grab_focus();
    }

    /// What the window keeps for its next start.
    fn settings(&self) -> Settings {
        let (width, height) = self.window.default_size();
        let mut widths = self.sidebar_widths.get();
        if self.laid_out.get() {
            widths[usize::from(self.narrow.get())] = self.split.sidebar_width();
        }
        let folder_pane = self.folder_pane.width_request();
        Settings {
            width,
            height,
            folder_pane,
            note_pane: widths[0] - folder_pane,
            tree_pane: widths[1],
            zoom: self.zoom.get(),
            expanded: self.expanded.borrow().clone(),
        }
    }

    /// The window, for a handler that must not keep it alive.
    pub fn weak(&self) -> Weak<Ui> {
        self.me.clone()
    }

    // ---- choosing ------------------------------------------------------------------

    /// A row chosen, by a click or by the arrows passing it.
    fn choose(self: &Rc<Self>, item: &Item) {
        match item {
            Item::Folder(folder) => self.open_folder(folder),
            Item::Note { folder, file } => {
                self.open_note(folder, file);
            }
        }
    }

    /// A row activated: Enter or a double click in the two lists, a click or
    /// Enter in the tree. A folder of the list goes on to its notes; one of
    /// the tree opens and unfolds, or folds when it was open and selected
    /// already. A note opens and goes on to the editor (in the tree, a click
    /// on a note that is not open yet only opens it).
    fn activate(self: &Rc<Self>, item: &Item) {
        match item {
            Item::Folder(folder) if self.narrow.get() => {
                let was = self.backend.borrow().current_folder() == folder.as_str();
                let expanded = self.expanded.borrow().iter().any(|f| f == folder);
                self.open_folder(folder);
                if self.backend.borrow().current_folder() == folder.as_str() && self.hits.borrow().is_none() {
                    self.set_expanded(folder, !(was && expanded));
                }
            }
            Item::Folder(_) => focus_list(&self.lists.notes),
            Item::Note { folder, file } => {
                let path = icloud_notes_path(folder, file);
                let open = self.backend.borrow().current_path() == path;
                if !open && !self.open_note(folder, file) {
                    return;
                }
                if open || !self.narrow.get() {
                    self.to_editor();
                }
            }
        }
    }

    /// The keyboard to the editor; one pane at a time, the editor shown.
    fn to_editor(&self) {
        if self.split.is_collapsed() {
            self.split.set_show_content(true);
        }
        self.editor.view.grab_focus();
    }

    /// The arrows across the panes: Right goes deeper (folders, notes,
    /// editor), Left and Escape back out; in the tree Right unfolds a folder
    /// and then steps into it, Left folds it or steps out to its parent.
    fn pane_keys(self: &Rc<Self>) {
        let on = |list: &gtk::ListBox, f: fn(&Rc<Ui>, &gtk::ListBox, gtk::gdk::Key) -> bool| {
            let keys = gtk::EventControllerKey::new();
            let weak = self.weak();
            let target = list.clone();
            keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
                Some(ui) if modifiers.is_empty() && f(&ui, &target, key) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            list.add_controller(keys);
        };
        on(&self.lists.folders, |ui, list, key| {
            key == gtk::gdk::Key::Right && {
                if let Some(item) = list
                    .focus_child()
                    .and_downcast::<gtk::ListBoxRow>()
                    .as_ref()
                    .and_then(lists::item_of)
                {
                    ui.activate(&item);
                }
                true
            }
        });
        on(&self.lists.notes, |ui, list, key| match key {
            gtk::gdk::Key::Left | gtk::gdk::Key::Escape => {
                focus_list(&ui.lists.folders);
                true
            }
            gtk::gdk::Key::Right => {
                if let Some(item) = list
                    .focus_child()
                    .and_downcast::<gtk::ListBoxRow>()
                    .as_ref()
                    .and_then(lists::item_of)
                {
                    ui.activate(&item);
                }
                true
            }
            _ => false,
        });
        on(&self.lists.tree, |ui, list, key| {
            use gtk::gdk::Key;
            if matches!(
                key,
                Key::Up | Key::Down | Key::Home | Key::End | Key::Page_Up | Key::Page_Down
            ) {
                // The list moves the keyboard, then the note it lands on opens.
                let weak = ui.weak();
                let list = list.clone();
                glib::idle_add_local_once(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.open_focused_note(&list);
                    }
                });
                return false;
            }
            let Some(row) = list.focus_child().and_downcast::<gtk::ListBoxRow>() else {
                return false;
            };
            let Some(item) = lists::item_of(&row) else { return false };
            let expanded = |folder: &str| ui.expanded.borrow().iter().any(|f| f == folder);
            match (key, &item) {
                (gtk::gdk::Key::Right, Item::Folder(folder)) => {
                    if !expanded(folder) && ui.hits.borrow().is_none() {
                        ui.toggle_folder(folder);
                        focus_row(list, row.index());
                    } else {
                        focus_row(list, row.index() + 1);
                        ui.open_focused_note(list);
                    }
                    true
                }
                (gtk::gdk::Key::Right, Item::Note { .. }) => {
                    ui.activate(&item);
                    ui.to_editor();
                    true
                }
                (gtk::gdk::Key::Left, Item::Folder(folder)) if expanded(folder) && ui.hits.borrow().is_none() => {
                    ui.toggle_folder(folder);
                    focus_row(list, row.index());
                    true
                }
                (gtk::gdk::Key::Left, _) => {
                    let depth = |row: &gtk::ListBoxRow| row.child().map_or(0, |c| c.margin_start());
                    let mine = depth(&row);
                    let parent = (0..row.index())
                        .rev()
                        .find(|&i| list.row_at_index(i).is_some_and(|r| depth(&r) < mine));
                    if let Some(parent) = parent {
                        focus_row(list, parent);
                    }
                    true
                }
                _ => false,
            }
        });
        // Escape in the editor walks back out to the list (one pane at a
        // time, the split itself goes back to the sidebar).
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
            Some(ui) if key == gtk::gdk::Key::Escape && !ui.split.is_collapsed() => {
                focus_list(if ui.narrow.get() {
                    &ui.lists.tree
                } else {
                    &ui.lists.notes
                });
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        self.editor.view.add_controller(keys);
    }

    /// The note the tree's keyboard is on opens, as the two lists open the
    /// row the arrows reach (a folder only opens on Enter or a click).
    fn open_focused_note(self: &Rc<Self>, tree: &gtk::ListBox) {
        let row = tree.focus_child().and_downcast::<gtk::ListBoxRow>();
        if let Some(Item::Note { folder, file }) = row.as_ref().and_then(lists::item_of)
            && self.backend.borrow().current_path() != icloud_notes_path(&folder, &file)
        {
            self.open_note(&folder, &file);
        }
    }

    fn toggle_folder(&self, folder: &str) {
        let open = self.expanded.borrow().iter().any(|f| f == folder);
        self.set_expanded(folder, !open);
    }

    /// A folder of the tree shown open, or folded.
    fn set_expanded(&self, folder: &str, open: bool) {
        {
            let mut expanded = self.expanded.borrow_mut();
            let at = expanded.iter().position(|f| f == folder);
            match (at, open) {
                (Some(at), false) => {
                    expanded.remove(at);
                }
                (None, true) => expanded.push(folder.to_string()),
                _ => return,
            }
        }
        self.show_lists();
    }

    pub fn open_folder(&self, folder: &str) {
        if folder == self.backend.borrow().current_folder() || !self.flush_edits() {
            return;
        }
        self.backend.borrow_mut().set_current_folder(folder);
        if self.narrow.get() {
            self.reveal_current();
        }
        self.show_vault();
    }

    /// Makes `file` of `folder` the open note, to edit it. False when edits
    /// that cannot be saved yet keep the open one.
    pub fn open_note(self: &Rc<Self>, folder: &str, file: &str) -> bool {
        let opened = self.select_note(folder, file);
        if opened {
            self.pull_if_stale();
        }
        opened
    }

    /// Makes it the open note, for acting on it (its menu, a drag) rather
    /// than editing it. False when edits that cannot be saved yet keep the
    /// open one.
    pub fn select_note(&self, folder: &str, file: &str) -> bool {
        if !self.flush_edits() {
            return false;
        }
        self.set_notice("");
        let before = self.backend.borrow().current_path();
        {
            let mut b = self.backend.borrow_mut();
            b.set_current_folder(folder);
            b.open_note(file);
        }
        if self.backend.borrow().current_path() != before {
            self.conflicts.as_text.set(false);
        }
        if self.narrow.get() {
            self.reveal_current();
        }
        self.load_editor();
        self.show_vault();
        true
    }

    // ---- saving --------------------------------------------------------------------

    fn text_changed(&self) {
        self.close_warned.set(false);
        self.unsaved.set_visible(self.editor.dirty());
        if let Some(source) = self.autosave.take() {
            source.remove();
        }
        let weak = self.weak();
        let source = glib::timeout_add_local_once(AUTOSAVE, move || {
            if let Some(ui) = weak.upgrade() {
                ui.autosave.take();
                ui.autosave();
            }
        });
        self.autosave.replace(Some(source));
    }

    /// Writes what typing left once it paused. Edits a guardrail holds back
    /// stay in the editor, flagged unsaved.
    fn autosave(&self) {
        if self.backend.borrow().current_note().is_empty() {
            return;
        }
        if !self.editor.dirty() {
            self.backend.borrow_mut().drop_queued_save();
        } else if self.backend.borrow().save_warning(&self.editor.text()).is_empty() {
            self.save();
        }
    }

    /// Saves the editor's text; false when it could not be written: the
    /// edits stay in the editor.
    pub fn save(&self) -> bool {
        let text = self.editor.text();
        let saved = self.backend.borrow_mut().save_note(&text, Some(&self.editor.saved()));
        if saved {
            self.editor.set_saved(&text);
            self.set_notice("");
        } else if self.editor.dirty() {
            self.set_notice(if self.backend.borrow().sync_running() {
                "Saving once the sync is done. Your edits are still in the editor."
            } else {
                "Could not save the note. Your edits are still in the editor."
            });
        }
        self.unsaved.set_visible(self.editor.dirty());
        self.handle_events();
        saved
    }

    /// Closing saves what autosave has not written yet; a save that waits
    /// for the window's own sync is written once that sync is stopped. Edits
    /// a guardrail holds back, or that wait behind another program's sync,
    /// stop the first close so they are not dropped unseen; closing again
    /// discards them.
    fn may_close(&self) -> bool {
        if !self.editor.dirty() || self.close_warned.get() {
            return true;
        }
        let held = !self.backend.borrow().save_warning(&self.editor.text()).is_empty();
        if !held && (self.save() || !self.editor.dirty()) {
            return true;
        }
        if !held && self.backend.borrow().close_writes_queued_save() {
            if let Some(stopper) = self.run_stopper.take() {
                stopper.stop();
            }
            self.backend.borrow_mut().write_queued_save_on_close();
            return true;
        }
        self.close_warned.set(true);
        self.set_notice(if !held && self.backend.borrow().sync_running() {
            "Your edits are not saved yet: another sync has the notes. Wait for it, or close again to discard them."
        } else {
            "Your edits are not saved (Ctrl+S shows why). Close again to discard them."
        });
        false
    }

    /// Saves before leaving the open note. Edits that fail the guardrails are
    /// never discarded: they stay in the editor, and this is false.
    pub fn flush_edits(&self) -> bool {
        if !self.editor.dirty() {
            return true;
        }
        if !self.backend.borrow().save_warning(&self.editor.text()).is_empty() {
            self.set_notice("Kept your edits in the editor. Resolve the warning (Ctrl+S) before moving on.");
            return false;
        }
        self.save()
    }

    /// What the backend says happened: a change worth a sync soon, the open
    /// note changed on disk under a save (its edits are merged once no sync
    /// runs), edits kept as a new note, a save that waited for the sync
    /// written.
    pub fn handle_events(&self) {
        let Some(ui) = self.me.upgrade() else { return };
        let events = self.backend.borrow_mut().take_events();
        for event in events {
            match event {
                Event::VaultChanged => {
                    self.show_lists();
                    ui.sync_soon();
                }
                Event::NoteChangedOnDisk => {
                    if !self.backend.borrow().sync_running() {
                        let base = self.editor.saved();
                        self.backend.borrow_mut().refresh();
                        ui.merge_edits(&base);
                        self.show_vault();
                    }
                }
                Event::EditsKeptAsNote(message) => self.set_notice(&message),
                Event::QueuedSaveWritten(body) => {
                    if self.editor.text() == body {
                        self.load_editor();
                        self.set_notice("");
                    } else {
                        // Typing went on: the edits now start from what was
                        // queued, merged with what the sync changed.
                        ui.merge_edits(&body);
                        if self.editor.dirty() {
                            self.editor.set_saved(&body);
                        }
                    }
                }
            }
        }
    }

    /// Watches the vault, its folders and the open note for changes made by
    /// other programs (a pull in a terminal, a file manager): the lists are
    /// read again, and the open note's unsaved edits kept apart from the change.
    fn watch_vault(&self) {
        let (root, folders, note) = {
            let b = self.backend.borrow();
            let note =
                (!b.current_note().is_empty()).then(|| b.vault().folder(b.current_folder()).join(b.current_note()));
            (b.vault().absolute(), b.folders().to_vec(), note)
        };
        let key = format!("{folders:?}{note:?}");
        if self.watching.borrow().0 == key {
            return;
        }
        let mut monitors = Vec::new();
        let paths = folders.iter().map(|folder| root.join(folder)).chain(note.clone());
        for path in paths {
            let file = gio::File::for_path(&path);
            let monitor = if Some(&path) == note.as_ref() {
                file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            } else {
                file.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            };
            let Ok(monitor) = monitor else { continue };
            let weak = self.weak();
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.vault_changed();
                }
            });
            monitors.push(monitor);
        }
        self.watching.replace((key, monitors));
    }

    /// Something changed in the vault: read again once it settles. A sync
    /// reads the vault itself once it is done.
    fn vault_changed(self: &Rc<Self>) {
        if self.reread.borrow().is_some() {
            return;
        }
        let weak = self.weak();
        let source = glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.reread.take();
            if ui.backend.borrow().sync_running() {
                return;
            }
            let before = ui.backend.borrow().note_content().to_string();
            ui.backend.borrow_mut().refresh();
            let changed = ui.backend.borrow().note_content() != before;
            ui.after_reload();
            if changed || !ui.editor.dirty() {
                ui.sync_soon();
            }
        });
        self.reread.replace(Some(source));
    }
}

/// The keyboard to a list: its selected (or current) row, else its first.
fn focus_list(list: &gtk::ListBox) {
    let mut current = list.selected_row();
    let mut i = 0;
    while current.is_none()
        && let Some(row) = list.row_at_index(i)
    {
        if row.has_css_class("current") {
            current = Some(row);
        }
        i += 1;
    }
    if let Some(row) = current.or_else(|| list.row_at_index(0)) {
        row.grab_focus();
    }
}

/// The keyboard to row `index` of a list that was just refilled.
fn focus_row(list: &gtk::ListBox, index: i32) {
    if let Some(row) = list.row_at_index(index) {
        row.grab_focus();
    }
}

/// The line under a search box: what it found.
fn summary(hits: &[lists::Hit], truncated: bool) -> String {
    let related = hits.iter().filter(|hit| hit.related).count();
    let matches = hits.len() - related;
    let mut line = format!("{matches} result{}", if matches == 1 { "" } else { "s" });
    if related > 0 {
        line.push_str(&format!(" · {related} related"));
    }
    if truncated {
        line.push_str(&format!(
            " · searched the first {} notes",
            icloud_notes::backend::WINDOW_SEARCH_NOTES
        ));
    }
    line
}

fn icloud_notes_path(folder: &str, file: &str) -> String {
    if folder.is_empty() {
        file.to_string()
    } else {
        format!("{folder}/{file}")
    }
}

/// When a note was last changed, as its editor's header says it.
fn long_date(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    glib::DateTime::from_unix_local(ms / 1000)
        .and_then(|date| date.format("%A, %-d %B %Y at %H:%M"))
        .map(String::from)
        .unwrap_or_default()
}
