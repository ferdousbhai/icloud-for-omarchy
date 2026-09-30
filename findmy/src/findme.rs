//! Apple's Find My web service (`findme` in the webservices map), as spoken by
//! icloud.com and documented by pyicloud's `FindMyiPhoneServiceManager`:
//!
//! - `POST {root}/fmipservice/client/web/initClient` starts a session and
//!   returns `serverContext`, `userInfo` and the devices in `content`.
//! - `POST .../refreshClient` with that `serverContext` (its `theftLoss`
//!   nulled) returns fresh positions; `shouldLocate` asks devices to report.
//! - `POST .../playSound` and `.../lostDevice` act on one device.
//!
//! All HTTP goes through [`Transport`], which the app backs with
//! `icloud_session::Session` and the tests back with recorded fixtures.

use serde_json::{Value, json};

use crate::models::Device;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sign in to iCloud required")]
    SignInRequired,
    /// Find My answered HTTP 450: it wants the Apple ID password entered
    /// again (`icloud_session::authorize_find_my`), not a new sign-in.
    #[error("Find My needs your Apple password")]
    FindMyAuthRequired,
    #[error("Find My is not enabled for this Apple ID")]
    NoService,
    #[error("iCloud answered HTTP {0}")]
    Http(u16),
    #[error("{0}")]
    Session(String),
    #[error("unexpected Find My response: {0}")]
    Parse(String),
    #[error("{0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<icloud_session::Error> for Error {
    fn from(e: icloud_session::Error) -> Self {
        match e {
            icloud_session::Error::SignInRequired => Error::SignInRequired,
            icloud_session::Error::FindMyAuthRequired => Error::FindMyAuthRequired,
            icloud_session::Error::Http { status, .. } => Error::Http(status),
            other => Error::Session(other.to_string()),
        }
    }
}

/// The two things Find My needs from a session: where the service lives,
/// and a way to POST JSON to it.
pub trait Transport: Send {
    /// The `findme` web service root, e.g. `https://p42-fmipweb.icloud.com:443`.
    fn service_root(&mut self) -> Result<String>;
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Value>;
    /// Forgets any per-account state, e.g. after a different Apple ID
    /// signed in.
    fn reset(&mut self) {}
}

/// [`Transport`] over the shared `icloud-session` crate. Connects to
/// `icloud-sessiond` lazily, so a missing sign-in surfaces as
/// `SignInRequired` on first use, and drops the connection on
/// `SignInRequired` or a daemon failure so the next call reconnects and a
/// later sign-in is picked up without restarting the app.
#[derive(Default)]
pub struct SessionTransport {
    session: Option<icloud_session::Session>,
}

impl SessionTransport {
    fn session(&mut self) -> Result<&icloud_session::Session> {
        if self.session.is_none() {
            self.session = Some(icloud_session::Session::connect()?);
        }
        Ok(self.session.as_ref().expect("just connected"))
    }

    /// Drops the connection after `SignInRequired` or a daemon failure, so
    /// the next call reconnects.
    fn drop_on_lost_session(&mut self, e: &icloud_session::Error) {
        if matches!(
            e,
            icloud_session::Error::SignInRequired | icloud_session::Error::Service(_)
        ) {
            self.session = None;
        }
    }
}

impl Transport for SessionTransport {
    fn service_root(&mut self) -> Result<String> {
        let result = self.session()?.webservices();
        let ws = result.inspect_err(|e| self.drop_on_lost_session(e))?;
        ws.url("findme")
            .map(|url| url.trim_end_matches('/').to_string())
            .ok_or(Error::NoService)
    }

    fn post_json(&mut self, url: &str, body: &Value) -> Result<Value> {
        let result = self.session()?.post_json(url, body);
        let resp = result.inspect_err(|e| self.drop_on_lost_session(e))?;
        if resp.body.is_empty() {
            return Ok(Value::Null);
        }
        resp.json::<Value>().map_err(Error::from)
    }

    /// Drops the daemon connection, and with it the old account's dsid.
    fn reset(&mut self) {
        self.session = None;
    }
}

pub const APP_NAME: &str = "iCloud Find (Web)";
pub const TIMEZONE: &str = "US/Pacific";

/// The body for `initClient` (no server context yet) or `refreshClient`.
pub fn refresh_body(server_ctx: Option<&Value>, locate: bool) -> Value {
    let mut body = json!({
        "clientContext": {
            "appName": APP_NAME,
            "appVersion": "2.0",
            "apiVersion": "3.0",
            "deviceListVersion": 1,
            "fmly": false,
            "timezone": TIMEZONE,
            "inactiveTime": 0,
        }
    });
    if let Some(ctx) = server_ctx {
        body["serverContext"] = ctx.clone();
        if locate {
            body["isUpdatingAllLocations"] = json!(true);
            body["clientContext"]["shouldLocate"] = json!(true);
            body["clientContext"]["selectedDevice"] = json!("all");
        }
    }
    body
}

