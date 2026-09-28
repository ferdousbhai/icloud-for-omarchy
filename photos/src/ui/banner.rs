//! The sign-in banner, driven by icloud-sessiond's state.
//!
//! A long-lived thread (`transport::watch_sign_in`) reports every change:
//! signed out shows the banner, an open sign-in window shows "Signing in…",
//! and signing back in hides it and syncs. A call that returns
//! `SignInRequired` also shows it. The button asks the daemon to open
//! Apple's sign-in page in its own window and returns at once.

use std::rc::Rc;

use icloud_photos::transport::{self, Error, SignInState};

use super::window::{App, Msg};

impl App {
    /// Starts the watch thread; call once.
    pub(super) fn watch_sign_in(&self) {
        let tx = self.tx.clone();
        transport::watch_sign_in(Box::new(move |s| {
            let _ = tx.send_blocking(Msg::SignIn(s));
        }));
    }

    pub fn show_sign_in_banner(&self) {
        if !self.signing_in.get() {
            self.banner.set_title("iCloud needs you to sign in again");
            self.banner.set_button_label(Some("Sign In"));
        }
        self.banner.set_revealed(true);
    }

    fn show_signing_in(&self) {
        self.signing_in.set(true);
        self.banner.set_title("Signing in… finish in the Apple window that opened");
        self.banner.set_button_label(None);
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
            if let Err(e) = transport::start_sign_in(&notify) {
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
        if !s.signed_in {
            if was_signing_in {
                self.banner.set_title("Sign-in did not finish");
                self.banner.set_button_label(Some("Try Again"));
            }
            self.show_sign_in_banner();
            return;
        }
        // Signed in. Only a banner that is up means something was waiting.
        if self.banner.is_revealed() {
            self.banner.set_revealed(false);
            self.banner.set_button_label(Some("Sign In"));
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
        self.banner.set_title("Could not open the sign-in window");
        self.banner.set_button_label(Some("Try Again"));
        self.banner.set_revealed(true);
    }
}
