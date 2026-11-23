//! The sign-in banner, driven by icloud-sessiond's state.
//!
//! A long-lived thread (`session::watch_sign_in`) reports every change:
//! signed out shows the banner (saying why, when the daemon knows), an
//! open sign-in window shows "Signing in…", and signing back in hides it
//! and syncs. A call that returns
//! `SignInRequired` also shows it. The button asks the daemon to open
//! Apple's sign-in page in its own window and returns at once.
//!
//! This is Photos' own rather than `icloud_app_ui::SignInBanner`: in mock
//! mode (the fake server, `dev/qa`, `dev/preview.sh`) there is no daemon to
//! report the account signed back in, and Photos' `session` module plays it
//! from the fake, which that banner does not know of.
//!
//! What the banner says is kept as a [`BannerView`] and written in the
//! language of the moment, again after each switch.

use std::rc::Rc;

use icloud_photos::session;
use icloud_photos::transport::{Error, SignInState};
use omui::{i18n, tr};

use super::window::{App, Msg};

/// What the banner says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerView {
    /// Signed out; why, as the daemon said it (in English), when it knows.
    SignIn(Option<String>),
    /// The sign-in window is open.
    SigningIn,
    /// The sign-in window closed without signing in.
    DidNotFinish,
    /// The sign-in window could not be opened.
    CouldNotOpen,
}

impl BannerView {
    /// The title and the button's label, in the language in use.
    pub fn text(&self) -> (String, Option<String>) {
        match self {
            BannerView::SignIn(reason) => {
                let ask = tr("iCloud needs you to sign in again");
                let title = match reason {
                    // Two sentences: why, then what to do about it.
                    Some(reason) => omui::tr!("{}. {}", i18n::translate(reason), ask),
                    None => ask,
                };
                (title, Some(tr("Sign In")))
            }
            BannerView::SigningIn => (tr("Signing in… finish in the Apple window that opened"), None),
            BannerView::DidNotFinish => (tr("Sign-in did not finish"), Some(tr("Try Again"))),
            BannerView::CouldNotOpen => (tr("Could not open the sign-in window"), Some(tr("Try Again"))),
        }
    }
}

impl App {
    /// Starts the watch thread; call once.
    pub(super) fn watch_sign_in(&self) {
        let tx = self.tx.clone();
        session::watch_sign_in(Box::new(move |s| {
            let _ = tx.send_blocking(Msg::SignIn(s));
        }));
    }

    /// Shows `view` on the banner.
    pub(super) fn set_banner(&self, view: BannerView) {
        self.banner_view.replace(view);
        self.render_banner();
    }

    /// Writes the banner's view in the language of the moment.
    pub(super) fn render_banner(&self) {
        let (title, button) = self.banner_view.borrow().text();
        self.banner.set_title(&title);
        self.banner.set_button_label(button.as_deref());
    }

    pub fn show_sign_in_banner(&self) {
        // A failed sign-in keeps saying so until the next try.
        let keep = self.signing_in.get() || *self.banner_view.borrow() == BannerView::DidNotFinish;
        if !keep {
            self.set_banner(BannerView::SignIn(self.sign_out_reason.borrow().clone()));
        }
        self.banner.set_revealed(true);
    }

    fn show_signing_in(&self) {
        self.signing_in.set(true);
        self.set_banner(BannerView::SigningIn);
        self.banner.set_revealed(true);
    }

    /// The banner's button: ask for the sign-in window off the main loop.
    pub fn sign_in(self: &Rc<Self>) {
        if self.signing_in.get() {
            return;
        }
        self.show_signing_in();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let notify = |s| {
                let _ = tx.send_blocking(Msg::SignIn(s));
            };
            if let Err(e) = session::start_sign_in(&notify) {
                let _ = tx.send_blocking(Msg::SignInFailed(e));
            }
        });
    }

    pub(super) fn on_sign_in_state(self: &Rc<Self>, s: SignInState) {
        if s.signing_in {
            self.show_signing_in();
            return;
        }
        let was_signing_in = self.signing_in.replace(false);
        self.sign_out_reason.replace(s.sign_out_reason);
        if !s.signed_in {
            if was_signing_in {
                self.set_banner(BannerView::DidNotFinish);
            } else {
                self.set_banner(BannerView::SignIn(self.sign_out_reason.borrow().clone()));
            }
            self.show_sign_in_banner();
            return;
        }
        // Signed in. Only a banner that is up means something was waiting.
        if self.banner.is_revealed() {
            self.banner.set_revealed(false);
            self.set_banner(BannerView::SignIn(None));
            if self.transport().is_some() {
                self.sync();
            } else {
                self.load_transport();
            }
        }
    }

    pub(super) fn on_sign_in_failed(&self, e: &Error) {
        self.signing_in.set(false);
        eprintln!("icloud-photos: sign-in: {e}");
        self.set_banner(BannerView::CouldNotOpen);
        self.banner.set_revealed(true);
    }
}

#[cfg(test)]
mod tests {
    use super::BannerView;

    #[test]
    fn the_banner_says_why_then_what_to_do() {
        let (title, button) = BannerView::SignIn(Some("Signed out of iCloud".into())).text();
        assert_eq!(title, "Signed out of iCloud. iCloud needs you to sign in again");
        assert_eq!(button.as_deref(), Some("Sign In"));
        assert_eq!(BannerView::SigningIn.text().1, None);
        assert_eq!(BannerView::DidNotFinish.text().1.as_deref(), Some("Try Again"));
    }
}
