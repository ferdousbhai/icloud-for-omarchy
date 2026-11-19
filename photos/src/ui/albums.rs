//! Sidebar: All Photos, then the user's albums with counts.

use gtk::prelude::*;
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
