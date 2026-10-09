//! The "sign in" banner. It follows `icloud-sessiond`'s status through
//! `icloud_session::watch_forever()` on one background thread: shown while
//! signed out, "Signing in…" while the daemon's sign-in window is open,
//! hidden (and the window synced) once the account is signed in. Its button
//! calls `icloud_session::sign_in()`, which only asks the daemon to open the
//! window. (Find My's banner, without its password page.)

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;

const TITLE: &str = "Sign in to iCloud to see your reminders";
const SIGNING_IN: &str = "Signing in…";
const BUTTON: &str = "Sign In";

#[derive(Debug, Default)]
struct State {
    status: Option<icloud_session::Status>,
    /// A request answered `SignInRequired` (the status may lag behind).
    sign_in_needed: bool,
}

impl State {
    /// `None` hidden, else the title and whether the button shows.
    fn view(&self) -> Option<(&'static str, bool)> {
        let signed_in = self.status.as_ref().map(|s| s.signed_in);
        if self.status.as_ref().is_some_and(|s| s.signing_in) {
            return Some((SIGNING_IN, false));
        }
        (signed_in == Some(false) || self.sign_in_needed).then_some((TITLE, true))
    }

    /// Takes a new status; true when the account just signed in.
    fn update(&mut self, status: icloud_session::Status) -> bool {
        let before = self.status.replace(status.clone());
        let was_signed_in = before.as_ref().map(|s| s.signed_in);
        let window_closed = before.as_ref().is_some_and(|s| s.signing_in) && !status.signing_in;
        if status.signed_in && (was_signed_in == Some(false) || (self.sign_in_needed && window_closed)) {
            self.sign_in_needed = false;
            return true;
        }
        false
    }
}

pub struct SignInBanner {
    pub widget: adw::Banner,
    state: Rc<RefCell<State>>,
}

impl SignInBanner {
    /// `on_signed_in` runs when the account turns signed in after being
    /// signed out; `on_error` gets a message for a toast.
    pub fn new(on_signed_in: impl Fn() + 'static, on_error: impl Fn(String) + 'static) -> Self {
        let widget = adw::Banner::builder()
            .title(TITLE)
            .button_label(BUTTON)
            .button_style(adw::BannerButtonStyle::Suggested)
            .revealed(false)
            .build();
        let state = Rc::new(RefCell::new(State::default()));
        let on_error = Rc::new(on_error);
        widget.connect_button_clicked(move |_| {
            let on_error = on_error.clone();
            super::background(icloud_session::sign_in, move |result| {
                let msg = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => format!("Could not start signing in: {e}"),
                    Err(e) => e,
                };
                on_error(msg);
            });
        });

        let (tx, rx) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("icloud-session-watch".into())
            .spawn(move || icloud_session::watch_forever(|status| tx.send_blocking(status).is_ok()))
            .expect("spawn the sign-in status thread");
        let this = Self { widget, state };
        let (banner, state) = (this.widget.clone(), this.state.clone());
        glib::spawn_future_local(async move {
            while let Ok(status) = rx.recv().await {
                let signed_in = state.borrow_mut().update(status);
                render(&banner, &state.borrow());
                if signed_in {
                    on_signed_in();
                }
            }
        });
        this
    }

    /// Shown by the window when a request answers `SignInRequired`.
    pub fn show(&self) {
        self.state.borrow_mut().sign_in_needed = true;
        render(&self.widget, &self.state.borrow());
    }

    /// A request succeeded.
    pub fn hide(&self) {
        self.state.borrow_mut().sign_in_needed = false;
        render(&self.widget, &self.state.borrow());
    }
}

fn render(banner: &adw::Banner, state: &State) {
    match state.view() {
        Some((title, button)) => {
            banner.set_title(title);
            banner.set_button_label(button.then_some(BUTTON));
            banner.set_revealed(true);
        }
        None => banner.set_revealed(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(signed_in: bool, signing_in: bool) -> icloud_session::Status {
        icloud_session::Status {
            signed_in,
            apple_id: None,
            full_name: None,
            dsid: None,
            expires_at: None,
            signing_in,
            find_my_authorized: false,
            find_my_password_stored: false,
        }
    }

    #[test]
    fn signed_out_signing_in_signed_in() {
        let mut state = State::default();
        assert!(!state.update(status(false, false)));
        assert_eq!(state.view(), Some((TITLE, true)));
        state.update(status(false, true));
        assert_eq!(state.view(), Some((SIGNING_IN, false)));
        assert!(state.update(status(true, false)));
        assert_eq!(state.view(), None);
    }

    #[test]
    fn a_refused_request_shows_it_until_the_window_closes_signed_in() {
        let mut state = State::default();
        state.update(status(true, false));
        state.sign_in_needed = true;
        assert_eq!(state.view(), Some((TITLE, true)));
        state.update(status(true, true));
        assert!(state.update(status(true, false)));
        assert_eq!(state.view(), None);
    }
}
