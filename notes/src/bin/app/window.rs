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
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::{gio, glib, prelude::*};
use icloud_notes::backend::{Backend, Event, Role};
use icloud_notes::lock::Taken;
use icloud_notes::syncmodel;
use icloud_notes::vault::Vault;
use omui::icons::name;

use crate::editor::Editor;
use crate::icons;
use crate::lists::{self, Handlers, Item, Lists};

/// The folder list's and the note list's widths beside the editor.
const FOLDER_PANE: i32 = 210;
const NOTE_PANE: i32 = 290;
/// The tree's width beside the editor, and the least it is shown at.
const TREE_PANE: i32 = 260;
const TREE_LEAST: i32 = 200;
/// The least the editor is given, and the width it is comfortable at.
const EDITOR_LEAST: i32 = 320;
const EDITOR_COMFORT: i32 = 560;
/// From this width the window has the two lists beside the editor.
const WIDE_FROM: i32 = FOLDER_PANE + NOTE_PANE + EDITOR_COMFORT + 2;
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
button.chevron { min-width: 12px; min-height: 12px; padding: 0; }
button.chevron image { -gtk-icon-size: 12px; color: @om_dim_text; }
.badges { margin-top: 2px; }
.badge.muted { background: alpha(@om_text, 0.08); color: @om_dim_text; }
.drag-note { background: @om_surface; border: 1px solid @om_accent; border-radius: 6px; padding: 4px 10px; }
entry.note-title { font-size: 1.6em; font-weight: 700; background: none; border-color: transparent; }
entry.note-title:focus-within { border-color: alpha(@om_border, 0.6); }
.status-bar { border-top: 1px solid alpha(@om_border, 0.6); padding: 3px 14px; border-spacing: 8px; }
.status-bar label { font-size: 0.85em; }
.notice { color: @om_accent; }
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
    /// Shown only where there is room for them; their shortcuts stay.
    pub roomy: Vec<gtk::Widget>,
    pub narrow: Cell<bool>,
    pub lists: Lists,
    pub handlers: Rc<Handlers>,
    pub folder_title: gtk::Label,
    pub list_summary: gtk::Label,
    /// The tree's open folders ("" is All Notes).
    pub expanded: RefCell<Vec<String>>,
    pub editor: Editor,
    autosave: RefCell<Option<glib::SourceId>>,
    pub status: gtk::Label,
    pub notice: gtk::Label,
    pub unsaved: gtk::Label,
    pub spinner: gtk::Spinner,
}

pub struct Window;

impl Window {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(app: &gtk::Application) -> gtk::ApplicationWindow {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Notes")
            .icon_name(crate::APP_ID)
            .default_width(1100)
            .default_height(700)
            .width_request(440)
            .build();

        // ---- the toolbar
        let header = omui::header(None);
        let sidebar_button = omui::icon_button(name::SIDEBAR, "Hide the sidebar");
        let new_folder = icons::button(icons::NEW_FOLDER, "New folder");
        new_folder.set_action_name(Some("win.new-folder"));
        let new_note = icons::button(icons::NEW_NOTE, "New note (Ctrl+N)");
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
        let bold = icons::button(icons::BOLD, "Bold (Ctrl+B)");
        bold.set_action_name(Some("win.bold"));
        let italic = icons::button(icons::ITALIC, "Italic (Ctrl+I)");
        italic.set_action_name(Some("win.italic"));
        let link = icons::button(icons::LINK, "Link (Ctrl+K)");
        link.set_action_name(Some("win.link"));
        let attach = icons::button(icons::ATTACH, "Attach a file (or drop or paste one)");
        attach.set_action_name(Some("win.attach"));
        let sync_status = omui::icon_button(name::REFRESH, "Sync status");
        sync_status.set_action_name(Some("win.sync-status"));
        let more = gtk::MenuButton::builder()
            .icon_name(name::MORE)
            .tooltip_text("More")
            .menu_model(&more_menu())
            .build();
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

            // ---- the panes
            let folder_pane = pane("sidebar", FOLDER_PANE);
            let caption = omui::label("ICLOUD", "dim-label");
            caption.add_css_class("pane-caption");
            folder_pane.append(&caption);
            folder_pane.append(&lists.folder_pane);
            let note_pane = pane("note-pane", NOTE_PANE);
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
            note_pane.append(&list_summary);
            note_pane.append(&lists.note_pane);
            let panes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            panes.append(&folder_pane);
            panes.append(&note_pane);
            let tree_pane = pane("sidebar", TREE_LEAST);
            tree_pane.append(&lists.tree_pane);
            let sidebar = gtk::Stack::builder().hhomogeneous(false).build();
            sidebar.add_named(&panes, Some("panes"));
            sidebar.add_named(&tree_pane, Some("tree"));

            let editor = Editor::new();
            editor.widget.set_width_request(EDITOR_LEAST);

            // The sidebar's width is the most of the split's limits and what
            // the sidebar itself needs: the two lists' widths, or the tree's.
            let split = omui::Split::new(&sidebar, &editor.widget, (TREE_PANE, TREE_PANE), Some(ONE_PANE_BELOW));
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

            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
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
                roomy: vec![bold.upcast(), italic.upcast(), link.upcast()],
                narrow: Cell::new(false),
                lists,
                handlers,
                folder_title,
                list_summary,
                expanded: RefCell::default(),
                editor,
                autosave: RefCell::default(),
                status,
                notice,
                unsaved,
                spinner,
            }
        });

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

        add_actions(&ui);
        ui.start();
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
    let menu = |_: &Item, _: &gtk::Widget, _: f64, _: f64| {};
    let dropped = |_: &Item, _: &str| {};
    Handlers {
        chosen: Box::new(chosen),
        menu: Box::new(menu),
        activated: Box::new(activated),
        dropped: Box::new(dropped),
    }
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

