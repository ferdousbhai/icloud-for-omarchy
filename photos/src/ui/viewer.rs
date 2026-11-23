//! One photo at a time: Apple's medium JPEG (or the downloaded original when
//! it is a JPEG/PNG), prev/next, download, delete, open in the default app,
//! copy the image, and make it the Omarchy background.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use icloud_photos::cloudkit::Kind;
use icloud_photos::still::{self, display_file};
use icloud_photos::thumbs::{Job, Priority};

use omui::icons::name;
use omui::{i18n, tr};

use super::decode;
use super::window::App;

pub struct Viewer {
    /// What the grid's [`omui::Nav`] pushes, tagged "viewer".
    pub page: gtk::Box,
    back: gtk::Button,
    title: omui::Title,
    picture: gtk::Picture,
    info: gtk::Label,
    spinner: gtk::Spinner,
    prev: gtk::Button,
    next: gtk::Button,
    /// The More menu, filled again in the language of the moment.
    menu: gio::Menu,
    ids: RefCell<Vec<String>>,
    index: Cell<usize>,
    /// Whether the picture shows the full rendition (not a thumb stand-in).
    sharp: Cell<bool>,
}

/// A round button over the photo, at its `halign` side (the start, or the
/// end, whichever way the language reads).
fn osd_button(icon: &str, action: &str, halign: gtk::Align) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .action_name(action)
        .halign(halign)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .build();
    b.add_css_class("osd");
    b.add_css_class("circular");
    b
}

/// The More menu's items, in the language in use.
fn fill_menu(menu: &gio::Menu) {
    menu.remove_all();
    menu.append(Some(&tr("Copy Image")), Some("win.viewer-copy"));
    if still::has_wallpaper_command() {
        menu.append(Some(&tr("Set as Wallpaper")), Some("win.viewer-wallpaper"));
    }
}

impl Viewer {
    pub fn new() -> Viewer {
        let title = omui::Title::new("", "");
        let header = omui::header(Some(title.widget.upcast_ref()));
        let back = omui::icon_button(name::BACK, "");
        i18n::bind_tooltip(&back, "Back");
        header.pack_start(&back);
        let menu = gio::Menu::new();
        fill_menu(&menu);
        let more = omui::icon_menu_button(name::MORE, "", &menu);
        i18n::bind_tooltip(&more, "More");
        header.pack_end(&more);
        let delete = omui::icon_button(name::DELETE, "");
        i18n::bind_tooltip(&delete, "Delete (Del)");
        delete.set_action_name(Some("win.viewer-delete"));
        header.pack_end(&delete);
        let download = omui::icon_button(name::DOWNLOAD, "");
        i18n::bind_tooltip(&download, "Download Original (Ctrl+S)");
        download.set_action_name(Some("win.viewer-download"));
        header.pack_end(&download);
        let open = omui::icon_button(name::EXTERNAL_LINK, "");
        i18n::bind_tooltip(&open, "Open in Default App (Ctrl+O)");
        open.set_action_name(Some("win.viewer-open"));
        header.pack_end(&open);

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let spinner = omui::spinner();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_visible(false);
        let prev = osd_button(name::BACK, "win.viewer-prev", gtk::Align::Start);
        i18n::bind_tooltip(&prev, "Previous (←)");
        let next = osd_button(name::FORWARD, "win.viewer-next", gtk::Align::End);
        i18n::bind_tooltip(&next, "Next (→)");
        let overlay = gtk::Overlay::builder().child(&picture).build();
        overlay.add_overlay(&spinner);
        overlay.add_overlay(&prev);
        overlay.add_overlay(&next);
        overlay.add_css_class("viewer");

        let info = gtk::Label::builder().xalign(0.5).margin_top(6).margin_bottom(6).build();
        info.add_css_class("dim-label");
        info.add_css_class("caption");

        overlay.set_vexpand(true);
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&header);
        page.append(&overlay);
        page.append(&info);

