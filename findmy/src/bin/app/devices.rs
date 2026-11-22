//! The sidebar: one row per device with its model icon, battery and when it
//! was last seen. The icons are omui's, mapped from the names the library
//! gives a device class and a battery level. Every text of a row but the
//! device's name (the user's own, never translated) is in the language of
//! the moment, and written again on a switch.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use omui::i18n::{self, format_int};
use omui::icons::name;
use omui::{tr, trn};

use icloud_findmy::models::{self, Device, DeviceClass};

/// What the empty list says, built anew in the language of the moment: its
/// title and its description.
type Placeholder = Box<dyn Fn() -> (String, String)>;
pub struct DeviceList {
    pub widget: gtk::ScrolledWindow,
    list: gtk::ListBox,
    placeholder: omui::EmptyState,
    placeholder_text: RefCell<Placeholder>,
    rows: RefCell<Vec<DeviceRow>>,
    /// The devices the rows show, to write them again on a switch of
    /// language.
    devices: RefCell<Vec<Device>>,
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
        let placeholder = omui::EmptyState::new(Some(name::LOCATE), "", None).compact();
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
            placeholder_text: RefCell::new(Box::new(|| (tr("Looking for devices…"), String::new()))),
            rows: RefCell::default(),
            devices: RefCell::default(),
            rebuilding: Cell::new(false),
        });
        this.show_placeholder();
        // A switch of language writes the rows and the empty list's text
        // again; the devices' names stay as they are.
        let weak = Rc::downgrade(&this);
        i18n::on_change(&this.widget, move || {
            let Some(this) = weak.upgrade() else { return };
            this.show_placeholder();
            let now = icloud_session::time::now_ms();
            for (row, d) in this.rows.borrow().iter().zip(this.devices.borrow().iter()) {
                row.update(d, now);
            }
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

    /// What the empty list says (e.g. "Sign in required"): `text` gives its
    /// title and description, translated, now and after each switch of
    /// language.
    pub fn set_placeholder(&self, text: impl Fn() -> (String, String) + 'static) {
        *self.placeholder_text.borrow_mut() = Box::new(text);
        self.show_placeholder();
    }

    fn show_placeholder(&self) {
        let (title, description) = (self.placeholder_text.borrow())();
        self.placeholder.set_title(&title);
        self.placeholder.set_description(&description);
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
        *self.devices.borrow_mut() = devices.to_vec();
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
        icon.add_css_class("device");
        row.add_prefix(&icon);
        let lost = gtk::Image::from_icon_name(name::WARNING);
        i18n::bind_tooltip(&lost, "Lost Mode is on");
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
        self.row.set_subtitle(&summary(d, now_ms));
        self.icon.set_icon_name(Some(device_icon(d.class)));
        omui::set_class(&self.icon, "dim-label", !d.online);
        self.row
            .widget
            .set_tooltip_text((!d.online).then(|| tr("Offline")).as_deref());
        self.lost.set_visible(d.lost_mode_enabled);
        match d.battery {
            Some(level) => {
                self.battery.set_icon_name(Some(battery_icon(level, d.charging)));
                self.battery
                    .set_tooltip_text(Some(&tr!("{}%", format_int(percent(level)))));
                omui::set_class(&self.battery, "error", level < 0.2 && !d.charging);
                self.battery.set_visible(true);
            }
            None => self.battery.set_visible(false),
        }
    }
}

/// A battery level (0..=1) as a whole percentage.
fn percent(level: f64) -> i64 {
    (level * 100.0).round() as i64
}

/// "iPhone 15 Pro · 82% · 5 min ago", for the device's row and its marker's
/// tooltip, in the language of the moment: the model's name (Apple's), the
/// battery, and when it was located. The library's `Device::summary` is the
/// command line's, in English.
pub fn summary(d: &Device, now_ms: i64) -> String {
    let mut parts = vec![];
    if !d.model_name.is_empty() && d.model_name != d.name {
        parts.push(d.model_name.clone());
    }
    if let Some(level) = d.battery {
        let pct = format_int(percent(level));
        parts.push(if d.charging {
            tr!("{}% charging", pct)
        } else {
            tr!("{}%", pct)
        });
    }
    parts.push(match &d.location {
        Some(fix) => located(now_ms, fix.ts_ms),
        None => tr("No location"),
    });
    parts.join(" · ")
}

/// How long ago a fix at `ts_ms` was: "just now", "5 min ago", "3 h ago",
/// "yesterday", "2 days ago".
pub fn located(now_ms: i64, ts_ms: i64) -> String {
    let secs = (now_ms - ts_ms).max(0) / 1000;
    match secs {
        0..60 => tr("just now"),
        60..3600 => trn!("{n} min ago", secs / 60),
        3600..86_400 => trn!("{n} h ago", secs / 3600),
        86_400..172_800 => tr("yesterday"),
        _ => trn!("{n} days ago", secs / 86_400),
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

    /// The row's text follows the language (one test: the language is the
    /// process's), with the language's digits grouping; the names stay.
    #[test]
    fn a_device_summary_is_in_the_language_of_the_moment() {
        // The language is the process's: no other test switches it while this one has it.
        let _language = omui::i18n::lock_language();
        let now = 1_790_000_000_000;
        let d = Device {
            id: "a".into(),
            name: "Test's iPhone".into(),
            model_name: "iPhone 15 Pro".into(),
            class: DeviceClass::IPhone,
            battery: Some(0.82),
            charging: false,
            online: true,
            location: Some(icloud_findmy::models::Fix {
                lat: 0.0,
                lon: 0.0,
                accuracy: 10.0,
                ts_ms: now - 5 * 60_000,
                is_old: false,
            }),
            can_play_sound: true,
            can_lost_mode: true,
            lost_mode_enabled: false,
        };
        assert_eq!(summary(&d, now), "iPhone 15 Pro · 82% · 5 min ago");
        assert_eq!(located(now, now - 60_000), "1 min ago");
        assert_eq!(located(now, now - 86_400_000), "yesterday");
        assert_eq!(located(now, now - 3 * 86_400_000), "3 days ago");
        assert_eq!(located(now, now + 5_000), "just now");
        omui::i18n::register_all(&[
            ("en", include_str!("../../i18n/en.json")),
            (
                "da",
                r#"{"{}%": "{} %", "{n} min ago": {"one": "for {n} minut siden", "other": "for {n} minutter siden"}}"#,
            ),
        ]);
        omui::i18n::set_language("da");
        let danish = summary(&d, now);
        assert_eq!(danish, "iPhone 15 Pro · 82 % · for 5 minutter siden");
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
