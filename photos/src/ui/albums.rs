//! Sidebar: All Photos, then the user's albums with counts.

use gtk::prelude::*;
use gtk::{gdk, glib};
use icloud_photos::catalog::AlbumRow;
use omui::icons::name;

pub struct Albums {
    pub list: gtk::ListBox,
}

/// All Photos is the row named "", an album the row named by its id.
fn row(id: Option<&str>, title: &str, count: i64, icon: &str) -> gtk::ListBoxRow {
    omui::sidebar_row(id.unwrap_or(""), Some(icon), title, Some(&count.to_string()))
}

impl Albums {
    pub fn new() -> Albums {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        // One Tab stop, with the arrow keys between albums. A list box walks
        // Tab from row to row and selects each, so Tab on the way to the grid
        // or the search entry would load every album it passed.
        let tab = gtk::EventControllerKey::new();
        tab.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = list.downgrade();
        tab.connect_key_pressed(move |_, key, _, modifiers| {
            let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            let direction = match key {
                _ if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) => None,
                gdk::Key::ISO_Left_Tab => Some(gtk::DirectionType::TabBackward),
                gdk::Key::Tab | gdk::Key::KP_Tab if shift => Some(gtk::DirectionType::TabBackward),
                gdk::Key::Tab | gdk::Key::KP_Tab => Some(gtk::DirectionType::TabForward),
                _ => None,
            };
            let list = weak.upgrade();
            let (Some(direction), Some(list)) = (direction, list) else {
                return glib::Propagation::Proceed;
            };
            let Some(root) = list.root() else {
                return glib::Propagation::Proceed;
            };
            // Unfocusable for the move, the list is passed over as a whole.
            list.set_can_focus(false);
            if !root.child_focus(direction) {
                // Off either end: round to the other, as the window's own Tab does.
                root.set_focus(gtk::Widget::NONE);
                root.child_focus(direction);
            }
            list.set_can_focus(true);
            glib::Propagation::Stop
        });
        list.add_controller(tab);
        Albums { list }
    }

    /// Rebuild, keeping the selection (falls back to All Photos).
    pub fn set(&self, total: i64, albums: &[AlbumRow], selected: Option<&str>) {
        omui::clear(&self.list);
        let all = row(None, "All Photos", total, name::GRID);
        self.list.append(&all);
        let mut to_select = all;
        for a in albums {
            let r = row(Some(&a.id), &a.name, a.count, name::IMAGE);
            if selected == Some(a.id.as_str()) {
                to_select = r.clone();
            }
            self.list.append(&r);
        }
        self.list.select_row(Some(&to_select));
    }

    /// The album id a row stands for; `None` is All Photos.
    pub fn id_of(row: &gtk::ListBoxRow) -> Option<String> {
        let name = row.widget_name();
        (!name.is_empty()).then(|| name.to_string())
    }
}
