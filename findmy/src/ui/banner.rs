//! The "sign in" banner. It follows `icloud-sessiond`'s status through
//! `icloud_session::watch()` on one background thread: shown while signed
//! out, "Signing in…" while the daemon's sign-in window is open, hidden (and
//! the window refreshed) once the account is signed in. Its button calls
//! `icloud_session::sign_in()`, which only asks the daemon to open the window.

use std::rc::Rc;
use std::time::Duration;

use gtk::glib;

const TITLE: &str = "Sign in to iCloud to see your devices";
const SIGNING_IN: &str = "Signing in…";
const BUTTON: &str = "Sign In";

/// How long the watcher waits before reconnecting when the watch ended or
/// could not start (the daemon idle-exited, or the bus was unreachable).
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub struct SignInBanner {
    pub widget: adw::Banner,
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
            .spawn(move || watch_status(&tx))
            .expect("spawn the sign-in status thread");
        let banner = widget.clone();
        glib::spawn_future_local(async move {
            let mut signed_in = None;
            while let Ok(status) = rx.recv().await {
                let was = signed_in.replace(status.signed_in);
                if status.signed_in {
                    banner.set_revealed(false);
                    if was == Some(false) {
                        on_signed_in();
                    }
                } else if status.signing_in {
                    banner.set_title(SIGNING_IN);
                    banner.set_button_label(None);
                    banner.set_revealed(true);
                } else {
                    banner.set_title(TITLE);
                    banner.set_button_label(Some(BUTTON));
                    banner.set_revealed(true);
                }
            }
        });
        Self { widget }
    }

    /// Shown by the window when a request answers `SignInRequired`.
    pub fn show(&self) {
        self.widget.set_revealed(true);
    }

    pub fn hide(&self) {
        self.widget.set_revealed(false);
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
