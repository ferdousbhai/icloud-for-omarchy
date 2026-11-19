//! The sidebar: one row per device with its model icon, battery and when it
//! was last seen. The icons are omui's, mapped from the names the library
//! gives a device class and a battery level.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use omui::icons::name;

use icloud_findmy::models::{self, Device, DeviceClass};

pub struct DeviceList {
    pub widget: gtk::ScrolledWindow,
    list: gtk::ListBox,
    placeholder: omui::EmptyState,
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
            .build();
        let placeholder = omui::EmptyState::new(Some(name::LOCATE), "Looking for devices…", None).compact();
        list.set_placeholder(Some(&placeholder.widget));
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .vexpand(true)
            .css_classes(["sidebar"])
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
        rows.iter().find(|r| &r.row.widget == row).map(|r| r.id.clone())
    }

    /// Shows an empty-state message (e.g. "Sign in required").
    pub fn set_placeholder(&self, title: &str, description: Option<&str>) {
        self.placeholder.set_title(title);
        self.placeholder.set_description(description.unwrap_or_default());
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
                self.list.append(&row.row.widget);
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
        if self.list.selected_row().as_ref() != Some(&row.row.widget) {
            self.list.select_row(Some(&row.row.widget));
        }
    }
}

/// One device's row and the widgets in it that change with the device.
struct DeviceRow {
    id: String,
    row: omui::Row,
    icon: gtk::Image,
    lost: gtk::Image,
    battery: gtk::Image,
}

impl DeviceRow {
    fn new(d: &Device, now_ms: i64) -> Self {
        let row = omui::Row::new("", "");
        row.widget.set_activatable(true);
        let icon = gtk::Image::new();
        icon.set_pixel_size(24);
        row.add_prefix(&icon);
        let lost = gtk::Image::from_icon_name(name::WARNING);
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
        self.icon.set_icon_name(Some(device_icon(d.class)));
        omui::set_class(&self.icon, "dim-label", !d.online);
        self.row.widget.set_tooltip_text((!d.online).then_some("Offline"));
        self.lost.set_visible(d.lost_mode_enabled);
        match d.battery {
            Some(level) => {
                self.battery.set_icon_name(Some(battery_icon(level, d.charging)));
                self.battery
                    .set_tooltip_text(Some(&format!("{}%", (level * 100.0).round() as i64)));
                omui::set_class(&self.battery, "error", level < 0.2 && !d.charging);
                self.battery.set_visible(true);
            }
            None => self.battery.set_visible(false),
        }
    }
}

/// omui's icon for a device class, by the Adwaita name the library gives it.
pub fn device_icon(class: DeviceClass) -> &'static str {
    match class.icon_name() {
        "phone-symbolic" => name::PHONE,
        "tablet-symbolic" => name::TABLET,
        "computer-symbolic" => name::COMPUTER,
        "preferences-system-time-symbolic" => name::CLOCK,
        "audio-headphones-symbolic" => name::HEADPHONES,
        _ => name::LOCATION,
    }
}

/// omui's battery icon, by the Adwaita name the library gives a level: its
/// five steps against the library's eleven.
fn battery_icon(level: f64, charging: bool) -> &'static str {
    match models::battery_icon_name(level, charging).as_str() {
        n if n.ends_with("-charging-symbolic") || n.ends_with("-charged-symbolic") => name::BATTERY_CHARGING,
        "battery-level-0-symbolic" | "battery-level-10-symbolic" => name::BATTERY_EMPTY,
        "battery-level-20-symbolic" | "battery-level-30-symbolic" => name::BATTERY_LOW,
        "battery-level-40-symbolic"
        | "battery-level-50-symbolic"
        | "battery-level-60-symbolic"
        | "battery-level-70-symbolic" => name::BATTERY_MEDIUM,
        _ => name::BATTERY_FULL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_device_class_has_its_own_icon() {
        use DeviceClass::*;
        let icons: Vec<_> = [IPhone, IPad, Mac, Watch, AirPods].map(device_icon).into();
        assert!(!icons.contains(&name::LOCATION), "{icons:?}");
        assert!(
            icons.iter().all(|i| icons.iter().filter(|j| j == &i).count() == 1),
            "{icons:?}"
        );
        assert_eq!(device_icon(Other), name::LOCATION);
    }

    #[test]
    fn battery_icons_step_with_the_level() {
        let steps: Vec<_> = (0..=10)
            .map(|tenth| battery_icon(f64::from(tenth) / 10.0, false))
            .collect();
        use name::{BATTERY_EMPTY as E, BATTERY_FULL as F, BATTERY_LOW as L, BATTERY_MEDIUM as M};
        assert_eq!(steps, [E, E, L, L, M, M, M, M, F, F, F]);
        assert_eq!(battery_icon(0.04, true), name::BATTERY_CHARGING);
        assert_eq!(battery_icon(1.0, true), name::BATTERY_CHARGING);
    }
}
