//! The sign-in banner. Shown whenever a call returns `SignInRequired`; its
//! button runs `Session::reauthenticate` (icloud-md's sign-in window) on a
//! background thread, then retries by syncing.

use std::rc::Rc;

use icloud_photos::transport::{self, Result};

use super::window::{App, Msg};

impl App {
    pub fn show_sign_in_banner(&self) {
        if !self.reauthing.get() {
            self.banner.set_title("iCloud needs you to sign in again");
            self.banner.set_button_label(Some("Sign In"));
        }
        self.banner.set_revealed(true);
    }

    /// The banner's button: run the interactive sign-in off the main loop,
    /// then pick up where we left off.
    pub fn reauthenticate(self: &Rc<Self>) {
        if self.reauthing.replace(true) {
            return;
        }
        self.banner.set_title("Finish signing in to Apple in the window that opened…");
        self.banner.set_button_label(None);
        let (tx, t) = (self.tx.clone(), self.transport());
        std::thread::spawn(move || {
            let result = match t {
                Some(t) => t.reauthenticate(),
                None => transport::sign_in(),
            };
            let _ = tx.send_blocking(Msg::ReauthDone(result));
        });
    }

    pub(super) fn on_reauth_done(self: &Rc<Self>, result: Result<()>) {
        self.reauthing.set(false);
        match result {
            Ok(()) => {
                self.banner.set_revealed(false);
                self.banner.set_button_label(Some("Sign In"));
                if self.transport().is_some() {
                    self.sync();
                } else {
                    self.load_transport();
                }
            }
            Err(e) => {
                self.banner.set_title("Sign-in did not finish");
                self.banner.set_button_label(Some("Try Again"));
                eprintln!("icloud-photos: reauthenticate: {e}");
            }
        }
    }
}
