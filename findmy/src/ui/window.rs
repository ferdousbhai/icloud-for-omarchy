//! The main window: device list beside the map, a header with refresh and
//! the device actions popover, and the sign-in banner. Refreshes every
//! [`REFRESH_SECS`] while the window is on screen.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use super::background;
use super::banner::SignInBanner;
use super::devices::DeviceList;
use super::map::DeviceMap;
use crate::findme::{self, FindMe, SessionTransport};
use crate::history::{History, Point};
use crate::models::{self, Device};

pub const REFRESH_SECS: u32 = 60;
/// How much history the trail shows.
const TRAIL_SECS: i64 = 24 * 3600;
const LOST_MESSAGE: &str = "This device has been lost. Please call me.";

type SharedFindMe = Arc<Mutex<FindMe<SessionTransport>>>;
type SharedHistory = Arc<Mutex<Option<History>>>;

pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    split: adw::OverlaySplitView,
    banner: SignInBanner,
    list: Rc<DeviceList>,
    map: Rc<DeviceMap>,
    findme: SharedFindMe,
    history: SharedHistory,
    devices: RefCell<Vec<Device>>,
    selected: RefCell<Option<String>>,
    busy: Cell<bool>,
    last_refresh: Cell<Option<Instant>>,
    last_error: RefCell<Option<String>>,
    centered: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let this = Rc::new_cyclic(|weak: &Weak<Window>| {
            let w = weak.clone();
            let list = DeviceList::new(move |id| {
                if let Some(this) = w.upgrade() {
                    this.select(id);
                }
            });
            let w = weak.clone();
            let map = DeviceMap::new(move |id| {
                if let Some(this) = w.upgrade() {
                    this.select(id);
                }
            });
            let (w1, w2) = (weak.clone(), weak.clone());
            let banner = SignInBanner::new(
                move || {
                    if let Some(this) = w1.upgrade() {
                        this.findme.lock().unwrap().reset();
                        this.refresh();
                    }
                },
                move |msg| {
                    if let Some(this) = w2.upgrade() {
                        this.toast(&msg);
                    }
                },
            );
            build(app, list, map, banner)
        });
        // Callbacks hold only weak refs; the window itself keeps this state
        // alive until it closes.
        let keep_alive = RefCell::new(Some(this.clone()));
        this.window.connect_close_request(move |_| {
            keep_alive.borrow_mut().take();
            glib::Propagation::Proceed
        });
        this.install_actions();
        this.start_refresh_loop();
        this.refresh();
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    fn selected_device(&self) -> Option<Device> {
        let id = self.selected.borrow().clone()?;
        self.devices.borrow().iter().find(|d| d.id == id).cloned()
    }

    fn install_actions(self: &Rc<Self>) {
        let refresh = gio::SimpleAction::new("refresh", None);
        let weak = Rc::downgrade(self);
        refresh.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                *this.last_error.borrow_mut() = None;
                this.refresh();
            }
        });
        self.window.add_action(&refresh);

        let sound = gio::SimpleAction::new("play-sound", None);
        let weak = Rc::downgrade(self);
        sound.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.play_sound();
            }
        });
        self.window.add_action(&sound);

        let lost = gio::SimpleAction::new("lost-mode", None);
        let weak = Rc::downgrade(self);
        lost.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.ask_lost_mode();
            }
        });
        self.window.add_action(&lost);
        self.update_actions();
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(action) = self
            .window
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(enabled);
        }
    }

    fn update_actions(&self) {
        let d = self.selected_device();
        self.set_action_enabled("play-sound", d.as_ref().is_some_and(|d| d.can_play_sound));
        self.set_action_enabled(
            "lost-mode",
            d.as_ref()
                .is_some_and(|d| d.can_lost_mode && !d.lost_mode_enabled),
        );
        self.set_action_enabled("refresh", !self.busy.get());
    }

    /// Ticks every [`REFRESH_SECS`]; refreshes only while the window is
    /// shown and not suspended (minimised, on another workspace), and
    /// catches up as soon as it comes back.
    fn start_refresh_loop(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(REFRESH_SECS, move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.window.is_visible() && !this.window.is_suspended() {
                this.refresh();
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        self.window.connect_suspended_notify(move |win| {
            let Some(this) = weak.upgrade() else { return };
            let stale = this
                .last_refresh
                .get()
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(REFRESH_SECS.into()));
            if !win.is_suspended() && stale {
                this.refresh();
            }
        });
    }

    /// Fetches devices on a worker thread, stores moved positions in the
    /// history, and updates the list and map.
    pub fn refresh(self: &Rc<Self>) {
        if self.busy.replace(true) {
            return;
        }
        self.update_actions();
        let (findme, history) = (self.findme.clone(), self.history.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let devices = findme.lock().unwrap().refresh()?;
                let mut history = history.lock().unwrap();
                if history.is_none() {
                    *history = History::open_default().ok();
                }
                if let Some(h) = history.as_ref()
                    && let Err(e) = h.record_devices(&devices)
                {
                    eprintln!("icloud-findmy: could not save history: {e}");
                }
                Ok::<_, findme::Error>(devices)
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.busy.set(false);
                this.last_refresh.set(Some(Instant::now()));
                match result {
                    Ok(Ok(devices)) => this.show_devices(devices),
                    Ok(Err(findme::Error::SignInRequired)) => {
                        this.banner.show();
                        if this.devices.borrow().is_empty() {
                            this.list.set_placeholder(
                                "Sign in required",
                                Some("Sign in to iCloud with the button above."),
                            );
                        }
                    }
                    Ok(Err(e)) => this.show_error(&e.to_string()),
                    Err(e) => this.show_error(&e),
                }
                this.update_actions();
            },
        );
    }

    /// Toasts an error once, not on every 60 s tick while it persists.
    fn show_error(&self, msg: &str) {
        if self.devices.borrow().is_empty() {
            self.list
                .set_placeholder("Could not load devices", Some(msg));
        }
        if self.last_error.borrow().as_deref() != Some(msg) {
            self.toast(msg);
            *self.last_error.borrow_mut() = Some(msg.to_string());
        }
    }

    fn show_devices(self: &Rc<Self>, devices: Vec<Device>) {
        self.banner.hide();
        *self.last_error.borrow_mut() = None;
        if devices.is_empty() {
            self.list.set_placeholder(
                "No devices",
                Some("Find My has no devices for this Apple ID."),
            );
        }
        // Keep the selection if the device is still there; otherwise pick
        // the first located one.
        let keep = self
            .selected
            .borrow()
            .clone()
            .filter(|id| devices.iter().any(|d| &d.id == id));
        let selected = keep.or_else(|| {
            devices
                .iter()
                .find(|d| d.location.is_some())
                .map(|d| d.id.clone())
        });
        *self.selected.borrow_mut() = selected.clone();
        self.list.set_devices(&devices, selected.as_deref());
        self.map.set_devices(&devices, selected.as_deref());
        *self.devices.borrow_mut() = devices;

        if let Some(d) = self.selected_device() {
            self.title.set_subtitle(&d.name);
            if !self.centered.replace(true)
                && let Some(fix) = d.location
            {
                self.map.show_initial(&fix);
            }
            self.load_trail(&d);
        }
        self.update_actions();
    }

    /// Selects a device (from the list or a marker): highlight, center, trail.
    fn select(self: &Rc<Self>, id: &str) {
        *self.selected.borrow_mut() = Some(id.to_string());
        self.list.select(id);
        let devices = self.devices.borrow().clone();
        self.map.set_devices(&devices, Some(id));
        let Some(d) = self.selected_device() else {
            return;
        };
        self.title.set_subtitle(&d.name);
        match d.location {
            Some(fix) => self.map.center_on(&fix),
            None => self.toast(&format!("No location for {}", d.name)),
        }
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
        self.load_trail(&d);
        self.update_actions();
    }

    fn load_trail(self: &Rc<Self>, device: &Device) {
        let (history, id) = (self.history.clone(), device.id.clone());
        let since = models::now_ms() / 1000 - TRAIL_SECS;
        let weak = Rc::downgrade(self);
        let for_id = id.clone();
        background(
            move || -> Vec<Point> {
                let history = history.lock().unwrap();
                history
                    .as_ref()
                    .and_then(|h| h.trail(&id, since).ok())
                    .unwrap_or_default()
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                // The user may have picked another device meanwhile.
                if this.selected.borrow().as_deref() != Some(for_id.as_str()) {
                    return;
                }
                let current = this.selected_device().and_then(|d| d.location);
                match result {
                    Ok(points) if points.len() + usize::from(current.is_some()) >= 2 => {
                        this.map.set_trail(&points, current.as_ref());
                    }
                    _ => this.map.clear_trail(),
                }
            },
        );
    }

    fn play_sound(self: &Rc<Self>) {
        let Some(device) = self.selected_device() else {
            return;
        };
        let findme = self.findme.clone();
        let weak = Rc::downgrade(self);
        let name = device.name.clone();
        background(
            move || findme.lock().unwrap().play_sound(&device),
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok(())) => this.toast(&format!("Playing a sound on {name}")),
                    Ok(Err(findme::Error::SignInRequired)) => this.banner.show(),
                    Ok(Err(e)) => this.toast(&e.to_string()),
                    Err(e) => this.toast(&e),
                }
            },
        );
    }

    fn ask_lost_mode(self: &Rc<Self>) {
        let Some(device) = self.selected_device() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&format!("Turn On Lost Mode for {}?", device.name)),
            Some(
                "The device locks and shows your message, with a button to call the number you give.",
            ),
        );
        let fields = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let phone = adw::EntryRow::builder()
            .title("Phone number")
            .input_purpose(gtk::InputPurpose::Phone)
            .build();
        let message = adw::EntryRow::builder()
            .title("Message")
            .text(LOST_MESSAGE)
            .build();
        fields.append(&phone);
        fields.append(&message);
        dialog.set_extra_child(Some(&fields));
        dialog.add_responses(&[("cancel", "_Cancel"), ("lost", "_Turn On")]);
        dialog.set_response_appearance("lost", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let weak = Rc::downgrade(self);
        dialog.connect_response(Some("lost"), move |_, _| {
            let Some(this) = weak.upgrade() else { return };
            let (phone, message) = (phone.text().to_string(), message.text().to_string());
            this.lost_mode(device.clone(), phone, message);
        });
        dialog.present(Some(&self.window));
    }

    fn lost_mode(self: &Rc<Self>, device: Device, phone: String, message: String) {
        let findme = self.findme.clone();
        let weak = Rc::downgrade(self);
        let name = device.name.clone();
        background(
            move || {
                findme
                    .lock()
                    .unwrap()
                    .lost_mode(&device, phone.trim(), message.trim())
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok(())) => {
                        this.toast(&format!("Lost Mode is on for {name}"));
                        this.refresh();
                    }
                    Ok(Err(findme::Error::SignInRequired)) => this.banner.show(),
                    Ok(Err(e)) => this.toast(&e.to_string()),
                    Err(e) => this.toast(&e),
                }
            },
        );
    }
}

