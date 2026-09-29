//! The "sign in" banner. It follows `icloud-sessiond`'s status through
//! `icloud_session::watch()` on one background thread: shown while signed
//! out, "Signing in…" while the daemon's sign-in window is open, hidden (and
//! the window refreshed) once the account is signed in. Its button calls
//! `icloud_session::sign_in()`, which only asks the daemon to open the window.
//!
//! Find My can also want the Apple password again (HTTP 450) while the
//! account is signed in. The window then calls [`SignInBanner::show_find_my`]
//! and the banner says so, with a button calling
//! `icloud_session::authorize_find_my()`; "Finish in the Apple window…"
//! while that window is open; hidden (and the window refreshed, locating)
//! once the daemon reports Find My authorized.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;

const TITLE: &str = "Sign in to iCloud to see your devices";
const SIGNING_IN: &str = "Signing in…";
const BUTTON: &str = "Sign In";
const FIND_MY_TITLE: &str = "Find My needs your Apple password";
/// The same when no password is stored for the daemon to use by itself.
const FIND_MY_TITLE_HINT: &str =
    "Find My needs your Apple password (to stop being asked: icloud-session set-password)";
const FIND_MY_WAITING: &str = "Finish in the Apple window…";
const FIND_MY_BUTTON: &str = "Enter Password";

/// How long the watcher waits before reconnecting when the watch ended or
/// could not start (the daemon idle-exited, or the bus was unreachable).
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// What the banner's button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    SignIn,
    AuthorizeFindMy,
}

/// What the banner knows: the daemon's last status and what the window
/// told it.
#[derive(Debug, Default)]
struct State {
    status: Option<icloud_session::Status>,
    /// A request answered `SignInRequired` (the status may lag behind).
    sign_in_needed: bool,
    /// A request answered `FindMyAuthRequired`.
    find_my_needed: bool,
}

/// How the banner looks: `None` hidden, else title, button and its action.
#[derive(Debug, PartialEq, Eq)]
struct View {
    title: &'static str,
    button: Option<(&'static str, Action)>,
}

impl State {
    fn view(&self) -> Option<View> {
        let signed_in = self.status.as_ref().map(|s| s.signed_in);
        let signing_in = self.status.as_ref().is_some_and(|s| s.signing_in);
        let find_my = self.find_my_needed && signed_in != Some(false);
        if signing_in {
            let title = if find_my { FIND_MY_WAITING } else { SIGNING_IN };
            return Some(View {
                title,
                button: None,
            });
        }
        if signed_in == Some(false) || self.sign_in_needed {
            return Some(View {
                title: TITLE,
                button: Some((BUTTON, Action::SignIn)),
            });
        }
        let stored = self
            .status
            .as_ref()
            .is_some_and(|s| s.find_my_password_stored);
        find_my.then_some(View {
            title: if stored {
                FIND_MY_TITLE
            } else {
                FIND_MY_TITLE_HINT
            },
            button: Some((FIND_MY_BUTTON, Action::AuthorizeFindMy)),
        })
    }

    /// Takes a new status. Returns what the window should do about it.
    fn update(&mut self, status: icloud_session::Status) -> Change {
        let before = self.status.replace(status.clone());
        let was_signed_in = before.as_ref().map(|s| s.signed_in);
        let was_authorized = before.as_ref().is_some_and(|s| s.find_my_authorized);
        let window_closed = before.as_ref().is_some_and(|s| s.signing_in) && !status.signing_in;
        // Signed in anew (a request had said so while the status still
        // read signed in, if the window just closed): a new session, so
        // whether Find My wants its password is for the next refresh to say.
        if status.signed_in
            && (was_signed_in == Some(false) || (self.sign_in_needed && window_closed))
        {
            self.sign_in_needed = false;
            self.find_my_needed = false;
            return Change::SignedIn;
        }
        if status.signed_in && status.find_my_authorized && !was_authorized && before.is_some() {
            self.find_my_needed = false;
            return Change::FindMyAuthorized;
        }
        Change::None
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    None,
    SignedIn,
    FindMyAuthorized,
}

pub struct SignInBanner {
    pub widget: adw::Banner,
    state: Rc<RefCell<State>>,
}

impl SignInBanner {
    /// `on_signed_in` runs when the account turns signed in after being
    /// signed out, and when Find My turns authorized (both start the Find My
    /// session over and locate); `on_error` gets a message for a toast.
    pub fn new(on_signed_in: impl Fn() + 'static, on_error: impl Fn(String) + 'static) -> Self {
        let widget = adw::Banner::builder()
            .title(TITLE)
            .button_label(BUTTON)
            .button_style(adw::BannerButtonStyle::Suggested)
            .revealed(false)
            .build();
        let state = Rc::new(RefCell::new(State::default()));
        let on_error = Rc::new(on_error);
        let clicked = state.clone();
        widget.connect_button_clicked(move |_| {
            let action = clicked
                .borrow()
                .view()
                .and_then(|v| v.button)
                .map(|(_, a)| a);
            let on_error = on_error.clone();
            let (open, what): (fn() -> icloud_session::Result<()>, _) = match action {
                Some(Action::AuthorizeFindMy) => (
                    icloud_session::authorize_find_my,
                    "Could not open Find My's password page",
                ),
                _ => (icloud_session::sign_in, "Could not start signing in"),
            };
            super::background(open, move |result| {
                let msg = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => format!("{what}: {e}"),
                    Err(e) => e,
                };
                on_error(msg);
            });
        });

