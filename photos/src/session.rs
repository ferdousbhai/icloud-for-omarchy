//! The only code that talks to the `icloud-session` crate.
//!
//! Porting to the D-Bus client API (brief, "Client crate"):
//! - `Session::load()` becomes `Session::connect()`.
//! - `sign_in()` below becomes `icloud_session::sign_in()` (returns at once),
//!   then block on `icloud_session::watch()` until a `Status` arrives with
//!   `signed_in` true (success) or `signing_in` false without it (gave up).
//!   Callers already run it on a background thread and treat its return as
//!   "sign-in finished", so nothing outside this file changes.
//! - `webservices`, `post_json`, `post_bytes`, `download` and
//!   `Error::SignInRequired` are unchanged.

use std::path::Path;

use serde_json::Value;

use crate::transport::{Error, Result, Transport};

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
    pub fn load() -> Result<Self> {
        // The session crate is young; never let a panic in it take the UI down.
        let session = std::panic::catch_unwind(icloud_session::Session::load)
            .map_err(|_| Error::Other("icloud-session could not load the session".into()))??;
        Ok(Self { session })
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

    fn post_bytes(&self, url: &str, content_type: &str, body: Vec<u8>) -> Result<Value> {
        Ok(self.session.post_bytes(url, content_type, body)?.json()?)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        Ok(self.session.download(url, dest)?)
    }

    fn reauthenticate(&self) -> Result<()> {
        sign_in()
    }
}

/// Run the interactive sign-in and wait for it to finish.
pub fn sign_in() -> Result<()> {
    std::panic::catch_unwind(icloud_session::Session::reauthenticate)
        .map_err(|_| Error::Other("icloud-session could not start the sign-in".into()))??;
    Ok(())
}
