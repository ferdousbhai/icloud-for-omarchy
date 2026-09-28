//! Sidebar: All Photos, then the user's albums with counts.

use adw::prelude::*;
use icloud_photos::catalog::AlbumRow;

pub struct Albums {
    pub list: gtk::ListBox,
}

fn row(id: Option<&str>, name: &str, count: i64, icon: &str) -> gtk::ListBoxRow {
    let b = gtk::Box::builder().spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(&gtk::Label::builder().label(name).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
    let n = gtk::Label::new(Some(&count.to_string()));
    n.add_css_class("dim-label");
    n.add_css_class("numeric");
    b.append(&n);
    let r = gtk::ListBoxRow::builder().child(&b).build();
    r.set_widget_name(id.unwrap_or(""));
    r
}

impl Albums {
    pub fn new() -> Albums {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        list.add_css_class("navigation-sidebar");
        Albums { list }
    }

    /// Rebuild, keeping the selection (falls back to All Photos).
    pub fn set(&self, total: i64, albums: &[AlbumRow], selected: Option<&str>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let all = row(None, "All Photos", total, "view-grid-symbolic");
        self.list.append(&all);
        let mut to_select = all.clone();
        for a in albums {
            let r = row(Some(&a.id), &a.name, a.count, "folder-pictures-symbolic");
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

    pub fn title_of(row: &gtk::ListBoxRow) -> String {
        row.child()
            .and_then(|b| b.first_child())
            .and_then(|i| i.next_sibling())
            .and_downcast::<gtk::Label>()
            .map(|l| l.label().to_string())
            .unwrap_or_else(|| "Photos".into())
    }
}