        let (tx, rx) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("icloud-session-watch".into())
            .spawn(move || watch_status(&tx))
            .expect("spawn the sign-in status thread");
        let this = Self { widget, state };
        let (banner, state) = (this.widget.clone(), this.state.clone());
        glib::spawn_future_local(async move {
            while let Ok(status) = rx.recv().await {
                let change = state.borrow_mut().update(status);
                render(&banner, &state.borrow());
                if change != Change::None {
                    on_signed_in();
                }
            }
        });
        this
    }

    /// Shown by the window when a request answers `SignInRequired`.
    pub fn show(&self) {
        self.state.borrow_mut().sign_in_needed = true;
        self.render();
    }

    /// Shown by the window when a request answers `FindMyAuthRequired`.
    pub fn show_find_my(&self) {
        self.state.borrow_mut().find_my_needed = true;
        self.render();
    }

    /// A request succeeded: nothing to ask for.
    pub fn hide(&self) {
        let mut state = self.state.borrow_mut();
        state.sign_in_needed = false;
        state.find_my_needed = false;
        drop(state);
        self.render();
    }

    fn render(&self) {
        render(&self.widget, &self.state.borrow());
    }
}

fn render(banner: &adw::Banner, state: &State) {
    match state.view() {
        Some(view) => {
            banner.set_title(view.title);
            banner.set_button_label(view.button.map(|(label, _)| label));
            banner.set_revealed(true);
        }
        None => banner.set_revealed(false),
    }
}

/// Sends the daemon's status, then every change, until the window is gone.
/// The watch ends or fails when the daemon cannot be reached; it is then
/// opened again after a short delay, which also reactivates the daemon.
fn watch_status(tx: &async_channel::Sender<icloud_session::Status>) {
    let mut reported = false;
    loop {
        match icloud_session::watch() {
            // Mock mode: no daemon, and the watch never yields.
            Ok(watch) if watch.current().is_none() => return,
            Ok(mut watch) => {
                reported = false;
                let first = watch.current().cloned();
                for status in first.into_iter().chain(&mut watch) {
                    if tx.send_blocking(status).is_err() {
                        return;
                    }
                }
            }
            // Report an outage once, not on every retry.
            Err(e) if !reported => {
                reported = true;
                eprintln!("icloud-findmy: sign-in status unavailable: {e}");
            }
            Err(_) => {}
        }
        if tx.is_closed() {
            return;
        }
        std::thread::sleep(RECONNECT_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(
        signed_in: bool,
        signing_in: bool,
        find_my_authorized: bool,
    ) -> icloud_session::Status {
        icloud_session::Status {
            signed_in,
            apple_id: None,
            dsid: None,
            expires_at: None,
            signing_in,
            find_my_authorized,
            find_my_password_stored: true,
        }
    }

    #[test]
    fn find_my_banner_is_distinct_from_sign_in() {
        let mut state = State::default();
        assert_eq!(state.update(status(true, false, false)), Change::None);
        assert_eq!(state.view(), None);

        // A 450: its own banner, whose button authorizes Find My.
        state.find_my_needed = true;
        assert_eq!(
            state.view(),
            Some(View {
                title: FIND_MY_TITLE,
                button: Some((FIND_MY_BUTTON, Action::AuthorizeFindMy)),
            })
        );
        // The window is open.
        assert_eq!(state.update(status(true, true, false)), Change::None);
        assert_eq!(
            state.view(),
            Some(View {
                title: FIND_MY_WAITING,
                button: None
            })
        );
        // Closed without finishing: offered again.
        state.update(status(true, false, false));
        assert_eq!(state.view().unwrap().title, FIND_MY_TITLE);
        state.update(status(true, true, false));
        // Authorized: hidden, and the window refreshes.
        assert_eq!(
            state.update(status(true, false, true)),
            Change::FindMyAuthorized
        );
        assert_eq!(state.view(), None);
    }

    #[test]
    fn signing_out_wins_over_find_my() {
        let mut state = State::default();
        state.update(status(true, false, false));
        state.find_my_needed = true;
        state.update(status(false, false, false));
        assert_eq!(
            state.view(),
            Some(View {
                title: TITLE,
                button: Some((BUTTON, Action::SignIn)),
            })
        );
        state.update(status(false, true, false));
        assert_eq!(state.view().unwrap().title, SIGNING_IN);
        assert_eq!(state.update(status(true, false, false)), Change::SignedIn);
        // A new session: the refresh the sign-in starts says whether Find
        // My wants its password.
        assert_eq!(state.view(), None);
    }

    #[test]
    fn signing_in_again_while_the_status_read_signed_in() {
        let mut state = State::default();
        state.update(status(true, false, false));
        state.sign_in_needed = true;
        assert_eq!(state.view().unwrap().title, TITLE);
        state.update(status(true, true, false));
        assert_eq!(state.update(status(true, false, false)), Change::SignedIn);
        assert_eq!(state.view(), None);
    }

    #[test]
    fn a_first_status_already_authorized_is_not_a_change() {
        let mut state = State::default();
        assert_eq!(state.update(status(true, false, true)), Change::None);
        assert_eq!(state.view(), None);
    }
}

#[cfg(test)]
mod password_hint_tests {
    use super::*;

    #[test]
    fn the_find_my_banner_mentions_set_password_when_none_is_stored() {
        let mut state = State::default();
        let mut status = icloud_session::Status {
            signed_in: true,
            apple_id: None,
            dsid: None,
            expires_at: None,
            signing_in: false,
            find_my_authorized: false,
            find_my_password_stored: false,
        };
        state.update(status.clone());
        state.find_my_needed = true;
        assert_eq!(state.view().unwrap().title, FIND_MY_TITLE_HINT);
        status.find_my_password_stored = true;
        state.update(status);
        assert_eq!(state.view().unwrap().title, FIND_MY_TITLE);
    }
}
