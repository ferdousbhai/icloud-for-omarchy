//! The main window and the app state every part of the UI shares.
//!
//! All network and disk-heavy work runs on threads; results come back as
//! [`Msg`]s over an async channel drained on the GTK main loop.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use icloud_photos::catalog::Catalog;
use icloud_photos::cloudkit::{CloudKit, Modified};
use icloud_photos::config::{Dirs, DownloadMode, Settings};
use icloud_photos::sync::{self, Progress, Report};
use icloud_photos::thumbs::{self, Downloader, Job, Priority};
use icloud_photos::transport::{Error, Result, SignInState, Transport};
use omui::icons::name;
use omui::{ShortcutOverlay, ShortcutSection, i18n, tr, trn};

use super::albums::Albums;
use super::banner::BannerView;
use super::decode;
use super::grid::Grid;
use super::upload::UploadMsg;
use super::viewer::Viewer;
use super::{Relabel, relabel};

pub enum Msg {
    TransportReady(Result<Arc<dyn Transport>>),
    SyncProgress(Progress),
    SyncDone(Result<Report>),
    Download(thumbs::Event),
    Upload(UploadMsg),
    /// icloud-sessiond's sign-in state changed (or was first read).
    SignIn(SignInState),
    /// The sign-in window could not be opened.
    SignInFailed(Error),
}

/// Recently decoded thumbnails, bounded by their decoded size.
#[derive(Default)]
pub struct Textures {
    map: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
    bytes: usize,
    loading: HashSet<String>,
}

impl Textures {
    /// Decoded RGBA, so a few hundred thumbnails, not thousands.
    const CAP_BYTES: usize = 256 << 20;

    pub fn get(&self, id: &str) -> Option<gdk::Texture> {
        self.map.get(id).cloned()
    }

    fn size(t: &gdk::Texture) -> usize {
        t.width().max(0) as usize * t.height().max(0) as usize * 4
    }

    fn insert(&mut self, id: String, t: gdk::Texture) {
        self.bytes += Self::size(&t);
        match self.map.insert(id.clone(), t) {
            Some(old) => self.bytes -= Self::size(&old),
            None => self.order.push_back(id),
        }
        while self.bytes > Self::CAP_BYTES && self.order.len() > 1 {
            if let Some(old) = self.order.pop_front()
                && let Some(t) = self.map.remove(&old)
            {
                self.bytes -= Self::size(&t);
            }
        }
    }
}

/// What a reload shows, read off the main loop.
struct Snapshot {
    albums: Vec<icloud_photos::catalog::AlbumRow>,
    total: i64,
    album: Option<String>,
    tiles: Vec<Arc<super::grid::Tile>>,
    rows: Vec<super::grid::RowItem>,
    columns: usize,
    first_sync: bool,
}

impl Snapshot {
    /// The album's name, which is the user's and never translated, or All Photos.
    fn title(&self) -> String {
        self.album
            .as_ref()
            .and_then(|id| self.albums.iter().find(|album| &album.id == id))
            .map_or_else(|| tr("All Photos"), |album| album.name.clone())
    }
}

/// What the line under the search entry says about the matches shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notice {
    /// No search.
    None,
    Searching,
    /// Recall ranked every match: none by filename.
    MeaningOnly,
    /// Filename matches, and this many more by meaning (maybe none).
    Filenames {
        names: usize,
        meaning: usize,
    },
    /// `~` asked for matches by meaning, and Recall could not give them.
    RecallUnavailable,
    /// Without Recall: matches by filename alone.
    FilenamesOnly,
}

impl Notice {
    /// In the language in use.
    fn text(self) -> String {
        match self {
            Notice::None => String::new(),
            Notice::Searching => tr("Searching…"),
            Notice::MeaningOnly => tr("Matches by meaning · cached thumbnails only"),
            Notice::Filenames { names, meaning: 0 } => trn("{n} filename matches", names as i64),
            Notice::Filenames { names, meaning } => omui::trnf(
                "{n} filename matches · {} by meaning",
                names as i64,
                &[&i18n::format_int(meaning as i64)],
            ),
            Notice::RecallUnavailable => tr("Recall unavailable or warming up · filename matches"),
            Notice::FilenamesOnly => tr("Filename matches"),
        }
    }
}

/// How old the last sync may be when the window comes back (to the front, or on screen) before it syncs again, and
/// how often a window on screen syncs by itself.
const FOCUS_SYNC_AFTER: Duration = Duration::from_secs(120);
const PERIODIC_SYNC_SECS: u32 = 600;

/// The periodic sync runs only for a window on screen: shown, and not suspended by the compositor.
fn timer_syncs(visible: bool, suspended: bool) -> bool {
    visible && !suspended
}

/// A window coming back syncs once its last sync, at `last`, is [`FOCUS_SYNC_AFTER`] old. Before the first sync
/// has ended there is one on its way already.
fn stale_on_return(last: Option<Instant>) -> bool {
    last.is_some_and(|at| at.elapsed() > FOCUS_SYNC_AFTER)
}

