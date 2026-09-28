//! The "sign in" banner shown on `SignInRequired`. Its button runs
//! `Session::reauthenticate` (icloud-md's sign-in window) on a worker thread,
//! then asks the window to retry.

use std::cell::Cell;
use std::rc::Rc;

const TITLE: &str = "Sign in to iCloud to see your devices";
const BUTTON: &str = "Sign In";

pub struct SignInBanner {
    pub widget: adw::Banner,
}

impl SignInBanner {
    /// `on_signed_in` runs after a successful sign-in; `on_error` gets a
    /// message for a toast.
    pub fn new(on_signed_in: impl Fn() + 'static, on_error: impl Fn(String) + 'static) -> Self {
        let widget = adw::Banner::builder()
            .title(TITLE)
            .button_label(BUTTON)
            .button_style(adw::BannerButtonStyle::Suggested)
            .revealed(false)
            .build();
        let running = Rc::new(Cell::new(false));
        let on_signed_in = Rc::new(on_signed_in);
        let on_error = Rc::new(on_error);
        widget.connect_button_clicked(move |banner| {
            if running.replace(true) {
                return;
            }
            banner.set_title("Finish signing in in the Apple window…");
            banner.set_button_label(None);
            let (banner, running) = (banner.clone(), running.clone());
            let (on_signed_in, on_error) = (on_signed_in.clone(), on_error.clone());
            super::background(icloud_session::Session::reauthenticate, move |result| {
                running.set(false);
                banner.set_title(TITLE);
                banner.set_button_label(Some(BUTTON));
                match result {
                    Ok(Ok(())) => {
                        banner.set_revealed(false);
                        on_signed_in();
                    }
                    Ok(Err(e)) => on_error(format!("Sign-in did not finish: {e}")),
                    Err(e) => on_error(e),
                }
            });
        });
        Self { widget }
    }

    pub fn show(&self) {
        self.widget.set_revealed(true);
    }

    pub fn hide(&self) {
        self.widget.set_revealed(false);
    }
}
