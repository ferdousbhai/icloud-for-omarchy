//! The sidebar's lists: the folder list and the note list of a wide window,
//! and the tree of folders and notes of a narrow one. Each is filled from the
//! backend when the vault or the selection changes, and rebuilt only when
//! what its rows show changed; the window says what a row does when it is
//! chosen.

use std::rc::Rc;

use gtk::{gdk, glib, prelude::*};
use icloud_notes::backend::Backend;
use icloud_notes::notetree::{self, TreeNote};
use omui::icons::name;

/// What a row of any list stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Folder(String),
    /// A note: the folder it is listed under, and its file there (which may
    /// lie in a subfolder of it).
    Note {
        folder: String,
        file: String,
    },
}

/// A folder's label: its last part, or All Notes for the vault itself.
pub fn folder_label(folder: &str) -> &str {
    if folder.is_empty() {
        "All Notes"
    } else {
        folder.rsplit('/').next().unwrap_or(folder)
    }
}

/// A note's file name without `.md`.
pub fn note_label(file: &str) -> &str {
    let name = file.rsplit('/').next().unwrap_or(file);
    name.strip_suffix(".md").unwrap_or(name)
}

/// A modification time as the lists show it, the locale's short date.
pub fn short_date(ms: i64) -> String {
    glib::DateTime::from_unix_local(ms / 1000)
        .and_then(|date| date.format("%x"))
        .map(String::from)
        .unwrap_or_default()
}

/// A note's badges: [label, omui badge class], from its flags.
fn badges(flags: &[&str]) -> Vec<(&'static str, &'static str)> {
    flags
        .iter()
        .map(|flag| match *flag {
            "conflict" => ("conflict", "error"),
            "new" => ("new", "success"),
            "missing-id" => ("no id", "warning"),
            "foreign-id" => ("foreign id", "warning"),
            "conflict-copy" => ("Syncthing copy, not synced", "warning"),
            "tables" => ("tables", "muted"),
            "read-only" => ("read-only", "muted"),
            _ => ("", "muted"),
        })
        .filter(|(label, _)| !label.is_empty())
        .collect()
}

/// The row's item, kept on the row itself.
fn set_item(row: &gtk::ListBoxRow, item: Item) {
    // SAFETY: only Item is ever stored under this key, and read back as Item.
    unsafe { row.set_data("item", item) };
}

pub fn item_of(row: &gtk::ListBoxRow) -> Option<Item> {
    // SAFETY: as set_item stores it.
    unsafe { row.data::<Item>("item").map(|item| item.as_ref().clone()) }
}

/// A list of rows in a scrolled pane.
fn list(class: &str) -> (gtk::ListBox, gtk::ScrolledWindow) {
    let list = gtk::ListBox::builder().css_classes([class]).build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    (list, scroller)
}

/// A row's right click: the row's item, the list, and where in it.
pub type MenuAt = Box<dyn Fn(&Item, &gtk::Widget, f64, f64)>;
/// A note dropped on a folder: the note, and the folder.
pub type Dropped = Box<dyn Fn(&Item, &str)>;

/// What the lists tell the window: a row chosen (by click or by the arrows),
/// a row right-clicked, and a row activated (Enter, a click).
pub struct Handlers {
    pub chosen: Box<dyn Fn(&Item)>,
    pub menu: MenuAt,
    pub activated: Box<dyn Fn(&Item)>,
    pub dropped: Dropped,
}

/// The rows the window puts on screen, and what they are for.
pub struct Lists {
    pub folders: gtk::ListBox,
    pub folder_pane: gtk::ScrolledWindow,
    pub notes: gtk::ListBox,
    pub note_pane: gtk::ScrolledWindow,
    pub tree: gtk::ListBox,
    pub tree_pane: gtk::ScrolledWindow,
}