pub struct App {
    pub window: gtk::ApplicationWindow,
    shell: Rc<omui::Shell>,
    pub banner: omui::Banner,
    /// What the banner says, written again after a switch of language.
    pub banner_view: RefCell<BannerView>,
    /// The photos, with the viewer pushed over them.
    pub nav: omui::Nav,
    grid_title: omui::Title,
    sidebar_title: omui::Title,
    pub split: omui::Split,
    /// The sync status line under the albums.
    status: Relabel<gtk::Label>,
    search: gtk::SearchEntry,
    search_hint: gtk::Label,
    search_notice: Relabel<gtk::Label>,
    search_query: RefCell<String>,
    pub spinner: gtk::Spinner,
    pub grid: Grid,
    pub albums: Albums,
    pub viewer: Viewer,
    pub dirs: Dirs,
    pub settings: RefCell<Settings>,
    pub cat: Catalog,
    pub tx: async_channel::Sender<Msg>,
    pub transport: RefCell<Option<Arc<dyn Transport>>>,
    pub downloader: RefCell<Option<Downloader>>,
    /// `load_transport` is running: one connection, one download pool.
    connecting: Cell<bool>,
    syncing: Cell<bool>,
    sync_again: Cell<bool>,
    pub last_sync: Cell<Option<Instant>>,
    pub album: RefCell<Option<String>>,
    pub textures: RefCell<Textures>,
    /// Originals to open, or to announce, once downloaded.
    want_open: RefCell<HashSet<String>>,
    want_toast: RefCell<HashSet<String>>,
    reloading: Cell<bool>,
    /// Bumped per reload; only the newest reload's snapshot is shown.
    reload_gen: Cell<u64>,
    /// The sign-in window is open (or was just asked for).
    pub signing_in: Cell<bool>,
    /// Why the account was signed out, as the daemon last said.
    pub sign_out_reason: RefCell<Option<String>>,
    pub upload: RefCell<Option<super::upload::UploadUi>>,
    /// The desktop's file chooser (the portal's), for uploads and the originals' folder.
    pub picker: Rc<dyn omui::FilePicker>,
    /// The window's menus, filled again in the language of the moment.
    main_menu: gio::Menu,
}

/// The main menu's items, in the language in use.
fn fill_main_menu(menu: &gio::Menu) {
    menu.remove_all();
    menu.append(Some(&tr("Sync Now")), Some("win.refresh"));
    menu.append(Some(&tr("Upload…")), Some("win.upload"));
    menu.append(Some(&tr("Preferences")), Some("win.preferences"));
    menu.append(Some(&tr("Keyboard Shortcuts")), Some("win.shortcuts"));
    menu.append(Some(&tr("About Photos")), Some("win.about"));
}

/// Every shortcut of the window, for its overlay (`?`, F1) and as the
/// README's table lists them. Key names are not translated.
pub fn shortcut_sections() -> Vec<ShortcutSection> {
    vec![
        ShortcutSection::new(
            &tr("Anywhere"),
            &[
                ("Ctrl+R / F5", &tr("Sync with iCloud")),
                ("Ctrl+U", &tr("Upload photos and videos")),
                ("Ctrl+,", &tr("Preferences")),
                ("? / F1", &tr("Show the keyboard shortcuts")),
                ("Ctrl+W", &tr("Close the window")),
                ("Ctrl+Q", &tr("Quit")),
            ],
        ),
        ShortcutSection::new(
            &tr("Viewer"),
            &[
                ("Left", &tr("Previous photo")),
                ("Right", &tr("Next photo")),
                ("Ctrl+S", &tr("Download the original")),
                ("Ctrl+O", &tr("Open the original in your default app")),
                ("Ctrl+C", &tr("Copy the image")),
                ("Delete", &tr("Move to Recently Deleted, after a question")),
                ("Escape / Alt+Left", &tr("Back to the grid")),
            ],
        ),
    ]
}

