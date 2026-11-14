//! Deleting a folder directory that looks empty here, when the folder still
//! holds notes this vault never tracks (locked or unsyncable ones).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::plan::PlanResolution;
use icloud_notes_sync::cmd::pull::{PullOptions, run_pull_with};
use icloud_notes_sync::cmd::push::{PushOptions, PushResult, run_push_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::vault::state::{Account, read_clone_state};
use serde_json::{Value, json};

const PRIVATE: &str = "/database/1/com.apple.notes/production/private";
const FOLDER: &str = "F-PRIVATE";

fn cassette(name: &str) -> Cassette {
    Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/differential/cassettes")
            .join(name),
    )
    .unwrap()
}

fn replay(cassette: Cassette, log: PathBuf) -> impl Connector {
    FnConnector(move || -> Result<Remote, Error> {
        let account = Account {
            apple_id: cassette.account.apple_id.clone(),
            dsid: cassette.account.dsid.clone(),
        };
        let transport = ReplayTransport::from_cassette(cassette.clone(), Some(log.clone()))?;
        Ok(Remote {
            db: Database::new(Box::new(transport)),
            account,
        })
    })
}

/// The private zone as it stands: nothing changed since `token`, and every
/// record in `records`.
struct Server {
    token: String,
    records: Vec<Value>,
    log: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Transport for Server {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        self.log.lock().unwrap().push((path.to_owned(), body.clone()));
        let path = path.split('?').next().unwrap_or(path);
        let zone = json!({ "zoneName": "Notes", "ownerRecordName": "_defaultOwner" });
        Ok(match path.strip_prefix(PRIVATE) {
            Some("/changes/zone") => {
                let since = body["zones"][0]["syncToken"].as_str();
                let records = if since == Some(self.token.as_str()) {
                    vec![]
                } else {
                    self.records.clone()
                };
                json!({ "zones": [{ "zoneID": zone, "moreComing": false, "syncToken": self.token, "records": records }] })
            }
            Some("/records/lookup") => {
                let wanted: Vec<&Value> = body["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| &r["recordName"])
                    .collect();
                let found: Vec<&Value> = self
                    .records
                    .iter()
                    .filter(|r| wanted.contains(&&r["recordName"]))
                    .collect();
                json!({ "records": found })
            }
            Some("/records/modify") => {
                let done: Vec<Value> = body["operations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|op| json!({ "recordName": op["record"]["recordName"], "deleted": true }))
                    .collect();
                json!({ "records": done })
            }
            _ if path.ends_with("/shared/changes/database") => {
                json!({ "zones": [], "moreComing": false, "syncToken": "S" })
            }
            _ => panic!("unexpected request to {path}"),
        })
    }

    fn download(&self, url: &str, _dest: &Path) -> Result<u64, CkError> {
        panic!("unexpected download: {url}")
    }
}

fn serve(server: Server) -> impl Connector {
    let server = Arc::new(server);
    FnConnector(move || -> Result<Remote, Error> {
        Ok(Remote {
            db: Database::new(Box::new(Shared(server.clone()))),
            account: Account {
                apple_id: "harness@example.com".into(),
                dsid: "10000000001".into(),
            },
        })
    })
}

struct Shared(Arc<Server>);

impl Transport for Shared {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        self.0.post_json(path, body)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        self.0.download(url, dest)
    }
}

fn folder_record() -> Value {
    json!({
        "recordName": FOLDER, "recordType": "Folder", "recordChangeTag": "2a", "deleted": false,
        "fields": { "TitleEncrypted": { "value": "UHJpdmF0ZQ==", "type": "ENCRYPTED_BYTES" } },
    })
}

/// A record in `FOLDER` that clone and pull skip.
fn skipped_note(record_name: &str, record_type: &str) -> Value {
    json!({
        "recordName": record_name, "recordType": record_type, "recordChangeTag": "3a", "deleted": false,
        "fields": {
            "Folder": {
                "value": {
                    "recordName": FOLDER, "action": "VALIDATE",
                    "zoneID": { "zoneName": "Notes", "ownerRecordName": "_defaultOwner" },
                },
                "type": "REFERENCE",
            },
            "Deleted": { "value": 0, "type": "INT64" },
        },
    })
}

/// tiny-clone plus a "Private" folder holding only `extra`.
fn clone_with(extra: &[Value]) -> (Cassette, Vec<Value>) {
    let mut c = cassette("tiny-clone.json");
    let records = c.interactions[0].response.body.as_mut().unwrap()["zones"][0]["records"]
        .as_array_mut()
        .unwrap();
    records.push(folder_record());
    records.extend_from_slice(extra);
    let all = records.clone();
    (c, all)
}

/// Clones with `extra`, deletes the "Private" directory (empty here) and
/// pushes: the result, and the bodies of the records/modify requests sent.
fn delete_its_folder(vault: &Path, extra: &[Value]) -> (PushResult, Vec<Value>, String) {
    let (clone, all_records) = clone_with(extra);
    let summary = run_clone_with(
        &replay(clone, vault.with_extension("clone.json")),
        vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.written, 1);
    let state = read_clone_state(vault).unwrap().unwrap();
    assert!(state.folders.as_ref().unwrap().contains_key(FOLDER));
    let folder_dir = vault.join("Private");
    assert_eq!(std::fs::read_dir(&folder_dir).unwrap().count(), 0, "looks empty");
    std::fs::remove_dir(&folder_dir).unwrap();

    let log = Arc::new(Mutex::new(Vec::new()));
    let token = state.sync_token.clone().unwrap();
    let server = Server {
        token: token.clone(),
        records: all_records,
        log: log.clone(),
    };
    let result = run_push_with(&serve(server), vault, &mut |_| {}, &PushOptions::default()).unwrap();
    let modifies: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path.contains("/records/modify"))
        .map(|(_, body)| body.clone())
        .collect();
    (result, modifies, token)
}

