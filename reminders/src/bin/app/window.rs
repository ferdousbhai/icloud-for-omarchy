//! The main window: the lists in a sidebar ("Upcoming" first: every open
//! reminder, soonest due first), the chosen list's reminders with a check
//! box each, a line to add one, and a dialog to edit or delete one. Syncs
//! on opening and every [`SYNC_SECS`] while shown. Notifications are not
//! the window's job: the background timer sends them whether it is open or
//! not.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use jiff::tz::TimeZone;

use super::background;
use super::banner::SignInBanner;
use icloud_reminders::cloudkit::{Error, SessionTransport};
use icloud_reminders::due::{self, Due};
use icloud_reminders::model::{Change, Reminder};
use icloud_reminders::service::Service;
use icloud_reminders::store::{Cache, Store};

const SYNC_SECS: u32 = 60;
/// The sidebar row of every list's open reminders.
const UPCOMING: &str = "";

pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: SignInBanner,
    lists: gtk::ListBox,
    rows: gtk::ListBox,
    content_title: adw::WindowTitle,
    split: adw::NavigationSplitView,
    new_title: gtk::Entry,
    new_due: gtk::Entry,
    show_completed: gtk::ToggleButton,
    transport: Arc<SessionTransport>,
    dir: Option<PathBuf>,
    local: TimeZone,
    cache: RefCell<Cache>,
    /// The sidebar's choice: a list id, or [`UPCOMING`].
    selected: RefCell<String>,
    syncing: Cell<bool>,
    last_error: RefCell<Option<String>>,
    /// A new reminder is being saved: a second Enter waits for it.
    adding: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application, local: TimeZone) -> Rc<Self> {
        let this = Rc::new_cyclic(|weak: &Weak<Window>| {
            let (w1, w2) = (weak.clone(), weak.clone());
            let banner = SignInBanner::new(
                move || {
                    if let Some(this) = w1.upgrade() {
                        this.sync();
                    }
                },
                move |msg| {
                    if let Some(this) = w2.upgrade() {
                        this.toast(&msg);
                    }
                },
            );
            build(app, banner, local)
        });
        let keep_alive = RefCell::new(Some(this.clone()));
        this.window.connect_close_request(move |_| {
            keep_alive.borrow_mut().take();
            glib::Propagation::Proceed
        });
        this.connect();
        if let Some(store) = this.store() {
            match store.cache() {
                Ok(cache) => *this.cache.borrow_mut() = cache,
                Err(e) => this.error(&e.to_string()),
            }
        }
        this.show_lists();
        this.sync();
        let weak = Rc::downgrade(&this);
        glib::timeout_add_seconds_local(SYNC_SECS, move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.window.is_visible() && !this.window.is_suspended() {
                this.sync();
            }
            glib::ControlFlow::Continue
        });
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    fn store(&self) -> Option<Store> {
        self.dir.clone().map(Store::new)
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.lists.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else { return };
            let id = row.widget_name().to_string();
            if *this.selected.borrow() != id {
                *this.selected.borrow_mut() = id;
                this.show_reminders();
            }
        });
        // Only a click (or Enter) opens the content page in the narrow
        // layout: show_lists re-selects a row after every sync and write.
        let weak = Rc::downgrade(self);
        self.lists.connect_row_activated(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.split.set_show_content(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.show_completed.connect_toggled(move |_| {
            if let Some(this) = weak.upgrade() {
                this.show_reminders();
            }
        });
        for entry in [&self.new_title, &self.new_due] {
            let weak = Rc::downgrade(self);
            entry.connect_activate(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.add();
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.rows.connect_row_activated(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            let id = row.widget_name().to_string();
            let found = this.cache.borrow().reminders.get(&id).cloned();
            if let Some(r) = found {
                this.edit(r);
            }
        });

        let action = |name: &str, f: fn(&Rc<Window>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    f(&this);
                }
            });
            self.window.add_action(&a);
        };
        action("refresh", |this| {
            *this.last_error.borrow_mut() = None;
            this.sync();
        });
        action("new", |this| {
            this.split.set_show_content(true);
            this.new_title.grab_focus();
        });
    }

    /// Syncs on a worker thread, then shows the cache.
    fn sync(self: &Rc<Self>) {
        if self.syncing.replace(true) {
            return;
        }
        let (t, dir) = (self.transport.clone(), self.dir.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let dir = dir.ok_or_else(|| Error::Other("no data directory ($HOME is not set)".into()))?;
                let svc = Service::new(&*t, Store::new(dir));
                svc.sync(false)?;
                svc.cache()
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.syncing.set(false);
                match result {
                    Ok(Ok(cache)) => {
                        this.banner.hide();
                        *this.last_error.borrow_mut() = None;
                        *this.cache.borrow_mut() = cache;
                        this.show_lists();
                    }
                    Ok(Err(Error::SignInRequired)) => this.banner.show(),
                    Ok(Err(e)) => this.error(&e.to_string()),
                    Err(e) => this.error(&e),
                }
            },
        );
    }

    /// Toasts an error once, not on every tick while it persists.
    fn error(&self, msg: &str) {
        if self.last_error.borrow().as_deref() != Some(msg) {
            self.toast(msg);
            *self.last_error.borrow_mut() = Some(msg.to_owned());
        }
    }

    /// Runs one write on a worker thread; shows the cache afterwards (the
    /// write updated it) and calls `done`, or shows the error and calls
    /// `failed`, which gives back what the user typed.
    fn write<T: Send + 'static>(
        self: &Rc<Self>,
        f: impl FnOnce(&Service) -> icloud_reminders::cloudkit::Result<T> + Send + 'static,
        done: impl FnOnce(&Rc<Self>, T) + 'static,
        failed: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        let (t, dir) = (self.transport.clone(), self.dir.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let dir = dir.ok_or_else(|| Error::Other("no data directory ($HOME is not set)".into()))?;
                let svc = Service::new(&*t, Store::new(dir));
                let out = f(&svc)?;
                // The write happened: a cache that can't be read is shown
                // as a warning, not as a failed write.
                Ok::<_, Error>((out, svc.cache().map_err(|e| e.to_string())))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok((out, cache))) => {
                        match cache {
                            Ok(cache) => *this.cache.borrow_mut() = cache,
                            Err(e) => this.toast(&format!("Saved in iCloud; cannot read the local cache: {e}")),
                        }
                        this.show_lists();
                        done(&this, out);
                    }
                    Ok(Err(Error::SignInRequired)) => {
                        this.banner.show();
                        this.show_reminders();
                        failed(&this);
                    }
                    Ok(Err(e)) => {
                        this.toast(&e.to_string());
                        this.show_reminders();
                        failed(&this);
                    }
                    Err(e) => {
                        this.toast(&e);
                        failed(&this);
                    }
                }
            },
        );
    }

    fn update(
        self: &Rc<Self>,
        r: Reminder,
        changes: Vec<Change>,
        toast: Option<String>,
        failed: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        self.write(
            move |svc| svc.update(&r, &changes),
            move |this, saved| {
                if let Some(w) = saved.warning {
                    this.toast(&w);
                } else if let Some(t) = toast {
                    this.toast(&t);
                }
            },
            failed,
        );
    }

    fn add(self: &Rc<Self>) {
        let title = self.new_title.text().trim().to_owned();
        if title.is_empty() || self.adding.get() {
            return;
        }
        let due = match self.parse_due(&self.new_due.text()) {
            Ok(due) => due,
            Err(e) => return self.toast(&e),
        };
        let cache = self.cache.borrow();
        let selected = self.selected.borrow().clone();
        let list = cache
            .list(&selected)
            .or_else(|| cache.only_list())
            .map(|l| l.id.clone());
        drop(cache);
        let Some(list) = list else {
            return self.toast("Choose a list in the sidebar first: iCloud records no default list");
        };
        // Cleared once it's saved (unless typed over meanwhile); kept on failure.
        let (sent_title, sent_due) = (self.new_title.text(), self.new_due.text());
        self.adding.set(true);
        self.write(
            move |svc| svc.add(&list, &title, "", due.as_ref()),
            move |this, saved| {
                this.adding.set(false);
                if let Some(w) = saved.warning {
                    this.toast(&w);
                }
                if this.new_title.text() == sent_title && this.new_due.text() == sent_due {
                    this.new_title.set_text("");
                    this.new_due.set_text("");
                }
            },
            |this| this.adding.set(false),
        );
    }

    /// An empty text is no due date.
    fn parse_due(&self, text: &str) -> Result<Option<Due>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        due::parse_when(text, &jiff::Zoned::now().with_time_zone(self.local.clone())).map(Some)
    }

    fn edit(self: &Rc<Self>, r: Reminder) {
        let draft = Draft {
            title: r.title.clone(),
            due: r.due.as_ref().map(|d| d.display(&self.local)).unwrap_or_default(),
            notes: r.notes.clone(),
        };
        self.edit_draft(r, draft);
    }

    /// The edit dialog, filled from `draft`: the reminder's values, or what
    /// was typed before a save that failed.
    fn edit_draft(self: &Rc<Self>, r: Reminder, draft: Draft) {
        let dialog = adw::AlertDialog::new(Some("Edit Reminder"), None);
        let fields = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let title = adw::EntryRow::builder().title("Title").text(&draft.title).build();
        let due_text = r.due.as_ref().map(|d| d.display(&self.local)).unwrap_or_default();
        let due = adw::EntryRow::builder()
            .title("Due (2026-10-10 09:00, tomorrow 9:00, +2h; empty for none)")
            .text(&draft.due)
            .build();
        fields.append(&title);
        fields.append(&due);
        let notes = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        notes.buffer().set_text(&draft.notes);
        let notes_frame = gtk::Frame::builder()
            .child(&gtk::ScrolledWindow::builder().child(&notes).min_content_height(90).build())
            .margin_top(12)
            .build();
        // What's wrong with the title or due date, while it is.
        let problem = gtk::Label::builder()
            .css_classes(["error", "caption"])
            .xalign(0.0)
            .wrap(true)
            .margin_top(8)
            .visible(false)
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&fields);
        body.append(&problem);
        body.append(&notes_frame);
        if r.alarms > 0 {
            body.append(
                &gtk::Label::builder()
                    .label("Alerts set on an Apple device keep their own time.")
                    .css_classes(["dim-label", "caption"])
                    .margin_top(8)
                    .wrap(true)
                    .build(),
            );
        }
        dialog.set_extra_child(Some(&body));
        dialog.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete"), ("save", "_Save")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        // Save is enabled only while the title and due date are valid, so
        // a mistake never closes the dialog and loses the edits.
        let weak = Rc::downgrade(self);
        let validate = {
            let (dialog, title, due, problem, due_text) = (dialog.clone(), title.clone(), due.clone(), problem.clone(), due_text.clone());
            move || {
                let Some(this) = weak.upgrade() else { return };
                let title_problem = title.text().trim().is_empty().then(|| "A reminder needs a title.".to_owned());
                let due_problem = (due.text().trim() != due_text)
                    .then(|| this.parse_due(&due.text()).err())
                    .flatten();
                for (row, bad) in [(&title, title_problem.is_some()), (&due, due_problem.is_some())] {
                    if bad {
                        row.add_css_class("error");
                    } else {
                        row.remove_css_class("error");
                    }
                }
                let text = title_problem.or(due_problem);
                problem.set_visible(text.is_some());
                problem.set_label(text.as_deref().unwrap_or(""));
                dialog.set_response_enabled("save", text.is_none());
            }
        };
        let validate = Rc::new(validate);
        for row in [&title, &due] {
            let validate = validate.clone();
            row.connect_changed(move |_| validate());
        }
        validate();

        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            match response {
                "delete" => {
                    let name = r.title.clone();
                    this.update(r.clone(), vec![Change::Deleted], Some(format!("Deleted \"{name}\"")), |_| {});
                }
                "save" => {
                    let buffer = notes.buffer();
                    let draft = Draft {
                        title: title.text().to_string(),
                        due: due.text().to_string(),
                        notes: buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string(),
                    };
                    let mut changes = Vec::new();
                    // Written (trimmed) only when edited: an untouched "Milk "
                    // stays as Apple has it.
                    let new_title = draft.title.trim();
                    if draft.title != r.title && new_title != r.title {
                        changes.push(Change::Title(new_title.to_owned()));
                    }
                    if draft.notes != r.notes {
                        changes.push(Change::Notes(draft.notes.clone()));
                    }
                    if draft.due.trim() != due_text {
                        // Validated while the dialog was open.
                        match this.parse_due(&draft.due) {
                            Ok(d) => changes.push(Change::Due(d)),
                            Err(e) => return this.toast(&e),
                        }
                    }
                    if !changes.is_empty() {
                        // A failed save opens the dialog again with what was typed.
                        let r2 = r.clone();
                        this.update(r.clone(), changes, None, move |this| this.edit_draft(r2, draft));
                    }
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Rebuilds the sidebar (keeping the selection), then the reminders.
    fn show_lists(self: &Rc<Self>) {
        let cache = self.cache.borrow();
        let selected = self.selected.borrow().clone();
        let open = |id: &str| {
            cache
                .reminders
                .values()
                .filter(|r| !r.completed && (id == UPCOMING || r.list_id == id))
                .count()
        };
        self.lists.remove_all();
        let mut select = None;
        let entries = std::iter::once((UPCOMING.to_owned(), "Upcoming".to_owned()))
            .chain(cache.lists.iter().map(|l| (l.id.clone(), l.name.clone())));
        for (id, name) in entries {
            let row = sidebar_row(&id, &name, open(&id));
            if id == selected {
                select = Some(row.clone());
            }
            self.lists.append(&row);
        }
        drop(cache);
        if select.is_none() {
            *self.selected.borrow_mut() = UPCOMING.to_owned();
        }
        let row = select.or_else(|| self.lists.row_at_index(0));
        self.lists.select_row(row.as_ref());
        self.show_reminders();
    }

    fn show_reminders(self: &Rc<Self>) {
        let cache = self.cache.borrow();
        let selected = self.selected.borrow().clone();
        let completed = self.show_completed.is_active();
        let list = cache.list(&selected);
        self.content_title
            .set_title(list.map_or("Upcoming", |l| l.name.as_str()));
        let rows = cache.sorted(&self.local, |r| {
            (selected == UPCOMING || r.list_id == selected) && (completed || !r.completed)
        });
        self.rows.remove_all();
        let now = jiff::Timestamp::now();
        for r in rows {
            self.rows.append(&self.reminder_row(r, &cache, selected == UPCOMING, now));
        }
    }

    fn reminder_row(self: &Rc<Self>, r: &Reminder, cache: &Cache, with_list: bool, now: jiff::Timestamp) -> adw::ActionRow {
        let mut parts = Vec::new();
        let overdue = !r.completed && r.due.as_ref().is_some_and(|d| d.instant(&self.local) < now);
        if let Some(d) = &r.due {
            parts.push(d.display(&self.local));
        }
        if with_list && let Some(l) = cache.list(&r.list_id) {
            parts.push(l.name.clone());
        }
        if let Some(first) = r.notes.lines().find(|l| !l.trim().is_empty()) {
            parts.push(first.to_owned());
        }
        let row = adw::ActionRow::builder()
            .title(&r.title)
            .subtitle(parts.join(" · "))
            .use_markup(false)
            .activatable(true)
            .subtitle_lines(1)
            .build();
        row.set_widget_name(&r.id);
        if overdue {
            row.add_css_class("overdue");
        }
        let check = gtk::CheckButton::builder()
            .active(r.completed)
            .valign(gtk::Align::Center)
            .tooltip_text(if r.completed { "Mark as Not Completed" } else { "Mark as Completed" })
            .build();
        check.add_css_class("selection-mode");
        let weak = Rc::downgrade(self);
        let reminder = r.clone();
        check.connect_toggled(move |c| {
            let Some(this) = weak.upgrade() else { return };
            let done = c.is_active();
            if done == reminder.completed {
                return;
            }
            let toast = done.then(|| format!("Completed \"{}\"", reminder.title));
            // One write at a time per box: a quick second click would compare
            // against the state this row was built with and be dropped. The
            // rebuild after the write (or its failure) brings a fresh box
            // showing the cached state.
            c.set_sensitive(false);
            this.update(reminder.clone(), vec![Change::Completed(done)], toast, |this| this.show_reminders());
        });
        row.add_prefix(&check);
        row
    }
}

/// The edit dialog's fields as typed.
struct Draft {
    title: String,
    due: String,
    notes: String,
}

fn sidebar_row(id: &str, name: &str, open: usize) -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    b.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if open > 0 {
        b.append(&gtk::Label::builder().label(open.to_string()).css_classes(["dim-label"]).build());
    }
    let row = gtk::ListBoxRow::builder().child(&b).build();
    row.set_widget_name(id);
    row
}

