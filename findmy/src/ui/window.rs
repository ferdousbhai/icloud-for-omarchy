//! The main window: device list beside the map, a header with refresh and
//! the device actions popover, and the sign-in (and Find My password)
//! banner. Refreshes every [`REFRESH_SECS`] while the window is on screen.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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

type SharedHistory = Arc<Mutex<Option<History>>>;

/// The Find My client, shared with the worker threads. Workers hold the
/// lock across blocking HTTP, so the main loop never takes it: it asks for
/// a reset through `reset_pending`, which the next worker applies.
struct Client {
    findme: Mutex<FindMe<SessionTransport>>,
    reset_pending: AtomicBool,
}

impl Default for Client {
    fn default() -> Self {
        Self {
            findme: Mutex::new(FindMe::new(SessionTransport::default())),
            reset_pending: AtomicBool::new(false),
        }
    }
}

impl Client {
    /// Worker threads only. Applies a pending reset; a client left behind
    /// by a panicked worker is reset rather than trusted.
    fn lock(&self) -> MutexGuard<'_, FindMe<SessionTransport>> {
        let mut findme = self.findme.lock().unwrap_or_else(|poisoned| {
            self.findme.clear_poison();
            let mut findme = poisoned.into_inner();
            findme.reset();
            findme
        });
        if self.reset_pending.swap(false, Ordering::SeqCst) {
            findme.reset();
        }
        findme
    }
}

/// Locks `m` even if a worker panicked while holding it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| {
        m.clear_poison();
        poisoned.into_inner()
    })
}

pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    split: adw::OverlaySplitView,
    banner: SignInBanner,
    list: Rc<DeviceList>,
    map: Rc<DeviceMap>,
    client: Arc<Client>,
    history: SharedHistory,
    devices: RefCell<Vec<Device>>,
    selected: RefCell<Option<String>>,
    /// The selected device's name, shown while it is unavailable.
    selected_name: RefCell<String>,
    /// A refresh is in flight.
    busy: Cell<bool>,
    /// A refresh asked for while one was in flight, and whether it should
    /// locate; runs when the current one finishes.
    refresh_queued: Cell<Option<bool>>,
    /// Play Sound or Lost Mode is in flight.
    acting: Cell<bool>,
    /// Bumped on every sign-in and Find My authorization: a
    /// `SignInRequired` or `FindMyAuthRequired` from a request started
    /// before the latest one is stale and ignored.
    sign_in_gen: Cell<u64>,
    /// Devices this app turned Lost Mode on for, until a refresh reports it.
    lost_sent: RefCell<HashSet<String>>,
    last_refresh: Cell<Option<Instant>>,
    last_error: RefCell<Option<String>>,
    centered: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let this = Rc::new_cyclic(|weak: &Weak<Window>| {
            let w = weak.clone();
            let w2 = weak.clone();
            let list = DeviceList::new(
                move |id| {
                    if let Some(this) = w.upgrade() {
                        this.select(id);
                    }
                },
                move |id| {
                    if let Some(this) = w2.upgrade() {
                        this.activate(id);
                    }
                },
            );
            let w = weak.clone();
            let map = DeviceMap::new(move |id| {
                if let Some(this) = w.upgrade() {
                    this.select(id);
                    this.hide_sidebar_if_overlaid();
                }
            });
            let (w1, w2) = (weak.clone(), weak.clone());
            let banner = SignInBanner::new(
                move || {
                    if let Some(this) = w1.upgrade() {
                        this.signed_in();
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
        this.refresh(true);
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// The account just signed in (maybe a different one), or Find My was
    /// just authorized with a fresh session: start the Find My session over
    /// and locate the devices, after any refresh in flight.
    fn signed_in(self: &Rc<Self>) {
        self.sign_in_gen.set(self.sign_in_gen.get() + 1);
        self.client.reset_pending.store(true, Ordering::SeqCst);
        *self.last_error.borrow_mut() = None;
        self.refresh(true);
    }

    /// Whether a `SignInRequired` from a request started at `sign_in_gen`
    /// still means what it says.
    fn sign_in_is_current(&self, sign_in_gen: u64) -> bool {
        sign_in_gen == self.sign_in_gen.get()
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
                this.refresh(true);
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
        let d = self.selected_device().filter(|_| !self.acting.get());
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
                this.refresh(false);
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
                this.refresh(false);
            }
        });
    }

    /// Fetches devices on a worker thread, stores moved positions in the
    /// history, and updates the list and map. `locate` asks every device to
    /// report its position: only on first load and when the user asks, not
    /// on timer ticks, so the devices are not woken every minute.
    /// While one is in flight, the next is queued (a queued `locate` wins).
    pub fn refresh(self: &Rc<Self>, locate: bool) {
        if self.busy.replace(true) {
            let queued = self.refresh_queued.get().unwrap_or(false);
            self.refresh_queued.set(Some(queued || locate));
            return;
        }
        self.update_actions();
        let (client, history) = (self.client.clone(), self.history.clone());
        let sign_in_gen = self.sign_in_gen.get();
        let weak = Rc::downgrade(self);
        background(
            move || {
                let devices = client.lock().refresh(locate)?;
                let mut history = lock(&history);
                if history.is_none() {
                    *history = History::open_default().ok();
                }
                if let Some(h) = history.as_ref()
                    && let Err(e) = h.record_devices(&devices, models::now_ms() / 1000)
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
                    // Started before the latest sign-in; the refresh queued
                    // by the sign-in answers for the new session.
                    Ok(Err(findme::Error::SignInRequired))
                        if !this.sign_in_is_current(sign_in_gen) => {}
                    Ok(Err(findme::Error::SignInRequired)) => {
                        this.banner.show();
                        if this.devices.borrow().is_empty() {
                            this.list.set_placeholder(
                                "Sign in required",
                                Some("Sign in to iCloud with the button above."),
                            );
                        }
                    }
                    Ok(Err(findme::Error::FindMyAuthRequired))
                        if !this.sign_in_is_current(sign_in_gen) => {}
                    Ok(Err(findme::Error::FindMyAuthRequired)) => {
                        this.banner.show_find_my();
                        if this.devices.borrow().is_empty() {
                            this.list.set_placeholder(
                                "Find My needs your Apple password",
                                Some("Enter it with the button above."),
                            );
                        }
                    }
                    Ok(Err(e)) => this.show_error(&e.to_string()),
                    Err(e) => this.show_error(&e),
                }
                this.update_actions();
                if let Some(locate) = this.refresh_queued.take() {
                    this.refresh(locate);
                }
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

    fn show_devices(self: &Rc<Self>, mut devices: Vec<Device>) {
        self.banner.hide();
        // Lost Mode this app turned on shows as on until Apple reports it,
        // so a refresh that started earlier does not re-offer it.
        {
            let mut sent = self.lost_sent.borrow_mut();
            for d in &mut devices {
                if d.lost_mode_enabled {
                    sent.remove(&d.id);
                } else if sent.contains(&d.id) {
                    d.lost_mode_enabled = true;
                }
            }
        }
        *self.last_error.borrow_mut() = None;
        if devices.is_empty() {
            self.list.set_placeholder(
                "No devices",
                Some("Find My has no devices for this Apple ID."),
            );
        }
        // Keep the selection even if the device left the list (it shows as
        // unavailable, and comes back if the device does); with nothing
        // selected yet, pick the first located device.
        let kept = self.selected.borrow().clone();
        let vanished = kept.as_ref().is_some_and(|id| {
            self.devices.borrow().iter().any(|d| &d.id == id)
                && !devices.iter().any(|d| &d.id == id)
        });
        let selected = kept.or_else(|| {
            devices
                .iter()
                .find(|d| d.location.is_some())
                .map(|d| d.id.clone())
        });
        *self.selected.borrow_mut() = selected.clone();
        self.list.set_devices(&devices, selected.as_deref());
        self.map.set_devices(&devices, selected.as_deref());
        *self.devices.borrow_mut() = devices;

        match (self.selected_device(), selected) {
            (Some(d), _) => {
                self.show_selected(&d);
                if !self.centered.replace(true)
                    && let Some(fix) = d.location
                {
                    self.map.show_initial(&fix);
                }
                self.load_trail(&d);
            }
            (None, Some(_)) => {
                let name = self.selected_name.borrow().clone();
                self.title.set_subtitle(&format!("{name} (unavailable)"));
                self.map.clear_trail();
                if vanished {
                    self.toast(&format!("{name} is no longer in Find My"));
                }
            }
            (None, None) => {
                self.title.set_subtitle("");
                self.map.clear_trail();
            }
        }
        self.update_actions();
    }

    /// Names the selected device in the header, and remembers the name in
    /// case the device later leaves the list.
    fn show_selected(&self, d: &Device) {
        self.title.set_subtitle(&d.name);
        self.selected_name.replace(d.name.clone());
    }

    /// Selects a device (from the list, by mouse or keyboard, or a marker):
    /// highlight, center, trail.
    fn select(self: &Rc<Self>, id: &str) {
        *self.selected.borrow_mut() = Some(id.to_string());
        self.list.select(id);
        let devices = self.devices.borrow().clone();
        self.map.set_devices(&devices, Some(id));
        let Some(d) = self.selected_device() else {
            return;
        };
        self.show_selected(&d);
        match d.location {
            Some(fix) => self.map.center_on(&fix),
            None => self.toast(&format!("No location for {}", d.name)),
        }
        self.load_trail(&d);
        self.update_actions();
    }

    /// A list row was clicked or had Enter pressed: it is already selected
    /// (row selection comes first), so re-center on it and, when the sidebar
    /// covers the map, get it out of the way.
    fn activate(&self, id: &str) {
        if self.selected.borrow().as_deref() != Some(id) {
            return;
        }
        if let Some(fix) = self.selected_device().and_then(|d| d.location) {
            self.map.center_on(&fix);
        }
        self.hide_sidebar_if_overlaid();
    }

    fn hide_sidebar_if_overlaid(&self) {
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
    }

    fn load_trail(self: &Rc<Self>, device: &Device) {
        let (history, id) = (self.history.clone(), device.id.clone());
        let since = models::now_ms() / 1000 - TRAIL_SECS;
        let weak = Rc::downgrade(self);
        let for_id = id.clone();
        background(
            move || -> Vec<Point> {
                let history = lock(&history);
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
                let Some(device) = this.selected_device() else {
                    // It left the list meanwhile.
                    this.map.clear_trail();
                    return;
                };
                let current = device.location;
                match result {
                    Ok(points) if points.len() + usize::from(current.is_some()) >= 2 => {
                        this.map.set_trail(&points, current.as_ref());
                    }
                    _ => this.map.clear_trail(),
                }
            },
        );
    }

    /// Marks a device action in flight, disabling Play Sound and Lost Mode
    /// until [`Self::end_action`]. False if one already is.
    fn begin_action(&self) -> bool {
        if self.acting.replace(true) {
            return false;
        }
        self.update_actions();
        true
    }

    fn end_action(&self) {
        self.acting.set(false);
        self.update_actions();
    }

    fn play_sound(self: &Rc<Self>) {
        let Some(device) = self.selected_device() else {
            return;
        };
        if !self.begin_action() {
            return;
        }
        let client = self.client.clone();
        let sign_in_gen = self.sign_in_gen.get();
        let weak = Rc::downgrade(self);
        let name = device.name.clone();
        background(
            move || client.lock().play_sound(&device),
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.end_action();
                match result {
                    Ok(Ok(())) => this.toast(&format!("Playing a sound on {name}")),
                    Ok(Err(findme::Error::SignInRequired))
                        if !this.sign_in_is_current(sign_in_gen) =>
                    {
                        this.toast("Signed in again: try once more");
                    }
                    Ok(Err(findme::Error::SignInRequired)) => this.banner.show(),
                    Ok(Err(findme::Error::FindMyAuthRequired))
                        if !this.sign_in_is_current(sign_in_gen) =>
                    {
                        this.toast("Find My was authorized again: try once more");
                    }
                    Ok(Err(findme::Error::FindMyAuthRequired)) => this.banner.show_find_my(),
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
        if self.acting.get() {
            return;
        }
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
        if !self.begin_action() {
            return;
        }
        let client = self.client.clone();
        let sign_in_gen = self.sign_in_gen.get();
        let weak = Rc::downgrade(self);
        let (id, name) = (device.id.clone(), device.name.clone());
        background(
            move || {
                client
                    .lock()
                    .lost_mode(&device, phone.trim(), message.trim())
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.acting.set(false);
                match result {
                    Ok(Ok(())) => {
                        this.toast(&format!("Lost Mode is on for {name}"));
                        this.lost_mode_sent(&id);
                        this.refresh(false);
                    }
                    Ok(Err(findme::Error::SignInRequired))
                        if !this.sign_in_is_current(sign_in_gen) =>
                    {
                        this.toast("Signed in again: try once more");
                    }
                    Ok(Err(findme::Error::SignInRequired)) => this.banner.show(),
                    Ok(Err(findme::Error::FindMyAuthRequired))
                        if !this.sign_in_is_current(sign_in_gen) =>
                    {
                        this.toast("Find My was authorized again: try once more");
                    }
                    Ok(Err(findme::Error::FindMyAuthRequired)) => this.banner.show_find_my(),
                    Ok(Err(e)) => this.toast(&e.to_string()),
                    Err(e) => this.toast(&e),
                }
                this.update_actions();
            },
        );
    }

    /// Lost Mode was turned on: show it right away rather than waiting for
    /// Apple to report it.
    fn lost_mode_sent(&self, id: &str) {
        self.lost_sent.borrow_mut().insert(id.to_string());
        let devices = {
            let mut devices = self.devices.borrow_mut();
            if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
                d.lost_mode_enabled = true;
            }
            devices.clone()
        };
        let selected = self.selected.borrow().clone();
        self.list.set_devices(&devices, selected.as_deref());
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
        .icon_name(super::APP_ID)
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
        client: Arc::default(),
        history: Arc::default(),
        devices: RefCell::default(),
        selected: RefCell::default(),
        selected_name: RefCell::default(),
        busy: Cell::new(false),
        refresh_queued: Cell::new(None),
        acting: Cell::new(false),
        sign_in_gen: Cell::new(0),
        lost_sent: RefCell::default(),
        last_refresh: Cell::new(None),
        last_error: RefCell::default(),
        centered: Cell::new(false),
    }
}
