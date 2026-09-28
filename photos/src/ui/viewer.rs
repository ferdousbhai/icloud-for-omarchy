//! One photo at a time: Apple's medium JPEG (or the downloaded original when
//! it is a JPEG/PNG), prev/next, download, delete, open in the default app.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use icloud_photos::catalog::Row;
use icloud_photos::cloudkit::Kind;
use icloud_photos::thumbs::{Job, Priority};

use super::window::App;

pub struct Viewer {
    pub page: adw::NavigationPage,
    title: adw::WindowTitle,
    picture: gtk::Picture,
    info: gtk::Label,
    spinner: adw::Spinner,
    prev: gtk::Button,
    next: gtk::Button,
    ids: RefCell<Vec<String>>,
    index: Cell<usize>,
    /// Whether the picture shows the full rendition (not a thumb stand-in).
    sharp: Cell<bool>,
}

fn osd_button(icon: &str, action: &str, halign: gtk::Align, tip: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .action_name(action)
        .halign(halign)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .tooltip_text(tip)
        .build();
    b.add_css_class("osd");
    b.add_css_class("circular");
    b
}

impl Viewer {
    pub fn new() -> Viewer {
        let title = adw::WindowTitle::new("", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_end(&gtk::Button::builder().icon_name("user-trash-symbolic").action_name("win.viewer-delete").tooltip_text("Delete (Del)").build());
        header.pack_end(&gtk::Button::builder().icon_name("document-save-symbolic").action_name("win.viewer-download").tooltip_text("Download Original (Ctrl+S)").build());
        header.pack_end(&gtk::Button::builder().icon_name("adw-external-link-symbolic").action_name("win.viewer-open").tooltip_text("Open in Default App (Ctrl+O)").build());

        let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).can_shrink(true).hexpand(true).vexpand(true).build();
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).visible(false).build();
        let prev = osd_button("go-previous-symbolic", "win.viewer-prev", gtk::Align::Start, "Previous (←)");
        let next = osd_button("go-next-symbolic", "win.viewer-next", gtk::Align::End, "Next (→)");
        let overlay = gtk::Overlay::builder().child(&picture).build();
        overlay.add_overlay(&spinner);
        overlay.add_overlay(&prev);
        overlay.add_overlay(&next);
        overlay.add_css_class("viewer");