impl Lists {
    pub fn new(handlers: Rc<Handlers>) -> Lists {
        let (folders, folder_pane) = list("folder-list");
        let (notes, note_pane) = list("note-list");
        let (tree, tree_pane) = list("note-tree");
        // In the two lists a click or the arrows open the row they reach, and
        // Enter (or a double click) goes on: to the notes, to the editor. In
        // the tree the arrows only move, and a click or Enter opens: a folder
        // the arrows pass is not opened.
        folders.set_activate_on_single_click(false);
        notes.set_activate_on_single_click(false);
        tree.set_selection_mode(gtk::SelectionMode::None);
        for list in [&folders, &notes, &tree] {
            // The arrows move the selection, and the row they reach is chosen,
            // as a click chooses it.
            let h = handlers.clone();
            list.connect_row_selected(move |list, row| {
                if let Some(item) = row.and_then(item_of)
                    && !updating(list)
                {
                    (h.chosen)(&item);
                }
            });
            let h = handlers.clone();
            list.connect_row_activated(move |_, row| {
                if let Some(item) = item_of(row) {
                    (h.activated)(&item);
                }
            });
            let menu = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
            let h = handlers.clone();
            menu.connect_pressed(move |gesture, _, x, y| {
                let Some(list) = gesture.widget().and_downcast::<gtk::ListBox>() else {
                    return;
                };
                if let Some(row) = list.row_at_y(y as i32)
                    && let Some(item) = item_of(&row)
                {
                    (h.menu)(&item, list.upcast_ref(), x, y);
                }
            });
            list.add_controller(menu);
        }
        Lists {
            folders,
            folder_pane,
            notes,
            note_pane,
            tree,
            tree_pane,
        }
    }
}

/// Set while a list is refilled or its selection put back: that is no choice.
fn updating(list: &gtk::ListBox) -> bool {
    // SAFETY: only a bool is stored under this key.
    unsafe { list.data::<bool>("updating").is_some_and(|on| *on.as_ref()) }
}

fn while_updating(list: &gtk::ListBox, f: impl FnOnce()) {
    // SAFETY: only a bool is stored under this key.
    unsafe { list.set_data("updating", true) };
    f();
    unsafe { list.set_data("updating", false) };
}

/// Fills `list` by `fill`, unless its rows show `shown` already (a
/// description of everything on them): then only the selection may have
/// changed, and the rows stay as they are. A list that is rebuilt keeps its
/// scroll, and the keyboard on the row of the same item.
fn refill(list: &gtk::ListBox, shown: String, fill: impl FnOnce()) {
    // SAFETY: only a String is stored under this key.
    if unsafe { list.data::<String>("shown") }.is_some_and(|was| *unsafe { was.as_ref() } == shown) {
        return;
    }
    let scroller = list
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>();
    let scrolled = scroller.as_ref().map(|s| s.vadjustment().value());
    let focused = list
        .root()
        .and_then(|root| root.focus())
        .and_then(|focus| focus.ancestor(gtk::ListBoxRow::static_type()))
        .and_downcast::<gtk::ListBoxRow>()
        .filter(|row| row.parent().as_ref() == Some(list.upcast_ref()))
        .and_then(|row| item_of(&row));
    while_updating(list, || {
        omui::clear(list);
        fill();
    });
    unsafe { list.set_data("shown", shown) };
    if let (Some(scroller), Some(value)) = (scroller, scrolled) {
        scroller.vadjustment().set_value(value);
    }
    if let Some(item) = focused {
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if item_of(&row).as_ref() == Some(&item) {
                row.grab_focus();
                break;
            }
            i += 1;
        }
    }
}

/// Selects the row standing for `item`, or none, without choosing it. A
/// list that selects nothing (the tree) marks it as the current row.
pub fn select(list: &gtk::ListBox, item: Option<&Item>) {
    if list.selection_mode() == gtk::SelectionMode::None {
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            let current = item.is_some_and(|item| item_of(&row).as_ref() == Some(item));
            omui::set_class(&row, "current", current);
            i += 1;
        }
        return;
    }
    while_updating(list, || {
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if item.is_some_and(|item| item_of(&row).as_ref() == Some(item)) {
                list.select_row(Some(&row));
                return;
            }
            i += 1;
        }
        list.unselect_all();
    });
}

/// Moves a note dragged from a row of a list onto a folder row.
fn drag_note(row: &gtk::ListBoxRow, item: &Item, title: &str) {
    let Item::Note { folder, file } = item else { return };
    let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
    let key = format!("{folder}\n{file}");
    source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&key.to_value())));
    let title = title.to_string();
    source.connect_drag_begin(move |source, _| {
        let label = gtk::Label::builder().label(&title).css_classes(["drag-note"]).build();
        source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&label))), 10, 14);
    });
    row.add_controller(source);
}

/// Takes a dragged note on a folder row (not All Notes).
fn drop_on_folder(row: &gtk::ListBoxRow, folder: &str, handlers: &Rc<Handlers>) {
    if folder.is_empty() {
        return;
    }
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let (h, folder) = (handlers.clone(), folder.to_string());
    target.connect_drop(move |_, value, _, _| {
        let Ok(key) = value.get::<String>() else { return false };
        let Some((from, file)) = key.split_once('\n') else {
            return false;
        };
        let note = Item::Note {
            folder: from.to_string(),
            file: file.to_string(),
        };
        (h.dropped)(&note, &folder);
        true
    });
    row.add_controller(target);
}

