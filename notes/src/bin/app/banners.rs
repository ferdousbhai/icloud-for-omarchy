//! The banners above the panes: a vault not linked to iCloud yet (or no
//! engine to sync it with), a vault an older release left elsewhere, a push
//! refused because most of the notes' files are gone, and the iCloud
//! sign-in when it expired, is too short to last, or lapses within days.
//! A vault that left iCloud for Omacloud is plain files: none of the iCloud
//! ones show.

use std::rc::Rc;

use gtk::prelude::*;
use omui::icons::name;

use crate::dialogs;
use crate::window::Ui;

/// Days left from which the sign-in banner warns.
const LAPSING_DAYS: i64 = 5;

const HOW_TO: &str = "In Apple's window, use your Apple ID and password (not the iPhone QR code), tick Keep me signed in, and click Trust.";

pub struct Banners {
    pub widget: gtk::Box,
    link: omui::Banner,
    link_icon: gtk::Image,
    clone: gtk::Button,
    vault: omui::Banner,
    deletions: omui::Banner,
    show_them: gtk::Button,
    delete_them: gtk::Button,
    sign_in: omui::Banner,
    sign_in_button: gtk::Button,
}

/// A banner with a glyph before its text. Its buttons are added after it
/// (omui's own button can be neither the suggested one nor disabled).
fn banner(icon: &str, class: &str) -> (omui::Banner, gtk::Image) {
    let banner = omui::Banner::new("", None);
    let glyph = omui::icon(icon);
    glyph.add_css_class(class);
    if let Some(content) = banner.widget.child().and_downcast::<gtk::Box>() {
        content.prepend(&glyph);
    }
    (banner, glyph)
}

impl Banners {
    pub fn new() -> Banners {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let (link, link_icon) = banner(name::CLOUD, "accent");
        let clone = link.add_button("Clone…", true);
        let (vault, _) = banner(name::WARNING, "warning");
        let (deletions, _) = banner(name::WARNING, "warning");
        let show_them = deletions.add_button("Show them", false);
        let delete_them = deletions.add_button("Delete them…", false);
        let (sign_in, _) = banner(name::WARNING, "warning");
        let sign_in_button = sign_in.add_button("Sign in", true);
        for banner in [&link, &vault, &deletions, &sign_in] {
            widget.append(&banner.widget);
        }
        Banners {
            widget,
            link,
            link_icon,
            clone,
            vault,
            deletions,
            show_them,
            delete_them,
            sign_in,
            sign_in_button,
        }
    }

    /// What the banners' buttons do.
    pub fn connect(&self, ui: &Rc<Ui>) {
        let weak = Rc::downgrade(ui);
        self.clone.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                dialogs::onboarding(&ui);
            }
        });
        let weak = Rc::downgrade(ui);
        self.show_them.connect_clicked(move |_| {
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
        self.sign_in_button.connect_clicked(move |_| {
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
            banners
                .link
                .set_title("This folder is not linked to iCloud yet. Clone to download your Apple Notes.");
        } else {
            banners.link.set_title(
                "The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes (sudo pacman -S icloud-notes) and restart to enable sync.",
            );
        }
        banners
            .link_icon
            .set_icon_name(Some(if engine { name::CLOUD } else { name::WARNING }));
        omui::set_class(&banners.link_icon, "accent", engine);
        omui::set_class(&banners.link_icon, "warning", !engine);
        banners.clone.set_visible(engine);
        banners.clone.set_sensitive(!syncing);
        banners.show_them.set_sensitive(!syncing);
        banners.delete_them.set_sensitive(!syncing);

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
        let signing_in = self.signing_in.get();
        banners
            .sign_in_button
            .set_label(if signing_in { "Signing in…" } else { "Sign in" });
        banners.sign_in_button.set_sensitive(!signing_in);
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
