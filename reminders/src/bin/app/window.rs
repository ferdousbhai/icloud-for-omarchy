//! The main window: the lists in a sidebar ("Upcoming" first: every open
//! reminder, soonest due first), the chosen list's reminders with a check
//! box each, a line to add one, and a dialog to edit or delete one. Syncs
//! on opening and every [`SYNC_SECS`] while shown. Notifications are not
//! the window's job: the background timer sends them whether it is open or
//! not.
//!
//! Its text is in Omarchy's language and follows a switch at once; the
//! reminders' titles and notes and the lists' names are the user's, never
//! translated. A due date reads as the language writes dates and times
//! ("Today, 09:00"), while the edit dialog's due field keeps the form the
//! field reads back ("2026-10-10 09:00"), as the command line and the files
//! in ~/Documents/Reminders write it.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gio, glib};
use jiff::tz::TimeZone;

use icloud_app_ui::{SignInBanner, sign_in_ask};
use icloud_reminders::cloudkit::{Error, SessionTransport};
use icloud_reminders::due::{self, Due};
use icloud_reminders::model::{Change, Reminder};
use icloud_reminders::service::Service;
use icloud_reminders::store::{Cache, Store};
use omui::{background, i18n, icons, tr};

const SYNC_SECS: u32 = 60;
/// The sidebar row of every list's open reminders.
const UPCOMING: &str = "";

