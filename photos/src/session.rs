//! The only code that talks to the `icloud-session` crate, the client of
//! `icloud-sessiond` (the D-Bus user service that owns the Apple sign-in).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::transport::{Error, Result, SignInState, Transport};

impl From<icloud_session::Error> for Error {
    fn from(e: icloud_session::Error) -> Self {
        match e {
            icloud_session::Error::SignInRequired => Error::SignInRequired,
            icloud_session::Error::Http { status, body } => Error::Http { status, body },
            icloud_session::Error::Io(e) => Error::Io(e),
            other => Error::Other(other.to_string()),
        }
    }
}

/// The real transport: every call goes through `icloud_session::Session`.
pub struct SessionTransport {
    session: icloud_session::Session,
}

impl SessionTransport {
    /// Asks icloud-sessiond (D-Bus activated) for the session;
    /// `SignInRequired` when signed out.
    pub fn connect() -> Result<Self> {
        Ok(Self { session: icloud_session::Session::connect()? })
    }
}

impl Transport for SessionTransport {
    fn service_url(&self, key: &str) -> Result<String> {
        let ws = self.session.webservices()?;
        ws.url(key)
            .map(str::to_owned)
            .ok_or_else(|| Error::Other(format!("iCloud did not offer the {key} service for this account")))
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        Ok(self.session.post_json(url, body)?.json()?)
    }

    fn post_file(&self, url: &str, content_type: &str, path: &Path) -> Result<Value> {
        Ok(self.session.post_file(url, content_type, path)?.json()?)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        Ok(self.session.download(url, dest)?)
    }
}

/// Asks the daemon to open its sign-in window; returns at once.
pub fn start_sign_in() -> Result<()> {
    Ok(icloud_session::sign_in()?)
}

/// Reconnect delays when the daemon cannot be reached or the watch ends.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Never returns: reports the current state on every (re)connect, then each
/// change. The watch ends when icloud-sessiond goes away (it exits when idle
/// or is restarted); reconnecting D-Bus-activates it again.
pub fn watch_sign_in(notify: &dyn Fn(SignInState)) {
    let state = |s: &icloud_session::Status| SignInState { signed_in: s.signed_in, signing_in: s.signing_in };
    let mut retry = RETRY_MIN;
    loop {
        let started = Instant::now();
        match icloud_session::watch() {
            Ok(mut watch) => {
                if let Some(s) = watch.current() {
                    notify(state(s));
                }
                for s in watch.by_ref() {
                    notify(state(&s));
                }
            }
            Err(e) => eprintln!("icloud-photos: watching the iCloud sign-in: {e}"),
        }
        // A watch that lasted a while ended normally: reconnect promptly.
        if started.elapsed() > RETRY_MAX {
            retry = RETRY_MIN;
        }
        std::thread::sleep(retry);
        retry = (retry * 2).min(RETRY_MAX);
    }
}
