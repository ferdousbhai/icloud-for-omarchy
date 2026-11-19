//! The window: the toolbar, the banners, the panes in their three layouts,
//! and the status line.
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
use std::rc::Rc;

use gtk::{gio, prelude::*};
use icloud_notes::backend::{Backend, Role};
use icloud_notes::lock::Taken;
use icloud_notes::vault::Vault;
use omui::icons::name;

use crate::icons;

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

/// Over omui's stylesheet: the panes' backgrounds, the status line.
pub const CSS: &str = "
.note-pane { background: @om_bg; border-right: 1px solid alpha(@om_border, 0.6); }
.pane-title { font-size: 1.3em; font-weight: 700; margin: 12px 20px 0 20px; }
.status-bar { border-top: 1px solid alpha(@om_border, 0.6); padding: 3px 14px; border-spacing: 8px; }
.status-bar label { font-size: 0.85em; }
.notice { color: @om_accent; }
.unsaved { color: @om_warning; }
";

/// The window's parts that change after it is built.
pub struct Ui {
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

        // ---- the panes
        let folder_pane = pane("sidebar", FOLDER_PANE);
        folder_pane.append(&omui::label("iCloud", "dim-label"));
        let note_pane = pane("note-pane", NOTE_PANE);
        note_pane.append(&omui::label("All Notes", "pane-title"));
        let panes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        panes.append(&folder_pane);
        panes.append(&note_pane);
        let tree_pane = pane("sidebar", TREE_LEAST);
        let sidebar = gtk::Stack::builder().hhomogeneous(false).build();
        sidebar.add_named(&panes, Some("panes"));
        sidebar.add_named(&tree_pane, Some("tree"));

        let editor_pane = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(EDITOR_LEAST)
            .hexpand(true)
            .build();
        let empty = omui::EmptyState::new(Some(name::DOCUMENT), "", Some("Select a note, or create a new one."));
        empty.widget.set_vexpand(true);
        editor_pane.append(&empty.widget);

        // The sidebar's width is the most of the split's limits and what the
        // sidebar itself needs: the two lists' widths, or the tree's.
        let split = omui::Split::new(&sidebar, &editor_pane, (TREE_PANE, TREE_PANE), Some(ONE_PANE_BELOW));
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

        let ui = Rc::new(Ui {
            backend: RefCell::new(Backend::new(Vault::locate(None), Role::App)),
            window: window.clone(),
            shell,
            split: split.clone(),
            sidebar,
            sidebar_button: sidebar_button.clone(),
            roomy: vec![bold.upcast(), italic.upcast(), link.upcast()],
            narrow: Cell::new(false),
            status,
            notice,
            unsaved,
            spinner,
        });

        // ---- the layouts
        let weak = Rc::downgrade(&ui);
        sidebar_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.toggle_sidebar();
            }
        });
        let weak = Rc::downgrade(&ui);
        split.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.sync_sidebar_button();
            }
        });
        let weak = Rc::downgrade(&ui);
        omui::on_narrow(&split.widget, WIDE_FROM, move |narrow| {
            if let Some(ui) = weak.upgrade() {
                ui.set_narrow(narrow);
            }
        });
        sidebar_button
            .bind_property("visible", &sidebar_separator, "visible")
            .sync_create()
            .build();
        ui.start();
        window
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

impl Ui {
    /// The vault's lock, held for as long as the window is open so no
    /// background sync runs the engine beside it, and the first status.
    fn start(&self) {
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

    /// Wide: the two lists. Narrow: the tree, and the button that puts the
    /// sidebar away. Either way the sidebar comes back.
    fn set_narrow(&self, narrow: bool) {
        self.narrow.set(narrow);
        self.sidebar
            .set_visible_child_name(if narrow { "tree" } else { "panes" });
        self.sidebar_button.set_visible(narrow);
        self.split.set_show_sidebar(true);
        self.split.set_show_content(false);
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
}
