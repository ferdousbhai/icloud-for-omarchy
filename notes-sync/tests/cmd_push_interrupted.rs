//! A push that fails or is killed part way: what it did before that is
//! recorded, so the next push doesn't do it again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::plan::PlanEntryKind;
use icloud_notes_sync::cmd::push::{PushOptions, run_push_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::vault::state::{Account, read_clone_state};
use serde_json::{Value, json};

const PRIVATE: &str = "/database/1/com.apple.notes/production/private";

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

/// The private zone with nothing changed since `token`, whose connection
/// drops on the `fail_at`th note created (counting from 1), or which takes
/// the whole run down there (`dies`: a panic, standing in for a kill, so
/// nothing after the request runs).
struct Server {
    token: String,
    fail_at: usize,
    dies: bool,
    created: Mutex<Vec<String>>,
}

impl Transport for Server {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        let path = path.split('?').next().unwrap_or(path);
        let zone = json!({ "zoneName": "Notes", "ownerRecordName": "_defaultOwner" });
        Ok(match path.strip_prefix(PRIVATE) {
            Some("/changes/zone") => {
                json!({ "zones": [{ "zoneID": zone, "moreComing": false, "syncToken": self.token, "records": [] }] })
            }
            Some("/records/lookup") => json!({ "records": [] }),
            Some("/records/modify") => {
                let mut created = self.created.lock().unwrap();
                let mut answers = Vec::new();
                for op in body["operations"].as_array().unwrap() {
                    assert_eq!(op["operationType"], "create", "{op}");
                    if created.len() + 1 == self.fail_at {
                        if self.dies {
                            drop(created);
                            std::panic::resume_unwind(Box::new("killed"));
                        }
                        return Err(CkError::Network("connection reset".into()));
                    }
                    let mut record = op["record"].clone();
                    record["recordChangeTag"] = json!(format!("c{}", created.len() + 1));
                    created.push(record["recordName"].as_str().unwrap().to_owned());
                    answers.push(record);
                }
                json!({ "records": answers })
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

struct Shared(Arc<Server>);

impl Transport for Shared {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        self.0.post_json(path, body)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        self.0.download(url, dest)
    }
}

fn serve(server: Arc<Server>) -> impl Connector {
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

/// Two new notes; the connection drops creating the second. The first is in
/// iCloud and its file carries its id, so state.json tracks it and the next
/// push creates only the second.
#[test]
fn a_note_created_before_the_push_failed_is_tracked_and_not_created_again() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    run_clone_with(
        &replay(cassette("tiny-clone.json"), dir.path().join("clone.json")),
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    let token = read_clone_state(&vault).unwrap().unwrap().sync_token.unwrap();
    std::fs::write(vault.join("Notes/A.md"), "A\nfirst").unwrap();
    std::fs::write(vault.join("Notes/B.md"), "B\nsecond").unwrap();

    let server = Arc::new(Server {
        token,
        fail_at: 2,
        dies: false,
        created: Mutex::new(Vec::new()),
    });
    let err = run_push_with(&serve(server.clone()), &vault, &mut |_| {}, &PushOptions::default()).unwrap_err();
    assert!(matches!(err, Error::CloudKit(CkError::Network(_))), "{err:?}");
    let created = server.created.lock().unwrap().clone();
    assert_eq!(created.len(), 1);

    let state = read_clone_state(&vault).unwrap().unwrap();
    let tracked = &state.notes[&created[0]];
    let first = tracked.file.clone();
    assert!(first == "Notes/A.md" || first == "Notes/B.md", "{first}");
    assert_eq!(tracked.record_change_tag, "c1");

    let preview = run_push_with(
        &serve(server.clone()),
        &vault,
        &mut |_| {},
        &PushOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    let planned: Vec<_> = preview
        .entries
        .iter()
        .map(|e| (e.entry.file.clone(), e.entry.kind))
        .collect();
    let second = if first == "Notes/A.md" {
        "Notes/B.md"
    } else {
        "Notes/A.md"
    };
    assert_eq!(planned, vec![(second.to_owned(), PlanEntryKind::Create)]);
}

/// Two new notes; the run is killed while creating the second, so none of
/// its error handling runs. The first was saved as soon as it was created,
/// and the next push creates only the second.
#[test]
fn a_note_created_before_the_push_was_killed_is_not_created_again() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    run_clone_with(
        &replay(cassette("tiny-clone.json"), dir.path().join("clone.json")),
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    let token = read_clone_state(&vault).unwrap().unwrap().sync_token.unwrap();
    std::fs::write(vault.join("Notes/A.md"), "A\nfirst").unwrap();
    std::fs::write(vault.join("Notes/B.md"), "B\nsecond").unwrap();

    let server = Arc::new(Server {
        token: token.clone(),
        fail_at: 2,
        dies: true,
        created: Mutex::new(Vec::new()),
    });
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_push_with(&serve(server.clone()), &vault, &mut |_| {}, &PushOptions::default())
    }));
    assert!(killed.is_err(), "the run should not have returned");
    let created = server.created.lock().unwrap().clone();
    assert_eq!(created.len(), 1);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes[&created[0]].record_change_tag, "c1");

    // The next push, over a connection that works.
    let server = Arc::new(Server {
        token,
        fail_at: 0,
        dies: false,
        created: Mutex::new(Vec::new()),
    });
    let result = run_push_with(&serve(server.clone()), &vault, &mut |_| {}, &PushOptions::default()).unwrap();
    assert_eq!(result.pushed, Some(1));
    assert_eq!(server.created.lock().unwrap().len(), 1, "only the second note");
    let state = read_clone_state(&vault).unwrap().unwrap();
    let files: Vec<&str> = state.notes.values().map(|e| e.file.as_str()).collect();
    assert!(
        files.contains(&"Notes/A.md") && files.contains(&"Notes/B.md"),
        "{files:?}"
    );
}