fn build(
    app: &adw::Application,
    list: Rc<DeviceList>,
    map: Rc<DeviceMap>,
    banner: SignInBanner,
) -> Window {
    let title = adw::WindowTitle::new("Find My", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();

    let sidebar_toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Devices")
        .build();
    header.pack_start(&sidebar_toggle);
    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh (Ctrl+R)")
        .action_name("win.refresh")
        .build();
    header.pack_start(&refresh);

    let actions = gio::Menu::new();
    actions.append(Some("Play Sound"), Some("win.play-sound"));
    actions.append(Some("Lost Mode…"), Some("win.lost-mode"));
    let actions_button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Device Actions")
        .menu_model(&actions)
        .build();
    header.pack_end(&actions_button);

    let split = adw::OverlaySplitView::builder()
        .sidebar(&list.widget)
        .content(&map.widget)
        .min_sidebar_width(260.0)
        .max_sidebar_width(340.0)
        .build();
    split
        .bind_property("show-sidebar", &sidebar_toggle, "active")
        .bidirectional()
        .sync_create()
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner.widget);
    toolbar.set_content(Some(&split));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Find My")
        .icon_name("icloud-findmy")
        .default_width(1100)
        .default_height(720)
        .width_request(360)
        .height_request(320)
        .content(&toasts)
        .build();
    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 640sp").expect("valid condition"),
    );
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    Window {
        window,
        toasts,
        title,
        split,
        banner,
        list,
        map,
        findme: Arc::new(Mutex::new(FindMe::new(SessionTransport::default()))),
        history: Arc::default(),
        devices: RefCell::default(),
        selected: RefCell::default(),
        busy: Cell::new(false),
        last_refresh: Cell::new(None),
        last_error: RefCell::default(),
        centered: Cell::new(false),
    }
}