/// Fills the folder list: All Notes, then every folder, indented by depth,
/// with its note count.
pub fn fill_folders(list: &gtk::ListBox, b: &Backend, handlers: &Rc<Handlers>) {
    let shown = format!("{:?}{:?}", b.folders(), b.folder_note_counts());
    refill(list, shown, || {
        for folder in b.folders() {
            let count = b.folder_note_counts().get(folder).copied().unwrap_or(0);
            let icon = if folder.is_empty() {
                name::FOLDER_OPEN
            } else {
                name::FOLDER
            };
            let count = (count > 0).then(|| count.to_string());
            let row = omui::sidebar_row(folder, Some(icon), folder_label(folder), count.as_deref());
            let depth = folder.matches('/').count();
            if let Some(content) = row.child() {
                content.set_margin_start(depth as i32 * 14);
            }
            set_item(&row, Item::Folder(folder.clone()));
            drop_on_folder(&row, folder, handlers);
            list.append(&row);
        }
    });
    select(list, Some(&Item::Folder(b.current_folder().to_string())));
}

/// One note's row: its title, its date and the start of its text, and its badges.
fn note_row(title: &str, date: &str, snippet: &str, badges: &[(&str, &str)]) -> gtk::ListBoxRow {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .css_classes(["note-row"])
        .build();
    column.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    if !date.is_empty() {
        line.append(
            &gtk::Label::builder()
                .label(date)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    line.append(
        &gtk::Label::builder()
            .label(snippet)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    column.append(&line);
    if !badges.is_empty() {
        let pills = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .row_spacing(4)
            .column_spacing(4)
            .max_children_per_line(6)
            .css_classes(["badges"])
            .build();
        for (label, class) in badges {
            pills.append(&omui::badge(label, class));
        }
        column.append(&pills);
    }
    let row = gtk::ListBoxRow::builder().child(&column).build();
    row.update_property(&[gtk::accessible::Property::Label(title)]);
    row
}

/// A search result: where it was found, for the note list and the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub folder: String,
    pub file: String,
    pub title: String,
    pub snippet: String,
    /// Found by Recall's ranking alone, not by its text.
    pub related: bool,
}

/// Fills the note list: the current folder's notes, newest first, or the
/// search's results.
pub fn fill_notes(
    list: &gtk::ListBox,
    b: &Backend,
    hits: Option<&[Hit]>,
    extra: &dyn Fn(&str) -> Vec<(&'static str, &'static str)>,
) {
    let folder = b.current_folder();
    let shown = match hits {
        None => b
            .notes()
            .iter()
            .map(|file| {
                format!(
                    "{file}{:?}{:?}{:?}",
                    b.note_details(file),
                    b.note_flags(file),
                    extra(file)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(hits) => format!("{hits:?}"),
    };
    refill(list, format!("{folder}\n{shown}"), || match hits {
        None => {
            for file in b.notes() {
                let details = b.note_details(file);
                let title = if details.title.is_empty() {
                    note_label(file)
                } else {
                    &details.title
                };
                let date = if details.modified_ms > 0 {
                    short_date(details.modified_ms)
                } else {
                    String::new()
                };
                let mut pills = badges(b.note_flags(file));
                pills.extend(extra(file));
                let row = note_row(title, &date, &details.snippet, &pills);
                let item = Item::Note {
                    folder: folder.to_string(),
                    file: file.clone(),
                };
                drag_note(&row, &item, title);
                set_item(&row, item);
                list.append(&row);
            }
        }
        Some(hits) => {
            let mut related_shown = false;
            for hit in hits {
                if hit.related && !related_shown {
                    related_shown = true;
                    let header = omui::label("Related by meaning", "heading");
                    header.add_css_class("related-header");
                    let row = gtk::ListBoxRow::builder()
                        .child(&header)
                        .selectable(false)
                        .activatable(false)
                        .build();
                    list.append(&row);
                }
                let place = if hit.folder.is_empty() {
                    "All Notes"
                } else {
                    &hit.folder
                };
                let row = note_row(&hit.title, place, &hit.snippet, &[]);
                let item = Item::Note {
                    folder: hit.folder.clone(),
                    file: hit.file.clone(),
                };
                drag_note(&row, &item, &hit.title);
                set_item(&row, item);
                list.append(&row);
            }
        }
    });
    let open = (!b.current_note().is_empty()).then(|| Item::Note {
        folder: b.current_folder().to_string(),
        file: b.current_note().to_string(),
    });
    select(list, open.as_ref());
}

/// Fills the tree: All Notes and the folders, each open on its subfolders
/// and notes when it is in `expanded`; while searching, only the hits.
pub fn fill_tree(
    list: &gtk::ListBox,
    b: &mut Backend,
    expanded: &[String],
    hits: Option<&[Hit]>,
    toggle: Rc<dyn Fn(&str)>,
    handlers: &Rc<Handlers>,
) {
    let all = b.all_notes();
    let matches: Vec<String> = hits
        .unwrap_or_default()
        .iter()
        .map(|hit| icloud_notes_path(&hit.folder, &hit.file))
        .collect();
    let rows = notetree::rows(
        b.folders(),
        b.folder_note_counts(),
        &all,
        expanded,
        hits.is_some(),
        &matches,
    );
    let note_open = !b.current_note().is_empty();
    let shown = format!("{rows:?}{}{note_open}", b.current_folder());
    refill(list, shown, || {
        for row in rows {
            list.append(&tree_row(&row, b.current_folder(), note_open, &toggle, handlers));
        }
    });
    let open_path = b.current_path();
    let current = if note_open {
        all.iter().find(|n| n.path == open_path).map(|n| Item::Note {
            folder: n.folder.clone(),
            file: n.file.clone(),
        })
    } else {
        Some(Item::Folder(b.current_folder().to_string()))
    };
    select(list, current.as_ref());
}

fn icloud_notes_path(folder: &str, file: &str) -> String {
    if folder.is_empty() {
        file.to_string()
    } else {
        format!("{folder}/{file}")
    }
}

fn tree_row(
    row: &notetree::Row<TreeNote>,
    current_folder: &str,
    note_open: bool,
    toggle: &Rc<dyn Fn(&str)>,
    handlers: &Rc<Handlers>,
) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .spacing(6)
        .margin_start(row.depth as i32 * 14)
        .css_classes(["tree-row"])
        .build();
    // The chevron folds and unfolds alone; the rest of the row opens.
    let chevron = gtk::Button::builder()
        .icon_name(if row.expanded { name::DOWN } else { name::FORWARD })
        .css_classes(["flat", "chevron"])
        .can_focus(false)
        .opacity(if row.expandable { 1.0 } else { 0.0 })
        .sensitive(row.expandable)
        .build();
    chevron.set_tooltip_text(Some(if row.expanded { "Fold" } else { "Unfold" }));
    let folder = row.folder.clone();
    let toggle = toggle.clone();
    chevron.connect_clicked(move |_| toggle(&folder));
    content.append(&chevron);
    let (item, text, side) = match &row.note {
        None => {
            let icon = if row.folder.is_empty() || row.expanded {
                name::FOLDER_OPEN
            } else {
                name::FOLDER
            };
            content.append(&omui::icon(icon));
            let count = if row.count > 0 {
                row.count.to_string()
            } else {
                String::new()
            };
            (
                Item::Folder(row.folder.clone()),
                folder_label(&row.folder).to_string(),
                count,
            )
        }
        Some(note) => {
            let title = if note.title.is_empty() {
                note_label(&note.file).to_string()
            } else {
                note.title.clone()
            };
            let date = if note.modified_ms > 0 {
                short_date(note.modified_ms)
            } else {
                String::new()
            };
            (
                Item::Note {
                    folder: note.folder.clone(),
                    file: note.file.clone(),
                },
                title,
                date,
            )
        }
    };
    let label = gtk::Label::builder()
        .label(&text)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    // A selected folder with a note open is marked by its label alone.
    let current = match &item {
        Item::Folder(folder) => folder == current_folder && note_open,
        Item::Note { .. } => false,
    };
    omui::set_class(&label, "accent", current);
    content.append(&label);
    content.append(
        &gtk::Label::builder()
            .label(&side)
            .css_classes(["caption", "dim-label", "numeric"])
            .build(),
    );
    let list_row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["sidebar-row"])
        .build();
    list_row.update_property(&[gtk::accessible::Property::Label(&text)]);
    if let Item::Note { .. } = &item {
        drag_note(&list_row, &item, &text);
    } else {
        drop_on_folder(&list_row, &row.folder, handlers);
    }
    if row.expandable {
        list_row.update_state(&[gtk::accessible::State::Expanded(Some(row.expanded))]);
    }
    set_item(&list_row, item);
    list_row
}
