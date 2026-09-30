//! Transports under [`super::Database`].
//!
//! The cassette and request-log types below are the Rust side of the format
//! documented in `tests/differential/README.md`; the Node driver
//! (`tests/differential/driver.mts`) reads and writes the same JSON, so a
//! vault cloned by icloud-md and one cloned by this crate can be served
//! identical responses and their requests compared.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::CkError;

/// What `Database` needs from the network.
pub trait Transport {
    /// POST `body` as JSON to `path` (relative to the account's
    /// `ckdatabasews` base URL, starting with `/database/1/`, query string
    /// included) and return the parsed JSON answer. Non-2xx is
    /// `CkError::Http`; 421/401 that icloud-sessiond confirms is
    /// `CkError::SignInRequired`.
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError>;

    /// GET an absolute (signed, cookie-less) asset URL into `dest`; returns
    /// the byte count.
    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError>;

    /// [`Transport::download`] into memory (a note's `TextDataAsset`). The
    /// default goes through a private temp file.
    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("icloud-notes-sync-{}-{n}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        let dest = dir.join("asset");
        let result = self.download(url, &dest).and_then(|_| Ok(fs::read(&dest)?));
        let _ = fs::remove_dir_all(&dir);
        result
    }
}

impl<T: Transport + ?Sized> Transport for &T {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        (**self).post_json(path, body)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        (**self).download(url, dest)
    }

    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        (**self).download_bytes(url)
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        (**self).post_json(path, body)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        (**self).download(url, dest)
    }

    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        (**self).download_bytes(url)
    }
}

/// The real thing: requests go through icloud-session.
pub struct LiveTransport {
    pub session: icloud_session::Session,
    /// `webservices().url("ckdatabasews")`, e.g.
    /// `https://p43-ckdatabasews.icloud.com:443`.
    pub base: String,
}

impl LiveTransport {
    /// `Session::connect()` + the `ckdatabasews` URL. No ckdatabasews in the
    /// webservices map is icloud-md's `NotesUnavailableError`.
    pub fn connect() -> Result<LiveTransport, CkError> {
        LiveTransport::from_session(icloud_session::Session::connect()?)
    }

    /// [`LiveTransport::connect`] over an existing session.
    pub fn from_session(session: icloud_session::Session) -> Result<LiveTransport, CkError> {
        let base = session
            .webservices()?
            .url("ckdatabasews")
            .ok_or(CkError::NotesUnavailable)?
            .trim_end_matches('/')
            .to_owned();
        Ok(LiveTransport { session, base })
    }

    /// The signed-in account's dsid (`--account` is checked against it).
    pub fn dsid(&self) -> &str {
        self.session.dsid()
    }

    pub fn apple_id(&self) -> &str {
        self.session.apple_id()
    }
}

impl Transport for LiveTransport {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        let response = self.session.post_json(&format!("{}{path}", self.base), body)?;
        serde_json::from_slice(&response.body)
            .map_err(|e| CkError::UnexpectedResponse(format!("invalid JSON from ckdatabasews: {e}")))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        Ok(self.session.download(url, dest)?)
    }
}

/// Serves requests from a cassette (see [`Cassette`]) and, with a record
/// path, rewrites a [`RequestLog`] there after every request, in the same
/// shape the Node driver writes, so the two can be diffed.
pub struct ReplayTransport {
    cassette: Cassette,
    ck_host: String,
    record: Option<PathBuf>,
    state: Mutex<ReplayState>,
}

#[derive(Default)]
struct ReplayState {
    used: HashSet<usize>,
    log: RequestLog,
}

/// Per-session query parameters whose values say nothing about the request.
const QUERY_NOISE: &[&str] = &[
    "clientId",
    "clientBuildNumber",
    "clientMasteringNumber",
    "dsid",
    "requestId",
];

pub const DEFAULT_CKDATABASEWS_URL: &str = "https://p00-ckdatabasews.icloud.com:443";

enum Answer {
    Json(Value),
    Bytes(Vec<u8>),
}