        Viewer {
            page,
            back,
            title,
            picture,
            info,
            spinner,
            prev,
            next,
            menu,
            ids: RefCell::default(),
            index: Cell::new(0),
            sharp: Cell::new(false),
        }
    }

    pub fn connect(&self, app: &Rc<App>) {
        let win = &app.window;
        let add = |name: &str, f: Box<dyn Fn()>| win.add_action(&super::window::action(name, f));
        let a = app.clone();
        add("viewer-prev", Box::new(move || a.viewer.step(&a, -1)));
        let a = app.clone();
        add("viewer-next", Box::new(move || a.viewer.step(&a, 1)));
        let a = app.clone();
        add(
            "viewer-delete",
            Box::new(move || {
                if let Some(id) = a.viewer.current() {
                    a.delete(&id)
                }
            }),
        );
        let a = app.clone();
        add(
            "viewer-download",
            Box::new(move || {
                if let Some(id) = a.viewer.current() {
                    a.fetch_original(&id, false)
                }
            }),
        );
        let a = app.clone();
        add(
            "viewer-open",
            Box::new(move || {
                if let Some(id) = a.viewer.current() {
                    a.fetch_original(&id, true)
                }
            }),
        );
        let a = app.clone();
        add("viewer-copy", Box::new(move || a.viewer.copy(&a)));
        let a = app.clone();
        add("viewer-wallpaper", Box::new(move || a.viewer.set_wallpaper(&a)));

        // While the keyboard is in the viewer, which it goes to with the page. The single keys do nothing in a
        // text field, where they move the cursor or delete a character; omui's dialogs hold them all back.
        let keys = omui::Keys::new(&self.page);
        let run = |action: &'static str| {
            let page = self.page.downgrade();
            move || {
                if let Some(page) = page.upgrade() {
                    let _ = page.activate_action(action, None);
                }
            }
        };
        keys.on("Left", run("win.viewer-prev"));
        keys.on("Right", run("win.viewer-next"));
        keys.on("Delete", run("win.viewer-delete"));
        keys.on_always("<Control>s", run("win.viewer-download"));
        keys.on_always("<Control>o", run("win.viewer-open"));
        keys.on_always("<Control>c", run("win.viewer-copy"));
        let nav = app.nav.downgrade();
        self.back.connect_clicked(move |_| {
            if let Some(nav) = nav.upgrade() {
                nav.pop();
            }
        });

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
        let Some(row) = app.cat.asset(&id).ok().flatten() else {
            return;
        };
        // The file's name is iCloud's, and stays as it is.
        self.title.set_title(&row.filename);
        self.set_position();
        self.refresh_info(app);

        // A blurry stand-in right away, then the full rendition.
        self.sharp.set(false);
        self.picture.set_paintable(app.textures.borrow().get(&id).as_ref());
        match display_file(&row) {
            Some(path) => {
                if row.medium_path.as_ref() == Some(&path) {
                    // Keeps it off the eviction end of the medium cache.
                    let p = path.clone();
                    std::thread::spawn(move || icloud_photos::thumbs::touch(&p));
                }
                // An original that cannot be decoded (too large, damaged)
                // falls back to Apple's medium JPEG, fetched if need be.
                let medium = row.medium_path.clone().filter(|m| *m != path);
                self.load(app, &id, &path, medium, row.medium_path.as_ref() != Some(&path));
            }
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
        // Warm the neighbours' medium renditions (a cached one is not fetched again).
        for n in [index + 1, index.wrapping_sub(1)] {
            if let (Some(nid), Some(d)) = (self.ids.borrow().get(n), app.downloader.borrow().as_ref()) {
                d.enqueue(nid, Job::Medium, Priority::Now);
            }
        }
    }

    /// "3 of 120", in the language in use.
    fn set_position(&self) {
        let len = self.ids.borrow().len() as i64;
        self.title.set_subtitle(&omui::tr!(
            "{} of {}",
            i18n::format_int(self.index.get() as i64 + 1),
            i18n::format_int(len)
        ));
    }

    /// Writes the viewer's own words again after a switch of language: its menu, where it is in the album, and the
    /// line under the photo.
    pub fn relabel(&self, app: &App) {
        fill_menu(&self.menu);
        if self.current().is_some() {
            self.set_position();
            self.refresh_info(app);
        }
    }

    pub fn refresh_info(&self, app: &App) {
        let Some(id) = self.current() else { return };
        let Some(row) = app.cat.asset(&id).ok().flatten() else {
            return;
        };
        let mut parts = Vec::new();
        if let Ok(dt) = glib::DateTime::from_unix_local(row.created) {
            parts.push(taken_at(&dt));
        }
        if row.w > 0 {
            parts.push(format!("{} × {}", i18n::format_int(row.w), i18n::format_int(row.h)));
        }
        if row.size > 0 {
            parts.push(super::format_size(row.size as u64));
        }
        match (row.kind, row.is_live) {
            (Kind::Video, _) => parts.push(tr("Video (open to play)")),
            (_, true) => parts.push(tr("Live Photo")),
            _ => {}
        }
        if let Some(p) = row.local_path.filter(|p| p.exists()) {
            parts.push(omui::tr!("Saved in {}", p.parent().unwrap_or(&p).display()));
        }
        self.info.set_label(&parts.join("  ·  "));
    }

    /// Show `path`, or `fallback` if that cannot be decoded; when neither
    /// can and `fetch_medium` is set, ask for Apple's medium JPEG instead
    /// (which comes back through [`on_medium`](Self::on_medium)).
    fn load(&self, app: &Rc<App>, id: &str, path: &Path, fallback: Option<PathBuf>, fetch_medium: bool) {
        self.spinner.set_visible(true);
        self.sharp.set(true);
        let medium = fallback;
        let (a, id, path) = (app.clone(), id.to_owned(), path.to_owned());
        glib::spawn_future_local(async move {
            let tex = gio::spawn_blocking(move || {
                decode::texture(&path)
                    .or_else(|e| medium.ok_or(e).and_then(|m| decode::texture(&m)))
                    .ok()
            })
            .await
            .ok()
            .flatten();
            // Only if the viewer is still on this photo.
            if a.viewer.current().as_deref() == Some(id.as_str()) {
                match (tex, a.downloader.borrow().as_ref()) {
                    (Some(t), _) => a.viewer.picture.set_paintable(Some(&t)),
                    (None, Some(d)) if fetch_medium => {
                        // The spinner stays until the medium arrives or fails.
                        d.enqueue(&id, Job::Medium, Priority::Now);
                        return;
                    }
                    (None, _) => {}
                }
                a.viewer.spinner.set_visible(false);
            }
        });
    }

    pub fn on_medium(&self, app: &Rc<App>, id: &str, path: &Path) {
        if self.current().as_deref() == Some(id) {
            self.load(app, id, path, None, false);
        }
    }

    pub fn on_medium_failed(&self, id: &str) {
        if self.current().as_deref() == Some(id) {
            self.spinner.set_visible(false);
        }
    }

    /// The file the viewer shows for the current photo, or a toast saying why
    /// there is none yet. Videos are left out: their still is only a poster.
    fn current_still(&self, app: &App) -> Option<PathBuf> {
        let row = app.cat.asset(&self.current()?).ok().flatten()?;
        if row.kind == Kind::Video {
            app.toast(&tr("Videos can't be used as an image"));
            return None;
        }
        let path = display_file(&row);
        if path.is_none() {
            app.toast(&tr("Still loading the photo"));
        }
        path
    }

    /// Put the photo on the clipboard as an image (GTK offers it as PNG, so a
    /// HEIC original pastes too, from its JPEG rendition).
    fn copy(&self, app: &Rc<App>) {
        let Some(path) = self.current_still(app) else { return };
        let a = app.clone();
        glib::spawn_future_local(async move {
            match gio::spawn_blocking(move || decode::texture(&path)).await {
                Ok(Ok(tex)) => {
                    a.window.clipboard().set_texture(&tex);
                    a.toast(&tr("Copied"));
                }
                Ok(Err(e)) => a.toast(&omui::tr!("Copy failed: {}", e)),
                Err(_) => a.toast(&tr("Copy failed")),
            }
        });
    }

    /// Make the photo the Omarchy background (see [`still::set_wallpaper`]).
    fn set_wallpaper(&self, app: &Rc<App>) {
        let Some(path) = self.current_still(app) else { return };
        let dirs = app.dirs.clone();
        let a = app.clone();
        // It writes a copy of the photo and the desktop's background, so the application waits for it.
        omui::background(
            move || still::set_wallpaper(&path, &dirs),
            move |result| match result.unwrap_or_else(Err) {
                Ok(_) => a.toast(&tr("Set as wallpaper")),
                Err(e) => a.toast(&omui::tr!("Couldn't set the wallpaper: {}", e)),
            },
        );
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
            let next = i.min(ids.len() - 1);
            self.show(app, ids, next);
        }
    }
}

/// When a photo was taken, in the language in use: its date, spelled out in
/// the order and form the catalog's `%e %B %Y` says (a `strftime` format a
/// translation may change), and the time of day on the language's clock.
fn taken_at(when: &glib::DateTime) -> String {
    let date = i18n::format_datetime(when, &tr("%e %B %Y"));
    omui::tr!("{0}, {1}", date.trim(), i18n::format_time_short(when))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_date_a_photo_was_taken_is_spelled_out() {
        let when = gtk::glib::DateTime::from_utc(2026, 9, 20, 14, 5, 0.0).unwrap();
        let text = super::taken_at(&when);
        assert!(text.starts_with("20 September 2026, "), "{text}");
        assert!(text.contains("05"), "{text}");
    }
}
