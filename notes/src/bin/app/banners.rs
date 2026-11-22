//! The banners above the panes: a vault not linked to iCloud yet (or no
//! engine to sync it with), a vault an older release left elsewhere, a push
//! refused because most of the notes' files are gone, and the iCloud
//! sign-in when it expired, is too short to last, or lapses within days.
//! A vault that left iCloud for Omacloud is plain files: none of the iCloud
//! ones show.

use std::rc::Rc;

use gtk::prelude::*;
use omui::icons::name;
use omui::{tr, trn};

use crate::dialogs;
use crate::window::Ui;

/// Days left from which the sign-in banner warns.
const LAPSING_DAYS: i64 = 5;

pub struct Banners {
    pub widget: gtk::Box,
    /// Its own button is Clone….
    link: omui::Banner,
    vault: omui::Banner,
    /// Its own button is Show them.
    deletions: omui::Banner,
    delete_them: gtk::Button,
    /// Its own button is Sign in.
    sign_in: omui::Banner,
}

/// A banner with `icon` before its text, tinted by `class`, and `button` as its own.
fn banner(icon: &str, class: &str, button: Option<&str>) -> omui::Banner {
    let banner = omui::Banner::new("", button);
    banner.set_icon(Some(icon), class);
    banner
}

impl Banners {
    pub fn new() -> Banners {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // Every label is set again as the banners are shown (show_banners).
        let link = banner(name::CLOUD, "accent", Some(""));
        link.set_button_suggested(true);
        let vault = banner(name::WARNING, "warning", None);
        let deletions = banner(name::WARNING, "warning", Some(""));
        let delete_them = deletions.add_button("", false);
        let sign_in = banner(name::WARNING, "warning", Some(""));
        sign_in.set_button_suggested(true);
        for banner in [&link, &vault, &deletions, &sign_in] {
            widget.append(&banner.widget);
        }
        Banners {
            widget,
            link,
            vault,
            deletions,
            delete_them,
            sign_in,
        }
    }

    /// What the banners' buttons do.
    pub fn connect(&self, ui: &Rc<Ui>) {
        let weak = Rc::downgrade(ui);
        self.link.connect_button_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                dialogs::onboarding(&ui);
            }
        });
        let weak = Rc::downgrade(ui);
        self.deletions.connect_button_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                dialogs::sync_status(&ui);
            }
        });
        let weak = Rc::downgrade(ui);
        self.delete_them.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                dialogs::delete_missing(&ui);
            }
        });
        let weak = Rc::downgrade(ui);
        self.sign_in.connect_button_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.sign_in();
            }
        });
    }
}

impl Ui {
    /// Each banner shown or not, with what it says now.
    pub fn show_banners(&self) {
        let banners = &self.banners;
        let b = self.backend.borrow();
        let left = b.vault().left_icloud();
        let engine = b.engine().is_some();
        let cloned = b.vault().cloned();

        let syncing = b.sync_running();
        banners.link.set_revealed(!left && (!engine || !cloned));
        if engine {
            banners.link.set_title(&tr(
                "This folder is not linked to iCloud yet. Clone to download your Apple Notes.",
            ));
        } else {
            banners.link.set_title(&tr(
                "The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes (sudo pacman -S icloud-notes) and restart to enable sync.",
            ));
        }
        if engine {
            banners.link.set_icon(Some(name::CLOUD), "accent");
        } else {
            banners.link.set_icon(Some(name::WARNING), "warning");
        }
        banners.link.set_button_label(engine.then(|| tr("Clone…")).as_deref());
        banners.link.set_button_enabled(!syncing);
        banners.deletions.set_button_enabled(!syncing);
        banners.delete_them.set_sensitive(!syncing);

        // The library's, in English.
        let notice = b.vault().notice.clone().unwrap_or_default();
        banners.vault.set_title(&notice);
        banners.vault.set_revealed(!notice.is_empty());

        let refused = self.deletions_refused.borrow().clone();
        // What the engine refused, in its English, then the window's own words.
        banners.deletions.set_title(&tr!(
            "{} If the files went missing by accident (a folder moved, a disk not mounted), put them back and Notes syncs as before.",
            refused
        ));
        banners.deletions.set_button_label(Some(&tr("Show them")));
        banners.delete_them.set_label(&tr("Delete them…"));
        banners.deletions.set_revealed(!refused.is_empty());

        // -1: a sign-in that lasts hours (a phone QR sign-in never offers
        // "Keep me signed in"); -2: unknown, or nobody signed in.
        let days = b.sign_in_days_left();
        let short = cloned && engine && days == -1;
        let lapsing = (0..=LAPSING_DAYS).contains(&days);
        let expired = b.auth_expired();
        let how_to = || {
            tr(
                "In Apple's window, use your Apple ID and password (not the iPhone QR code), tick Keep me signed in, and click Trust.",
            )
        };
        let text = if expired {
            tr!(
                "Your iCloud sign-in expired, so syncing is paused. Your edits are safe on this computer and go up once you sign in. {}",
                how_to()
            )
        } else if short {
            tr!(
                "This iCloud sign-in only lasts a few hours unused. To stay signed in, sign in again. {}",
                how_to()
            )
        } else {
            match days {
                0 => tr("Your iCloud sign-in ends today. Sign in again now to keep syncing without a pause."),
                1 => tr("Your iCloud sign-in ends tomorrow. Sign in again now to keep syncing without a pause."),
                n => trn!(
                    "Your iCloud sign-in ends in {n} days. Sign in again now to keep syncing without a pause.",
                    n
                ),
            }
        };
        banners.sign_in.set_title(&text);
        let signing_in = self.signing_in.get();
        banners
            .sign_in
            .set_button_label(Some(&if signing_in { tr("Signing in…") } else { tr("Sign in") }));
        banners.sign_in.set_button_enabled(!signing_in);
        banners.sign_in.set_revealed(!left && (expired || short || lapsing));
    }

    /// Asks icloud-session to open Apple's sign-in window; syncing resumes
    /// when it reports the sign-in.
    pub fn sign_in(self: &Rc<Self>) {
        if self.signing_in.replace(true) {
            return;
        }
        self.show_banners();
        self.set_status(|| tr("Sign in to iCloud in the window that opened. Syncing resumes on its own."));
        let weak = Rc::downgrade(self);
        omui::background(icloud_session::sign_in, move |result| {
            let Some(ui) = weak.upgrade() else { return };
            if matches!(result, Ok(Ok(()))) {
                ui.refresh_sign_in();
            } else {
                ui.signing_in.set(false);
                ui.clone_wanted.set(false);
                ui.set_status(|| tr("Could not open the iCloud sign-in: icloud-session is not available."));
                ui.show_banners();
            }
        });
    }
}
