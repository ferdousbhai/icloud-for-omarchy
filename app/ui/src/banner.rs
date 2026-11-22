//! The "sign in" banner. It follows `icloud-sessiond`'s status through
//! `icloud_session::watch_forever()` on one background thread: shown while
//! signed out (saying why, when the daemon knows), "Signing in…" while the
//! daemon's sign-in window is open, hidden (and the window refreshed) once
//! the account is signed in. Its button calls `icloud_session::sign_in()`,
//! which only asks the daemon to open the window. omui draws it.
//!
//! Find My can also want the Apple password again (HTTP 450) while the
//! account is signed in. Its window then calls `SignInBanner::show_find_my`
//! and the banner says so, with a button calling
//! `icloud_session::authorize_find_my()`; "Finish in the Apple window…"
//! while that window is open; hidden (and the window refreshed, locating)
//! once the daemon reports Find My authorized.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use omui::{background, i18n, tr};

/// Marks `text`, an app's ask for [`SignInBanner::new`], as a string of the
/// app's catalogs: the app's catalog test ([`crate::catalog_check`]) finds
/// it there, and the banner shows it translated. Returns it as it is.
pub fn sign_in_ask(text: &'static str) -> &'static str {
    text
}

fn signing_in_title() -> String {
    tr("Signing in…")
}
fn button() -> String {
    tr("Sign In")
}
fn find_my_title() -> String {
    tr("Find My needs your Apple password")
}
/// The same when no password is stored for the daemon to use by itself.
fn find_my_title_hint() -> String {
    tr("Find My needs your Apple password (to stop being asked: icloud-session set-password)")
}
fn find_my_waiting() -> String {
    tr("Finish in the Apple window…")
}
fn find_my_button() -> String {
    tr("Enter Password")
}

/// Why the account was signed out, as the daemon says it (its
/// `SignOutReason`s, in English), in the language in use.
fn reason_text(reason: &str) -> String {
    match reason {
        "Signed out of iCloud" => tr("Signed out of iCloud"),
        "Apple ended the iCloud session (it expired, the password was changed, or it was signed out on another device)" => {
            tr(
                "Apple ended the iCloud session (it expired, the password was changed, or it was signed out on another device)",
            )
        }
        "The iCloud session was removed from the keyring" => tr("The iCloud session was removed from the keyring"),
        "The iCloud session in the keyring was damaged" => tr("The iCloud session in the keyring was damaged"),
        "The keyring held another Apple ID's iCloud session" => {
            tr("The keyring held another Apple ID's iCloud session")
        }
        "The iCloud account file could not be read" => tr("The iCloud account file could not be read"),
        // One with a detail of its own ("The last sign-in could not be
        // kept: …") stays as the daemon wrote it.
        other => i18n::translate(other),
    }
}

/// What the banner's button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    SignIn,
    AuthorizeFindMy,
}

/// What the banner knows: the daemon's last status and what the window
/// told it.
#[derive(Debug)]
struct State {
    /// The app's ask, "Sign in to iCloud to see your reminders".
    title: &'static str,
    /// The window is Find My's: Find My turning authorized is a change too.
    find_my: bool,
    status: Option<icloud_session::Status>,
    /// A request answered `SignInRequired` (the status may lag behind).
    sign_in_needed: bool,
    /// A request answered `FindMyAuthRequired`.
    find_my_needed: bool,
}

/// How the banner looks: `None` hidden, else title, button and its action.
#[derive(Debug, PartialEq, Eq)]
struct View {
    title: String,
    button: Option<(String, Action)>,
}

impl State {
    fn new(title: &'static str, find_my: bool) -> State {
        State {
            title,
            find_my,
            status: None,
            sign_in_needed: false,
            find_my_needed: false,
        }
    }

