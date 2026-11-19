//! The banners above the panes: a vault not linked to iCloud yet (or no
//! engine to sync it with), a vault an older release left elsewhere, a push
//! refused because most of the notes' files are gone, and the iCloud
//! sign-in when it expired, is too short to last, or lapses within days.
//! A vault that left iCloud for Omacloud is plain files: none of the iCloud
//! ones show.

use std::rc::Rc;

use gtk::prelude::*;

use crate::dialogs;
use crate::window::Ui;

/// Days left from which the sign-in banner warns.
const LAPSING_DAYS: i64 = 5;

const HOW_TO: &str = "In Apple's window, use your Apple ID and password (not the iPhone QR code), tick Keep me signed in, and click Trust.";

pub struct Banners {
    pub widget: gtk::Box,
    link: omui::Banner,
    vault: omui::Banner,
    deletions: omui::Banner,
    sign_in: omui::Banner,
}

impl Banners {
    pub fn new() -> Banners {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let link = omui::Banner::new("", Some("Clone…"));
        let vault = omui::Banner::new("", None);
        let deletions = omui::Banner::new("", Some("Show them"));
        let sign_in = omui::Banner::new("", Some("Sign in"));
        for banner in [&link, &vault, &deletions, &sign_in] {
            widget.append(&banner.widget);
        }
        Banners {
            widget,
            link,
            vault,
            deletions,
            sign_in,
        }
    }

    /// What the banners' buttons do.
    pub fn connect(&self, ui: &Rc<Ui>) {
        let weak = Rc::downgrade(ui);
        self.link.connect_button_clicked(move || {
            if let Some(ui) = weak.upgrade()
                && !ui.backend.borrow().sync_running()
            {
                dialogs::onboarding(&ui);
            }
        });
        let weak = Rc::downgrade(ui);
        self.deletions.connect_button_clicked(move || {
            if let Some(ui) = weak.upgrade()
                && !ui.backend.borrow().sync_running()
            {
                dialogs::sync_status(&ui);
            }
        });
        let delete = self.deletions.add_button("Delete them…", false);
        let weak = Rc::downgrade(ui);
        delete.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade()
                && !ui.backend.borrow().sync_running()
            {
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

        banners.link.set_revealed(!left && (!engine || !cloned));
        if engine {
            banners
                .link
                .set_title("This folder is not linked to iCloud yet. Clone to download your Apple Notes.");
            banners.link.set_button_label(Some("Clone…"));
        } else {
            banners.link.set_title(
                "The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes (sudo pacman -S icloud-notes) and restart to enable sync.",
            );
            banners.link.set_button_label(None);
        }

        let notice = b.vault().notice.clone().unwrap_or_default();
        banners.vault.set_title(&notice);
        banners.vault.set_revealed(!notice.is_empty());

        let refused = self.deletions_refused.borrow().clone();
        banners.deletions.set_title(&format!(
            "{refused} If the files went missing by accident (a folder moved, a disk not mounted), put them back and Notes syncs as before."
        ));
        banners.deletions.set_revealed(!refused.is_empty());

        // -1: a sign-in that lasts hours (a phone QR sign-in never offers
        // "Keep me signed in"); -2: unknown, or nobody signed in.
        let days = b.sign_in_days_left();
        let short = cloned && engine && days == -1;
        let lapsing = (0..=LAPSING_DAYS).contains(&days);
        let expired = b.auth_expired();
        let text = if expired {
            format!(
                "Your iCloud sign-in expired, so syncing is paused. Your edits are safe on this computer and go up once you sign in. {HOW_TO}"
            )
        } else if short {
            format!("This iCloud sign-in only lasts a few hours unused. To stay signed in, sign in again. {HOW_TO}")
        } else {
            let when = match days {
                0 => "today".to_string(),
                1 => "tomorrow".to_string(),
                n => format!("in {n} days"),
            };
            format!("Your iCloud sign-in ends {when}. Sign in again now to keep syncing without a pause.")
        };
        banners.sign_in.set_title(&text);
        banners.sign_in.set_button_label(Some(if self.signing_in.get() {
            "Signing in…"
        } else {
            "Sign in"
        }));
        banners.sign_in.set_revealed(!left && (expired || short || lapsing));
    }

    /// Asks icloud-session to open Apple's sign-in window; syncing resumes
    /// when it reports the sign-in.
    pub fn sign_in(self: &Rc<Self>) {
        if self.signing_in.replace(true) {
            return;
        }
        self.show_banners();
        self.set_status("Sign in to iCloud in the window that opened. Syncing resumes on its own.");
        let weak = Rc::downgrade(self);
        omui::background(icloud_session::sign_in, move |result| {
            let Some(ui) = weak.upgrade() else { return };
            if matches!(result, Ok(Ok(()))) {
                ui.refresh_sign_in();
            } else {
                ui.signing_in.set(false);
                ui.clone_wanted.set(false);
                ui.set_status("Could not open the iCloud sign-in: icloud-session is not available.");
                ui.show_banners();
            }
        });
    }
}