pub struct Window {
    window: gtk::ApplicationWindow,
    shell: Rc<omui::Shell>,
    banner: SignInBanner,
    lists: gtk::ListBox,
    rows: gtk::ListBox,
    content_title: omui::Title,
    sidebar_title: omui::Title,
    /// What the reminders' list shows when it has none.
    empty: omui::EmptyState,
    split: omui::Split,
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
    pub fn new(app: &gtk::Application, local: TimeZone) -> Rc<Self> {
        let this = Rc::new_cyclic(|weak: &Weak<Window>| {
            let (w1, w2) = (weak.clone(), weak.clone());
            let banner = SignInBanner::new(
                sign_in_ask("Sign in to iCloud to see your reminders"),
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
        keys(&this.window, true);
        // The sidebar, the reminders and their due dates are built from the
        // cache in the language of the moment: built again after a switch.
        let weak = Rc::downgrade(&this);
        i18n::on_change(&this.window, move || {
            if let Some(this) = weak.upgrade() {
                this.retranslate();
            }
        });
        if let Ok(store) = store_in(this.dir.clone()) {
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

    /// What is the window's own text and not a widget's bound text.
    fn retranslate(self: &Rc<Self>) {
        self.sidebar_title.set_title(&tr("Lists"));
        self.empty.set_title(&tr("No Reminders"));
        self.show_lists();
    }

    fn toast(&self, msg: &str) {
        self.shell.toast(msg);
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.lists.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
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
                let svc = Service::new(&t, store_in(dir)?);
                let warning = svc.sync(false)?.warning();
                Ok::<_, Error>((svc.cache()?, warning))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.syncing.set(false);
                match result {
                    Ok(Ok((cache, warning))) => {
                        this.banner.hide();
                        *this.cache.borrow_mut() = cache;
                        this.show_lists();
                        // Skipped records: toasted once, like an error.
                        match warning {
                            Some(w) => this.error(&w),
                            None => *this.last_error.borrow_mut() = None,
                        }
                    }
                    Ok(Err(e)) if e.is_sign_in() => this.banner.show(),
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
    /// write updated it) and calls `done`, whose message is toasted, or shows
    /// the error and calls `failed`, which gives back what the user typed.
    fn write<T: Send + 'static>(
        self: &Rc<Self>,
        f: impl FnOnce(&Service) -> icloud_reminders::cloudkit::Result<T> + Send + 'static,
        done: impl FnOnce(&Rc<Self>, T) -> Option<String> + 'static,
        failed: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        let (t, dir) = (self.transport.clone(), self.dir.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let svc = Service::new(&t, store_in(dir)?);
                let out = f(&svc)?;
                // The write happened: a cache that can't be read is shown
                // as a warning, not as a failed write.
                Ok::<_, Error>((out, svc.cache().map_err(|e| e.to_string())))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok((out, cache))) => {
                        let unread = match cache {
                            Ok(cache) => {
                                *this.cache.borrow_mut() = cache;
                                None
                            }
                            Err(e) => Some(tr!("Saved in iCloud; cannot read the local cache: {}", e)),
                        };
                        this.show_lists();
                        // One toast: a second would replace the first at once.
                        let msgs: Vec<String> = unread.into_iter().chain(done(&this, out)).collect();
                        if !msgs.is_empty() {
                            this.toast(&msgs.join("\n"));
                        }
                    }
                    Ok(Err(e)) => {
                        if e.is_sign_in() {
                            this.banner.show();
                        } else {
                            this.toast(&e.to_string());
                        }
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
            move |_, saved| saved.warning.or(toast),
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
            return self.toast(&tr("Choose a list in the sidebar first"));
        };
        // Cleared once it's saved (unless typed over meanwhile); kept on failure.
        let (sent_title, sent_due) = (self.new_title.text(), self.new_due.text());
        self.adding.set(true);
        self.write(
            move |svc| svc.add(&list, &title, "", due.as_ref()),
            move |this, saved| {
                this.adding.set(false);
                if this.new_title.text() == sent_title && this.new_due.text() == sent_due {
                    this.new_title.set_text("");
                    this.new_due.set_text("");
                }
                saved.warning
            },
            |this| this.adding.set(false),
        );
    }

    /// An empty text is no due date. The words it reads (`today`,
    /// `tomorrow`) are English in every language for now, as in the files
    /// and on the command line; what it says when it cannot read one is in
    /// the language.
    fn parse_due(&self, text: &str) -> Result<Option<Due>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        due::parse_when(text, &jiff::Zoned::now().with_time_zone(self.local.clone()))
            .map(Some)
            .map_err(|_| {
                tr!(
                    "Cannot read \"{}\" as a due date: write one like 2026-10-10, \"2026-10-10 17:30\", today, \"tomorrow 9:00\", 17:30 or +2h",
                    text
                )
            })
    }

    fn edit(self: &Rc<Self>, r: Reminder) {
        let draft = Draft {
            title: r.title.clone(),
            due: self.due_text(&r),
            notes: r.notes.clone(),
        };
        self.edit_draft(r, draft);
    }

    /// The due date as the edit dialog's field holds it, in the form the
    /// field reads back (not translated); empty for none.
    fn due_text(&self, r: &Reminder) -> String {
        r.due.as_ref().map(|d| d.display(&self.local)).unwrap_or_default()
    }

    /// The edit dialog, filled from `draft`: the reminder's values, or what
    /// was typed before a save that failed.
    fn edit_draft(self: &Rc<Self>, r: Reminder, draft: Draft) {
        // The heading, the buttons and the fields' titles follow a switch of
        // language while it is open.
        let dialog = omui::Dialog::new(Some(""), None);
        dialog.set_heading_translated("Edit Reminder");
        let fields = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let (title_row, title) = omui::entry_row(&tr("Title"), &draft.title);
        let due_text = self.due_text(&r);
        let (due_row, due) = omui::entry_row(
            &tr("Due (2026-10-10 09:00, tomorrow 9:00, +2h; empty for none)"),
            &draft.due,
        );
        fields.append(&title_row.widget);
        fields.append(&due_row.widget);
        let notes = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        notes.buffer().set_text(&draft.notes);
        let notes_frame = gtk::Frame::builder()
            .child(
                &gtk::ScrolledWindow::builder()
                    .child(&notes)
                    .min_content_height(90)
                    .build(),
            )
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
            let alerts = gtk::Label::builder()
                .css_classes(["dim-label", "caption"])
                .xalign(0.0)
                .margin_top(8)
                .wrap(true)
                .build();
            i18n::bind_label(&alerts, "Alerts set on an Apple device keep their own time.");
            body.append(&alerts);
        }
        title_row.set_title_translated("Title");
        due_row.set_title_translated("Due (2026-10-10 09:00, tomorrow 9:00, +2h; empty for none)");
        dialog.set_extra_child(Some(body.upcast_ref()));
        dialog.set_content_size(360, -1);
        for id in ["cancel", "delete", "save"] {
            dialog.add_response(id, "");
        }
        dialog.set_response_translated("cancel", "_Cancel");
        dialog.set_response_translated("delete", "_Delete");
        dialog.set_response_translated("save", "_Save");
        dialog.set_response_appearance("delete", omui::Appearance::Destructive);
        dialog.set_response_appearance("save", omui::Appearance::Suggested);
        dialog.set_default_response("save");
        dialog.set_close_response("cancel");

        // Save is enabled only while the title and due date are valid, so
        // a mistake never closes the dialog and loses the edits.
        // Weak: the dialog owns the fields, and they own this.
        let weak = Rc::downgrade(self);
        let validate = Rc::new({
            let (dialog, title, due) = (dialog.downgrade(), title.downgrade(), due.downgrade());
            // Weak too: the language listener below holds this, and must not
            // hold the label it is anchored on.
            let (problem, due_text) = (problem.downgrade(), due_text.clone());
            move || {
                let (Some(this), Some(dialog), Some(title), Some(due), Some(problem)) = (
                    weak.upgrade(),
                    dialog.upgrade(),
                    title.upgrade(),
                    due.upgrade(),
                    problem.upgrade(),
                ) else {
                    return;
                };
                let title_problem = title.text().trim().is_empty().then(|| tr("A reminder needs a title."));
                let due_problem = (due.text().trim() != due_text)
                    .then(|| this.parse_due(&due.text()).err())
                    .flatten();
                for (field, bad) in [(&title, title_problem.is_some()), (&due, due_problem.is_some())] {
                    if let Some(row) = field.ancestor(gtk::ListBoxRow::static_type()) {
                        omui::set_class(&row, "error", bad);
                    }
                }
                let text = title_problem.or(due_problem);
                problem.set_visible(text.is_some());
                problem.set_label(text.as_deref().unwrap_or(""));
                dialog.set_response_enabled("save", text.is_none());
            }
        });
        for field in [&title, &due] {
            let validate = validate.clone();
            field.connect_changed(move |_| validate());
        }
        validate();
        // What is wrong, said again in the new language.
        i18n::on_change(&problem, {
            let validate = validate.clone();
            move || validate()
        });

        let weak = Rc::downgrade(self);
        dialog.connect_response(move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            match response {
                "delete" => {
                    let name = r.title.clone();
                    this.update(
                        r.clone(),
                        vec![Change::Deleted],
                        Some(tr!("Deleted \"{}\"", name)),
                        |_| {},
                    );
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
        dialog.present(&self.window);
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
        let entries = std::iter::once((UPCOMING.to_owned(), tr("Upcoming")))
            .chain(cache.lists.iter().map(|l| (l.id.clone(), l.name.clone())));
        for (id, name) in entries {
            let open = open(&id);
            let count = (open > 0).then(|| i18n::format_int(open as i64));
            let row = omui::sidebar_row(&id, None, &name, count.as_deref());
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
            .set_title(&list.map_or_else(|| tr("Upcoming"), |l| l.name.clone()));
        let rows = cache.sorted(&self.local, |r| {
            (selected == UPCOMING || r.list_id == selected) && (completed || !r.completed)
        });
        // Row by row: `remove_all` would take the placeholder with them.
        while let Some(row) = self.rows.row_at_index(0) {
            self.rows.remove(&row);
        }
        let now = jiff::Timestamp::now();
        // Within this one rebuild, today is one day for every row.
        let today = now.to_zoned(self.local.clone()).date();
        for r in rows {
            self.rows
                .append(&self.reminder_row(r, &cache, selected == UPCOMING, now, today));
        }
    }

    fn reminder_row(
        self: &Rc<Self>,
        r: &Reminder,
        cache: &Cache,
        with_list: bool,
        now: jiff::Timestamp,
        today: jiff::civil::Date,
    ) -> gtk::ListBoxRow {
        let mut parts = Vec::new();
        let overdue = !r.completed && r.due.as_ref().is_some_and(|d| d.instant(&self.local) < now);
        if let Some(d) = &r.due {
            parts.push(due_label(d, &self.local, today));
        }
        if with_list && let Some(l) = cache.list(&r.list_id) {
            parts.push(l.name.clone());
        }
        if let Some(first) = r.notes.lines().find(|l| !l.trim().is_empty()) {
            parts.push(first.to_owned());
        }
        let row = omui::Row::new(&r.title, &parts.join(" · "));
        row.set_subtitle_lines(1);
        row.widget.set_activatable(true);
        row.widget.set_widget_name(&r.id);
        omui::set_class(&row.widget, "overdue", overdue);
        let check = gtk::CheckButton::builder()
            .active(r.completed)
            .valign(gtk::Align::Center)
            .tooltip_text(if r.completed {
                tr("Mark as Not Completed")
            } else {
                tr("Mark as Completed")
            })
            .build();
        let weak = Rc::downgrade(self);
        let reminder = r.clone();
        check.connect_toggled(move |c| {
            let Some(this) = weak.upgrade() else { return };
            let done = c.is_active();
            if done == reminder.completed {
                return;
            }
            let toast = done.then(|| tr!("Completed \"{}\"", reminder.title));
            // One write at a time per box: a quick second click would compare
            // against the state this row was built with and be dropped. The
            // rebuild after the write (or its failure) brings a fresh box
            // showing the cached state.
            c.set_sensitive(false);
            this.update(reminder.clone(), vec![Change::Completed(done)], toast, |this| {
                this.show_reminders()
            });
        });
        row.add_prefix(&check);
        row.widget
    }
}

/// The edit dialog's fields as typed.
struct Draft {
    title: String,
    due: String,
    notes: String,
}

/// A due date as a reminder's row says it, in the language: "Today",
/// "Tomorrow, 09:00", or the language's short date ("10/12/26, 09:00" in
/// English). An all-day one is a date alone; a timed one is shown in
/// `local`, as [`Due::instant`] reads it.
fn due_label(d: &Due, local: &TimeZone, today: jiff::civil::Date) -> String {
    let (date, time) = if d.all_day {
        (d.date(), None)
    } else {
        let z = d.instant(local).to_zoned(local.clone());
        (z.date(), Some(z.time()))
    };
    // The wall clock as it is shown, in a GLib date of no zone of its own:
    // the formats read its fields and never convert it.
    let wall = glib::DateTime::from_utc(
        i32::from(date.year()),
        i32::from(date.month()),
        i32::from(date.day()),
        time.map_or(0, |t| i32::from(t.hour())),
        time.map_or(0, |t| i32::from(t.minute())),
        0.0,
    );
    let Ok(wall) = wall else {
        // Out of GLib's range (years 1 to 9999 are both libraries'): as the
        // command line writes it.
        return d.display(local);
    };
    let time = time.map(|_| i18n::format_time_short(&wall));
    let relative = if Some(date) == today.yesterday().ok() {
        Some(-1)
    } else if date == today {
        Some(0)
    } else if Some(date) == today.tomorrow().ok() {
        Some(1)
    } else {
        None
    };
    match (relative, time) {
        (Some(-1), None) => tr("Yesterday"),
        (Some(-1), Some(t)) => tr!("Yesterday, {}", t),
        (Some(0), None) => tr("Today"),
        (Some(0), Some(t)) => tr!("Today, {}", t),
        (Some(_), None) => tr("Tomorrow"),
        (Some(_), Some(t)) => tr!("Tomorrow, {}", t),
        (None, None) => i18n::format_date(&wall),
        (None, Some(t)) => tr!("{}, {}", i18n::format_date(&wall), t),
    }
}

/// The keyboard of a window of the app: Ctrl+W closes it and Ctrl+Q every
/// window (`omui::quit_keys`), and `?` or F1 shows every shortcut. `main`
/// is the reminders' window, whose Ctrl+N, Ctrl+R and F5 are the
/// application's accelerators (main.rs).
fn keys(window: &gtk::ApplicationWindow, main: bool) {
    omui::quit_keys(window);
    omui::ShortcutOverlay::new(move || {
        let window_keys = omui::ShortcutSection::new(
            &tr("Window"),
            &[
                ("Ctrl+W", &tr("Close the window")),
                ("Ctrl+Q", &tr("Quit")),
                ("? / F1", &tr("Show the shortcuts")),
            ],
        );
        if !main {
            return vec![window_keys];
        }
        vec![
            omui::ShortcutSection::new(
                &tr("Reminders"),
                &[
                    ("Ctrl+N", &tr("Add a reminder")),
                    ("Ctrl+R / F5", &tr("Sync now")),
                    ("Enter", &tr("Edit the reminder in focus")),
                    ("Space", &tr("Complete or reopen the reminder whose box is in focus")),
                    ("Escape", &tr("Back to the lists, in a narrow window")),
                ],
            ),
            omui::ShortcutSection::new(
                &tr("Edit Reminder"),
                &[
                    ("Alt+S", &tr("Save")),
                    ("Alt+D", &tr("Delete")),
                    ("Escape", &tr("Close without saving")),
                ],
            ),
            window_keys,
        ]
    })
    .install(window, false);
}

/// The store in `dir`, which is `None` without a home directory.
fn store_in(dir: Option<PathBuf>) -> Result<Store, Error> {
    dir.map(Store::new)
        .ok_or_else(|| Error::Other("no data directory ($HOME is not set)".into()))
}

/// Over omui's stylesheet: an overdue reminder's date, and a field of the
/// edit dialog that cannot be saved as it is (omui's entry row has no look
/// for that).
pub const CSS: &str = "\
row.overdue .row-subtitle { color: @om_danger; }
row.entry-row.error { outline: 2px solid @om_danger; outline-offset: -2px; }
row.entry-row.error .row-title { color: @om_danger; }
";

/// The window once the reminders left iCloud on this computer: where they
/// are now, and a button that opens the folder.
pub fn left(app: &gtk::Application, left: &icloud_app::leave::Left) -> gtk::ApplicationWindow {
    let folder = left.folder.clone();
    let open = gtk::Button::builder().css_classes(["pill", "suggested-action"]).build();
    i18n::bind_label(&open, "Open Folder");
    let page = omui::EmptyState::new(Some(icons::name::CHECKED), "", None);
    page.set_child(Some(open.upcast_ref()));
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .icon_name(super::NAME)
        .default_width(560)
        .default_height(480)
        .child(&page.widget)
        .build();
    i18n::bind(&window, |window| window.set_title(Some(&tr("Reminders"))));
    // The page is the window's child: held by the window's listener, it goes
    // when the window is closed.
    let shown = folder.display().to_string();
    let words = move || {
        page.set_title(&tr("Your Reminders Are Files Now"));
        page.set_description(&tr!(
            "Reminders left iCloud on this computer. Each list is a checklist in {}, which Omacloud keeps in sync on your computers, and due reminders still notify.",
            shown
        ));
    };
    words();
    i18n::on_change(&window, words);
    window.set_titlebar(Some(&omui::header(None)));
    keys(&window, false);
    let parent = window.clone();
    open.connect_clicked(move |_| {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&folder)));
        launcher.launch(Some(&parent), gio::Cancellable::NONE, |_| {});
    });
    window
}

fn build(app: &gtk::Application, banner: SignInBanner, local: TimeZone) -> Window {
    // Sidebar: the lists, under a header of their own.
    let lists = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .build();
    let sidebar_title = omui::Title::new(&tr("Lists"), "");
    let sidebar_header = omui::header(Some(sidebar_title.widget.upcast_ref()));
    let refresh = omui::icon_button(icons::name::REFRESH, "");
    i18n::bind_tooltip(&refresh, "Sync Now (Ctrl+R)");
    refresh.set_action_name(Some("win.refresh"));
    sidebar_header.pack_start(&refresh);
    let sidebar_list = gtk::ScrolledWindow::builder()
        .child(&lists)
        .vexpand(true)
        .css_classes(["sidebar"])
        .build();
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.append(&sidebar_header);
    sidebar.append(&sidebar_list);

    // Content: the add line and the reminders.
    let content_title = omui::Title::new(&tr("Upcoming"), "");
    let content_header = omui::header(Some(content_title.widget.upcast_ref()));
    // Shown while the window is too narrow for both panes.
    let back = omui::icon_button(icons::name::BACK, "");
    i18n::bind_tooltip(&back, "Lists");
    back.set_visible(false);
    content_header.pack_start(&back);
    let show_completed = gtk::ToggleButton::builder()
        .icon_name(icons::name::CHECKED)
        .css_classes(["header-action"])
        .build();
    i18n::bind_tooltip(&show_completed, "Show Completed");
    content_header.pack_end(&show_completed);

    let new_title = gtk::Entry::builder().hexpand(true).build();
    i18n::bind(&new_title, |entry| {
        entry.set_placeholder_text(Some(&tr("New reminder (Ctrl+N)")));
    });
    let new_due = gtk::Entry::builder().width_chars(16).build();
    i18n::bind(&new_due, |entry| {
        entry.set_placeholder_text(Some(&tr("Due: tomorrow 9:00")));
    });
    let add_line = gtk::Box::builder().spacing(6).margin_bottom(12).build();
    add_line.append(&new_title);
    add_line.append(&new_due);

    let rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .valign(gtk::Align::Start)
        .build();
    let empty = omui::EmptyState::new(Some(icons::name::CHECKED), &tr("No Reminders"), None).compact();
    rows.set_placeholder(Some(&empty.widget));
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&add_line);
    column.append(&rows);
    let clamp = omui::clamp(&column, 720);
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&content_header);
    content.append(&gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build());

    let split = omui::Split::new(&sidebar, &content, (200, 280), Some(600));
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
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner.widget);
    split.widget.set_vexpand(true);
    body.append(&split.widget);

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .icon_name(super::NAME)
        .default_width(900)
        .default_height(640)
        .width_request(360)
        .height_request(320)
        .build();
    i18n::bind(&window, |window| window.set_title(Some(&tr("Reminders"))));
    // Each pane has a header of its own.
    omui::claim_titlebar(&window);
    let shell = omui::Shell::new(&window, &body);

    Window {
        window,
        shell,
        banner,
        lists,
        rows,
        content_title,
        sidebar_title,
        empty,
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