const CSS: &str = "row.overdue .subtitle { color: var(--error-color); opacity: 1; }";

fn build(app: &adw::Application, banner: SignInBanner, local: TimeZone) -> Window {
    let css = gtk::CssProvider::new();
    css.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    // Sidebar: the lists.
    let lists = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .css_classes(["navigation-sidebar"])
        .build();
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.pack_start(
        &gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Sync Now (Ctrl+R)")
            .action_name("win.refresh")
            .build(),
    );
    let sidebar_tv = adw::ToolbarView::new();
    sidebar_tv.add_top_bar(&sidebar_header);
    sidebar_tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&lists).vexpand(true).build()));
    let sidebar_page = adw::NavigationPage::builder().title("Lists").child(&sidebar_tv).build();

    // Content: the add line and the reminders.
    let content_title = adw::WindowTitle::new("Upcoming", "");
    let content_header = adw::HeaderBar::builder().title_widget(&content_title).build();
    let show_completed = gtk::ToggleButton::builder()
        .icon_name("checkbox-checked-symbolic")
        .tooltip_text("Show Completed")
        .build();
    content_header.pack_end(&show_completed);

    let new_title = gtk::Entry::builder()
        .placeholder_text("New reminder (Ctrl+N)")
        .hexpand(true)
        .build();
    let new_due = gtk::Entry::builder()
        .placeholder_text("Due: tomorrow 9:00")
        .width_chars(16)
        .build();
    let add_line = gtk::Box::builder().spacing(6).margin_bottom(12).build();
    add_line.append(&new_title);
    add_line.append(&new_due);

    let rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .valign(gtk::Align::Start)
        .build();
    rows.set_placeholder(Some(
        &adw::StatusPage::builder()
            .icon_name("checkbox-checked-symbolic")
            .title("No Reminders")
            .css_classes(["compact"])
            .build(),
    ));
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&add_line);
    column.append(&rows);
    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .child(&column)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let content_tv = adw::ToolbarView::new();
    content_tv.add_top_bar(&content_header);
    content_tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build()));
    let content_page = adw::NavigationPage::builder().title("Reminders").child(&content_tv).build();

    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&content_page)
        .min_sidebar_width(200.0)
        .max_sidebar_width(280.0)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner.widget);
    split.set_vexpand(true);
    body.append(&split);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Reminders")
        .icon_name(super::APP_ID)
        .default_width(900)
        .default_height(640)
        .width_request(360)
        .height_request(320)
        .content(&toasts)
        .build();
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 600sp").expect("valid condition"));
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    Window {
        window,
        toasts,
        banner,
        lists,
        rows,
        content_title,
        split,
        new_title,
        new_due,
        show_completed,
        transport: Arc::default(),
        dir: Store::default_dir(),
        local,
        cache: RefCell::default(),
        selected: RefCell::new(UPCOMING.to_owned()),
        syncing: Cell::new(false),
        last_error: RefCell::default(),
        adding: Cell::default(),
    }
}
