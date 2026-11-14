use std::sync::Mutex;

use icloud_session::{Error, Response, Result, Session};
use serde_json::Value;

/// HTTP with the iCloud session, over `icloud-session`. Connects to
/// `icloud-sessiond` lazily, so a missing sign-in surfaces as
/// `SignInRequired` on first use, and drops the connection on
/// `SignInRequired` or a daemon failure so that the next call reconnects
/// and a later sign-in is picked up without restarting. In icloud-session's
/// mock mode the same transport talks to an app's fake server.
#[derive(Default)]
pub struct SessionTransport {
    session: Mutex<Option<Session>>,
}

impl SessionTransport {
    /// Runs `f` with the session, connecting first if there is none.
    pub fn with<T>(&self, f: impl FnOnce(&Session) -> Result<T>) -> Result<T> {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(Session::connect()?);
        }
        let result = f(guard.as_ref().expect("just connected"));
        if let Err(Error::SignInRequired | Error::KeyringUnavailable(_) | Error::Service(_) | Error::Daemon { .. }) =
            &result
        {
            *guard = None;
        }
        result
    }

    /// Drops the connection, and with it the account it was for (after a
    /// different Apple ID signed in, say).
    pub fn reset(&self) {
        *self.session.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Base URL of a `webservices` entry (`ckdatabasews`, `contacts`,
    /// `findme`); `None` when iCloud does not offer it for this account.
    pub fn service_url(&self, key: &str) -> Result<Option<String>> {
        let ws = self.with(|s| s.webservices())?;
        Ok(ws.url(key).map(str::to_owned))
    }

    pub fn get_json(&self, url: &str) -> Result<Value> {
        json(url, &self.with(|s| s.get(url))?)
    }

    pub fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        json(url, &self.with(|s| s.post_json(url, body))?)
    }
}

/// A 200 that isn't JSON is the service misbehaving, not the network.
fn json(url: &str, response: &Response) -> Result<Value> {
    serde_json::from_slice(&response.body).map_err(|e| {
        let endpoint = url.split('?').next().unwrap_or(url);
        Error::BadResponse(format!(
            "iCloud answered {endpoint} with something that isn't JSON: {e}"
        ))
    })
}
