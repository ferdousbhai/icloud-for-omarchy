//! Sidebar: All Photos, then the user's albums with counts.

use gtk::prelude::*;
use icloud_photos::catalog::AlbumRow;
use omui::icons::name;

pub struct Albums {
    pub list: gtk::ListBox,
}

/// All Photos is the row named "", an album the row named by its id. An
/// album's name is the user's, never translated; the count is written as the
/// language writes numbers.
fn row(id: Option<&str>, title: &str, count: i64, icon: &str) -> gtk::ListBoxRow {
    omui::sidebar_row(
        id.unwrap_or(""),
        Some(icon),
        title,
        Some(&omui::i18n::format_int(count)),
    )
}

impl Albums {
    pub fn new() -> Albums {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        // One Tab stop, with the arrow keys between albums: Tab on the way to
        // the grid or the search entry would otherwise load every album it passed.
        omui::single_tab_stop(&list);
        Albums { list }
    }

    /// Rebuild (also after a switch of language, for All Photos and the counts), keeping the selection (falls back to All Photos) and, when it is in the list, the keyboard: GTK
    /// takes it off a removed row only after the next frame, and a key pressed before then goes to that row, in no
    /// window, where no shortcut hears it.
    pub fn set(&self, total: i64, albums: &[AlbumRow], selected: Option<&str>) {
        let focused = self.list.focus_child().is_some();
        omui::clear(&self.list);
        let all = row(None, &omui::tr("All Photos"), total, name::GRID);
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
        if focused {
            to_select.grab_focus();
        }
    }

    /// The album id a row stands for; `None` is All Photos.
    pub fn id_of(row: &gtk::ListBoxRow) -> Option<String> {
        let name = row.widget_name();
        (!name.is_empty()).then(|| name.to_string())
    }
}