    fn view(&self) -> Option<View> {
        let signed_in = self.status.as_ref().map(|s| s.signed_in);
        let signing_in = self.status.as_ref().is_some_and(|s| s.signing_in);
        let find_my = self.find_my_needed && signed_in != Some(false);
        if signing_in {
            let title = if find_my { find_my_waiting() } else { signing_in_title() };
            return Some(View { title, button: None });
        }
        if signed_in == Some(false) || self.sign_in_needed {
            let ask = i18n::translate(self.title);
            let reason = self.status.as_ref().and_then(|s| s.sign_out_reason.as_deref());
            return Some(View {
                title: match reason {
                    // Two sentences: why, then what to do about it.
                    Some(reason) => tr!("{}. {}", reason_text(reason), ask),
                    None => ask,
                },
                button: Some((button(), Action::SignIn)),
            });
        }
        let stored = self.status.as_ref().is_some_and(|s| s.find_my_password_stored);
        find_my.then(|| View {
            title: if stored { find_my_title() } else { find_my_title_hint() },
            button: Some((find_my_button(), Action::AuthorizeFindMy)),
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
        if status.signed_in && (was_signed_in == Some(false) || (self.sign_in_needed && window_closed)) {
            self.sign_in_needed = false;
            self.find_my_needed = false;
            return Change::SignedIn;
        }
        if self.find_my && status.signed_in && status.find_my_authorized && !was_authorized && before.is_some() {
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

/// What the banner knows, and the banner that shows it.
struct Core {
    state: RefCell<State>,
    banner: omui::Banner,
}

impl Core {
    /// The banner's button: asks the daemon for the window its view names.
    fn clicked(&self, on_error: Rc<dyn Fn(String)>) {
        let action = self.state.borrow().view().and_then(|v| v.button).map(|(_, a)| a);
        let (open, what): (fn() -> icloud_session::Result<()>, bool) = match action {
            Some(Action::AuthorizeFindMy) => (icloud_session::authorize_find_my, true),
            _ => (icloud_session::sign_in, false),
        };
        background(open, move |result| {
            let msg = match result {
                Ok(Ok(())) => return,
                Ok(Err(e)) if what => tr!("Could not open Find My's password page: {}", e),
                Ok(Err(e)) => tr!("Could not start signing in: {}", e),
                Err(e) => e,
            };
            on_error(msg);
        });
    }

    fn render(&self) {
        let view = self.state.borrow().view();
        if let Some(view) = &view {
            self.banner.set_title(&view.title);
            self.banner
                .set_button_label(view.button.as_ref().map(|(label, _)| label.as_str()));
        }
        self.banner.set_revealed(view.is_some());
    }
}

pub struct SignInBanner {
    pub widget: gtk::Widget,
    core: Rc<Core>,
}

impl SignInBanner {
    /// `title` is the app's ask, in English and marked for its catalogs
    /// (`sign_in_ask("Sign in to iCloud to see your reminders")`): the
    /// banner shows it in the language of the moment. `on_signed_in` runs when the account turns signed in
    /// after being signed out (the window then syncs); `on_error` gets a
    /// message for a toast.
    pub fn new(title: &'static str, on_signed_in: impl Fn() + 'static, on_error: impl Fn(String) + 'static) -> Self {
        Self::build(State::new(title, false), on_signed_in, on_error)
    }

    /// [`SignInBanner::new`] for Find My's window, which also calls
    /// [`SignInBanner::show_find_my`]: `on_signed_in` runs when Find My
    /// turns authorized too (both start the Find My session over and
    /// locate).
    pub fn with_find_my(
        title: &'static str,
        on_signed_in: impl Fn() + 'static,
        on_error: impl Fn(String) + 'static,
    ) -> Self {
        Self::build(State::new(title, true), on_signed_in, on_error)
    }

    /// Follows the daemon's status for as long as the process lives.
    fn build(state: State, on_signed_in: impl Fn() + 'static, on_error: impl Fn(String) + 'static) -> Self {
        let banner = omui::Banner::new(&i18n::translate(state.title), Some(&button()));
        banner.set_button_suggested(true);
        let widget = banner.widget.clone().upcast();
        let core = Rc::new(Core {
            state: RefCell::new(state),
            banner,
        });
        let (tx, rx) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("icloud-session-watch".into())
            // The daemon's status, then every change, until the window is gone.
            .spawn(move || icloud_session::watch_forever(|status| tx.send_blocking(status).is_ok()))
            .expect("spawn the sign-in status thread");
        let following = core.clone();
        glib::spawn_future_local(async move {
            while let Ok(status) = rx.recv().await {
                let change = following.state.borrow_mut().update(status);
                following.render();
                if change != Change::None {
                    on_signed_in();
                }
            }
        });
        // In the language of the moment, after every switch.
        let weak = Rc::downgrade(&core);
        i18n::on_change(&core.banner.widget, move || {
            if let Some(core) = weak.upgrade() {
                core.render();
            }
        });
        let (weak, on_error) = (Rc::downgrade(&core), Rc::new(on_error));
        core.banner.connect_button_clicked(move || {
            if let Some(core) = weak.upgrade() {
                core.clicked(on_error.clone());
            }
        });
        Self { widget, core }
    }

    /// Shown by the window when a request answers `SignInRequired`.
    pub fn show(&self) {
        self.core.state.borrow_mut().sign_in_needed = true;
        self.core.render();
    }

    /// Shown by the window when a request answers `FindMyAuthRequired`.
    pub fn show_find_my(&self) {
        self.core.state.borrow_mut().find_my_needed = true;
        self.core.render();
    }

    /// A request succeeded: nothing to ask for.
    pub fn hide(&self) {
        let mut state = self.core.state.borrow_mut();
        state.sign_in_needed = false;
        state.find_my_needed = false;
        drop(state);
        self.core.render();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE: &str = "Sign in to iCloud to see your devices";

    fn state() -> State {
        State::new(TITLE, true)
    }

    fn status(signed_in: bool, signing_in: bool, find_my_authorized: bool) -> icloud_session::Status {
        icloud_session::Status {
            signed_in,
            apple_id: None,
            full_name: None,
            dsid: None,
            expires_at: None,
            signing_in,
            find_my_authorized,
            find_my_password_stored: true,
            sign_out_reason: None,
        }
    }

    #[test]
    fn find_my_banner_is_distinct_from_sign_in() {
        let mut state = state();
        assert_eq!(state.update(status(true, false, false)), Change::None);
        assert_eq!(state.view(), None);

        // A 450: its own banner, whose button authorizes Find My.
        state.find_my_needed = true;
        assert_eq!(
            state.view(),
            Some(View {
                title: find_my_title(),
                button: Some((find_my_button(), Action::AuthorizeFindMy)),
            })
        );
        // The window is open.
        assert_eq!(state.update(status(true, true, false)), Change::None);
        assert_eq!(
            state.view(),
            Some(View {
                title: find_my_waiting(),
                button: None
            })
        );
        // Closed without finishing: offered again.
        state.update(status(true, false, false));
        assert_eq!(state.view().unwrap().title, find_my_title());
        state.update(status(true, true, false));
        // Authorized: hidden, and the window refreshes.
        assert_eq!(state.update(status(true, false, true)), Change::FindMyAuthorized);
        assert_eq!(state.view(), None);
    }

    #[test]
    fn signing_out_wins_over_find_my() {
        let mut state = state();
        state.update(status(true, false, false));
        state.find_my_needed = true;
        state.update(status(false, false, false));
        assert_eq!(
            state.view(),
            Some(View {
                title: TITLE.into(),
                button: Some((button(), Action::SignIn)),
            })
        );
        state.update(icloud_session::Status {
            sign_out_reason: Some("Signed out of iCloud".into()),
            ..status(false, false, false)
        });
        assert_eq!(state.view().unwrap().title, format!("Signed out of iCloud. {TITLE}"));
        state.update(status(false, true, false));
        assert_eq!(state.view().unwrap().title, signing_in_title());
        assert_eq!(state.update(status(true, false, false)), Change::SignedIn);
        // A new session: the refresh the sign-in starts says whether Find
        // My wants its password.
        assert_eq!(state.view(), None);
    }

    #[test]
    fn signing_in_again_while_the_status_read_signed_in() {
        let mut state = state();
        state.update(status(true, false, false));
        state.sign_in_needed = true;
        assert_eq!(state.view().unwrap().title, TITLE);
        state.update(status(true, true, false));
        assert_eq!(state.update(status(true, false, false)), Change::SignedIn);
        assert_eq!(state.view(), None);
    }

    #[test]
    fn find_my_turning_authorized_is_no_change_for_another_app() {
        let mut state = State::new(TITLE, false);
        state.update(status(true, false, false));
        assert_eq!(state.update(status(true, false, true)), Change::None);
        // Signing in is.
        state.update(status(false, false, false));
        assert_eq!(state.view().unwrap().button, Some((button(), Action::SignIn)));
        assert_eq!(state.update(status(true, false, false)), Change::SignedIn);
    }

    #[test]
    fn a_first_status_already_authorized_is_not_a_change() {
        let mut state = state();
        assert_eq!(state.update(status(true, false, true)), Change::None);
        assert_eq!(state.view(), None);
    }

    #[test]
    fn the_find_my_banner_mentions_set_password_when_none_is_stored() {
        let mut state = state();
        state.update(icloud_session::Status {
            find_my_password_stored: false,
            ..status(true, false, false)
        });
        state.find_my_needed = true;
        assert_eq!(state.view().unwrap().title, find_my_title_hint());
        state.update(status(true, false, false));
        assert_eq!(state.view().unwrap().title, find_my_title());
    }
}