pub fn build(application: &gtk::Application) -> Rc<App> {
    let dirs = Dirs::from_env();
    let settings = Settings::load(&dirs);
    let cat = match Catalog::open(&dirs.catalog()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "icloud-photos: cannot open {}: {e}; using a temporary catalog",
                dirs.catalog().display()
            );
            Catalog::open_in_memory().expect("in-memory catalog")
        }
    };

    // Sidebar: albums, sync status at the bottom.
    let albums = Albums::new();
    let status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    let status_text = relabel(&status);
    let spinner = omui::spinner();
    spinner.set_visible(false);
    let footer = gtk::Box::builder()
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    footer.append(&spinner);
    footer.append(&status);
    let sidebar_title = omui::Title::new(&tr("Albums"), "");
    let sidebar_header = omui::header(Some(sidebar_title.widget.upcast_ref()));
    let menu = gio::Menu::new();
    fill_main_menu(&menu);
    let menu_button = omui::icon_menu_button(name::MENU, "", &menu);
    i18n::bind_tooltip(&menu_button, "Main Menu");
    sidebar_header.pack_end(&menu_button);
    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sidebar"])
        .build();
    sidebar.append(&sidebar_header);
    sidebar.append(&gtk::ScrolledWindow::builder().child(&albums.list).vexpand(true).build());
    sidebar.append(&footer);

    // Content: the grid page, with the viewer pushed on top.
    let grid = Grid::new();
    let grid_title = omui::Title::new(&tr("All Photos"), "");
    let grid_header = omui::header(Some(grid_title.widget.upcast_ref()));
    // Shown while the window is too narrow for both panes.
    let back = omui::icon_button(name::BACK, "");
    i18n::bind_tooltip(&back, "Albums");
    back.set_visible(false);
    grid_header.pack_start(&back);
    let upload = omui::icon_button(name::ADD, "");
    i18n::bind_tooltip(&upload, "Upload Photos (Ctrl+U)");
    upload.set_action_name(Some("win.upload"));
    grid_header.pack_end(&upload);
    let refresh = omui::icon_button(name::REFRESH, "");
    i18n::bind_tooltip(&refresh, "Sync Now (Ctrl+R)");
    refresh.set_action_name(Some("win.refresh"));
    grid_header.pack_end(&refresh);
    let search = gtk::SearchEntry::builder().hexpand(true).build();
    // The placeholder is also what a screen reader calls the entry.
    i18n::bind(&search, |search| {
        let text = tr("Search this album");
        search.set_placeholder_text(Some(&text));
        search.update_property(&[gtk::accessible::Property::Label(&text)]);
    });
    let search_hint = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
    let search_notice = relabel(&search_hint);
    let search_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    search_box.set_margin_start(12);
    search_box.set_margin_end(12);
    search_box.set_margin_bottom(8);
    search_box.append(&search);
    search_box.append(&search_hint);
    search_hint.set_visible(false);
    let grid_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    grid_box.append(&grid_header);
    grid_box.append(&search_box);
    grid_box.append(&grid.root);
    let viewer = Viewer::new();
    let nav = omui::Nav::new(&grid_box);

    let split = omui::Split::new(&sidebar, &nav.widget, (200, 280), Some(640));
    split.connect_changed({
        let back = back.downgrade();
        move |split| {
            if let Some(back) = back.upgrade() {
                back.set_visible(split.is_collapsed());
            }
        }
    });
    back.connect_clicked({
        let split = split.downgrade();
        move |_| {
            if let Some(split) = split.upgrade() {
                split.set_show_content(false);
            }
        }
    });
    let banner_view = BannerView::SignIn(None);
    let (title, button) = banner_view.text();
    let banner = omui::Banner::new(&title, button.as_deref());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner.widget);
    split.widget.set_vexpand(true);
    body.append(&split.widget);

    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .icon_name("icloud-photos")
        .default_width(1100)
        .default_height(760)
        .width_request(360)
        .height_request(320)
        .build();
    // Each pane has a header of its own.
    omui::claim_titlebar(&window);
    let shell = omui::Shell::new(&window, &body);
    i18n::bind(&window, |window| window.set_title(Some(&tr("Photos"))));

    let (tx, rx) = async_channel::unbounded::<Msg>();
    let app = Rc::new(App {
        window,
        shell,
        banner,
        banner_view: RefCell::new(banner_view),
        nav,
        grid_title,
        sidebar_title,
        split,
        status: status_text,
        search,
        search_hint,
        search_notice,
        search_query: RefCell::new(String::new()),
        spinner,
        grid,
        albums,
        viewer,
        dirs,
        settings: RefCell::new(settings),
        cat,
        tx,
        transport: RefCell::new(None),
        downloader: RefCell::new(None),
        connecting: Cell::new(false),
        syncing: Cell::new(false),
        sync_again: Cell::new(false),
        last_sync: Cell::new(None),
        album: RefCell::new(None),
        textures: RefCell::new(Textures::default()),
        want_open: RefCell::new(HashSet::new()),
        want_toast: RefCell::new(HashSet::new()),
        reloading: Cell::new(false),
        reload_gen: Cell::new(0),
        signing_in: Cell::new(false),
        sign_out_reason: RefCell::new(None),
        upload: RefCell::new(None),
        picker: Rc::new(omui::PortalFilePicker::new()),
        main_menu: menu,
    });

    let a = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            a.handle(msg);
        }
    });
    let a = app.clone();
    app.search.connect_activate(move |entry| {
        *a.search_query.borrow_mut() = entry.text().trim().to_owned();
        a.grid.start_at_top();
        a.load_snapshot();
    });
    let a = app.clone();
    app.search.connect_search_changed(move |entry| {
        if entry.text().is_empty() && !a.search_query.borrow().is_empty() {
            a.search_query.borrow_mut().clear();
            a.grid.start_at_top();
            a.load_snapshot();
        }
    });
    app.connect();
    app
}

