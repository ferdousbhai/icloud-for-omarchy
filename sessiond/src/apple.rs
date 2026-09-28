//! `POST setup.icloud.com/setup/ws/1/validate`, made exactly as icloud-md's
//! `checkAuthentication` (`cloudkit/setupClient.js`) makes it, and Find My's
//! one-factor `accountLogin`, made as pyicloud's
//! `_authenticate_with_credentials_service("find")` makes it.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::cookies::{self, Cookie};
use crate::files::{CLIENT_BUILD_NUMBER, CLIENT_ID, CLIENT_MASTERING_NUMBER};

/// Apple's setup host, where `/setup/ws/1/validate` lives.
pub const SETUP_URL: &str = "https://setup.icloud.com";

pub fn setup_url() -> String {
    std::env::var("ICLOUD_SESSION_SETUP_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| SETUP_URL.to_string())
}

#[derive(Debug)]
pub struct Validated {
    pub dsid: String,
    pub apple_id: String,
    pub webservices: BTreeMap<String, String>,
    pub set_cookies: Vec<String>,
}

#[derive(Debug)]
pub enum ValidateError {
    /// 421/401, or a 200 that still wants a 2FA challenge.
    SignedOut,
    /// Anything else: no answer, another status, an unexpected body.
    Failed(String),
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        // Well inside a D-Bus caller's own timeout (QtDBus waits 25 s), so a
        // `Session()` while Apple is unreachable still answers in time.
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("icloud-sessiond/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// The setup endpoint's URL with the client params (and `dsid`) in the query.
fn setup_endpoint(
    setup_url: &str,
    path: &str,
    params: &BTreeMap<String, String>,
    dsid: Option<&str>,
) -> Result<url::Url, String> {
    let mut url =
        url::Url::parse(&format!("{setup_url}/setup/ws/1/{path}")).map_err(|e| format!("bad setup URL: {e}"))?;
    {
        let param = |k: &str| params.get(k).map(String::as_str).unwrap_or_default();
        let mut query = url.query_pairs_mut();
        query
            .append_pair(CLIENT_BUILD_NUMBER, param(CLIENT_BUILD_NUMBER))
            .append_pair(CLIENT_MASTERING_NUMBER, param(CLIENT_MASTERING_NUMBER))
            .append_pair(CLIENT_ID, param(CLIENT_ID))
            .append_pair("requestId", &uuid::Uuid::new_v4().to_string());
        if let Some(dsid) = dsid.filter(|d| !d.is_empty()) {
            query.append_pair("dsid", dsid);
        }
    }
    Ok(url)
}

/// A 2xx answer from a setup endpoint: its `Set-Cookie`s and JSON body.
struct SetupReply {
    set_cookies: Vec<String>,
    body: Value,
}

/// Reads a setup endpoint's answer: `Set-Cookie`s and a JSON body.
fn read_reply(response: ureq::Response, what: &str) -> Result<SetupReply, String> {
    let set_cookies: Vec<String> = response.all("set-cookie").into_iter().map(str::to_string).collect();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(8 << 20)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading {what}: {e}"))?;
    let body: Value = serde_json::from_slice(&body).map_err(|e| format!("bad {what} JSON: {e}"))?;
    Ok(SetupReply { set_cookies, body })
}

/// `/validate` with `cookie`, the answer read but not judged.
fn post_validate(
    agent: &ureq::Agent,
    setup_url: &str,
    cookie: &str,
    params: &BTreeMap<String, String>,
    dsid: Option<&str>,
) -> Result<SetupReply, ValidateError> {
    let url = setup_endpoint(setup_url, "validate", params, dsid).map_err(ValidateError::Failed)?;
    let result = agent
        .post(url.as_str())
        .set("Cookie", cookie)
        .set("Origin", "https://www.icloud.com")
        .set("Referer", "https://www.icloud.com/")
        .set("Accept", "application/json")
        .send_bytes(&[]);
    match result {
        Ok(r) => read_reply(r, "/validate").map_err(ValidateError::Failed),
        Err(ureq::Error::Status(401 | 421, _)) => Err(ValidateError::SignedOut),
        Err(ureq::Error::Status(status, _)) => Err(ValidateError::Failed(format!("/validate answered HTTP {status}"))),
        Err(e) => Err(ValidateError::Failed(format!("/validate: {e}"))),
    }
}

pub fn validate(
    agent: &ureq::Agent,
    setup_url: &str,
    cookie: &str,
    params: &BTreeMap<String, String>,
    dsid: Option<&str>,
) -> Result<Validated, ValidateError> {
    let failed = |m: String| ValidateError::Failed(m);
    let SetupReply { set_cookies, body } = post_validate(agent, setup_url, cookie, params, dsid)?;
    let Some(ds_info) = body.get("dsInfo").filter(|v| v.is_object()) else {
        return Err(failed("unexpected /validate response (missing dsInfo)".into()));
    };
    // A sign-in stuck at 2FA also answers 200; it is not signed in.
    let challenge = |v: &Value| v.get("hsaChallengeRequired") == Some(&Value::Bool(true));
    if challenge(&body) || challenge(ds_info) {
        return Err(ValidateError::SignedOut);
    }
    let text = |k: &str| ds_info.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let (Some(dsid), Some(apple_id)) = (text("dsid"), text("appleId")) else {
        return Err(failed("/validate response lacks dsid or appleId".into()));
    };
    let webservices = body
        .get("webservices")
        .and_then(Value::as_object)
        .map(|services| {
            services
                .iter()
                .filter_map(|(key, service)| Some((key.clone(), service.get("url")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Validated {
        dsid: dsid.to_string(),
        apple_id: apple_id.to_string(),
        webservices,
        set_cookies,
    })
}

/// A one-factor Find My sign-in: a fresh jar of its own.
#[derive(Debug)]
pub struct FindMyLogin {
    /// Every cookie Apple set, `X-APPLE-WEBAUTH-FMIP` among them.
    pub cookies: Vec<Cookie>,
    /// The account Apple says it signed in, if it said.
    pub dsid: Option<String>,
}

#[derive(Debug)]
pub enum LoginError {
    /// 401/403: Apple refused the Apple ID or password. Not worth retrying
    /// until the password changes.
    Rejected,
    /// No answer, another status, an answer without Find My's cookie.
    Failed(String),
}

/// Find My's one-factor sign-in, as www.icloud.com/find's password prompt
/// (and pyicloud's `_authenticate_with_credentials_service("find")`) does it:
/// `POST /setup/ws/1/accountLogin` with `{"appName": "find", "apple_id",
/// "password"}` and no cookies at all, so it starts a session of its own
/// and never touches the main one (a second holder of the main session's
/// token gets that session ended). Apple answers with a jar that Find My
/// accepts although `/validate` would still want 2FA
/// (`hsaChallengeRequired`), which is expected here. If the login itself
/// set no `X-APPLE-WEBAUTH-FMIP`, one `/validate` on the new jar (never
/// judged by `hsaChallengeRequired`) collects it; it also names the dsid
/// when the login's answer did not.
///
/// `params` are the client params to send (a fresh clientId); `dsid` the
/// account's, sent as the web client does.
pub fn find_my_login(
    agent: &ureq::Agent,
    setup_url: &str,
    apple_id: &str,
    password: &str,
    params: &BTreeMap<String, String>,
    dsid: Option<&str>,
) -> Result<FindMyLogin, LoginError> {
    let failed = |m: String| LoginError::Failed(m);
    let url = setup_endpoint(setup_url, "accountLogin", params, dsid).map_err(failed)?;
    let body = serde_json::json!({"appName": "find", "apple_id": apple_id, "password": password});
    let result = agent
        .post(url.as_str())
        .set("Origin", "https://www.icloud.com")
        .set("Referer", "https://www.icloud.com/")
        .set("Accept", "application/json")
        // Like pyicloud's `data=json.dumps(...)`: a JSON body, no Content-Type.
        .send_bytes(body.to_string().as_bytes());
    let reply = match result {
        Ok(r) => read_reply(r, "accountLogin").map_err(failed)?,
        Err(ureq::Error::Status(401 | 403, _)) => return Err(LoginError::Rejected),
        Err(ureq::Error::Status(status, _)) => return Err(failed(format!("accountLogin answered HTTP {status}"))),
        Err(e) => return Err(failed(format!("accountLogin: {e}"))),
    };
    let now = crate::daemon::now_unix();
    let mut jar = Vec::new();
    cookies::merge_set_cookies(&mut jar, &reply.set_cookies, now);
    let mut found_dsid = body_dsid(&reply.body);
    if !cookies::find_my_cookie(&jar, now) || found_dsid.is_none() {
        match post_validate(agent, setup_url, &cookies::header(&jar, now), params, dsid) {
            Ok(v) => {
                cookies::merge_set_cookies(&mut jar, &v.set_cookies, now);
                found_dsid = found_dsid.or_else(|| body_dsid(&v.body));
            }
            // Only read for cookies and the dsid; the login's own answer
            // decides.
            Err(ValidateError::SignedOut | ValidateError::Failed(_)) => {}
        }
    }
    if !cookies::find_my_cookie(&jar, now) {
        return Err(failed(format!("accountLogin set no {} cookie", cookies::FIND_MY)));
    }
    Ok(FindMyLogin {
        dsid: found_dsid.or_else(|| cookies::user_dsid(&jar)),
        cookies: jar,
    })
}

/// `dsInfo.dsid` of a setup answer.
fn body_dsid(body: &Value) -> Option<String> {
    let dsid = body.get("dsInfo")?.get("dsid")?;
    dsid.as_str()
        .map(str::to_string)
        .or_else(|| dsid.as_u64().map(|n| n.to_string()))
        .filter(|d| !d.is_empty())
}
