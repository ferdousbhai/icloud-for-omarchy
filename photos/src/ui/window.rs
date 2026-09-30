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

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use icloud_photos::catalog::Catalog;
use icloud_photos::cloudkit::{CloudKit, Modified};
use icloud_photos::config::{Dirs, DownloadMode, Settings};
use icloud_photos::sync::{self, Mode, Progress, Report};
use icloud_photos::thumbs::{self, Downloader, Job, Priority};
use icloud_photos::transport::{Error, Result, SignInState, Transport};

use super::albums::Albums;
use super::grid::Grid;
use super::upload::UploadMsg;
use super::viewer::Viewer;

pub enum Msg {
    TransportReady(Result<Arc<dyn Transport>>),
    SyncProgress(Progress),
    SyncDone(Result<Report>),
    Download(thumbs::Event),
    Deleted(String, Result<Modified>),
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
    assets: Vec<icloud_photos::catalog::Row>,
    rows: Vec<super::grid::RowItem>,
    columns: usize,
    first_sync: bool,
}

/// How often a focused window re-syncs, and the background cadence.
const FOCUS_SYNC_AFTER: Duration = Duration::from_secs(120);
const PERIODIC_SYNC_SECS: u32 = 600;

pub struct App {
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub banner: adw::Banner,
    pub nav: adw::NavigationView,
    grid_page: adw::NavigationPage,
    pub split: adw::NavigationSplitView,
    pub status: gtk::Label,
    pub spinner: adw::Spinner,
    pub grid: Grid,
    pub albums: Albums,
    pub viewer: Viewer,
    pub dirs: Dirs,
    pub settings: RefCell<Settings>,
    pub cat: Catalog,
    pub tx: async_channel::Sender<Msg>,
    pub transport: RefCell<Option<Arc<dyn Transport>>>,
    pub downloader: RefCell<Option<Downloader>>,
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
    pub upload: RefCell<Option<super::upload::UploadUi>>,
}

pub fn build(application: &adw::Application) -> Rc<App> {
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

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("iCloud Photos")
        .default_width(1100)
        .default_height(760)
        .build();
    window.set_icon_name(Some("com.ferdousbhai.IcloudPhotos"));

    // Sidebar: albums, sync status at the bottom.
    let albums = Albums::new();
    let status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    let spinner = adw::Spinner::new();
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
    let sidebar_header = adw::HeaderBar::new();
    let menu = gio::Menu::new();
    menu.append(Some("Sync Now"), Some("win.refresh"));
    menu.append(Some("Upload…"), Some("win.upload"));
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("About iCloud Photos"), Some("win.about"));
    sidebar_header.pack_end(
        &gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main Menu")
            .build(),
    );
    let sidebar_tv = adw::ToolbarView::new();
    sidebar_tv.add_top_bar(&sidebar_header);
    sidebar_tv.set_content(Some(
        &gtk::ScrolledWindow::builder().child(&albums.list).vexpand(true).build(),
    ));
    sidebar_tv.add_bottom_bar(&footer);
    let sidebar_page = adw::NavigationPage::builder()
        .title("Albums")
        .child(&sidebar_tv)
        .build();

    // Content: the grid page, with the viewer pushed on top.
    let grid = Grid::new();
    let grid_header = adw::HeaderBar::new();
    grid_header.pack_end(
        &gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .action_name("win.upload")
            .tooltip_text("Upload Photos (Ctrl+U)")
            .build(),
    );
    grid_header.pack_end(
        &gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .action_name("win.refresh")
            .tooltip_text("Sync Now (Ctrl+R)")
            .build(),
    );
    let grid_tv = adw::ToolbarView::new();
    grid_tv.add_top_bar(&grid_header);
    grid_tv.set_content(Some(&grid.root));
    let grid_page = adw::NavigationPage::builder()
        .title("All Photos")
        .tag("grid")
        .child(&grid_tv)
        .build();
    let viewer = Viewer::new();
    let nav = adw::NavigationView::new();
    nav.add(&grid_page);
    let content_page = adw::NavigationPage::builder().title("Photos").child(&nav).build();

    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&content_page)
        .min_sidebar_width(200.0)
        .max_sidebar_width(280.0)
        .build();
    let banner = adw::Banner::builder()
        .title("iCloud needs you to sign in again")
        .button_label("Sign In")
        .revealed(false)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner);
    split.set_vexpand(true);
    body.append(&split);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));
    window.set_content(Some(&toasts));

    let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 640sp").expect("breakpoint"));
    bp.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(bp);

    let (tx, rx) = async_channel::unbounded::<Msg>();
    let app = Rc::new(App {
        window,
        toasts,
        banner,
        nav,
        grid_page,
        split,
        status,
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
        upload: RefCell::new(None),
    });

    let a = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            a.handle(msg);
        }
    });
    app.connect();
    app
}

