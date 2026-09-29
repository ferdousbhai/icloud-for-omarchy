//! A `Transport` that answers from `tests/fixtures/` and records every call.
#![allow(dead_code)]

pub mod fake_server;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use icloud_photos::cloudkit::DB_PATH;
use icloud_photos::transport::{Error, Result, Transport};
use serde_json::Value;

pub const CK_ROOT: &str = "https://p00-ckdatabasews.icloud.com:443";
pub const UPLOAD_ROOT: &str = "https://p00-uploadphotos.icloud.com:443";

#[derive(Debug, Clone)]
pub struct Call {
    pub url: String,
    /// `records/query`, `changes/zone`, `/photosupload/putAsset`, or the full
    /// URL for uploads to a content host.
    pub op: String,
    pub body: Value,
    pub bytes: Option<Vec<u8>>,
}

impl Call {
    pub fn record_type(&self) -> Option<&str> {
        self.body.pointer("/query/recordType").and_then(Value::as_str)
    }

    /// The value of an EQUALS filter in a records/query body.
    pub fn filter(&self, field: &str) -> Option<&Value> {
        self.body
            .pointer("/query/filterBy")?
            .as_array()?
            .iter()
            .find(|f| f["fieldName"] == field)
            .map(|f| &f["fieldValue"]["value"])
    }
}

type Handler = Box<dyn Fn(&Call) -> Result<Value> + Send + Sync>;

pub struct FixtureTransport {
    pub calls: Mutex<Vec<Call>>,
    pub downloads: Mutex<Vec<String>>,
    handler: Handler,
    /// Download URL → bytes; anything else is a 404. URLs containing
    /// `/expired/` answer 403 like an expired signed URL.
    pub files: Mutex<HashMap<String, Vec<u8>>>,
    pub mock: bool,
}

impl FixtureTransport {
    pub fn new(handler: impl Fn(&Call) -> Result<Value> + Send + Sync + 'static) -> Self {
        Self {
            calls: Mutex::default(),
            downloads: Mutex::default(),
            handler: Box::new(handler),
            files: Mutex::default(),
            mock: false,
        }
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn ops(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c.op).collect()
    }

    pub fn serve(&self, url: &str, bytes: &[u8]) {
        self.files.lock().unwrap().insert(url.to_owned(), bytes.to_vec());
    }

    fn call(&self, url: &str, body: Value, bytes: Option<Vec<u8>>) -> Result<Value> {
        let path = url.split('?').next().unwrap_or(url);
        let op = if let Some(i) = path.find(DB_PATH) {
            path[i + DB_PATH.len() + 1..].to_owned()
        } else if let Some(i) = path.find("/photosupload/") {
            path[i..].to_owned()
        } else {
            url.to_owned()
        };
        let call = Call {
            url: url.to_owned(),
            op,
            body,
            bytes,
        };
        self.calls.lock().unwrap().push(call.clone());
        (self.handler)(&call)
    }
}

impl Transport for FixtureTransport {
    fn service_url(&self, key: &str) -> Result<String> {
        match key {
            "ckdatabasews" => Ok(CK_ROOT.into()),
            "photosupload" => Ok(UPLOAD_ROOT.into()),
            other => Err(Error::Other(format!("no {other} service"))),
        }
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        self.call(url, body.clone(), None)
    }

    fn post_file(&self, url: &str, _content_type: &str, path: &Path) -> Result<Value> {
        self.call(url, Value::Null, Some(std::fs::read(path)?))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        self.downloads.lock().unwrap().push(url.to_owned());
        if url.contains("/expired/") {
            return Err(Error::Http {
                status: 403,
                body: "expired".into(),
            });
        }
        let bytes = self.files.lock().unwrap().get(url).cloned().ok_or(Error::Http {
            status: 404,
            body: url.into(),
        })?;
        // Exactly what icloud-session does: a temp file next to `dest`, then
        // a rename over it; no directory is created.
        icloud_photos::transport::write_atomically(dest, &mut bytes.as_slice())
    }

    fn is_mock(&self) -> bool {
        self.mock
    }
}

pub fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

/// Answers the read side of a small library (see tests/fixtures/README.md):
/// assets in two pages split mid-pair, albums with one folder, two albums'
/// memberships, zones/list and master lookups.
pub fn library(call: &Call) -> Result<Value> {
    match call.op.as_str() {
        "records/query" => {
            let rank = call.filter("startRank").and_then(Value::as_i64);
            let parent = call.filter("parentId").and_then(Value::as_str);
            Ok(match (call.record_type().unwrap_or(""), parent, rank) {
                ("CheckIndexingState", ..) => fixture("check_indexing_state.json"),
                ("CPLAlbumByPositionLive", None, _) => fixture("albums_root.json"),
                ("CPLAlbumByPositionLive", Some("A-TRIPS-FOLDER"), _) => fixture("albums_in_folder.json"),
                ("CPLAssetAndMasterByAssetDateWithoutHiddenOrDeleted", None, Some(0)) => fixture("assets_page1.json"),
                ("CPLAssetAndMasterByAssetDateWithoutHiddenOrDeleted", None, Some(2)) => fixture("assets_page2.json"),
                ("CPLContainerRelationLiveByAssetDate", Some("A-FAMILY"), Some(0)) => {
                    fixture("album_family_members.json")
                }
                ("CPLContainerRelationLiveByAssetDate", Some("A-ITALY"), Some(0)) => {
                    fixture("album_italy_members.json")
                }
                _ => fixture("assets_empty.json"),
            })
        }
        "zones/list" => Ok(fixture("zones_list.json")),
        "records/lookup" => {
            let name = call
                .body
                .pointer("/records/0/recordName")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(match name {
                "AX/m006+fff" => fixture("lookup_m006.json"),
                "AX/m002+bbb" => fixture("lookup_m002_fresh.json"),
                _ => serde_json::json!({ "records": [] }),
            })
        }
        other => Err(Error::Http {
            status: 404,
            body: format!("no fixture for {other}"),
        }),
    }
}

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("icloud-photos-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
