//! Shared iCloud web session, read from icloud-md's session file.
//!
//! icloud-md owns sign-in and stores the icloud.com cookie jar in
//! `~/.config/icloud-md/accounts/<dsid>/session.local.json`. This crate
//! reads that file before every request, attaches the jar, and merges any
//! cookies Apple rotates back into it under a lock, written atomically.
//! `/validate` results are cached machine-wide for ten minutes so several
//! apps do not each rotate the token on their own schedule.
//!
//! Environment:
//! - `ICLOUD_MD_CONFIG_DIR` replaces `~/.config/icloud-md`.
//! - `ICLOUD_SESSION_CACHE_DIR` replaces `~/.cache/icloud-session`.
//! - `ICLOUD_SESSION_MOCK=1` gives a fake signed-in session whose
//!   webservices, and every request, go to `ICLOUD_SESSION_MOCK_URL`
//!   (default `http://127.0.0.1:8765`). No file is read or written.
//! - `ICLOUD_MD_BIN` names the icloud-md executable (default `icloud-md`).
//! - `ICLOUD_SESSION_SETUP_URL` replaces `https://setup.icloud.com` (tests).

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub mod expiry;

mod cache;
mod cookies;
mod store;

use cache::Cache;
pub use cache::{MAX_AGE as VALIDATE_MAX_AGE, format_time};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Missing session file, or HTTP 421/401 after icloud-md's silent recovery.
    /// Apps show a sign-in banner that runs `icloud-session reauthenticate`.
    #[error("sign in to iCloud required")]
    SignInRequired,
    #[error("session file is corrupt: {0}")]
    Corrupt(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The `webservices` map from `/validate`, e.g. `ckdatabasews`, `findme`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Webservices {
    pub urls: HashMap<String, String>,
}

impl Webservices {
    pub fn url(&self, key: &str) -> Option<&str> {
        self.urls.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Network(format!("bad JSON: {e}")))
    }
}

/// Apple's setup host, where `/setup/ws/1/validate` lives.
pub const SETUP_URL: &str = "https://setup.icloud.com";
/// Where mock mode sends everything unless `ICLOUD_SESSION_MOCK_URL` says otherwise.
pub const DEFAULT_MOCK_URL: &str = "http://127.0.0.1:8765";
/// The dsid of the mock session.
pub const MOCK_DSID: &str = "mock";
/// The Apple ID of the mock session.
pub const MOCK_APPLE_ID: &str = "mock@example.com";
/// Webservices keys the mock session reports, all pointing at the mock URL.
pub const MOCK_WEBSERVICES: &[&str] = &[
    "account",
    "calendar",
    "ckdatabasews",
    "ckdeviceservice",
    "contacts",
    "docws",
    "drivews",
    "findme",
    "iworkexportws",
    "keyvalue",
    "photos",
    "photosupload",
    "push",
    "reminders",
    "streams",
    "ubiquity",
    "uploadimagews",
];

const ORIGIN: &str = "https://www.icloud.com";
const REFERER: &str = "https://www.icloud.com/";

/// Where things live. [`Config::from_env`] is what [`Session::load`] and
/// [`status`] use; tests and tools can build one pointing anywhere.
#[derive(Debug, Clone)]
pub struct Config {
    /// icloud-md's `accounts` directory (`~/.config/icloud-md/accounts`).
    pub accounts_dir: PathBuf,
    /// Validate cache directory (`~/.cache/icloud-session`).
    pub cache_dir: PathBuf,
    /// Base URL of Apple's setup service ([`SETUP_URL`]).
    pub setup_url: String,
    /// `Some(base)` in mock mode: no file access, everything goes to `base`.
    pub mock_url: Option<String>,
    /// The icloud-md executable `reauthenticate` runs.
    pub icloud_md_bin: PathBuf,
    /// Cloned icloud-md folders `reauthenticate` may run in, checked in order
    /// for one bound to the session's account: `ICLOUD_NOTES_VAULT`, then
    /// iCloud Notes' default vault.
    pub notes_vaults: Vec<PathBuf>,
}

