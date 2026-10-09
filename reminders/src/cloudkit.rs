//! iCloud Reminders over CloudKit web services: the private database of the
//! `com.apple.reminders` container, custom zone `Reminders`. Every call is a
//! POST of a JSON body to
//! `{ckdatabasews}/database/1/com.apple.reminders/production/private/<op>`.
//!
//! The request shapes are timlaing/pyicloud's Reminders service
//! (`services/reminders/{client,_reads,_writes}.py`), the only public
//! description of them; Reminders left CalDAV for this store in iOS 13.
//! All HTTP goes through [`Transport`], which the app backs with
//! `icloud_session::Session` and the tests with a fake server.

use std::sync::Mutex;

use serde_json::{Map, Value, json};

pub const DB_PATH: &str = "/database/1/com.apple.reminders/production/private";
pub const ZONE: &str = "Reminders";
/// Records per changes/zone page (pyicloud asks for 200 too).
const PAGE_LIMIT: u64 = 200;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The iCloud web session is gone: `icloud-session sign-in`.
    #[error("sign in to iCloud required")]
    SignInRequired,
    /// No network (or Apple unreachable): a background run skips its sync.
    #[error("offline: {0}")]
    Offline(String),
    #[error("iCloud answered HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// CloudKit answered 200 but refused a record or the zone.
    #[error("CloudKit {code}: {reason}")]
    CloudKit { code: String, reason: String },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<icloud_session::Error> for Error {
    fn from(e: icloud_session::Error) -> Self {
        match e {
            icloud_session::Error::SignInRequired => Error::SignInRequired,
            icloud_session::Error::Http { status, body } => Error::Http { status, body },
            icloud_session::Error::Offline(m) | icloud_session::Error::Network(m) => Error::Offline(m),
            other => Error::Other(other.to_string()),
        }
    }
}

impl Error {
    /// CloudKit refused a write because the record changed on the server
    /// since its change tag was read.
    pub fn is_conflict(&self) -> bool {
        matches!(self, Error::CloudKit { code, .. } if code == "CONFLICT")
    }
}

/// What the Reminders code needs from an HTTP client with an iCloud session.
pub trait Transport: Send + Sync {
    /// Base URL of a `webservices` entry (`ckdatabasews`).
    fn service_url(&self, key: &str) -> Result<String>;
    fn post_json(&self, url: &str, body: &Value) -> Result<Value>;
    /// The signed-in account's dsid, so a cache of another account's
    /// reminders is never shown.
    fn account(&self) -> Result<String>;
}

/// [`Transport`] over `icloud-session`. Connects lazily, so a missing
/// sign-in surfaces as `SignInRequired` on first use, and drops the
/// connection on `SignInRequired` or a daemon failure so that a later
/// sign-in is picked up without restarting.
#[derive(Default)]
pub struct SessionTransport {
    session: Mutex<Option<icloud_session::Session>>,
}

impl SessionTransport {
    fn with<T>(&self, f: impl FnOnce(&icloud_session::Session) -> icloud_session::Result<T>) -> Result<T> {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(icloud_session::Session::connect()?);
        }
        let result = f(guard.as_ref().expect("just connected"));
        if let Err(icloud_session::Error::SignInRequired | icloud_session::Error::Service(_)) = &result {
            *guard = None;
        }
        Ok(result?)
    }
}

impl Transport for SessionTransport {
    fn service_url(&self, key: &str) -> Result<String> {
        let ws = self.with(|s| s.webservices())?;
        ws.url(key)
            .map(str::to_owned)
            .ok_or_else(|| Error::Other(format!("iCloud did not offer the {key} service for this account")))
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        let response = self.with(|s| s.post_json(url, body))?;
        // A 200 that isn't JSON is CloudKit misbehaving, not the network:
        // parsed here so it isn't reported as offline.
        serde_json::from_slice(&response.body).map_err(|e| {
            let endpoint = url.split('?').next().unwrap_or(url);
            Error::Other(format!("CloudKit answered {endpoint} with something that isn't JSON: {e}"))
        })
    }

    fn account(&self) -> Result<String> {
        self.with(|s| Ok(s.dsid().to_owned()))
    }
}

/// One record as CloudKit sends it, or a tombstone (`deleted`, no type).
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub name: String,
    pub record_type: Option<String>,
    pub change_tag: Option<String>,
    /// A tombstone: the record is gone from the zone.
    pub deleted: bool,
    pub fields: Map<String, Value>,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    /// A per-record refusal (`CONFLICT`, ...), with its reason.
    pub error: Option<(String, String)>,
}

impl Record {
    pub fn from_value(v: &Value) -> Option<Record> {
        let error = v.get("serverErrorCode").and_then(Value::as_str).map(|code| {
            let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
            (code.to_owned(), reason.to_owned())
        });
        Some(Record {
            name: v.get("recordName")?.as_str()?.to_owned(),
            record_type: v.get("recordType").and_then(Value::as_str).map(str::to_owned),
            change_tag: v.get("recordChangeTag").and_then(Value::as_str).map(str::to_owned),
            deleted: v.get("deleted").and_then(Value::as_bool).unwrap_or(false),
            fields: v.get("fields").and_then(Value::as_object).cloned().unwrap_or_default(),
            created_ms: v.pointer("/created/timestamp").and_then(Value::as_i64),
            modified_ms: v.pointer("/modified/timestamp").and_then(Value::as_i64),
            error,
        })
    }