/// What an action does to the window, with its parameter.
type Activate = Box<dyn Fn(&Ui, Option<&glib::Variant>)>;

/// The window's actions: what its buttons, menus and keys do, and what the
/// preview script drives over D-Bus.
fn add_actions(ui: &Rc<Ui>) {
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
    // A vault-relative note path, as `icloud-notes` names notes.
    action(
        "open-note",
        Some(glib::VariantTy::STRING),
        Box::new(|ui, parameter| {
            let path: String = parameter.and_then(|p| p.get()).unwrap_or_default();
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
}

impl Ui {
    /// The vault's lock, held for as long as the window is open so no
    /// background sync runs the engine beside it, and the first status.
    fn start(&self) {
        {
            let mut b = self.backend.borrow_mut();
            match b.lock_vault() {
                Taken::Locked => {}
                Taken::Busy => {
                    let line = format!("{} has the notes; syncing here waits for it.", b.lock_holder());
                    b.log(&line);
                }
                Taken::Failed => {
                    let line = format!(
                        "Could not open the sync lock {}; nothing syncs until it opens.",
                        b.vault().lock_path().display()
                    );
                    b.log(&line);
                }
            }
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
        self.narrow.set(narrow);
        self.sidebar
            .set_visible_child_name(if narrow { "tree" } else { "panes" });
        self.sidebar_button.set_visible(narrow);
        self.split.set_show_sidebar(true);
        self.split.set_show_content(false);
        if narrow {
            self.reveal_current();
        }
        self.show_lists();
        self.sync_sidebar_button();
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
        // One pane at a time, the formatting buttons keep only their shortcuts.
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
            let mut b = self.backend.borrow_mut();
            lists::fill_tree(&self.lists.tree, &mut b, &expanded, None, toggle, &self.handlers);
        } else {
            let b = self.backend.borrow();
            lists::fill_folders(&self.lists.folders, &b, &self.handlers);
            lists::fill_notes(&self.lists.notes, &b, None, &|_| Vec::new());
            self.folder_title.set_label(lists::folder_label(b.current_folder()));
            let n = b.notes().len();
            self.list_summary
                .set_label(&format!("{n} note{}", if n == 1 { "" } else { "s" }));
        }
    }

    /// The editor's header: the date, the title of a file-titled note, and
    /// why it cannot be edited.
    fn show_note_header(&self) {
        let mut b = self.backend.borrow_mut();
        let file = b.current_note().to_string();
        if file.is_empty() {
            self.editor.close();
            return;
        }
        self.editor.widget.set_visible_child_name("note");
        let details = b.note_details(&file);
        self.editor.date.set_label(&long_date(details.modified_ms));
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
        self.editor.view.set_editable(reason.is_empty());
        self.editor
            .conflict_copy
            .set_visible(b.note_flags(&file).contains(&"conflict-copy"));
    }

    /// Loads the open note into the editor.
    fn load_editor(&self) {
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

    /// The window, for a handler that must not keep it alive.
    fn weak(&self) -> Weak<Ui> {
        self.me.clone()
    }

    // ---- choosing ------------------------------------------------------------------

    /// A row chosen, by a click or by the arrows passing it.
    fn choose(&self, item: &Item) {
        match item {
            Item::Folder(folder) => self.open_folder(folder),
            Item::Note { folder, file } => {
                self.open_note(folder, file);
            }
        }
    }

    /// A row activated (a click, Enter). A folder of the tree unfolds, or
    /// folds when it was open and selected already; a note goes to the
    /// editor when one pane is shown at a time.
    fn activate(&self, item: &Item) {
        match item {
            Item::Folder(folder) if self.narrow.get() => self.toggle_folder(folder),
            Item::Folder(_) => {}
            Item::Note { .. } => {
                if self.split.is_collapsed() {
                    self.split.set_show_content(true);
                }
            }
        }
    }

    fn toggle_folder(&self, folder: &str) {
        {
            let mut expanded = self.expanded.borrow_mut();
            match expanded.iter().position(|f| f == folder) {
                Some(at) => {
                    expanded.remove(at);
                }
                None => expanded.push(folder.to_string()),
            }
        }
        self.show_lists();
    }

    fn open_folder(&self, folder: &str) {
        if folder == self.backend.borrow().current_folder() || !self.flush_edits() {
            return;
        }
        self.backend.borrow_mut().set_current_folder(folder);
        if self.narrow.get() {
            self.reveal_current();
        }
        self.show_vault();
    }

    /// Makes `file` of `folder` the open note. False when edits that cannot
    /// be saved yet keep the open one.
    pub fn open_note(&self, folder: &str, file: &str) -> bool {
        if !self.flush_edits() {
            return false;
        }
        self.set_notice("");
        {
            let mut b = self.backend.borrow_mut();
            b.set_current_folder(folder);
            b.open_note(file);
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
    fn save(&self) -> bool {
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

    /// Saves before leaving the open note. Edits that fail the guardrails are
    /// never discarded: they stay in the editor, and this is false.
    fn flush_edits(&self) -> bool {
        if !self.editor.dirty() {
            return true;
        }
        if !self.backend.borrow().save_warning(&self.editor.text()).is_empty() {
            self.set_notice("Kept your edits in the editor. Resolve the warning (Ctrl+S) before moving on.");
            return false;
        }
        self.save()
    }

    /// What the backend says happened.
    fn handle_events(&self) {
        let events = self.backend.borrow_mut().take_events();
        if events.contains(&Event::VaultChanged) {
            self.show_lists();
        }
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