/// What one `initClient` / `refreshClient` answered.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// To send back on the next refresh, `theftLoss` already nulled.
    pub server_ctx: Option<Value>,
    pub devices: Vec<Device>,
}

/// Parses an `initClient` / `refreshClient` response.
pub fn parse_response(resp: &Value) -> Result<Snapshot> {
    let obj = resp
        .as_object()
        .ok_or_else(|| Error::Parse("not a JSON object".into()))?;
    if let Some(code) = obj.get("statusCode").and_then(Value::as_str)
        && code != "200"
    {
        return Err(Error::Parse(format!("statusCode {code}")));
    }
    let server_ctx = obj.get("serverContext").cloned().map(|mut ctx| {
        if ctx.get("theftLoss").is_some() {
            ctx["theftLoss"] = Value::Null;
        }
        ctx
    });
    let devices = match obj.get("content") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(items)) => items.iter().filter_map(Device::from_json).collect(),
        Some(_) => return Err(Error::Parse("content is not a list".into())),
    };
    Ok(Snapshot { server_ctx, devices })
}

pub fn play_sound_body(device_id: &str, subject: &str) -> Value {
    json!({
        "device": device_id,
        "subject": subject,
        "clientContext": { "fmly": true },
    })
}

pub fn lost_mode_body(device_id: &str, phone: &str, message: &str, passcode: &str) -> Value {
    json!({
        "text": message,
        "userText": true,
        "ownerNbr": phone,
        "lostModeEnabled": true,
        "trackingEnabled": true,
        "device": device_id,
        "passcode": passcode,
    })
}

/// A Find My client: remembers the service root and server context between
/// refreshes. Blocking; run it off the main loop.
pub struct FindMe<T: Transport> {
    transport: T,
    root: Option<String>,
    server_ctx: Option<Value>,
}

impl<T: Transport> FindMe<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            root: None,
            server_ctx: None,
        }
    }

    /// Forget the Find My session, e.g. after the user signed in again
    /// (possibly as someone else): the next call reconnects from scratch.
    pub fn reset(&mut self) {
        self.root = None;
        self.server_ctx = None;
        self.transport.reset();
    }

    fn url(&mut self, endpoint: &str) -> Result<String> {
        if self.root.is_none() {
            self.root = Some(self.transport.service_root()?);
        }
        let root = self.root.as_deref().expect("just set");
        Ok(format!("{root}/fmipservice/client/web/{endpoint}"))
    }

    /// Fetches all devices: `initClient` the first time, then
    /// `refreshClient`. With `locate` the refresh asks every device to report
    /// its position (which wakes them), so pass it only when the user asked;
    /// periodic refreshes pass `false` and get what Apple last heard, as
    /// pyicloud's monitor does. On `SignInRequired` or `FindMyAuthRequired`
    /// the context is dropped so the next call starts over with `initClient`.
    ///
    /// HTTP 500 means the Find My server session lapsed: start over with
    /// `initClient` once before giving up. HTTP 450 is not retried: it is
    /// `FindMyAuthRequired` (the password must be entered again), and
    /// another `initClient` would only answer 450 again.
    pub fn refresh(&mut self, locate: bool) -> Result<Vec<Device>> {
        let mut result = self.refresh_inner(locate);
        if matches!(result, Err(Error::Http(500))) {
            self.reset();
            result = self.refresh_inner(locate);
        }
        if matches!(result, Err(Error::SignInRequired | Error::FindMyAuthRequired)) {
            self.reset();
        }
        result
    }

    fn refresh_inner(&mut self, locate: bool) -> Result<Vec<Device>> {
        let endpoint = if self.server_ctx.is_some() {
            "refreshClient"
        } else {
            "initClient"
        };
        let url = self.url(endpoint)?;
        let body = refresh_body(self.server_ctx.as_ref(), locate);
        let resp = self.transport.post_json(&url, &body)?;
        let snap = parse_response(&resp)?;
        if snap.server_ctx.is_some() {
            self.server_ctx = snap.server_ctx;
        }
        Ok(snap.devices)
    }

    pub fn play_sound(&mut self, device: &Device) -> Result<()> {
        if !device.can_play_sound {
            return Err(Error::Unsupported("This device cannot play a sound."));
        }
        let url = self.url("playSound")?;
        self.transport
            .post_json(&url, &play_sound_body(&device.id, "Find My iPhone Alert"))?;
        Ok(())
    }

    /// Turns on Lost Mode: the device locks and shows `message`, with a
    /// button to call `phone`.
    pub fn lost_mode(&mut self, device: &Device, phone: &str, message: &str) -> Result<()> {
        if !device.can_lost_mode {
            return Err(Error::Unsupported("This device does not support Lost Mode."));
        }
        let url = self.url("lostDevice")?;
        self.transport
            .post_json(&url, &lost_mode_body(&device.id, phone, message, ""))?;
        Ok(())
    }
}