impl Config {
    /// Real paths, honouring `ICLOUD_MD_CONFIG_DIR`, `ICLOUD_SESSION_CACHE_DIR`,
    /// `XDG_CACHE_HOME`, `ICLOUD_SESSION_MOCK`, `ICLOUD_SESSION_MOCK_URL`,
    /// `ICLOUD_MD_BIN`, and (for tests) `ICLOUD_SESSION_SETUP_URL`.
    pub fn from_env() -> Config {
        let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = env("HOME").unwrap_or_else(|| PathBuf::from("/"));
        let config_dir = env("ICLOUD_MD_CONFIG_DIR").unwrap_or_else(|| home.join(".config/icloud-md"));
        let cache_dir = env("ICLOUD_SESSION_CACHE_DIR").unwrap_or_else(|| {
            env("XDG_CACHE_HOME")
                .unwrap_or_else(|| home.join(".cache"))
                .join("icloud-session")
        });
        let mock = std::env::var("ICLOUD_SESSION_MOCK").is_ok_and(|v| !v.is_empty() && v != "0");
        let mock_url = mock.then(|| {
            std::env::var("ICLOUD_SESSION_MOCK_URL")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_MOCK_URL.to_string())
        });
        Config {
            accounts_dir: config_dir.join("accounts"),
            cache_dir,
            setup_url: std::env::var("ICLOUD_SESSION_SETUP_URL")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| SETUP_URL.to_string()),
            mock_url,
            icloud_md_bin: env("ICLOUD_MD_BIN").unwrap_or_else(|| PathBuf::from("icloud-md")),
            notes_vaults: env("ICLOUD_NOTES_VAULT")
                .into_iter()
                .chain([documents_dir(&home).join("icloud-notes")])
                .collect(),
        }
    }

    /// Paths under one icloud-md config dir and one cache dir, real Apple URLs.
    pub fn at(icloud_md_config_dir: &Path, cache_dir: &Path) -> Config {
        Config {
            accounts_dir: icloud_md_config_dir.join("accounts"),
            cache_dir: cache_dir.to_path_buf(),
            setup_url: SETUP_URL.to_string(),
            mock_url: None,
            icloud_md_bin: PathBuf::from("icloud-md"),
            notes_vaults: Vec::new(),
        }
    }

    /// A mock-mode config sending everything to `base_url`.
    pub fn mock(base_url: &str) -> Config {
        Config {
            mock_url: Some(base_url.trim_end_matches('/').to_string()),
            ..Config::at(Path::new("/nonexistent"), Path::new("/nonexistent"))
        }
    }

    fn account_dir(&self, dsid: &str) -> PathBuf {
        self.accounts_dir.join(dsid)
    }

    fn session_path(&self, dsid: &str) -> PathBuf {
        self.account_dir(dsid).join("session.local.json")
    }

    fn cache_path(&self, dsid: &str) -> PathBuf {
        self.cache_dir.join(format!("{dsid}.json"))
    }

    /// The account to use: the one whose session file was written last.
    /// Accounts without a session file come after, newest directory first.
    fn newest_account(&self) -> Option<String> {
        let entries = fs::read_dir(&self.accounts_dir).ok()?;
        let mut accounts: Vec<(Option<SystemTime>, Option<SystemTime>, String)> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let dsid = e.file_name().into_string().ok()?;
                let session = mtime(&self.session_path(&dsid));
                let dir = mtime(&e.path());
                Some((session, dir, dsid))
            })
            .collect();
        accounts.sort();
        accounts.pop().map(|(_, _, dsid)| dsid)
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// One signed-in Apple account. Cheap to clone; every request re-reads the
/// session file, so a clone never holds stale cookies.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    dsid: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("dsid", &self.inner.dsid)
            .field("mock", &self.inner.config.mock_url.is_some())
            .finish()
    }
}

/// The request a public method asked for, before cookies and params.
struct Request<'a> {
    method: &'a str,
    url: &'a str,
    content_type: Option<&'a str>,
    body: Option<&'a [u8]>,
    client_params: bool,
    accept: &'a str,
}

impl Session {
    /// Newest account under `~/.config/icloud-md/accounts`. `SignInRequired` if none.
    /// With `ICLOUD_SESSION_MOCK=1`, a fake session (see `mock` docs in README).
    pub fn load() -> Result<Session> {
        Session::load_with(Config::from_env())
    }

    /// A specific account directory's session.
    pub fn load_dsid(dsid: &str) -> Result<Session> {
        Session::load_dsid_with(Config::from_env(), dsid)
    }

    /// [`Session::load`] with explicit paths.
    pub fn load_with(config: Config) -> Result<Session> {
        if config.mock_url.is_some() {
            return Ok(Session::new(config, MOCK_DSID.to_string()));
        }
        let dsid = config.newest_account().ok_or(Error::SignInRequired)?;
        Session::load_dsid_with(config, &dsid)
    }