fn deleting_its_folder_keeps(extra: Value) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let (result, modifies, token) = delete_its_folder(&vault, &[extra]);
    assert!(modifies.is_empty(), "deleted the folder: {modifies:?}");
    let refused = &result
        .entries
        .iter()
        .find(|e| e.entry.file == "Private")
        .expect("a folder entry")
        .entry;
    assert_eq!(refused.resolution, PlanResolution::Refused);
    let reason = refused.reason.as_deref().unwrap_or("");
    assert!(reason.contains("1 note(s) this tool can't sync"), "{reason}");
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(state.folders.as_ref().unwrap().contains_key(FOLDER));

    // As the reason says, a pull brings the directory back.
    let server = Server {
        token,
        records: vec![],
        log: Arc::default(),
    };
    run_pull_with(
        &serve(server),
        &vault,
        &mut NoProgress,
        &mut |_| {},
        &PullOptions::default(),
    )
    .unwrap();
    assert!(vault.join("Private").is_dir());
}

#[test]
fn a_folder_holding_a_locked_note_is_not_deleted() {
    deleting_its_folder_keeps(skipped_note("LOCKED-1", "PasswordProtectedNote"));
}

#[test]
fn a_folder_holding_an_unreadable_note_is_not_deleted() {
    let mut note = skipped_note("UNREADABLE-1", "Note");
    note["fields"]["TextDataEncrypted"] = json!({ "value": "bm90IGEgbm90ZQ==", "type": "ENCRYPTED_BYTES" });
    deleting_its_folder_keeps(note);
}

#[test]
fn an_empty_folder_and_one_holding_only_deleted_notes_are_deleted() {
    let mut deleted = skipped_note("DELETED-1", "PasswordProtectedNote");
    deleted["deleted"] = json!(true);
    let mut trashed = skipped_note("TRASHED-1", "Note");
    trashed["fields"]["Deleted"]["value"] = json!(1);
    trashed["fields"]["TextDataEncrypted"] = json!({ "value": "bm90IGEgbm90ZQ==", "type": "ENCRYPTED_BYTES" });
    for extra in [vec![], vec![deleted, trashed]] {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        let (_, modifies, _) = delete_its_folder(&vault, &extra);
        assert_eq!(modifies.len(), 1, "{modifies:?}");
        assert_eq!(modifies[0]["operations"][0]["operationType"], "delete");
        assert_eq!(modifies[0]["operations"][0]["record"]["recordName"], FOLDER);
        let state = read_clone_state(&vault).unwrap().unwrap();
        assert!(!state.folders.as_ref().unwrap().contains_key(FOLDER));
    }
}