        let info = gtk::Label::builder().xalign(0.5).margin_top(6).margin_bottom(6).build();
        info.add_css_class("dim-label");
        info.add_css_class("caption");

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&overlay));
        tv.add_bottom_bar(&info);
        let page = adw::NavigationPage::builder().title("Photo").tag("viewer").child(&tv).build();

        Viewer { page, title, picture, info, spinner, prev, next, ids: RefCell::default(), index: Cell::new(0), sharp: Cell::new(false) }
    }

    pub fn connect(&self, app: &Rc<App>) {
        let win = &app.window;
        let add = |name: &str, f: Box<dyn Fn()>| {
            let act = gio::SimpleAction::new(name, None);
            act.connect_activate(move |_, _| f());
            win.add_action(&act);
        };
        let a = app.clone();
        add("viewer-prev", Box::new(move || a.viewer.step(&a, -1)));
        let a = app.clone();
        add("viewer-next", Box::new(move || a.viewer.step(&a, 1)));
        let a = app.clone();
        add("viewer-delete", Box::new(move || if let Some(id) = a.viewer.current() { a.delete(&id) }));
        let a = app.clone();
        add("viewer-download", Box::new(move || if let Some(id) = a.viewer.current() { a.fetch_original(&id, false) }));
        let a = app.clone();
        add("viewer-open", Box::new(move || if let Some(id) = a.viewer.current() { a.fetch_original(&id, true) }));

        let keys = gtk::ShortcutController::new();
        keys.set_scope(gtk::ShortcutScope::Managed);
        for (trigger, action) in [
            ("Left", "win.viewer-prev"),
            ("Right", "win.viewer-next"),
            ("Delete", "win.viewer-delete"),
            ("<Primary>s", "win.viewer-download"),
            ("<Primary>o", "win.viewer-open"),
        ] {
            keys.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(gtk::NamedAction::new(action))));
        }
        self.page.add_controller(keys);
        let esc = gtk::ShortcutController::new();
        esc.set_scope(gtk::ShortcutScope::Managed);
        let nav = app.nav.clone();
        esc.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::CallbackAction::new(move |_, _| {
                nav.pop();
                glib::Propagation::Stop
            })),
        ));
        self.page.add_controller(esc);

        // Swipe or scroll sideways between photos.
        let a = app.clone();
        let swipe = gtk::GestureSwipe::new();
        swipe.connect_swipe(move |_, vx, vy| {
            if vx.abs() > 400.0 && vx.abs() > vy.abs() * 2.0 {
                a.viewer.step(&a, if vx < 0.0 { 1 } else { -1 });
            }
        });
        self.picture.add_controller(swipe);
    }

    pub fn current(&self) -> Option<String> {
        self.ids.borrow().get(self.index.get()).cloned()
    }

    fn step(&self, app: &Rc<App>, delta: isize) {
        let len = self.ids.borrow().len();
        let i = self.index.get() as isize + delta;
        if i >= 0 && (i as usize) < len {
            let ids = self.ids.borrow().clone();
            self.show(app, ids, i as usize);
        }
    }

    pub fn show(&self, app: &Rc<App>, ids: Vec<String>, index: usize) {
        *self.ids.borrow_mut() = ids;
        self.index.set(index);
        let len = self.ids.borrow().len();
        self.prev.set_visible(index > 0);
        self.next.set_visible(index + 1 < len);
        let Some(id) = self.current() else { return };
        let Some(row) = app.cat.asset(&id).ok().flatten() else { return };
        self.title.set_title(&row.filename);
        self.title.set_subtitle(&format!("{} of {len}", index + 1));
        self.page.set_title(&row.filename);
        self.refresh_info(app);

        // A blurry stand-in right away, then the full rendition.
        self.sharp.set(false);
        self.picture.set_paintable(app.textures.borrow().get(&id).as_ref());
        match display_file(&row) {
            Some(path) => self.load(app, &id, &path),
            None => {
                self.spinner.set_visible(true);
                if let Some(d) = app.downloader.borrow().as_ref() {
                    d.enqueue(&id, Job::Medium, Priority::Now);
                }
            }
        }
        if app.textures.borrow().get(&id).is_none()
            && let Some(p) = row.thumb_path.as_ref().filter(|p| p.exists())
        {
            app.load_texture(&id, p);
        }
        // Warm the neighbours' medium renditions.
        for n in [index + 1, index.wrapping_sub(1)] {
            if let Some(nid) = self.ids.borrow().get(n)
                && let (Some(d), Ok(Some(r))) = (app.downloader.borrow().as_ref(), app.cat.asset(nid))
                && r.medium_path.as_ref().is_none_or(|p| !p.exists())
            {
                d.enqueue(nid, Job::Medium, Priority::Now);
            }
        }
    }

    pub fn refresh_info(&self, app: &App) {
        let Some(id) = self.current() else { return };
        let Some(row) = app.cat.asset(&id).ok().flatten() else { return };
        let mut parts = Vec::new();
        if let Ok(dt) = glib::DateTime::from_unix_local(row.created)
            && let Ok(s) = dt.format("%e %B %Y, %H:%M")
        {
            parts.push(s.trim().to_owned());
        }
        if row.w > 0 {
            parts.push(format!("{} × {}", row.w, row.h));
        }
        if row.size > 0 {
            parts.push(glib::format_size(row.size as u64).to_string());
        }
        match (row.kind, row.is_live) {
            (Kind::Video, _) => parts.push("Video (open to play)".into()),
            (_, true) => parts.push("Live Photo".into()),
            _ => {}
        }
        if let Some(p) = row.local_path.filter(|p| p.exists()) {
            parts.push(format!("Saved in {}", p.parent().unwrap_or(&p).display()));
        }
        self.info.set_label(&parts.join("  ·  "));
    }

    fn load(&self, app: &Rc<App>, id: &str, path: &Path) {
        self.spinner.set_visible(true);
        self.sharp.set(true);
        let (a, id, path) = (app.clone(), id.to_owned(), path.to_owned());
        glib::spawn_future_local(async move {
            let tex = gio::spawn_blocking(move || gdk::Texture::from_filename(&path).ok()).await.ok().flatten();
            // Only if the viewer is still on this photo.
            if a.viewer.current().as_deref() == Some(id.as_str()) {
                a.viewer.spinner.set_visible(false);
                if let Some(t) = tex {
                    a.viewer.picture.set_paintable(Some(&t));
                }
            }
        });
    }

    pub fn on_medium(&self, app: &Rc<App>, id: &str, path: &Path) {
        if self.current().as_deref() == Some(id) {
            self.load(app, id, path);
        }
    }

    pub fn on_medium_failed(&self, id: &str) {
        if self.current().as_deref() == Some(id) {
            self.spinner.set_visible(false);
        }
    }

    /// A thumbnail arrived; use it as the stand-in if nothing better is shown.
    pub fn on_thumb(&self, app: &App, id: &str) {
        if self.current().as_deref() == Some(id) && !self.sharp.get() {
            self.picture.set_paintable(app.textures.borrow().get(id).as_ref());
        }
    }

    /// After a delete: show the next photo, or go back to the grid.
    pub fn after_delete(&self, app: &Rc<App>, id: &str) {
        if self.current().as_deref() != Some(id) {
            return;
        }
        let mut ids = self.ids.borrow().clone();
        let i = self.index.get();
        ids.remove(i);
        if ids.is_empty() {
            app.nav.pop();
        } else {
            self.show(app, ids.clone(), i.min(ids.len() - 1));
        }
    }
}

/// The best local file to display: a JPEG/PNG original, else Apple's medium JPEG.
fn display_file(row: &Row) -> Option<std::path::PathBuf> {
    let viewable_original = matches!(row.orig_type.as_deref(), Some("public.jpeg" | "public.png"));
    row.local_path
        .as_ref()
        .filter(|p| viewable_original && p.exists())
        .or(row.medium_path.as_ref().filter(|p| p.exists()))
        .cloned()
}
