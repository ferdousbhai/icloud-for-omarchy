//! The sidebar: one row per device with its model icon, battery and when it
//! was last seen.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::models::{self, Device};

pub struct DeviceList {
    pub widget: gtk::ScrolledWindow,
    list: gtk::ListBox,
    placeholder: adw::StatusPage,
    ids: RefCell<Vec<String>>,
    /// Set while rows are rebuilt, so restoring the selection does not
    /// re-center the map on every refresh.
    rebuilding: Cell<bool>,
}

impl DeviceList {
    /// `on_select` runs whenever the user moves the selection (click or
    /// keyboard); `on_activate` when they click a row or press Enter on it.
    pub fn new(on_select: impl Fn(&str) + 'static, on_activate: impl Fn(&str) + 'static) -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["navigation-sidebar"])
            .build();
        let placeholder = adw::StatusPage::builder()
            .icon_name("find-location-symbolic")
            .title("Looking for devices…")
            .css_classes(["compact"])
            .build();
        list.set_placeholder(Some(&placeholder));
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .vexpand(true)
            .build();
        let this = Rc::new(Self {
            widget,
            list,
            placeholder,
            ids: RefCell::default(),
            rebuilding: Cell::new(false),
        });
        // The selection drives the window's selected device, so moving the
        // highlight with the arrow keys selects that device too.
        let weak = Rc::downgrade(&this);
        this.list.connect_row_selected(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(id) = row.and_then(|r| this.id_at(r)) {
                on_select(&id);
            }
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(id) = this.id_at(row) {
                on_activate(&id);
            }
        });
        this.list.set_activate_on_single_click(true);
        this
    }

    /// The device id of a row, or `None` while the rows are rebuilt.
    fn id_at(&self, row: &gtk::ListBoxRow) -> Option<String> {
        if self.rebuilding.get() {
            return None;
        }
        self.ids.borrow().get(usize::try_from(row.index()).ok()?).cloned()
    }

    /// Shows an empty-state message (e.g. "Sign in required").
    pub fn set_placeholder(&self, title: &str, description: Option<&str>) {
        self.placeholder.set_title(title);
        self.placeholder.set_description(description);
    }

    pub fn set_devices(&self, devices: &[Device], selected: Option<&str>) {
        self.rebuilding.set(true);
        self.list.remove_all();
        let now = models::now_ms();
        let mut ids = Vec::with_capacity(devices.len());
        for d in devices {
            let row = device_row(d, now);
            self.list.append(&row);
            if selected == Some(d.id.as_str()) {
                self.list.select_row(Some(&row));
            }
            ids.push(d.id.clone());
        }
        *self.ids.borrow_mut() = ids;
        self.rebuilding.set(false);
    }

    pub fn select(&self, id: &str) {
        let index = self.ids.borrow().iter().position(|i| i == id);
        if let Some(row) = index.and_then(|i| self.list.row_at_index(i as i32)) {
            self.rebuilding.set(true);
            self.list.select_row(Some(&row));
            self.rebuilding.set(false);
        }
    }
}

fn device_row(d: &Device, now_ms: i64) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&d.name)
        .subtitle(d.summary(now_ms))
        .use_markup(false)
        .activatable(true)
        .build();
    let icon = gtk::Image::from_icon_name(d.class.icon_name());
    icon.set_pixel_size(24);
    if !d.online {
        icon.add_css_class("dim-label");
        row.set_tooltip_text(Some("Offline"));
    }
    row.add_prefix(&icon);
    if d.lost_mode_enabled {
        let lost = gtk::Image::from_icon_name("dialog-warning-symbolic");
        lost.set_tooltip_text(Some("Lost Mode is on"));
        lost.add_css_class("warning");
        row.add_suffix(&lost);
    }
    if let Some(level) = d.battery {
        let battery = gtk::Image::from_icon_name(&models::battery_icon_name(level, d.charging));
        battery.set_tooltip_text(Some(&format!("{}%", (level * 100.0).round() as i64)));
        if level < 0.2 && !d.charging {
            battery.add_css_class("error");
        }
        row.add_suffix(&battery);
    }
    row
}