impl App {
    fn connect(self: &Rc<Self>) {
        let a = self.clone();
        *self.grid.on_bind.borrow_mut() = Some(Box::new(move |id, thumb, picture| a.bind_tile(id, thumb, picture)));
        // A tile scrolled away: its thumbnail need not download now (fast
        // scrolling would otherwise queue one per tile it passed).
        let a = self.clone();
        *self.grid.on_unbind.borrow_mut() = Some(Box::new(move |id| {
            if let Some(d) = a.downloader.borrow().as_ref() {
                d.cancel(id, Job::Thumb);
            }
        }));

        let a = self.clone();
        self.albums.list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            if a.reloading.get() {
                return;
            }
            // The snapshot sets the page title along with the grid.
            *a.album.borrow_mut() = Albums::id_of(row);
            a.grid.start_at_top();
            a.nav.pop_to_root();
            a.load_snapshot();
            a.split.set_show_content(true);
        });

        // Back at the grid, the keyboard is on the photo the viewer left. The pages return it to the tile it was on
        // themselves, but a sync that came in under the viewer built the rows anew, and that tile is gone. Not when
        // the keyboard is outside the pages: an album chosen in the sidebar closes the viewer too.
        let a = self.clone();
        self.nav.connect_changed(move |nav| {
            let outside = gtk::prelude::RootExt::focus(&a.window).is_some_and(|at| !at.is_ancestor(&nav.widget));
            if nav.visible_tag().is_none()
                && !outside
                && let Some(id) = a.viewer.current()
            {
                a.grid.focus(&id);
            }
        });

        let a = self.clone();
        self.banner.connect_button_clicked(move || a.sign_in());

        let a = self.clone();
        self.window.connect_is_active_notify(move |w| {
            if w.is_active() && stale_on_return(a.last_sync.get()) {
                a.sync();
            }
        });
        // Back on screen without the keyboard (its workspace shown on another monitor): the sync the timer left out
        // while nobody could see the window.
        let a = self.clone();
        self.window.connect_suspended_notify(move |w| {
            if !w.is_suspended() && stale_on_return(a.last_sync.get()) {
                a.sync();
            }
        });
        // iCloud does not tell a web client that the library changed, so it is asked on a timer, and only while the
        // window is on screen: hidden, minimised or on another workspace, nobody sees what a sync would bring.
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(PERIODIC_SYNC_SECS, move || match weak.upgrade() {
            Some(a) => {
                if timer_syncs(a.window.is_visible(), a.window.is_suspended()) {
                    a.sync();
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        self.add_actions();
        self.viewer.connect(self);
        super::upload::connect_drop(self);

        // After a switch of language, what is not bound to a fixed string is written again: the menus and titles
        // here, the sidebar, grid and search line from a fresh snapshot (its month headers and counts too), the
        // viewer's lines and the banner. The status line and the upload dialog's lines follow by themselves.
        let weak = Rc::downgrade(self);
        i18n::on_change(&self.window, move || {
            if let Some(a) = weak.upgrade() {
                a.relabel();
            }
        });
    }

    fn relabel(self: &Rc<Self>) {
        fill_main_menu(&self.main_menu);
        self.viewer.relabel(self);
        self.sidebar_title.set_title(&tr("Albums"));
        self.render_banner();
        self.load_snapshot();
    }

    fn add_actions(self: &Rc<Self>) {
        let win = &self.window;
        let action = |name: &str, f: Box<dyn Fn()>| win.add_action(&action(name, f));
        let a = self.clone();
        action("refresh", Box::new(move || a.sync()));
        let a = self.clone();
        action("upload", Box::new(move || super::upload::choose(&a)));
        let a = self.clone();
        action("preferences", Box::new(move || super::prefs::show(&a)));
        let a = self.clone();
        action("regrid", Box::new(move || a.grid.regrid()));
        let a = self.clone();
        action("about", Box::new(move || about(&a.window)));
        // `?` and F1: every shortcut, from the menu too. `?` is a character in the search entry, where it types.
        let shortcuts = ShortcutOverlay::new(shortcut_sections);
        shortcuts.install(&self.window, false);
        let (overlay, window) = (shortcuts.clone(), self.window.downgrade());
        action(
            "shortcuts",
            Box::new(move || {
                if let Some(window) = window.upgrade() {
                    overlay.present(&window);
                }
            }),
        );
        // Ctrl+Q closes every window and Ctrl+W this one, each as its close button would; an upload or a delete on
        // its way holds the application until it is done.
        omui::quit_keys(&self.window);
        let open = gio::SimpleAction::new("open-asset", Some(glib::VariantTy::STRING));
        let a = self.clone();
        open.connect_activate(move |_, v| {
            if let Some(id) = v.and_then(|v| v.get::<String>()) {
                a.open_viewer(&id);
            }
        });
        win.add_action(&open);
        // A file to upload by its path, as a drop on the window does: for what drives the window from outside it
        // (`dev/preview.sh`, over the session bus), where no file chooser can be typed into.
        let upload_file = gio::SimpleAction::new("upload-file", Some(glib::VariantTy::STRING));
        let a = self.clone();
        upload_file.connect_activate(move |_, v| {
            if let Some(path) = v.and_then(|v| v.get::<String>()) {
                super::upload::start(&a, vec![PathBuf::from(path)]);
            }
        });
        win.add_action(&upload_file);

        if let Some(app) = win.application() {
            app.set_accels_for_action("win.refresh", &["<Primary>r", "F5"]);
            app.set_accels_for_action("win.upload", &["<Primary>u"]);
            app.set_accels_for_action("win.preferences", &["<Primary>comma"]);
        }
    }

    /// Show what the catalog has, then connect to iCloud and sync.
    pub fn start(self: &Rc<Self>) {
        self.load_snapshot();
        self.update_status();
        self.window.present();
        self.load_transport();
        self.watch_sign_in();
    }

    pub(super) fn load_transport(&self) {
        if self.connecting.replace(true) {
            return;
        }
        let tx = self.tx.clone();
        self.set_busy(true, || tr("Connecting to iCloud…"));
        std::thread::spawn(move || {
            let _ = tx.send_blocking(Msg::TransportReady(icloud_photos::session::connect()));
        });
    }

    fn handle(self: &Rc<Self>, msg: Msg) {
        if matches!(msg, Msg::TransportReady(_)) {
            self.connecting.set(false);
        }
        match msg {
            Msg::TransportReady(Ok(t)) => {
                let tx = self.tx.clone();
                let settings = self.settings.borrow().clone();
                let targets = thumbs::Targets {
                    dirs: self.dirs.clone(),
                    library: settings.library_dir,
                    export: false,
                };
                let pool = Downloader::start(
                    t.clone(),
                    targets,
                    thumbs::DOWNLOAD_WORKERS,
                    Box::new(move |e| {
                        let _ = tx.send_blocking(Msg::Download(e));
                    }),
                );
                *self.transport.borrow_mut() = Some(t);
                *self.downloader.borrow_mut() = Some(pool);
                self.set_idle();
                self.sync();
            }
            Msg::TransportReady(Err(e)) => {
                self.set_idle();
                self.fail(&tr("Could not connect to iCloud"), &e);
            }
            Msg::SyncProgress(p) => self.status.set(move || match p {
                Progress::Albums => tr("Syncing albums…"),
                Progress::Assets(n) => trn("Syncing… {n} photos", n as i64),
                Progress::AlbumMembers { done, total } => omui::tr!(
                    "Syncing album {} of {}…",
                    i18n::format_int(done as i64 + 1),
                    i18n::format_int(total as i64)
                ),
                Progress::Changes(n) => trn("Syncing {n} changes…", n as i64),
            }),
            Msg::SyncDone(result) => self.on_sync_done(result),
            Msg::Download(e) => self.on_download(e),
            Msg::Upload(m) => super::upload::on_msg(self, m),
            Msg::SignIn(s) => self.on_sign_in_state(s),
            Msg::SignInFailed(e) => self.on_sign_in_failed(&e),
        }
    }

    pub fn toast(&self, text: &str) {
        self.shell.toast(text);
    }

    /// Report an error: the banner for a lapsed sign-in, a toast otherwise. `what` is in the language in use; the
    /// error's own words are the library's, in English.
    pub fn fail(&self, what: &str, e: &Error) {
        if e.is_sign_in() {
            self.show_sign_in_banner();
        } else {
            eprintln!("icloud-photos: {what}: {e}");
            self.toast(&omui::tr!("{}: {}", what, e));
        }
    }

    /// The spinner and `text` on the status line, which builds it again after a switch of language.
    fn set_busy(&self, busy: bool, text: impl Fn() -> String + 'static) {
        self.spinner.set_visible(busy);
        self.status.set(text);
    }

    /// No spinner, and the library's count on the status line.
    fn set_idle(&self) {
        self.spinner.set_visible(false);
        self.update_status();
    }

    fn update_status(&self) {
        let count = self.cat.count().unwrap_or(0);
        let when = self
            .cat
            .meta(icloud_photos::catalog::LAST_SYNC_KEY)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|t| glib::DateTime::from_unix_local(t).ok());
        self.status.set(move || match &when {
            Some(when) => omui::trnf("{n} items · synced {}", count, &[&i18n::format_time_short(when)]),
            None => trn("{n} items", count),
        });
    }

    pub fn transport(&self) -> Option<Arc<dyn Transport>> {
        self.transport.borrow().clone()
    }

    pub fn sync(self: &Rc<Self>) {
        // Not connected (the first try failed: keyring locked, daemon not
        // up yet): connect again; a sync follows once that works.
        let Some(t) = self.transport() else {
            self.load_transport();
            return;
        };
        if self.syncing.replace(true) {
            self.sync_again.set(true);
            return;
        }
        self.set_busy(true, || tr("Syncing…"));
        let (tx, dirs) = (self.tx.clone(), self.dirs.clone());
        std::thread::spawn(move || {
            let ptx = tx.clone();
            let result = sync::run(&*t, &dirs, false, &move |p| {
                let _ = ptx.send_blocking(Msg::SyncProgress(p));
            });
            let _ = tx.send_blocking(Msg::SyncDone(result));
        });
    }

    fn on_sync_done(self: &Rc<Self>, result: Result<Report>) {
        self.syncing.set(false);
        self.last_sync.set(Some(Instant::now()));
        self.set_idle();
        match result {
            Ok(report) => {
                self.banner.set_revealed(false);
                if let Some(why) = &report.fell_back {
                    eprintln!("icloud-photos: incremental sync failed ({why}); did a full sync");
                }
                // Relationship and master-only changes do not increment the
                // report's asset/album counts, but still change the view.
                self.load_snapshot();
                self.update_status();
                if self.settings.borrow().download == DownloadMode::All {
                    self.queue_all_originals();
                }
            }
            Err(e) => self.fail(&tr("Sync failed"), &e),
        }
        if self.sync_again.replace(false) {
            self.sync();
        }
    }

    pub fn queue_all_originals(&self) {
        if let (Some(d), Ok(ids)) = (self.downloader.borrow().as_ref(), self.cat.missing_originals()) {
            for id in ids {
                d.enqueue(&id, Job::Original, Priority::Background);
            }
        }
    }

    /// Re-read albums and the current grid from the catalog: query it (and
    /// chunk the grid rows) on a worker thread with its own connection, then
    /// apply the result on the main loop. Falls back to the main-loop
    /// connection when the catalog is not on disk.
    fn load_snapshot(self: &Rc<Self>) {
        let generation = self.reload_gen.get() + 1;
        self.reload_gen.set(generation);
        let (path, album, columns) = (self.dirs.catalog(), self.album.borrow().clone(), self.grid.columns());
        let query = self.search_query.borrow().clone();
        self.search_hint.set_visible(!query.is_empty());
        self.search_notice.set(|| Notice::Searching.text());
        let a = self.clone();
        glib::spawn_future_local(async move {
            let snap = gio::spawn_blocking(move || {
                Catalog::open(&path).ok().map(|cat| {
                    let mut snap = snapshot(&cat, album, columns);
                    let notice = filter_snapshot(&mut snap, &query);
                    (snap, notice)
                })
            })
            .await
            .ok()
            .flatten();
            if a.reload_gen.get() != generation {
                return;
            }
            let (snap, notice) = snap.unwrap_or_else(|| {
                let mut snap = snapshot(&a.cat, a.album.borrow().clone(), columns);
                // Do not run inference on the main loop for an in-memory catalog.
                let notice = filter_snapshot_with(&mut snap, &a.search_query.borrow(), |_, _| None);
                (snap, notice)
            });
            a.search_notice.set(move || notice.text());
            a.apply_snapshot(snap);
        });
    }

    fn apply_snapshot(&self, snap: Snapshot) {
        *self.album.borrow_mut() = snap.album.clone();
        self.grid_title.set_title(&snap.title());
        self.reloading.set(true);
        self.albums.set(snap.total, &snap.albums, snap.album.as_deref());
        self.reloading.set(false);
        let first = snap.first_sync;
        self.grid.empty.set_title(&if first {
            tr("Getting your photos")
        } else {
            tr("No photos here")
        });
        self.grid.empty.set_description(&if first {
            tr("The first sync lists your whole library; thumbnails follow as you scroll.")
        } else {
            tr("Photos you add in iCloud, or upload from here, appear in this view.")
        });
        self.grid.set_rows(snap.tiles, snap.rows, snap.columns);
    }

    fn bind_tile(self: &Rc<Self>, id: &str, thumb: Option<&PathBuf>, picture: &gtk::Picture) {
        if let Some(t) = self.textures.borrow().get(id) {
            picture.set_paintable(Some(&t));
            return;
        }
        // No stat here, on the main loop: decoding the cached file (off it)
        // finds out whether it is still there.
        match thumb {
            Some(p) => self.load_texture_or_download(id, p),
            None => self.download_thumb(id),
        }
    }

    fn download_thumb(&self, id: &str) {
        if let Some(d) = self.downloader.borrow().as_ref() {
            d.enqueue(id, Job::Thumb, Priority::Now);
        }
    }

    /// Decode a thumbnail off the main loop, then show it wherever it is bound.
    pub fn load_texture(self: &Rc<Self>, id: &str, path: &Path) {
        self.decode_texture(id, path, false);
    }

    /// [`load_texture`](Self::load_texture) for a cached path the catalog
    /// recorded: when it cannot be read (the cache was cleared), download it.
    fn load_texture_or_download(self: &Rc<Self>, id: &str, path: &Path) {
        self.decode_texture(id, path, true);
    }

    fn decode_texture(self: &Rc<Self>, id: &str, path: &Path, download_if_missing: bool) {
        if !self.textures.borrow_mut().loading.insert(id.to_owned()) {
            return;
        }
        let (a, id, path) = (self.clone(), id.to_owned(), path.to_owned());
        glib::spawn_future_local(async move {
            let tex = gio::spawn_blocking(move || decode::texture(&path).ok())
                .await
                .ok()
                .flatten();
            let mut textures = a.textures.borrow_mut();
            textures.loading.remove(&id);
            match tex {
                Some(t) => {
                    textures.insert(id.clone(), t.clone());
                    drop(textures);
                    a.grid.show_texture(&id, &t);
                    a.viewer.on_thumb(&a, &id);
                }
                None if download_if_missing => {
                    drop(textures);
                    a.download_thumb(&id);
                }
                None => {}
            }
        });
    }

    fn on_download(self: &Rc<Self>, e: thumbs::Event) {
        match (e.job, e.result.map(|f| (f.path, f.live_error))) {
            (Job::Thumb, Ok((path, _))) => self.load_texture(&e.id, &path),
            (Job::Medium, Ok((path, _))) => self.viewer.on_medium(self, &e.id, &path),
            (Job::Original, Ok((path, live_error))) => {
                if let Some(err) = &live_error {
                    // The photo is saved; the next download fetches only the video.
                    if err.is_sign_in()
                        || self.want_open.borrow().contains(&e.id)
                        || self.want_toast.borrow().contains(&e.id)
                    {
                        self.fail(&tr("The Live Photo's video did not download"), err);
                    } else {
                        eprintln!("icloud-photos: Live Photo video {}: {err}", e.id);
                    }
                }
                if self.want_open.borrow_mut().remove(&e.id) {
                    self.launch(&path);
                }
                if self.want_toast.borrow_mut().remove(&e.id) {
                    let saved = omui::tr!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy());
                    let (win, file) = (self.window.clone(), gio::File::for_path(&path));
                    self.shell.toast_action(&saved, &tr("Show"), 5, move || {
                        gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                            Some(&win),
                            gio::Cancellable::NONE,
                            |_| {},
                        );
                    });
                }
                self.viewer.refresh_info(self);
            }
            (job, Err(err)) => {
                let wanted = job != Job::Thumb
                    && (self.want_open.borrow_mut().remove(&e.id) | self.want_toast.borrow_mut().remove(&e.id));
                if err.is_sign_in() || wanted || job == Job::Medium {
                    self.fail(&tr("Download failed"), &err);
                } else {
                    eprintln!("icloud-photos: {job:?} {}: {err}", e.id);
                }
                if job == Job::Medium {
                    self.viewer.on_medium_failed(&e.id);
                }
            }
        }
    }

    fn launch(&self, path: &Path) {
        let win = self.window.clone();
        gtk::FileLauncher::new(Some(&gio::File::for_path(path))).launch(Some(&win), gio::Cancellable::NONE, |r| {
            if let Err(e) = r {
                eprintln!("icloud-photos: open: {e}");
            }
        });
    }

    /// Download the original (then optionally open it).
    pub fn fetch_original(&self, id: &str, open: bool) {
        if let Ok(Some(row)) = self.cat.asset(id)
            && let Some(p) = row.local_path.filter(|p| p.exists())
        {
            if open {
                self.launch(&p);
            } else {
                self.toast(&omui::tr!("Already saved as {}", p.display()));
            }
            return;
        }
        let Some(d) = self.downloader.borrow().clone() else {
            self.toast(&tr("Not connected to iCloud yet"));
            return;
        };
        if open {
            self.want_open.borrow_mut().insert(id.to_owned());
        } else {
            self.want_toast.borrow_mut().insert(id.to_owned());
        }
        self.toast(&tr("Downloading the original…"));
        d.enqueue(id, Job::Original, Priority::Now);
    }

    pub fn delete(self: &Rc<Self>, id: &str) {
        let Some(row) = self.cat.asset(id).ok().flatten() else {
            return;
        };
        let Some(t) = self.transport() else {
            self.toast(&tr("Not connected to iCloud yet"));
            return;
        };
        let body = omui::tr!(
            "{} moves to Recently Deleted in iCloud on all your devices, and can be recovered there for about 30 days.",
            row.filename
        );
        let a = self.clone();
        // Enter and Escape cancel: only the button deletes.
        omui::confirm(
            &self.window,
            "Delete this photo?",
            &body,
            "Delete",
            omui::Appearance::Destructive,
            move || {
                // A write: it holds the application (`omui::background`) until iCloud has answered and the catalog
                // says so, even once the last window has closed.
                let id = row.id.clone();
                omui::background(
                    move || CloudKit::connect(&*t).and_then(|ck| ck.delete_asset(&row.id, row.change_tag.as_deref())),
                    move |result| a.on_deleted(&id, flatten(result)),
                );
            },
        );
    }

    fn on_deleted(self: &Rc<Self>, id: &str, result: Result<Modified>) {
        match result {
            Ok(m) => {
                let _ = self.cat.mark_deleted(id, m.change_tag.as_deref());
                self.viewer.after_delete(self, id);
                self.load_snapshot();
                self.update_status();
                let (a, id, tag) = (self.clone(), id.to_owned(), m.change_tag);
                self.shell
                    .toast_action(&tr("Moved to Recently Deleted"), &tr("Undo"), 10, move || {
                        a.restore(&id, tag.as_deref())
                    });
            }
            Err(Error::CloudKit { code, .. }) if code == "CONFLICT" => {
                self.toast(&tr("This photo changed on another device; syncing, then try again"));
                self.sync();
            }
            Err(e) => self.fail(&tr("Delete failed"), &e),
        }
    }

    /// Undo a delete: bring the photo back from Recently Deleted.
    fn restore(self: &Rc<Self>, id: &str, change_tag: Option<&str>) {
        let Some(t) = self.transport() else {
            self.toast(&tr("Not connected to iCloud yet"));
            return;
        };
        let (a, id, tag) = (self.clone(), id.to_owned(), change_tag.map(str::to_owned));
        let asset = id.clone();
        // A write, held as a delete is.
        omui::background(
            move || CloudKit::connect(&*t).and_then(|ck| ck.restore_asset(&asset, tag.as_deref())),
            move |result| a.on_restored(&id, flatten(result)),
        );
    }

    fn on_restored(self: &Rc<Self>, id: &str, result: Result<Modified>) {
        match result {
            Ok(m) => {
                let _ = self.cat.mark_restored(id, m.change_tag.as_deref());
                self.load_snapshot();
                self.update_status();
                self.toast(&tr("Restored"));
            }
            Err(Error::CloudKit { code, .. }) if code == "CONFLICT" => {
                self.toast(&tr(
                    "This photo changed on another device; it is still in Recently Deleted on your iPhone or Mac",
                ));
                self.sync();
            }
            Err(e) => self.fail(&tr("Undo failed"), &e),
        }
    }

    fn open_viewer(self: &Rc<Self>, id: &str) {
        let ids = self.grid.ids();
        if let Some(i) = ids.iter().position(|x| x == id) {
            self.viewer.show(self, ids, i);
            if self.nav.visible_tag().as_deref() != Some("viewer") {
                self.nav.push("viewer", &self.viewer.page);
            }
        }
    }
}

