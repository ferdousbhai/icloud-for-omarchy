//! `POST setup.icloud.com/setup/ws/1/validate`, made exactly as icloud-md's
//! `checkAuthentication` (`cloudkit/setupClient.js`) makes it.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde_json::Value;

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
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(60))
        .user_agent(concat!("icloud-sessiond/", env!("CARGO_PKG_VERSION")))
        .build()
}

pub fn validate(
    agent: &ureq::Agent,
    setup_url: &str,
    cookie: &str,
    params: &BTreeMap<String, String>,
    dsid: Option<&str>,
) -> Result<Validated, ValidateError> {
    let failed = |m: String| ValidateError::Failed(m);
    let mut url = url::Url::parse(&format!("{setup_url}/setup/ws/1/validate"))
        .map_err(|e| failed(format!("bad setup URL: {e}")))?;
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
    let result = agent
        .post(url.as_str())
        .set("Cookie", cookie)
        .set("Origin", "https://www.icloud.com")
        .set("Referer", "https://www.icloud.com/")
        .set("Accept", "application/json")
        .send_bytes(&[]);
    let response = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(401 | 421, _)) => return Err(ValidateError::SignedOut),
        Err(ureq::Error::Status(status, _)) => return Err(failed(format!("/validate answered HTTP {status}"))),
        Err(e) => return Err(failed(format!("/validate: {e}"))),
    };
    let set_cookies: Vec<String> = response.all("set-cookie").into_iter().map(str::to_string).collect();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(8 << 20)
        .read_to_end(&mut body)
        .map_err(|e| failed(format!("reading /validate: {e}")))?;
    let body: Value = serde_json::from_slice(&body).map_err(|e| failed(format!("bad /validate JSON: {e}")))?;
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