    pub fn is_type(&self, t: &str) -> bool {
        self.record_type.as_deref() == Some(t)
    }

    /// A field's `value` (`None` when absent or null).
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.fields.get(key).and_then(|f| f.get("value")).filter(|v| !v.is_null())
    }

    /// An INT64 or TIMESTAMP field.
    pub fn int(&self, key: &str) -> Option<i64> {
        self.value(key)?.as_i64()
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.value(key).and_then(Value::as_str)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.int(key).is_some_and(|v| v != 0)
    }

    /// The `recordName` a REFERENCE field points at.
    pub fn reference(&self, key: &str) -> Option<&str> {
        self.value(key)?.get("recordName")?.as_str()
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        match self.value(key) {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    }
}

/// One page of changes/zone.
#[derive(Debug)]
pub struct ZoneChanges {
    pub records: Vec<Record>,
    pub sync_token: String,
    pub more_coming: bool,
}

pub struct CloudKit<'t> {
    t: &'t dyn Transport,
    base: String,
}

pub fn zone_id() -> Value {
    json!({ "zoneName": ZONE, "zoneType": "REGULAR_CUSTOM_ZONE" })
}

impl<'t> CloudKit<'t> {
    pub fn connect(t: &'t dyn Transport) -> Result<Self> {
        let root = t.service_url("ckdatabasews")?;
        Ok(Self {
            t,
            base: format!("{}{DB_PATH}", root.trim_end_matches('/')),
        })
    }

    /// The session appends its client params (clientBuildNumber, dsid, ...).
    fn post(&self, op: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/{op}?remapEnums=true&getCurrentSyncToken=true", self.base);
        let v = self.t.post_json(&url, body)?;
        server_error(&v)?;
        Ok(v)
    }

    /// One page of changes/zone for `types`, from `token` (`None`: the whole
    /// zone). A zone-level error (an expired token) is returned, so the
    /// caller can start over without one.
    pub fn zone_changes(&self, types: &[&str], token: Option<&str>) -> Result<ZoneChanges> {
        let mut zone = json!({ "zoneID": zone_id(), "desiredRecordTypes": types, "reverse": false });
        if let Some(t) = token {
            zone["syncToken"] = json!(t);
        }
        let v = self.post("changes/zone", &json!({ "zones": [zone], "resultsLimit": PAGE_LIMIT }))?;
        let z = v
            .get("zones")
            .and_then(Value::as_array)
            .and_then(|zs| zs.first())
            .ok_or_else(|| Error::Other("changes/zone returned no zone".into()))?;
        server_error(z)?;
        Ok(ZoneChanges {
            records: records_of(z),
            sync_token: z
                .get("syncToken")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Other("changes/zone returned no syncToken".into()))?
                .to_owned(),
            more_coming: z.get("moreComing").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// Every record of `types` changed since `token` (all of them without
    /// one), page by page, and the token to ask from next time.
    pub fn all_changes(&self, types: &[&str], token: Option<&str>) -> Result<(Vec<Record>, String)> {
        let mut page = self.zone_changes(types, token)?;
        let mut records = std::mem::take(&mut page.records);
        while page.more_coming {
            page = self.zone_changes(types, Some(&page.sync_token))?;
            records.append(&mut page.records);
        }
        Ok((records, page.sync_token))
    }

    /// Records by name (missing ones come back with an error code).
    pub fn lookup(&self, names: &[&str]) -> Result<Vec<Record>> {
        let records: Vec<Value> = names.iter().map(|n| json!({ "recordName": n })).collect();
        let v = self.post("records/lookup", &json!({ "records": records, "zoneID": zone_id() }))?;
        Ok(records_of(&v))
    }

    /// One records/modify of `operations`, atomic. Returns the records
    /// CloudKit answered with; a per-record refusal is the error.
    pub fn modify(&self, operations: Vec<Value>) -> Result<Vec<Record>> {
        let body = json!({ "operations": operations, "zoneID": zone_id(), "atomic": true });
        let records = records_of(&self.post("records/modify", &body)?);
        if let Some((code, reason)) = records.iter().find_map(|r| r.error.clone()) {
            return Err(Error::CloudKit { code, reason });
        }
        Ok(records)
    }
}

fn server_error(v: &Value) -> Result<()> {
    match v.get("serverErrorCode").and_then(Value::as_str) {
        Some(code) => Err(Error::CloudKit {
            code: code.to_owned(),
            reason: v.get("reason").and_then(Value::as_str).unwrap_or("").to_owned(),
        }),
        None => Ok(()),
    }
}

fn records_of(v: &Value) -> Vec<Record> {
    v.get("records")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Record::from_value).collect())
        .unwrap_or_default()
}