/// A window action without a parameter. omui holds the window's shortcuts
/// and the application's accelerators back while a dialog is open, so none
/// of them needs a check of its own.
pub fn action(name: &str, f: Box<dyn Fn()>) -> gio::SimpleAction {
    let act = gio::SimpleAction::new(name, None);
    act.connect_activate(move |_, _| f());
    act
}

/// A write's result from [`omui::background`]: a worker that panicked is an error like any other.
fn flatten<T>(result: std::result::Result<Result<T>, String>) -> Result<T> {
    result.unwrap_or_else(|crashed| Err(Error::Io(std::io::Error::other(crashed))))
}

/// The app's name, version, author, licence and website.
fn about(window: &gtk::ApplicationWindow) {
    omui::about(
        window,
        &tr("Photos"),
        env!("CARGO_PKG_VERSION"),
        &tr(
            "Browse, download, upload and delete your iCloud photos, over the shared icloud-session sign-in.\n\nBy Ferdous Bhai. MIT License.",
        ),
        Some("https://github.com/ferdousbhai/icloud-for-omarchy"),
    );
}

/// Keep the sidebar and grid in the same snapshot, including when an album
/// switch supersedes a sync reload. Fall back to All Photos for a deleted album.
fn snapshot(cat: &Catalog, mut album: Option<String>, columns: usize) -> Snapshot {
    let albums = cat.albums().unwrap_or_default();
    if album.as_ref().is_some_and(|c| !albums.iter().any(|a| &a.id == c)) {
        album = None;
    }
    let assets = cat.grid_assets(album.as_deref()).unwrap_or_default();
    let first_sync = assets.is_empty()
        && cat
            .meta(icloud_photos::catalog::SYNC_TOKEN_KEY)
            .ok()
            .flatten()
            .is_none();
    let tiles = super::grid::tiles_of(assets);
    let rows = super::grid::rows_of(&tiles, columns);
    Snapshot {
        albums,
        total: cat.count().unwrap_or(0),
        album,
        tiles,
        rows,
        columns,
        first_sync,
    }
}