impl ReplayTransport {
    /// Loads `cassette`; `record` is where the request log goes.
    pub fn open(cassette: &Path, record: Option<PathBuf>) -> Result<ReplayTransport, CkError> {
        ReplayTransport::from_cassette(Cassette::load(cassette)?, record)
    }

    pub fn from_cassette(cassette: Cassette, record: Option<PathBuf>) -> Result<ReplayTransport, CkError> {
        if cassette.version != 1 {
            return Err(CkError::Other(format!(
                "unsupported cassette version {}",
                cassette.version
            )));
        }
        let ck_host = Url::parse(cassette.base_url())
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .ok_or_else(|| CkError::Other(format!("bad ckdatabasewsUrl {}", cassette.base_url())))?;
        Ok(ReplayTransport {
            cassette,
            ck_host,
            record,
            state: Mutex::new(ReplayState::default()),
        })
    }

    /// The cassette's account (what `--account` must match).
    pub fn account(&self) -> &CassetteAccount {
        &self.cassette.account
    }

    /// Everything requested so far.
    pub fn request_log(&self) -> RequestLog {
        self.lock().log.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReplayState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Logs the request, answers it from the first matching unused
    /// interaction (HTTP 599 when none matches, as the Node driver does).
    fn serve(&self, method: &str, url: &Url, body: Option<&Value>) -> Result<Answer, CkError> {
        let service = if url.host_str() == Some(self.ck_host.as_str()) {
            "ckdatabasews"
        } else {
            "other"
        };
        let path = if service == "other" {
            format!("{}{}", url.origin().ascii_serialization(), url.path())
        } else {
            url.path().to_owned()
        };
        let query: BTreeMap<String, String> = url
            .query_pairs()
            .filter(|(k, _)| !QUERY_NOISE.contains(&k.as_ref()))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        let mut state = self.lock();
        let index = self
            .cassette
            .interactions
            .iter()
            .enumerate()
            .position(|(i, interaction)| {
                let request = &interaction.request;
                if (state.used.contains(&i) && !interaction.repeat) || !request.method.eq_ignore_ascii_case(method) {
                    return false;
                }
                let place_matches = if service == "ckdatabasews" {
                    request.path.as_deref() == Some(url.path())
                } else {
                    match request.url.as_deref().and_then(|u| Url::parse(u).ok()) {
                        None => false,
                        Some(wanted) => {
                            let same_query = wanted.query().unwrap_or("").is_empty() || wanted.query() == url.query();
                            wanted.origin() == url.origin() && wanted.path() == url.path() && same_query
                        }
                    }
                };
                place_matches && request.body.as_ref().is_none_or(|b| Some(b) == body)
            });
        state.log.requests.push(LoggedRequest {
            method: method.to_owned(),
            service: service.to_owned(),
            path: path.clone(),
            query,
            body: body.cloned(),
            matched: index,
        });
        if let Some(i) = index {
            state.used.insert(i);
        }
        self.write_log(&state.log)?;
        drop(state);

        let Some(index) = index else {
            eprintln!("[driver] no cassette interaction for {method} {path}");
            return Err(CkError::Http {
                status: 599,
                body: r#"{"error":"no cassette interaction matched"}"#.into(),
            });
        };
        let response = &self.cassette.interactions[index].response;
        let bytes = response.body_base64.as_deref().map(crate::js::base64_decode);
        if !(200..300).contains(&response.status) {
            let body = match &bytes {
                Some(b) => String::from_utf8_lossy(b).into_owned(),
                None => response
                    .body
                    .as_ref()
                    .unwrap_or(&Value::Object(Default::default()))
                    .to_string(),
            };
            return Err(CkError::Http {
                status: response.status,
                body,
            });
        }
        Ok(match bytes {
            Some(b) => Answer::Bytes(b),
            None => Answer::Json(response.body.clone().unwrap_or(Value::Object(Default::default()))),
        })
    }

    fn write_log(&self, log: &RequestLog) -> Result<(), CkError> {
        let Some(path) = &self.record else {
            return Ok(());
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(log).map_err(|e| CkError::Other(e.to_string()))?;
        fs::write(path, text + "\n")?;
        Ok(())
    }
}

impl Transport for ReplayTransport {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        let url = Url::parse(&format!("{}{path}", self.cassette.base_url().trim_end_matches('/')))
            .map_err(|e| CkError::Other(format!("bad request path {path}: {e}")))?;
        match self.serve("POST", &url, Some(body))? {
            Answer::Json(v) => Ok(v),
            Answer::Bytes(b) => serde_json::from_slice(&b)
                .map_err(|e| CkError::UnexpectedResponse(format!("invalid JSON from ckdatabasews: {e}"))),
        }
    }

    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        let parsed = Url::parse(url).map_err(|e| CkError::Other(format!("bad asset URL {url}: {e}")))?;
        Ok(match self.serve("GET", &parsed, None)? {
            Answer::Bytes(b) => b,
            Answer::Json(v) => v.to_string().into_bytes(),
        })
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        let bytes = self.download_bytes(url)?;
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut tmp_name = dest.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".{}.tmp", std::process::id()));
        let tmp = dest.with_file_name(tmp_name);
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, dest)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(bytes.len() as u64)
    }
}