impl App {
    fn connect(self: &Rc<Self>) {
        let a = self.clone();
        *self.grid.on_bind.borrow_mut() = Some(Box::new(move |id, thumb, picture| a.bind_tile(id, thumb, picture)));

        let a = self.clone();
        self.albums.list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            if a.reloading.get() {
                return;
            }
            *a.album.borrow_mut() = Albums::id_of(row);
            a.grid_page.set_title(&Albums::title_of(row));
            a.nav.pop_to_tag("grid");
            a.show_assets();
            a.split.set_show_content(true);
        });

        let a = self.clone();
        self.banner.connect_button_clicked(move |_| a.sign_in());

        let a = self.clone();
        self.window.connect_is_active_notify(move |w| {
            if w.is_active() && a.last_sync.get().is_some_and(|t| t.elapsed() > FOCUS_SYNC_AFTER) {
                a.sync();
            }
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(PERIODIC_SYNC_SECS, move || match weak.upgrade() {
            Some(a) => {
                a.sync();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        self.add_actions();
        self.viewer.connect(self);
        super::upload::connect_drop(self);
    }

    fn add_actions(self: &Rc<Self>) {
        let win = &self.window;
        let action = |name: &str, f: Box<dyn Fn()>| {
            let act = gio::SimpleAction::new(name, None);
            act.connect_activate(move |_, _| f());
            win.add_action(&act);
        };
        let a = self.clone();
        action("refresh", Box::new(move || a.sync()));
        let a = self.clone();
        action("upload", Box::new(move || super::upload::choose(&a)));
        let a = self.clone();
        action("preferences", Box::new(move || super::prefs::show(&a)));
        let a = self.clone();
        action("regrid", Box::new(move || a.grid.regrid()));
        let a = self.clone();
        action(
            "about",
            Box::new(move || {
                adw::AboutDialog::builder()
                    .application_name("iCloud Photos")
                    .application_icon("com.ferdousbhai.IcloudPhotos")
                    .developer_name("Ferdous Bhai")
                    .version(env!("CARGO_PKG_VERSION"))
                    .website("https://github.com/ferdousbhai/icloud-for-omarchy")
                    .license_type(gtk::License::MitX11)
                    .comments("Browse, download, upload and delete your iCloud photos, over the shared icloud-session sign-in.")
                    .build()
                    .present(Some(&a.window));
            }),
        );
        let open = gio::SimpleAction::new("open-asset", Some(glib::VariantTy::STRING));
        let a = self.clone();
        open.connect_activate(move |_, v| {
            if let Some(id) = v.and_then(|v| v.get::<String>()) {
                a.open_viewer(&id);
            }
        });
        win.add_action(&open);

        if let Some(app) = win.application() {
            app.set_accels_for_action("win.refresh", &["<Primary>r", "F5"]);
            app.set_accels_for_action("win.upload", &["<Primary>u"]);
            app.set_accels_for_action("win.preferences", &["<Primary>comma"]);
            app.set_accels_for_action("window.close", &["<Primary>w"]);
            app.set_accels_for_action("app.quit", &["<Primary>q"]);
        }
    }

    /// Show what the catalog has, then connect to iCloud and sync.
    pub fn start(self: &Rc<Self>) {
        self.reload();
        self.update_status();
        self.window.present();
        self.load_transport();
        self.watch_sign_in();
        #[cfg(debug_assertions)]
        self.dev_screenshot();
    }

    /// Debug builds only: `ICLOUD_PHOTOS_SCREENSHOT=out.png` renders the
    /// window to a PNG after the first sync settles and quits;
    /// `ICLOUD_PHOTOS_SCREENSHOT_VIEW=viewer` opens the newest photo first.
    /// Lets the UI be checked headless (GDK_BACKEND=broadway) against the
    /// fake server.
    #[cfg(debug_assertions)]
    fn dev_screenshot(self: &Rc<Self>) {
        let Some(path) = std::env::var_os("ICLOUD_PHOTOS_SCREENSHOT") else {
            return;
        };
        let view = std::env::var("ICLOUD_PHOTOS_SCREENSHOT_VIEW").unwrap_or_default();
        let a = self.clone();
        glib::timeout_add_seconds_local_once(6, move || {
            if view == "viewer"
                && let Some(id) = a.grid.ids().first()
            {
                a.open_viewer(id);
            }
            if view == "prefs" {
                super::prefs::show(&a);
            }
            glib::timeout_add_seconds_local_once(4, move || {
                let w = &a.window;
                let paintable = gtk::WidgetPaintable::new(Some(w));
                let snap = gtk::Snapshot::new();
                paintable.snapshot(&snap, f64::from(w.width()), f64::from(w.height()));
                if let (Some(node), Some(renderer)) = (snap.to_node(), w.native().and_then(|n| n.renderer())) {
                    let tex = renderer.render_texture(node, None);
                    if let Err(e) = tex.save_to_png(&path) {
                        eprintln!("icloud-photos: screenshot: {e}");
                    }
                }
                if let Some(app) = w.application() {
                    app.quit();
                }
            });
        });
    }

    pub(super) fn load_transport(&self) {
        let tx = self.tx.clone();
        self.set_busy(true, "Connecting to iCloud…");
        std::thread::spawn(move || {
            let _ = tx.send_blocking(Msg::TransportReady(icloud_photos::session::connect()));
        });
    }

    fn handle(self: &Rc<Self>, msg: Msg) {
        match msg {
            Msg::TransportReady(Ok(t)) => {
                let tx = self.tx.clone();
                let settings = self.settings.borrow().clone();
                let pool = Downloader::start(
                    t.clone(),
                    self.dirs.clone(),
                    settings.library_dir.clone(),
                    4,
                    Box::new(move |e| {
                        let _ = tx.send_blocking(Msg::Download(e));
                    }),
                );
                *self.transport.borrow_mut() = Some(t);
                *self.downloader.borrow_mut() = Some(pool);
                self.set_busy(false, "");
                self.sync();
            }
            Msg::TransportReady(Err(e)) => {
                self.set_busy(false, "");
                self.fail("Could not connect to iCloud", &e);
            }
            Msg::SyncProgress(p) => {
                let text = match p {
                    Progress::Albums => "Syncing albums…".to_owned(),
                    Progress::Assets(n) => format!("Syncing… {n} photos"),
                    Progress::AlbumMembers { done, total } => format!("Syncing album {} of {total}…", done + 1),
                    Progress::Changes(n) => format!("Syncing {n} changes…"),
                };
                self.status.set_label(&text);
            }
            Msg::SyncDone(result) => self.on_sync_done(result),
            Msg::Download(e) => self.on_download(e),
            Msg::Deleted(id, result) => self.on_deleted(&id, result),
            Msg::Upload(m) => super::upload::on_msg(self, m),
            Msg::SignIn(s) => self.on_sign_in_state(s),
            Msg::SignInFailed(e) => self.on_sign_in_failed(&e),
        }
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(
            adw::Toast::builder()
                .title(glib::markup_escape_text(text))
                .timeout(4)
                .build(),
        );
    }

    /// Report an error: the banner for a lapsed sign-in, a toast otherwise.
    pub fn fail(&self, what: &str, e: &Error) {
        if e.is_sign_in() {
            self.show_sign_in_banner();
        } else {
            eprintln!("icloud-photos: {what}: {e}");
            self.toast(&format!("{what}: {e}"));
        }
    }

    fn set_busy(&self, busy: bool, text: &str) {
        self.spinner.set_visible(busy);
        if busy {
            self.status.set_label(text);
        } else {
            self.update_status();
        }
    }

    fn update_status(&self) {
        let count = self.cat.count().unwrap_or(0);
        let when = self
            .cat
            .meta(icloud_photos::catalog::LAST_SYNC_KEY)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|t| glib::DateTime::from_unix_local(t).ok())
            .and_then(|d| d.format("%H:%M").ok());
        let photos = if count == 1 {
            "1 item".to_owned()
        } else {
            format!("{count} items")
        };
        self.status.set_label(&match when {
            Some(w) => format!("{photos} · synced {w}"),
            None => photos,
        });
    }

    pub fn transport(&self) -> Option<Arc<dyn Transport>> {
        self.transport.borrow().clone()
    }

    pub fn sync(self: &Rc<Self>) {
        let Some(t) = self.transport() else { return };
        if self.syncing.replace(true) {
            self.sync_again.set(true);
            return;
        }
        self.set_busy(true, "Syncing…");
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
        self.set_busy(false, "");
        match result {
            Ok(report) => {
                self.banner.set_revealed(false);
                if let Some(why) = &report.fell_back {
                    eprintln!("icloud-photos: incremental sync failed ({why}); did a full sync");
                }
                if report.mode == Mode::Full || report.assets + report.removed + report.albums > 0 {
                    self.reload();
                }
                self.update_status();
                if self.settings.borrow().download == DownloadMode::All {
                    self.queue_all_originals();
                }
            }
            Err(e) => self.fail("Sync failed", &e),
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

    /// Re-read albums and the current grid from the catalog.
    fn reload(self: &Rc<Self>) {
        self.load_snapshot(true);
    }

    fn show_assets(self: &Rc<Self>) {
        self.load_snapshot(false);
    }

    /// Query the catalog (and chunk the grid rows) on a worker thread with
    /// its own connection, then apply the result on the main loop. Falls back
    /// to the main-loop connection when the catalog is not on disk.
    fn load_snapshot(self: &Rc<Self>, albums: bool) {
        let generation = self.reload_gen.get() + 1;
        self.reload_gen.set(generation);
        let (path, album, columns) = (self.dirs.catalog(), self.album.borrow().clone(), self.grid.columns());
        let a = self.clone();
        glib::spawn_future_local(async move {
            let snap = gio::spawn_blocking(move || {
                Catalog::open(&path)
                    .ok()
                    .map(|cat| snapshot(&cat, album, albums, columns))
            })
            .await
            .ok()
            .flatten();
            if a.reload_gen.get() != generation {
                return;
            }
            let snap = snap.unwrap_or_else(|| snapshot(&a.cat, a.album.borrow().clone(), albums, columns));
            a.apply_snapshot(snap, albums);
        });
    }

    fn apply_snapshot(&self, snap: Snapshot, albums: bool) {
        if albums {
            if snap.album.is_none() && self.album.borrow().is_some() {
                *self.album.borrow_mut() = None;
                self.grid_page.set_title("All Photos");
            }
            self.reloading.set(true);
            self.albums.set(snap.total, &snap.albums, snap.album.as_deref());
            self.reloading.set(false);
        }
        let first = snap.first_sync;
        self.grid
            .empty
            .set_title(if first { "Getting your photos" } else { "No photos here" });
        self.grid.empty.set_description(Some(if first {
            "The first sync lists your whole library; thumbnails follow as you scroll."
        } else {
            "Photos you add in iCloud, or upload from here, appear in this view."
        }));
        self.grid.set_rows(snap.assets, snap.rows, snap.columns);
    }

    fn bind_tile(self: &Rc<Self>, id: &str, thumb: Option<&PathBuf>, picture: &gtk::Picture) {
        if let Some(t) = self.textures.borrow().get(id) {
            picture.set_paintable(Some(&t));
            return;
        }
        match thumb.filter(|p| p.exists()) {
            Some(p) => self.load_texture(id, p),
            None => {
                if let Some(d) = self.downloader.borrow().as_ref() {
                    d.enqueue(id, Job::Thumb, Priority::Now);
                }
            }
        }
    }

    /// Decode a thumbnail off the main loop, then show it wherever it is bound.
    pub fn load_texture(self: &Rc<Self>, id: &str, path: &Path) {
        if !self.textures.borrow_mut().loading.insert(id.to_owned()) {
            return;
        }
        let (a, id, path) = (self.clone(), id.to_owned(), path.to_owned());
        glib::spawn_future_local(async move {
            let tex = gio::spawn_blocking(move || gdk::Texture::from_filename(&path).ok())
                .await
                .ok()
                .flatten();
            let mut textures = a.textures.borrow_mut();
            textures.loading.remove(&id);
            if let Some(t) = tex {
                textures.insert(id.clone(), t.clone());
                drop(textures);
                a.grid.show_texture(&id, &t);
                a.viewer.on_thumb(&a, &id);
            }
        });
    }

    fn on_download(self: &Rc<Self>, e: thumbs::Event) {
        if let Some(err) = &e.live_error {
            // The photo is saved; the next download fetches only the video.
            if err.is_sign_in() || self.want_open.borrow().contains(&e.id) || self.want_toast.borrow().contains(&e.id) {
                self.fail("The Live Photo's video did not download", err);
            } else {
                eprintln!("icloud-photos: Live Photo video {}: {err}", e.id);
            }
        }
        match (e.job, e.result) {
            (Job::Thumb, Ok(path)) => self.load_texture(&e.id, &path),
            (Job::Medium, Ok(path)) => self.viewer.on_medium(self, &e.id, &path),
            (Job::Original, Ok(path)) => {
                if self.want_open.borrow_mut().remove(&e.id) {
                    self.launch(&path);
                }
                if self.want_toast.borrow_mut().remove(&e.id) {
                    let toast = adw::Toast::builder()
                        .title(glib::markup_escape_text(&format!(
                            "Saved {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        )))
                        .button_label("Show")
                        .timeout(5)
                        .build();
                    let (win, file) = (self.window.clone(), gio::File::for_path(&path));
                    toast.connect_button_clicked(move |_| {
                        gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                            Some(&win),
                            gio::Cancellable::NONE,
                            |_| {},
                        );
                    });
                    self.toasts.add_toast(toast);
                }
                self.viewer.refresh_info(self);
            }
            (job, Err(err)) => {
                let wanted = job != Job::Thumb
                    && (self.want_open.borrow_mut().remove(&e.id) | self.want_toast.borrow_mut().remove(&e.id));
                if err.is_sign_in() || wanted || job == Job::Medium {
                    self.fail("Download failed", &err);
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
                self.toast(&format!("Already saved as {}", p.display()));
            }
            return;
        }
        let Some(d) = self.downloader.borrow().clone() else {
            self.toast("Not connected to iCloud yet");
            return;
        };
        if open {
            self.want_open.borrow_mut().insert(id.to_owned());
        } else {
            self.want_toast.borrow_mut().insert(id.to_owned());
        }
        self.toast("Downloading the original…");
        d.enqueue(id, Job::Original, Priority::Now);
    }

    pub fn delete(self: &Rc<Self>, id: &str) {
        let Some(row) = self.cat.asset(id).ok().flatten() else {
            return;
        };
        let Some(t) = self.transport() else {
            self.toast("Not connected to iCloud yet");
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("Delete this photo?"),
            Some(&format!(
                "{} moves to Recently Deleted in iCloud on all your devices, and can be recovered there for about 30 days.",
                row.filename
            )),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let a = self.clone();
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |response| {
            if response != "delete" {
                return;
            }
            let tx = a.tx.clone();
            std::thread::spawn(move || {
                let result = CloudKit::connect(&*t).and_then(|ck| ck.delete_asset(&row.id, row.change_tag.as_deref()));
                let _ = tx.send_blocking(Msg::Deleted(row.id, result));
            });
        });
    }

    fn on_deleted(self: &Rc<Self>, id: &str, result: Result<Modified>) {
        match result {
            Ok(m) => {
                let _ = self.cat.mark_deleted(id, m.change_tag.as_deref());
                self.viewer.after_delete(self, id);
                self.reload();
                self.update_status();
                self.toast("Moved to Recently Deleted");
            }
            Err(Error::CloudKit { code, .. }) if code == "CONFLICT" => {
                self.toast("This photo changed on another device; syncing, then try again");
                self.sync();
            }
            Err(e) => self.fail("Delete failed", &e),
        }
    }

    fn open_viewer(self: &Rc<Self>, id: &str) {
        let ids = self.grid.ids();
        if let Some(i) = ids.iter().position(|x| x == id) {
            self.viewer.show(self, ids, i);
            if self
                .nav
                .visible_page()
                .is_none_or(|p| p.tag().as_deref() != Some("viewer"))
            {
                self.nav.push(&self.viewer.page);
            }
        }
    }
}

/// Albums (when `with_albums`), the total, and the grid rows for `album`,
/// which falls back to All Photos when that album is gone.
fn snapshot(cat: &Catalog, mut album: Option<String>, with_albums: bool, columns: usize) -> Snapshot {
    let albums = if with_albums {
        cat.albums().unwrap_or_default()
    } else {
        Vec::new()
    };
    if with_albums && album.as_ref().is_some_and(|c| !albums.iter().any(|a| &a.id == c)) {
        album = None;
    }
    let assets = cat.assets(album.as_deref()).unwrap_or_default();
    let first_sync = assets.is_empty()
        && cat
            .meta(icloud_photos::catalog::SYNC_TOKEN_KEY)
            .ok()
            .flatten()
            .is_none();
    let rows = super::grid::rows_of(&assets, columns);
    Snapshot {
        albums,
        total: if with_albums { cat.count().unwrap_or(0) } else { 0 },
        album,
        assets,
        rows,
        columns,
        first_sync,
    }
}
