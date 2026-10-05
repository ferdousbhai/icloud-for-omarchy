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
    rows: RefCell<Vec<DeviceRow>>,
    /// Set while rows are updated or the highlight is moved for the
    /// window, so that does not count as the user selecting (and does not
    /// re-center the map on every refresh).
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
            rows: RefCell::default(),
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

    /// The device id of a row, or `None` while the rows are updated.
    fn id_at(&self, row: &gtk::ListBoxRow) -> Option<String> {
        if self.rebuilding.get() {
            return None;
        }
        let rows = self.rows.borrow();
        rows.iter()
            .find(|r| r.row.upcast_ref::<gtk::ListBoxRow>() == row)
            .map(|r| r.id.clone())
    }

    /// Shows an empty-state message (e.g. "Sign in required").
    pub fn set_placeholder(&self, title: &str, description: Option<&str>) {
        self.placeholder.set_title(title);
        self.placeholder.set_description(description);
    }

    /// Shows `devices`. When the list holds the same devices in the same
    /// order (as on nearly every minute's refresh) the rows are updated in
    /// place, which costs next to nothing when nothing changed; otherwise
    /// they are rebuilt.
    pub fn set_devices(&self, devices: &[Device], selected: Option<&str>) {
        let now = icloud_session::time::now_ms();
        let same = {
            let rows = self.rows.borrow();
            rows.len() == devices.len() && rows.iter().zip(devices).all(|(r, d)| r.id == d.id)
        };
        self.rebuilding.set(true);
        if same {
            for (row, d) in self.rows.borrow().iter().zip(devices) {
                row.update(d, now);
            }
        } else {
            self.list.remove_all();
            let rows: Vec<DeviceRow> = devices.iter().map(|d| DeviceRow::new(d, now)).collect();
            for row in &rows {
                self.list.append(&row.row);
            }
            *self.rows.borrow_mut() = rows;
        }
        self.select_quietly(selected);
        self.rebuilding.set(false);
    }

    /// Moves the highlight to `id` without reporting it as the user's
    /// selection.
    pub fn select(&self, id: &str) {
        self.rebuilding.set(true);
        self.select_quietly(Some(id));
        self.rebuilding.set(false);
    }

    fn select_quietly(&self, id: Option<&str>) {
        let rows = self.rows.borrow();
        let Some(row) = id.and_then(|id| rows.iter().find(|r| r.id == id)) else {
            return;
        };
        let row = row.row.upcast_ref::<gtk::ListBoxRow>();
        if self.list.selected_row().as_ref() != Some(row) {
            self.list.select_row(Some(row));
        }
    }
}

/// One device's row and the widgets in it that change with the device.
struct DeviceRow {
    id: String,
    row: adw::ActionRow,
    icon: gtk::Image,
    lost: gtk::Image,
    battery: gtk::Image,
}

impl DeviceRow {
    fn new(d: &Device, now_ms: i64) -> Self {
        let row = adw::ActionRow::builder().use_markup(false).activatable(true).build();
        let icon = gtk::Image::new();
        icon.set_pixel_size(24);
        row.add_prefix(&icon);
        let lost = gtk::Image::from_icon_name("dialog-warning-symbolic");
        lost.set_tooltip_text(Some("Lost Mode is on"));
        lost.add_css_class("warning");
        row.add_suffix(&lost);
        let battery = gtk::Image::new();
        row.add_suffix(&battery);
        let this = Self {
            id: d.id.clone(),
            row,
            icon,
            lost,
            battery,
        };
        this.update(d, now_ms);
        this
    }

    /// Sets every property from `d`; unchanged values cost nothing.
    fn update(&self, d: &Device, now_ms: i64) {
        self.row.set_title(&d.name);
        self.row.set_subtitle(&d.summary(now_ms));
        self.icon.set_icon_name(Some(d.class.icon_name()));
        set_class(&self.icon, "dim-label", !d.online);
        self.row.set_tooltip_text((!d.online).then_some("Offline"));
        self.lost.set_visible(d.lost_mode_enabled);
        match d.battery {
            Some(level) => {
                self.battery
                    .set_icon_name(Some(&models::battery_icon_name(level, d.charging)));
                self.battery
                    .set_tooltip_text(Some(&format!("{}%", (level * 100.0).round() as i64)));
                set_class(&self.battery, "error", level < 0.2 && !d.charging);
                self.battery.set_visible(true);
            }
            None => self.battery.set_visible(false),
        }
    }
}

/// Adds or removes a CSS class.
pub fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}