// --- cassette format (tests/differential/README.md) ------------------------

/// A recorded or hand-written set of server answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cassette {
    /// Always 1.
    pub version: u32,
    pub account: CassetteAccount,
    /// Base URL of the fake ckdatabasews host; default
    /// `https://p00-ckdatabasews.icloud.com:443`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ckdatabasews_url: Option<String>,
    /// Raw `/validate` answer for the Node side; synthesized from `account`
    /// and `ckdatabasewsUrl` when absent. The Rust side never calls
    /// `/validate` (icloud-session does), so it ignores this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate: Option<Value>,
    pub interactions: Vec<Interaction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassetteAccount {
    pub dsid: String,
    pub apple_id: String,
}

/// One request/answer pair. Matching: the first not-yet-used interaction
/// whose method matches, whose `path` (ckdatabasews) or `url` (anything else)
/// matches with the query string ignored (unless the cassette's `url` has
/// one), and whose `body`, when given, is JSON-equal to the request body.
/// `repeat: true` interactions are never used up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub request: CassetteRequest,
    pub response: CassetteResponse,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeat: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassetteRequest {
    pub method: String,
    /// ckdatabasews path without query, e.g.
    /// `/database/1/com.apple.notes/production/private/changes/zone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Absolute URL for any other host (asset downloads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassetteResponse {
    #[serde(default = "default_status")]
    pub status: u16,
    /// JSON answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// Binary answer (assets), base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
}

fn default_status() -> u16 {
    200
}

impl Cassette {
    pub fn load(path: &Path) -> Result<Cassette, CkError> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| CkError::Other(format!("{}: {e}", path.display())))
    }

    /// `ckdatabasewsUrl` or the default.
    pub fn base_url(&self) -> &str {
        self.ckdatabasews_url.as_deref().unwrap_or(DEFAULT_CKDATABASEWS_URL)
    }
}

/// What a run sent, in order. Written by the Node driver (`--requests`) and
/// by a recording `ReplayTransport`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLog {
    pub requests: Vec<LoggedRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggedRequest {
    pub method: String,
    /// `setup` (`/validate`, Node side only), `ckdatabasews`, or `other`.
    pub service: String,
    /// Path for setup/ckdatabasews, absolute URL without query for `other`.
    pub path: String,
    /// Query parameters minus the per-session noise (`clientId`,
    /// `clientBuildNumber`, `clientMasteringNumber`, `dsid`, `requestId`),
    /// keys sorted.
    pub query: BTreeMap<String, String>,
    /// Parsed JSON body, or the raw string if it wasn't JSON; absent for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// Index of the cassette interaction that answered, or `None` if nothing
    /// matched (the run then got HTTP 599).
    #[serde(default)]
    pub matched: Option<usize>,
}
