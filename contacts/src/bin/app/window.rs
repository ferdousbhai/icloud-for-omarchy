//! The main window: every contact in a searchable sidebar (sorted by last
//! name), the chosen one's card on the right, and one dialog to add or edit
//! a contact. Phone numbers copy on a click, emails and links open. Syncs on
//! opening and every [`SYNC_SECS`] while shown.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::background;
use super::banner::SignInBanner;
use icloud_contacts::api::{Error, SessionTransport};
use icloud_contacts::model::{self, Change, Contact, Field, Multi};
use icloud_contacts::service::Service;
use icloud_contacts::store::{Cache, Store};

const SYNC_SECS: u32 = 300;

pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: SignInBanner,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    card: gtk::Box,
    card_stack: gtk::Stack,
    split: adw::NavigationSplitView,
    edit_button: gtk::Button,
    delete_button: gtk::Button,
    transport: Arc<SessionTransport>,
    dir: Option<PathBuf>,
    cache: RefCell<Cache>,
    /// The chosen contact's id; empty for none.
    selected: RefCell<String>,
    /// The ids the search matches; `None` without a search.
    visible: RefCell<Option<HashSet<String>>>,
    syncing: Cell<bool>,
    last_error: RefCell<Option<String>>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
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
            build(app, banner)
        });
        let keep_alive = RefCell::new(Some(this.clone()));
        this.window.connect_close_request(move |_| {
            keep_alive.borrow_mut().take();
            glib::Propagation::Proceed
        });
        this.connect();
        if let Some(store) = this.dir.clone().map(Store::new) {
            match store.cache() {
                Ok(cache) => *this.cache.borrow_mut() = cache,
                Err(e) => this.error(&e.to_string()),
            }
        }
        this.show_list();
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

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            let id = row.widget_name().to_string();
            if *this.selected.borrow() != id {
                *this.selected.borrow_mut() = id;
                this.show_card();
            }
        });
        // Only a click (or Enter) opens the card in the narrow layout:
        // show_list re-selects a row after every sync and write.
        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.split.set_show_content(true);
            }
        });
        // The rows are built once per cache; a search only filters them.
        let weak = Rc::downgrade(self);
        self.list.set_filter_func(move |row| {
            weak.upgrade().is_none_or(|this| {
                this.visible
                    .borrow()
                    .as_ref()
                    .is_none_or(|ids| ids.contains(row.widget_name().as_str()))
            })
        });
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.follow_search();
            }
        });
        // Enter in the search box opens the first match.
        let weak = Rc::downgrade(self);
        self.search.connect_activate(move |_| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(row) = this.first_visible() {
                this.list.select_row(Some(&row));
                this.split.set_show_content(true);
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
        action("new", |this| this.edit(None));
        action("find", |this| {
            this.split.set_show_content(false);
            this.search.grab_focus();
        });
        action("edit", |this| {
            if let Some(c) = this.chosen() {
                this.edit(Some(c));
            }
        });
        action("delete", |this| {
            if let Some(c) = this.chosen() {
                this.confirm_delete(c);
            }
        });
    }

    fn chosen(&self) -> Option<Contact> {
        self.cache.borrow().get(&self.selected.borrow()).cloned()
    }

    /// Syncs on a worker thread, then shows the cache.
    fn sync(self: &Rc<Self>) {
        if self.syncing.replace(true) {
            return;
        }
        let (t, dir) = (self.transport.clone(), self.dir.clone());
        let weak = Rc::downgrade(self);
        background(
            move || service(&t, dir)?.sync(),
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.syncing.set(false);
                match result {
                    Ok(Ok((report, cache))) => {
                        this.banner.hide();
                        *this.cache.borrow_mut() = cache;
                        *this.last_error.borrow_mut() = None;
                        // Most syncs change nothing: the rows stay.
                        if report.changed > 0 {
                            this.show_list();
                        }
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
        f: impl FnOnce(&Service) -> icloud_contacts::api::Result<T> + Send + 'static,
        done: impl FnOnce(&Rc<Self>, T) + 'static,
        failed: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        let (t, dir) = (self.transport.clone(), self.dir.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let svc = service(&t, dir)?;
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
                        done(&this, out);
                        this.show_list();
                    }
                    Ok(Err(Error::SignInRequired)) => {
                        this.banner.show();
                        failed(&this);
                    }
                    Ok(Err(e)) => {
                        this.toast(&e.to_string());
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

    fn confirm_delete(self: &Rc<Self>, c: Contact) {
        let name = c.display_name();
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete {name}?")),
            Some("The contact is deleted on all your devices. iCloud keeps no Recently Deleted for contacts."),
        );
        dialog.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(Some("delete"), move |_, _| {
            let Some(this) = weak.upgrade() else { return };
            let (c, name) = (c.clone(), name.clone());
            this.write(
                move |svc| svc.delete(&c),
                move |this, saved| {
                    this.selected.borrow_mut().clear();
                    this.split.set_show_content(false);
                    this.toast(&saved.warning.unwrap_or_else(|| format!("Deleted {name}")));
                },
                |_| {},
            );
        });
        dialog.present(Some(&self.window));
    }

    /// The add (`None`) or edit dialog.
    fn edit(self: &Rc<Self>, contact: Option<Contact>) {
        let draft = contact.as_ref().map(Draft::of).unwrap_or_default();
        self.edit_draft(contact, draft);
    }

    /// The dialog filled from `draft`: the contact's values, or what was
    /// typed before a save that failed.
    fn edit_draft(self: &Rc<Self>, contact: Option<Contact>, draft: Draft) {
        let heading = if contact.is_some() {
            "Edit Contact"
        } else {
            "New Contact"
        };
        let dialog = adw::AlertDialog::new(Some(heading), None);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let group = |rows: &[&adw::EntryRow]| {
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            for r in rows {
                list.append(*r);
            }
            body.append(&list);
            list
        };
        let entry = |title: &str, text: &str| adw::EntryRow::builder().title(title).text(text).build();
        let scalars: Vec<(Field, adw::EntryRow)> = SCALARS
            .iter()
            .map(|(f, title)| (*f, entry(title, draft.scalar(*f))))
            .collect();
        group(&scalars[..3].iter().map(|(_, r)| r).collect::<Vec<_>>());
        group(&scalars[3..].iter().map(|(_, r)| r).collect::<Vec<_>>());
        // One row per value, titled by its label, and an empty one to add.
        let mut multis: Vec<(Multi, Vec<(String, adw::EntryRow)>)> = Vec::new();
        for (m, add_title) in [
            (Multi::Phone, "Add phone (work=+1 555 0100)"),
            (Multi::Email, "Add email"),
            (Multi::Url, "Add URL"),
        ] {
            let mut rows: Vec<(String, adw::EntryRow)> = draft
                .values(m)
                .iter()
                .map(|(label, value)| (label.clone(), entry(&model::label_display(label), value)))
                .collect();
            rows.push((String::new(), entry(add_title, "")));
            group(&rows.iter().map(|(_, r)| r).collect::<Vec<_>>());
            multis.push((m, rows));
        }
        let notes = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        notes.buffer().set_text(&draft.notes);
        body.append(
            &gtk::Frame::builder()
                .child(
                    &gtk::ScrolledWindow::builder()
                        .child(&notes)
                        .min_content_height(70)
                        .build(),
                )
                .build(),
        );
        let problem = gtk::Label::builder()
            .css_classes(["error", "caption"])
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .build();
        body.append(&problem);
        dialog.set_extra_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&body)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(520)
                .build(),
        ));
        dialog.add_responses(&[("cancel", "_Cancel"), ("save", "_Save")]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let scalars = Rc::new(scalars);
        let multis = Rc::new(multis);
        let read = Rc::new({
            let (scalars, multis, notes) = (scalars.clone(), multis.clone(), notes.clone());
            move || {
                let buffer = notes.buffer();
                Draft {
                    scalars: scalars.iter().map(|(f, r)| (*f, r.text().to_string())).collect(),
                    multis: multis
                        .iter()
                        .map(|(m, rows)| {
                            let values = rows
                                .iter()
                                .map(|(label, r)| {
                                    let text = r.text().to_string();
                                    if label.is_empty() {
                                        let l = model::parse_labeled(&text, m.default_label());
                                        (l.label, l.value)
                                    } else {
                                        (label.clone(), text.trim().to_owned())
                                    }
                                })
                                .filter(|(_, v)| !v.is_empty())
                                .collect();
                            (*m, values)
                        })
                        .collect(),
                    notes: buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string(),
                }
            }
        });
        // Save is enabled only while the draft is valid, so a mistake never
        // closes the dialog and loses the edits.
        let validate = Rc::new({
            let (dialog, problem, read) = (dialog.clone(), problem.clone(), read.clone());
            move || {
                let text = read().problem();
                problem.set_visible(text.is_some());
                problem.set_label(text.as_deref().unwrap_or(""));
                dialog.set_response_enabled("save", text.is_none());
            }
        });
        for (_, r) in scalars.iter() {
            let validate = validate.clone();
            r.connect_changed(move |_| validate());
        }
        for (_, rows) in multis.iter() {
            for (_, r) in rows {
                let validate = validate.clone();
                r.connect_changed(move |_| validate());
            }
        }
        validate();

        let before = contact.as_ref().map(Draft::of).unwrap_or_default();
        let weak = Rc::downgrade(self);
        dialog.connect_response(Some("save"), move |_, _| {
            let Some(this) = weak.upgrade() else { return };
            let typed = read();
            let changes = typed.changes(&before);
            let retry = {
                let (contact, typed) = (contact.clone(), typed.clone());
                move |this: &Rc<Window>| this.edit_draft(contact, typed)
            };
            match contact.clone() {
                None => this.write(
                    move |svc| svc.add(&changes, false),
                    |this, saved| {
                        *this.selected.borrow_mut() = saved.value.id().to_owned();
                        this.toast(
                            &saved
                                .warning
                                .unwrap_or_else(|| format!("Added {}", saved.value.display_name())),
                        );
                    },
                    retry,
                ),
                Some(_) if changes.is_empty() => {}
                Some(c) => this.write(
                    move |svc| svc.update(&c, &changes),
                    |this, saved| {
                        if let Some(w) = saved.warning {
                            this.toast(&w);
                        }
                    },
                    retry,
                ),
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Rebuilds the sidebar from the cache (keeping the selection), then
    /// the card.
    fn show_list(self: &Rc<Self>) {
        let cache = self.cache.borrow();
        let selected = self.selected.borrow().clone();
        self.list.remove_all();
        for c in &cache.contacts {
            let row = contact_row(c, cache.is_me(c));
            self.list.append(&row);
            if c.id() == selected {
                self.list.select_row(Some(&row));
            }
        }
        drop(cache);
        self.follow_search();
        self.show_card();
    }

    /// Filters the rows by the search text. In the wide layout a contact
    /// is always shown, as Contacts does: the first match when the chosen
    /// one is filtered out.
    fn follow_search(self: &Rc<Self>) {
        let query = self.search.text();
        *self.visible.borrow_mut() = (!query.trim().is_empty()).then(|| {
            let cache = self.cache.borrow();
            cache
                .contacts
                .iter()
                .filter(|c| c.matches(&query))
                .map(|c| c.id().to_owned())
                .collect()
        });
        self.list.invalidate_filter();
        let shown = self.list.selected_row().is_some_and(|r| r.is_child_visible());
        if !shown && !self.split.is_collapsed() {
            let first = self.first_visible();
            *self.selected.borrow_mut() = first.as_ref().map(|r| r.widget_name().to_string()).unwrap_or_default();
            self.list.select_row(first.as_ref());
            self.show_card();
        }
    }

    fn first_visible(&self) -> Option<gtk::ListBoxRow> {
        (0..)
            .map_while(|i| self.list.row_at_index(i))
            .find(|r| r.is_child_visible())
    }

    fn show_card(self: &Rc<Self>) {
        let chosen = self.chosen();
        self.edit_button.set_sensitive(chosen.is_some());
        self.delete_button.set_sensitive(chosen.is_some());
        let Some(c) = chosen else {
            self.card_stack.set_visible_child_name("empty");
            return;
        };
        self.card_stack.set_visible_child_name("card");
        while let Some(child) = self.card.first_child() {
            self.card.remove(&child);
        }
        let name = c.display_name();
        let avatar = adw::Avatar::new(96, Some(&name), true);
        avatar.set_margin_top(12);
        self.card.append(&avatar);
        self.card.append(
            &gtk::Label::builder()
                .label(&name)
                .css_classes(["title-1"])
                .wrap(true)
                .justify(gtk::Justification::Center)
                .selectable(true)
                .build(),
        );
        let job = c.work(" · ");
        let nickname = c.field(Field::Nickname);
        for line in [job.as_str(), nickname] {
            if !line.is_empty() {
                self.card.append(
                    &gtk::Label::builder()
                        .label(line)
                        .css_classes(["dim-label"])
                        .wrap(true)
                        .selectable(true)
                        .build(),
                );
            }
        }
        let group = |rows: Vec<adw::ActionRow>| {
            if rows.is_empty() {
                return;
            }
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .margin_top(6)
                .build();
            for r in rows {
                list.append(&r);
            }
            self.card.append(&list);
        };
        let action = |m: Multi, v: &str| match m {
            Multi::Phone => Action::Copy,
            Multi::Email => Action::Open(format!("mailto:{v}")),
            Multi::Url if v.contains("://") => Action::Open(v.to_owned()),
            Multi::Url => Action::Open(format!("https://{v}")),
        };
        for m in [Multi::Phone, Multi::Email, Multi::Url] {
            let rows = c
                .values(m)
                .into_iter()
                .map(|l| self.value_row(&model::label_display(&l.label), &l.value, action(m, &l.value)))
                .collect();
            group(rows);
        }
        let addresses = c
            .addresses()
            .into_iter()
            .map(|a| {
                let city = [a.postal_code.as_str(), a.city.as_str(), a.state.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let text = [a.street.clone(), city, a.country.clone()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.value_row(&model::label_display(&a.label), &text, Action::Copy)
            })
            .collect();
        group(addresses);
        let birthday = c.field(Field::Birthday);
        if !birthday.is_empty() {
            group(vec![self.value_row(
                "birthday",
                &model::birthday_display(birthday),
                Action::Copy,
            )]);
        }
        let notes = c.field(Field::Notes);
        if !notes.is_empty() {
            group(vec![self.value_row("notes", notes, Action::Copy)]);
        }
    }

    /// A row showing `value` under `label`; a click copies or opens it.
    fn value_row(self: &Rc<Self>, label: &str, value: &str, action: Action) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(label)
            .subtitle(value)
            .use_markup(false)
            .activatable(true)
            .css_classes(["property"])
            .build();
        row.set_subtitle_selectable(true);
        let (icon, tip) = match &action {
            Action::Copy => ("edit-copy-symbolic", "Copy"),
            Action::Open(_) => ("adw-external-link-symbolic", "Open"),
        };
        row.add_suffix(&gtk::Image::builder().icon_name(icon).tooltip_text(tip).build());
        let weak = Rc::downgrade(self);
        let value = value.to_owned();
        row.connect_activated(move |_| {
            let Some(this) = weak.upgrade() else { return };
            match &action {
                Action::Copy => {
                    this.window.clipboard().set_text(&value);
                    this.toast("Copied");
                }
                Action::Open(uri) => {
                    let weak = Rc::downgrade(&this);
                    gtk::UriLauncher::new(uri).launch(Some(&this.window), gio::Cancellable::NONE, move |r| {
                        if let (Err(e), Some(this)) = (r, weak.upgrade()) {
                            this.toast(&format!("Cannot open it: {e}"));
                        }
                    });
                }
            }
        });
        row
    }
}

enum Action {
    Copy,
    Open(String),
}

/// The dialog's single-line fields, in the order shown (two groups of
/// three, then the rest).
const SCALARS: &[(Field, &str)] = &[
    (Field::First, "First name"),
    (Field::Last, "Last name"),
    (Field::Company, "Company"),
    (Field::JobTitle, "Job title"),
    (Field::Nickname, "Nickname"),
    (Field::Birthday, "Birthday (1990-05-01, or 05-01)"),
];

/// The dialog's fields as typed: values stored as iCloud stores them
/// (birthday excepted: as typed).
#[derive(Debug, Clone, Default)]
struct Draft {
    scalars: Vec<(Field, String)>,
    multis: Vec<(Multi, Vec<(String, String)>)>,
    notes: String,
}

impl Draft {
    fn of(c: &Contact) -> Draft {
        Draft {
            scalars: SCALARS
                .iter()
                .map(|(f, _)| {
                    let v = c.field(*f);
                    (
                        *f,
                        if *f == Field::Birthday {
                            model::birthday_display(v)
                        } else {
                            v.to_owned()
                        },
                    )
                })
                .collect(),
            multis: [Multi::Phone, Multi::Email, Multi::Url]
                .into_iter()
                .map(|m| (m, c.values(m).into_iter().map(|l| (l.label, l.value)).collect()))
                .collect(),
            notes: c.field(Field::Notes).to_owned(),
        }
    }

    fn scalar(&self, f: Field) -> &str {
        self.scalars.iter().find(|(g, _)| *g == f).map_or("", |(_, v)| v)
    }

    fn values(&self, m: Multi) -> &[(String, String)] {
        self.multis.iter().find(|(n, _)| *n == m).map_or(&[], |(_, v)| v)
    }

    /// What is wrong with it, while something is.
    fn problem(&self) -> Option<String> {
        let birthday = self.scalar(Field::Birthday);
        if !birthday.trim().is_empty()
            && let Err(e) = model::parse_birthday(birthday)
        {
            return Some(e);
        }
        for (m, values) in &self.multis {
            for (_, v) in values {
                if let Err(e) = m.check(v) {
                    return Some(e);
                }
            }
        }
        (!model::names_someone(&self.changes(&Draft::default())))
            .then(|| "A contact needs a name, a company, a phone number or an email.".into())
    }

    /// The changes from `before` to this draft.
    fn changes(&self, before: &Draft) -> Vec<Change> {
        let mut out = Vec::new();
        for (f, v) in &self.scalars {
            let v = v.trim();
            if v != before.scalar(*f).trim() {
                let v = if *f == Field::Birthday && !v.is_empty() {
                    model::parse_birthday(v).unwrap_or_else(|_| v.to_owned())
                } else {
                    v.to_owned()
                };
                out.push(Change::Set(*f, v));
            }
        }
        if self.notes != before.notes {
            out.push(Change::Set(Field::Notes, self.notes.clone()));
        }
        for (m, now) in &self.multis {
            let was = before.values(*m);
            for (label, v) in was {
                if !now.iter().any(|(l, n)| l == label && n == v) {
                    out.push(Change::Remove(*m, v.clone()));
                }
            }
            for (label, v) in now {
                if !was.iter().any(|(l, w)| l == label && w == v) {
                    out.push(Change::Add(
                        *m,
                        model::Labeled {
                            label: label.clone(),
                            value: v.clone(),
                        },
                    ));
                }
            }
        }
        out
    }
}

fn contact_row(c: &Contact, me: bool) -> gtk::ListBoxRow {
    let name = c.display_name();
    let b = gtk::Box::builder().spacing(10).margin_top(4).margin_bottom(4).build();
    b.append(&adw::Avatar::new(32, Some(&name), true));
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    text.append(
        &gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    let company = c.field(Field::Company);
    let sub = if me {
        "My Card"
    } else if c.is_company() {
        ""
    } else {
        company
    };
    if !sub.is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(sub)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    b.append(&text);
    let row = gtk::ListBoxRow::builder().child(&b).build();
    row.set_widget_name(c.id());
    row
}

fn build(app: &adw::Application, banner: SignInBanner) -> Window {
    // Sidebar: search and the contacts.
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search (Ctrl+F)")
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(6)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .css_classes(["navigation-sidebar"])
        .build();
    list.set_placeholder(Some(
        &adw::StatusPage::builder()
            .icon_name("avatar-default-symbolic")
            .title("No Contacts")
            .css_classes(["compact"])
            .build(),
    ));
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.pack_start(
        &gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Sync Now (Ctrl+R)")
            .action_name("win.refresh")
            .build(),
    );
    sidebar_header.pack_end(
        &gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Contact (Ctrl+N)")
            .action_name("win.new")
            .build(),
    );
    let sidebar_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_body.append(&search);
    sidebar_body.append(&gtk::ScrolledWindow::builder().child(&list).vexpand(true).build());
    let sidebar_tv = adw::ToolbarView::new();
    sidebar_tv.add_top_bar(&sidebar_header);
    sidebar_tv.set_content(Some(&sidebar_body));
    let sidebar_page = adw::NavigationPage::builder()
        .title("Contacts")
        .child(&sidebar_tv)
        .build();

    // Content: the chosen card.
    let edit_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Edit (Ctrl+E)")
        .action_name("win.edit")
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Delete")
        .action_name("win.delete")
        .build();
    let content_header = adw::HeaderBar::builder().show_title(false).build();
    content_header.pack_end(&delete_button);
    content_header.pack_end(&edit_button);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let clamp = adw::Clamp::builder()
        .maximum_size(560)
        .child(&card)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    let card_stack = gtk::Stack::new();
    card_stack.add_named(
        &gtk::ScrolledWindow::builder().child(&clamp).vexpand(true).build(),
        Some("card"),
    );
    card_stack.add_named(
        &adw::StatusPage::builder()
            .icon_name("avatar-default-symbolic")
            .title("No Contact Selected")
            .build(),
        Some("empty"),
    );
    let content_tv = adw::ToolbarView::new();
    content_tv.add_top_bar(&content_header);
    content_tv.set_content(Some(&card_stack));
    let content_page = adw::NavigationPage::builder()
        .title("Contact")
        .child(&content_tv)
        .build();

    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&content_page)
        .min_sidebar_width(240.0)
        .max_sidebar_width(320.0)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner.widget);
    split.set_vexpand(true);
    body.append(&split);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Contacts")
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
        search,
        list,
        card,
        card_stack,
        split,
        edit_button,
        delete_button,
        transport: Arc::default(),
        dir: Store::default_dir(),
        cache: RefCell::default(),
        selected: RefCell::default(),
        visible: RefCell::default(),
        syncing: Cell::new(false),
        last_error: RefCell::default(),
    }
}

/// The service over this window's transport and data directory.
fn service(t: &SessionTransport, dir: Option<PathBuf>) -> icloud_contacts::api::Result<Service<'_>> {
    let dir = dir.ok_or_else(|| Error::Other("no data directory ($HOME is not set)".into()))?;
    Ok(Service::new(t, Store::new(dir)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_draft_changes_only_what_was_edited() {
        let c = Contact(
            json!({
                "contactId": "A", "firstName": "Ann", "birthday": "1604-05-01",
                "phones": [{"label": "MOBILE", "field": "+1 555"}, {"label": "WORK", "field": "+1 666"}],
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let before = Draft::of(&c);
        assert_eq!(before.scalar(Field::Birthday), "05-01");
        assert!(before.changes(&before).is_empty());
        let mut after = before.clone();
        after.scalars.iter_mut().find(|(f, _)| *f == Field::Last).unwrap().1 = "Lee".into();
        after.multis[0].1 = vec![("MOBILE".into(), "+1 555".into()), ("HOME".into(), "+1 777".into())];
        let changes = after.changes(&before);
        assert_eq!(
            changes,
            [
                Change::Set(Field::Last, "Lee".into()),
                Change::Remove(Multi::Phone, "+1 666".into()),
                Change::Add(
                    Multi::Phone,
                    model::Labeled {
                        label: "HOME".into(),
                        value: "+1 777".into()
                    }
                ),
            ]
        );
        assert!(Draft::default().problem().is_some());
        assert_eq!(after.problem(), None);
        // A valid phone does not hide an invalid email after it.
        let mut bad = after.clone();
        bad.multis[1].1 = vec![("HOME".into(), "nope".into())];
        assert_eq!(bad.problem().as_deref(), Some("\"nope\" is not an email address"));
    }
}