/// Search never fetches originals or expands the selected album. Filename
/// matches come first, then local Recall's matches by meaning among the
/// cached thumbnails; "~ query" asks for the matches by meaning alone.
/// Without Recall (or cached thumbnails) it is a filename search.
fn filter_snapshot(snap: &mut Snapshot, query: &str) -> Notice {
    filter_snapshot_with(snap, query, |q, records| {
        crate::recall_client::rank("icloud-photos", q, records, 100)
    })
}

fn filter_snapshot_with(
    snap: &mut Snapshot,
    query: &str,
    rank: impl FnOnce(&str, &[serde_json::Value]) -> Option<Vec<String>>,
) -> Notice {
    let meaning_only = query.starts_with('~');
    let q = if meaning_only { query[1..].trim() } else { query.trim() };
    if q.is_empty() {
        return Notice::None;
    }
    let records: Vec<_> = snap
        .tiles
        .iter()
        .filter_map(|tile| {
            let path = tile.thumb.as_ref().filter(|p| p.is_absolute() && p.is_file())?;
            Some(serde_json::json!({"id":tile.id,"title":tile.filename,"text":"","kind":"image","path":path}))
        })
        .collect();
    let ranked = if records.is_empty() { None } else { rank(q, &records) };
    let filename_query = q.to_lowercase();
    let all = std::mem::take(&mut snap.tiles);
    let (named, rest): (Vec<_>, Vec<_>) = if meaning_only && ranked.is_some() {
        (Vec::new(), all)
    } else {
        all.into_iter()
            .partition(|tile| tile.filename.to_lowercase().contains(&filename_query))
    };
    let names = named.len();
    snap.tiles = named;
    let mut rest: HashMap<_, _> = rest.into_iter().map(|tile| (tile.id.clone(), tile)).collect();
    let before = snap.tiles.len();
    snap.tiles
        .extend(ranked.iter().flatten().filter_map(|id| rest.remove(id)));
    let meaning = snap.tiles.len() - before;
    snap.rows = super::grid::rows_of(&snap.tiles, snap.columns);
    match (ranked.is_some(), names) {
        (true, 0) => Notice::MeaningOnly,
        (true, names) => Notice::Filenames { names, meaning },
        (false, _) if meaning_only => Notice::RecallUnavailable,
        (false, _) => Notice::FilenamesOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use icloud_photos::cloudkit::Album;

    /// The overlay and the README's table list the same keys.
    #[test]
    fn the_shortcuts_overlay_and_the_readme_agree() {
        let readme = include_str!("../../README.md");
        let table = readme
            .split("### Keyboard shortcuts")
            .nth(1)
            .and_then(|rest| rest.split("\n\n").find(|part| part.trim_start().starts_with('|')))
            .expect("the README has a table of shortcuts");
        let mut documented: Vec<String> = table
            .lines()
            .filter_map(|line| line.split('|').nth(1))
            .flat_map(|keys| {
                keys.split('`')
                    .skip(1)
                    .step_by(2)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut overlay: Vec<String> = shortcut_sections()
            .into_iter()
            .flat_map(|section| section.keys.into_iter().map(|(key, _)| key))
            // Keys that do the same share a row, as they do in the README.
            .flat_map(|keys| keys.split(" / ").map(str::to_owned).collect::<Vec<_>>())
            .collect();
        documented.sort();
        overlay.sort();
        assert_eq!(documented, overlay);
    }

    #[test]
    fn the_periodic_sync_waits_for_a_window_on_screen() {
        assert!(timer_syncs(true, false));
        assert!(!timer_syncs(true, true), "minimised or on another workspace");
        assert!(!timer_syncs(false, false), "hidden");
        // Coming back syncs once, when the timer's turn was missed, and not before a first sync has ended.
        assert!(!stale_on_return(None));
        assert!(!stale_on_return(Some(Instant::now())));
        if let Some(long_ago) = Instant::now().checked_sub(FOCUS_SYNC_AFTER + Duration::from_secs(1)) {
            assert!(stale_on_return(Some(long_ago)));
        }
    }

    #[test]
    fn semantic_without_cached_thumbnails_retains_filename_search() {
        let cat = Catalog::open_in_memory().unwrap();
        let mut snap = snapshot(&cat, None, 4);
        let tile = |id: &str, filename: &str| super::super::grid::Tile {
            id: id.into(),
            filename: filename.into(),
            thumb: None,
            kind: icloud_photos::cloudkit::Kind::Photo,
            is_live: false,
            month: 1,
        };
        snap.tiles = vec![tile("a", "Holiday.jpg").into(), tile("b", "Work.jpg").into()];
        assert!(
            filter_snapshot(&mut snap, "~ HOLIDAY")
                .text()
                .contains("filename matches")
        );
        assert_eq!(snap.tiles.len(), 1);
        assert_eq!(snap.tiles[0].id, "a");
    }

    #[test]
    fn plain_search_lists_filename_matches_then_matches_by_meaning() {
        // The plural forms are the catalog's.
        omui::i18n::register("en", crate::ENGLISH);
        let cached = tempfile::NamedTempFile::new().unwrap();
        let cat = Catalog::open_in_memory().unwrap();
        let tile = |id: &str, filename: &str| {
            super::super::grid::Tile {
                id: id.into(),
                filename: filename.into(),
                thumb: Some(cached.path().to_path_buf()),
                kind: icloud_photos::cloudkit::Kind::Photo,
                is_live: false,
                month: 1,
            }
            .into()
        };
        let fresh = || {
            let mut snap = snapshot(&cat, None, 4);
            snap.tiles = vec![tile("a", "IMG_1.jpg"), tile("b", "beach.jpg"), tile("c", "IMG_2.jpg")];
            snap
        };
        let ids = |snap: &Snapshot| snap.tiles.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
        let ranked = |_: &str, records: &[serde_json::Value]| {
            assert_eq!(records.len(), 3);
            Some(vec!["c".to_string(), "b".to_string()])
        };

        let mut snap = fresh();
        assert_eq!(
            filter_snapshot_with(&mut snap, "beach", ranked).text(),
            "1 filename match · 1 by meaning"
        );
        assert_eq!(ids(&snap), ["b", "c"]);

        let mut snap = fresh();
        assert_eq!(
            filter_snapshot_with(&mut snap, "sunset", ranked).text(),
            "Matches by meaning · cached thumbnails only"
        );
        assert_eq!(ids(&snap), ["c", "b"]);

        let mut snap = fresh();
        assert_eq!(
            filter_snapshot_with(&mut snap, "~ beach", ranked).text(),
            "Matches by meaning · cached thumbnails only"
        );
        assert_eq!(ids(&snap), ["c", "b"]);

        let mut snap = fresh();
        assert_eq!(
            filter_snapshot_with(&mut snap, "img", |_, _| None).text(),
            "Filename matches"
        );
        assert_eq!(ids(&snap), ["a", "c"]);
    }

    #[test]
    fn album_switch_snapshot_carries_sidebar_changes_and_deleted_album_fallback() {
        let cat = Catalog::open_in_memory().unwrap();
        let mut album = Album {
            id: "album-1".into(),
            name: "Before".into(),
            position: 0,
            is_folder: false,
            deleted: false,
        };
        cat.upsert_album(&album).unwrap();
        let before = snapshot(&cat, Some(album.id.clone()), 4);
        assert_eq!(before.album.as_deref(), Some("album-1"));
        assert_eq!(before.albums[0].name, "Before");

        // A sync updates the sidebar while the user switches albums. The
        // newer snapshot must carry those changes even if it supersedes
        // the sync's pending snapshot.
        album.name = "After".into();
        cat.upsert_album(&album).unwrap();
        let switched = snapshot(&cat, Some(album.id.clone()), 4);
        assert_eq!(switched.albums[0].name, "After");
        assert_eq!(switched.title(), "After");

        album.deleted = true;
        cat.upsert_album(&album).unwrap();
        let deleted = snapshot(&cat, Some(album.id), 4);
        assert!(deleted.album.is_none());
        assert!(deleted.albums.is_empty());
        assert_eq!(deleted.title(), "All Photos");
    }
}
