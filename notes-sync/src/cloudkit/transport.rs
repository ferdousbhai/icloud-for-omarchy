//! Transports under [`super::Database`]. Owner: workstream A.
//!
//! The cassette and request-log types below are the Rust side of the format
//! documented in `tests/differential/README.md`; the Node driver
//! (`tests/differential/driver.ts`) reads and writes the same JSON, so a
//! vault cloned by icloud-md and one cloned by this crate can be served
//! identical responses and their requests compared.
#![allow(unused_variables)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        todo!()
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
        todo!()
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        todo!()
    }
}

/// Serves requests from a cassette (see [`Cassette`]) and, with `record`,
/// appends every request to a [`RequestLog`] written next to it, in the same
/// shape the Node driver writes, so the two can be diffed.
pub struct ReplayTransport {
    pub cassette: PathBuf,
    /// Where to write the request log; `None` = don't record.
    pub record: Option<PathBuf>,
}

impl ReplayTransport {
    /// Environment hook for CLI-level differential runs:
    /// `ICLOUD_NOTES_SYNC_CASSETTE=<cassette.json>` (and optionally
    /// `ICLOUD_NOTES_SYNC_REQUEST_LOG=<out.json>`) makes the binary use a
    /// ReplayTransport instead of icloud-session.
    pub fn from_env() -> Option<ReplayTransport> {
        todo!()
    }
}

impl Transport for ReplayTransport {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        todo!()
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        todo!()
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
    pub query: std::collections::BTreeMap<String, String>,
    /// Parsed JSON body, or the raw string if it wasn't JSON; absent for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// Index of the cassette interaction that answered, or `None` if nothing
    /// matched (the run then got HTTP 599).
    #[serde(default)]
    pub matched: Option<usize>,
}
