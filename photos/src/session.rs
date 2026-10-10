//! The only code that talks to the `icloud-session` client, the client of
//! `icloud-sessiond` (the D-Bus user service that owns the Apple sign-in).
//! In that crate's mock mode (`ICLOUD_SESSION_MOCK=1`) there is no D-Bus:
//! every request goes to `ICLOUD_SESSION_MOCK_URL`, served by `cargo run
//! --example fake_cloudkit`, which plays the account (and can sign it out).

use std::path::Path;
use std::sync::Arc;

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
        Ok(Self {
            session: icloud_session::Session::connect()?,
        })
    }

    /// A mock session sending everything to `base_url` (the fake server).
    pub fn mock(base_url: &str) -> Self {
        Self {
            session: icloud_session::Session::mock(base_url),
        }
    }
}

/// The transport the app and the command line use.
pub fn connect() -> Result<Arc<dyn Transport>> {
    Ok(Arc::new(SessionTransport::connect()?))
}

/// icloud-session's mock mode is on.
pub fn is_mock() -> bool {
    icloud_session::mock_url().is_some()
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

    fn download_cache(&self, url: &str, dest: &Path) -> Result<u64> {
        Ok(self.session.download_cache(url, dest)?)
    }
}

/// The daemon's current `SignedIn` / `SigningIn` properties (one D-Bus
/// round trip, no request to Apple). In mock mode the fake server is asked,
/// since it is what plays the signed-out account.
pub fn sign_in_state() -> Result<SignInState> {
    if is_mock() {
        let t = SessionTransport::connect()?;
        let url = format!(
            "{}{}/zones/list",
            t.service_url("ckdatabasews")?,
            crate::cloudkit::DB_PATH
        );
        return match t.post_json(&url, &Value::Object(Default::default())) {
            Ok(_) => Ok(SignInState {
                signed_in: true,
                signing_in: false,
                sign_out_reason: None,
            }),
            Err(Error::SignInRequired) => Ok(SignInState {
                signed_in: false,
                signing_in: false,
                sign_out_reason: None,
            }),
            Err(e) => Err(e),
        };
    }
    let s = icloud_session::status()?;
    Ok(SignInState {
        signed_in: s.signed_in,
        signing_in: s.signing_in,
        sign_out_reason: s.sign_out_reason,
    })
}

/// Asks the daemon to open its sign-in window and returns at once; the
/// outcome arrives through [`watch_sign_in`]. In mock mode the fake server
/// is signed back in and `notify` hears so straight away. Call it off the
/// main loop.
pub fn start_sign_in(notify: &dyn Fn(SignInState)) -> Result<()> {
    icloud_session::sign_in()?;
    if is_mock() {
        notify(SignInState {
            signed_in: true,
            signing_in: false,
            sign_out_reason: None,
        });
    }
    Ok(())
}

/// Reports the sign-in state on a long-lived background thread: once on
/// connecting to icloud-sessiond (and on every reconnect), then after every
/// change. Nothing in mock mode, where [`start_sign_in`] reports for itself.
pub fn watch_sign_in(notify: Box<dyn Fn(SignInState) + Send>) {
    std::thread::Builder::new()
        .name("sign-in watch".into())
        .spawn(move || {
            icloud_session::watch_forever(|s| {
                notify(SignInState {
                    signed_in: s.signed_in,
                    signing_in: s.signing_in,
                    sign_out_reason: s.sign_out_reason,
                });
                true
            })
        })
        .expect("spawn the sign-in watch thread");
}