    /// [`Session::load_dsid`] with explicit paths.
    pub fn load_dsid_with(config: Config, dsid: &str) -> Result<Session> {
        if config.mock_url.is_some() {
            return Ok(Session::new(config, dsid.to_string()));
        }
        if dsid.is_empty() || dsid.contains('/') || dsid == "." || dsid == ".." {
            return Err(Error::SignInRequired);
        }
        store::read_session(&config.session_path(dsid))?;
        Ok(Session::new(config, dsid.to_string()))
    }

    fn new(config: Config, dsid: String) -> Session {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(120))
            .timeout_write(Duration::from_secs(120))
            .user_agent(concat!("icloud-session/", env!("CARGO_PKG_VERSION")))
            .build();
        Session {
            inner: Arc::new(Inner { config, dsid, agent }),
        }
    }

    pub fn dsid(&self) -> &str {
        &self.inner.dsid
    }

    /// The config this session was loaded with.
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Path of the icloud-md session file this session reads.
    pub fn session_path(&self) -> PathBuf {
        self.inner.config.session_path(&self.inner.dsid)
    }

    fn mock_url(&self) -> Option<&str> {
        self.inner.config.mock_url.as_deref()
    }

    fn cache_path(&self) -> PathBuf {
        self.inner.config.cache_path(&self.inner.dsid)
    }

    /// Apple ID from the last `/validate`, if known.
    pub fn apple_id(&self) -> Option<String> {
        if self.mock_url().is_some() {
            return Some(MOCK_APPLE_ID.to_string());
        }
        Cache::read(&self.cache_path())
            .apple_id
            .or_else(|| meta_apple_id(&self.inner.config.account_dir(&self.inner.dsid)))
    }

    /// When `/validate` last succeeded on this machine, from the shared cache.
    pub fn validated_at(&self) -> Option<SystemTime> {
        if self.mock_url().is_some() {
            return Some(SystemTime::now());
        }
        Cache::read(&self.cache_path()).validated_at()
    }

    /// Cached `/validate` result if younger than ~10 minutes, otherwise
    /// validates under the lock, merges rotated cookies, updates the cache.
    pub fn webservices(&self) -> Result<Webservices> {
        if let Some(base) = self.mock_url() {
            return Ok(mock_webservices(base));
        }
        let cache_path = self.cache_path();
        if let Some(urls) = Cache::read(&cache_path).fresh_webservices(SystemTime::now()) {
            return Ok(Webservices { urls: urls.clone() });
        }
        let session_path = self.session_path();
        let _lock = store::lock(&session_path)?;
        // Another process may have validated while we waited for the lock.
        let cache = Cache::read(&cache_path);
        if let Some(urls) = cache.fresh_webservices(SystemTime::now()) {
            return Ok(Webservices { urls: urls.clone() });
        }
        self.validate_locked(cache)
    }

    /// POST `/setup/ws/1/validate` as icloud-md's `checkAuthentication`
    /// does. The caller holds the session lock.
    fn validate_locked(&self, mut cache: Cache) -> Result<Webservices> {
        let session_path = self.session_path();
        let session = store::read_session(&session_path)?;
        let mut url = url::Url::parse(&format!("{}/setup/ws/1/validate", self.inner.config.setup_url))
            .map_err(|e| Error::Network(format!("bad setup URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("clientBuildNumber", session.field("clientBuildNumber"))
            .append_pair("clientMasteringNumber", session.field("clientMasteringNumber"))
            .append_pair("clientId", session.field("clientId"))
            .append_pair("requestId", &uuid::Uuid::new_v4().to_string())
            .append_pair("dsid", &self.inner.dsid);
        let result = self
            .inner
            .agent
            .post(url.as_str())
            .set("Cookie", session.cookie())
            .set("Origin", ORIGIN)
            .set("Referer", REFERER)
            .set("Accept", "application/json")
            .send_bytes(&[]);
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(401 | 421, _)) => {
                mark_sign_in_required(&mut cache, &self.cache_path());
                return Err(Error::SignInRequired);
            }
            Err(ureq::Error::Status(status, response)) => {
                return Err(Error::Http {
                    status,
                    body: read_body_lossy(response),
                });
            }
            Err(e) => return Err(Error::Network(e.to_string())),
        };
        let set_cookies = set_cookie_headers(&response);
        let body = read_body(response)?;
        let body: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| Error::Network(format!("bad /validate JSON: {e}")))?;
        let Some(ds_info) = body.get("dsInfo").filter(|v| v.is_object()) else {
            return Err(Error::Network("unexpected /validate response (missing dsInfo)".into()));
        };
        // A sign-in stuck at 2FA also answers 200; it is not signed in.
        let challenge = |v: &serde_json::Value| v.get("hsaChallengeRequired") == Some(&serde_json::Value::Bool(true));
        if challenge(&body) || challenge(ds_info) {
            mark_sign_in_required(&mut cache, &self.cache_path());
            return Err(Error::SignInRequired);
        }
        store::merge_rotation_locked(&session_path, &set_cookies)?;

        let urls: HashMap<String, String> = body
            .get("webservices")
            .and_then(|v| v.as_object())
            .map(|services| {
                services
                    .iter()
                    .filter_map(|(key, service)| Some((key.clone(), service.get("url")?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        cache.validated_at = Some(cache::format_time_millis(SystemTime::now()));
        cache.webservices = Some(urls.clone());
        if let Some(apple_id) = ds_info.get("appleId").and_then(|v| v.as_str()) {
            cache.apple_id = Some(apple_id.to_string());
        }
        cache.sign_in_required_at = None;
        cache.write(&self.cache_path())?;
        Ok(Webservices { urls })
    }

    /// GET with the cookie jar, `Origin: https://www.icloud.com`, and the
    /// client query params (clientBuildNumber, clientMasteringNumber, clientId, dsid)
    /// appended. Set-Cookie rotation is merged back into the session file.
    /// 421/401 → `SignInRequired`; other non-2xx → `Http`.
    pub fn get(&self, url: &str) -> Result<Response> {
        self.request(Request {
            method: "GET",
            url,
            content_type: None,
            body: None,
            client_params: true,
            accept: "application/json",
        })
    }

    /// POST a JSON body, same behaviour as `get`.
    pub fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<Response> {
        let bytes = serde_json::to_vec(body).map_err(|e| Error::Network(format!("cannot encode JSON: {e}")))?;
        self.request(Request {
            method: "POST",
            url,
            content_type: Some("application/json"),
            body: Some(&bytes),
            client_params: true,
            accept: "application/json",
        })
    }

    /// POST raw bytes with a content type (uploads), same behaviour as `get`.
    pub fn post_bytes(&self, url: &str, content_type: &str, body: Vec<u8>) -> Result<Response> {
        self.request(Request {
            method: "POST",
            url,
            content_type: Some(content_type),
            body: Some(&body),
            client_params: true,
            accept: "application/json",
        })
    }

    fn request(&self, request: Request<'_>) -> Result<Response> {
        let response = self.send(&request)?;
        let status = response.status();
        let body = read_body(response)?;
        Ok(Response { status, body })
    }

    /// Streams a URL to `dest` (written to a temp file, renamed on success).
    /// Returns bytes written. Cookies attached; no client params appended.
    pub fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let response = self.send(&Request {
            method: "GET",
            url,
            content_type: None,
            body: None,
            client_params: false,
            accept: "*/*",
        })?;
        let tmp = store::temp_sibling(dest);
        let result = (|| -> Result<u64> {
            let mut file = fs::File::create(&tmp)?;
            let written = io::copy(&mut response.into_reader(), &mut file)
                .map_err(|e| Error::Network(format!("download interrupted: {e}")))?;
            file.sync_all()?;
            fs::rename(&tmp, dest)?;
            Ok(written)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Sends one request with a freshly read cookie jar. Returns the 2xx
    /// response after merging its `Set-Cookie`s; maps every other status.
    fn send(&self, request: &Request<'_>) -> Result<ureq::Response> {
        let (url, cookie) = self.prepare(request)?;
        let mut req = self
            .inner
            .agent
            .request(request.method, &url)
            .set("Origin", ORIGIN)
            .set("Referer", REFERER)
            .set("Accept", request.accept);
        if !cookie.is_empty() {
            req = req.set("Cookie", &cookie);
        }
        if let Some(content_type) = request.content_type {
            req = req.set("Content-Type", content_type);
        }
        let result = match request.body {
            Some(body) => req.send_bytes(body),
            None => req.call(),
        };
        match result {
            Ok(response) => {
                if self.mock_url().is_none() {
                    store::merge_rotation(&self.session_path(), &set_cookie_headers(&response))?;
                }
                Ok(response)
            }
            Err(ureq::Error::Status(401 | 421, _)) => {
                if self.mock_url().is_none() {
                    let cache_path = self.cache_path();
                    let _lock = store::lock(&self.session_path());
                    mark_sign_in_required(&mut Cache::read(&cache_path), &cache_path);
                }
                Err(Error::SignInRequired)
            }
            Err(ureq::Error::Status(status, response)) => Err(Error::Http {
                status,
                body: read_body_lossy(response),
            }),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// The final URL (client params added, mock rewrite) and cookie header.
    fn prepare(&self, request: &Request<'_>) -> Result<(String, String)> {
        let bad_url = |e: url::ParseError| Error::Network(format!("bad URL {}: {e}", request.url));
        let mut url = url::Url::parse(request.url).map_err(bad_url)?;
        let (cookie, params) = match self.mock_url() {
            Some(base) => {
                if !request.url.starts_with(base) {
                    let mut rewritten = format!("{base}{}", url.path());
                    if let Some(query) = url.query() {
                        rewritten.push('?');
                        rewritten.push_str(query);
                    }
                    url = url::Url::parse(&rewritten).map_err(bad_url)?;
                }
                let params = ["mock", "mock", "mock"].map(String::from);
                (String::new(), params)
            }
            None => {
                let session = store::read_session(&self.session_path())?;
                let params = ["clientBuildNumber", "clientMasteringNumber", "clientId"]
                    .map(|name| session.field(name).to_string());
                (session.cookie().to_string(), params)
            }
        };
        if request.client_params {
            let present: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
            let [build, mastering, client_id] = &params;
            let wanted = [
                ("clientBuildNumber", build.as_str()),
                ("clientMasteringNumber", mastering.as_str()),
                ("clientId", client_id.as_str()),
                ("dsid", self.inner.dsid.as_str()),
            ];
            let missing: Vec<_> = wanted.iter().filter(|(k, _)| !present.iter().any(|p| p == k)).collect();
            if !missing.is_empty() {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in missing {
                    pairs.append_pair(k, v);
                }
            }
        }
        Ok((url.into(), cookie))
    }

    /// Runs `icloud-md reauthenticate` interactively and waits for it.
    ///
    /// icloud-md only signs in again for a cloned Notes folder. This runs it
    /// in the first of [`Config::notes_vaults`] bound to the session's
    /// account, else in the current directory; see
    /// [`Session::reauthenticate_in`] to name the folder.
    pub fn reauthenticate() -> Result<()> {
        reauthenticate_with(&Config::from_env(), None)
    }

    /// `icloud-md reauthenticate <dir>` for a cloned icloud-md folder.
    pub fn reauthenticate_in(dir: &Path) -> Result<()> {
        reauthenticate_with(&Config::from_env(), Some(dir))
    }

    /// When the persistent X-APPLE-WEBAUTH-TOKEN cookie expires, read from
    /// the Chromium Cookies DB in this account's browser profile.
    pub fn expires_at(&self) -> Option<SystemTime> {
        if self.mock_url().is_some() {
            return Some(SystemTime::now() + Duration::from_secs(30 * 24 * 3600));
        }
        expiry::latest_token_expiry(&self.inner.config.account_dir(&self.inner.dsid))
    }
}

/// Runs `<config.icloud_md_bin> reauthenticate [dir]` with inherited stdio.
/// A non-zero exit (cancelled, failed 2FA) is `SignInRequired`.
pub fn reauthenticate_with(config: &Config, dir: Option<&Path>) -> Result<()> {
    if config.mock_url.is_some() {
        return Ok(());
    }
    let found;
    let dir = match dir {
        Some(dir) => Some(dir),
        None => {
            found = notes_vault_for(config);
            found.as_deref()
        }
    };
    let mut command = Command::new(&config.icloud_md_bin);
    command.arg("reauthenticate");
    if let Some(dir) = dir {
        command.arg(dir);
    }
    let status = command.status().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Error::Io(io::Error::new(
                e.kind(),
                format!(
                    "{} not found; install it with `npm install -g icloud-md`",
                    config.icloud_md_bin.display()
                ),
            ))
        } else {
            Error::Io(e)
        }
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::SignInRequired)
    }
}

fn mark_sign_in_required(cache: &mut Cache, cache_path: &Path) {
    cache.sign_in_required_at = Some(cache::format_time_millis(SystemTime::now()));
    // Best effort: the error itself is what the caller acts on.
    let _ = cache.write(cache_path);
}

fn meta_apple_id(account_dir: &Path) -> Option<String> {
    let bytes = fs::read(account_dir.join("meta.json")).ok()?;
    let meta: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    meta.get("appleId")?.as_str().map(str::to_string)
}

fn mock_webservices(base: &str) -> Webservices {
    Webservices {
        urls: MOCK_WEBSERVICES
            .iter()
            .map(|k| (k.to_string(), base.to_string()))
            .collect(),
    }
}

fn set_cookie_headers(response: &ureq::Response) -> Vec<String> {
    response.all("set-cookie").into_iter().map(str::to_string).collect()
}

fn read_body(response: ureq::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| Error::Network(format!("reading response: {e}")))?;
    Ok(body)
}

fn read_body_lossy(response: ureq::Response) -> String {
    let mut body = String::new();
    let _ = response.into_reader().take(64 * 1024).read_to_string(&mut body);
    body
}

/// Offline status for `icloud-session status` (no network call).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
    pub signed_in: bool,
    pub apple_id: Option<String>,
    pub dsid: Option<String>,
    /// RFC 3339 UTC, e.g. "2026-10-28T09:00:00Z".
    pub expires_at: Option<String>,
    pub validated_at: Option<String>,
}

pub fn status() -> Status {
    status_with(&Config::from_env())
}

/// [`status`] with explicit paths. Reads files only, never the network.
///
/// `signed_in` is false when the session file is missing, when Apple
/// answered 421/401 (to any of our processes) after the session file was
/// last written, or when the persistent sign-in cookie has expired.
pub fn status_with(config: &Config) -> Status {
    if config.mock_url.is_some() {
        let now = SystemTime::now();
        return Status {
            signed_in: true,
            apple_id: Some(MOCK_APPLE_ID.to_string()),
            dsid: Some(MOCK_DSID.to_string()),
            expires_at: Some(format_time(now + Duration::from_secs(30 * 24 * 3600))),
            validated_at: Some(format_time(now)),
        };
    }
    let Some(dsid) = config.newest_account() else {
        return Status {
            signed_in: false,
            apple_id: None,
            dsid: None,
            expires_at: None,
            validated_at: None,
        };
    };
    let account_dir = config.account_dir(&dsid);
    let cache = Cache::read(&config.cache_path(&dsid));
    let session_mtime = mtime(&config.session_path(&dsid));
    let expires = expiry::latest_token_expiry(&account_dir);
    let marked = cache.sign_in_required_at();
    let signed_in = match session_mtime {
        None => false,
        Some(written) => marked.is_none_or(|m| m < written) && expires.is_none_or(|e| e > SystemTime::now()),
    };
    Status {
        signed_in,
        apple_id: cache.apple_id.clone().or_else(|| meta_apple_id(&account_dir)),
        dsid: Some(dsid),
        expires_at: expires.map(format_time),
        validated_at: cache.validated_at().map(format_time),
    }
}

/// The first of `config.notes_vaults` that icloud-md cloned for the current
/// session's account (`.icloud-md/state.json` names it), or for any account
/// when there is no session to compare with.
fn notes_vault_for(config: &Config) -> Option<PathBuf> {
    let dsid = Session::load_with(config.clone()).ok().map(|s| s.dsid().to_string());
    config
        .notes_vaults
        .iter()
        .find(|vault| {
            let Ok(raw) = std::fs::read(vault.join(".icloud-md/state.json")) else {
                return false;
            };
            let Ok(state) = serde_json::from_slice::<serde_json::Value>(&raw) else {
                return false;
            };
            let bound = state.pointer("/account/dsid").and_then(serde_json::Value::as_str);
            match (&dsid, bound) {
                (Some(dsid), Some(bound)) => dsid == bound,
                (None, Some(_)) => true,
                _ => false,
            }
        })
        .cloned()
}

/// `XDG_DOCUMENTS_DIR` from the environment or `~/.config/user-dirs.dirs`,
/// else `~/Documents` (where Qt's DocumentsLocation, and so iCloud Notes, looks).
fn documents_dir(home: &Path) -> PathBuf {
    let expand = |value: &str| {
        let value = value.trim().trim_matches('"');
        match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(value),
        }
    };
    if let Some(dir) = std::env::var("XDG_DOCUMENTS_DIR").ok().filter(|v| !v.is_empty()) {
        return expand(&dir);
    }
    let user_dirs = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("user-dirs.dirs");
    std::fs::read_to_string(user_dirs)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("XDG_DOCUMENTS_DIR=").map(expand))
        })
        .unwrap_or_else(|| home.join("Documents"))
}
