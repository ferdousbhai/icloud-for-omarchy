//! What a push must not do to notes: read an iCloud error as "gone", take a
//! copy for a moved note, create a note twice after a run that was killed,
//! delete most of the vault unasked, or write a merge over a later save.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::plan::{PlanEntryKind, PlanResolution};
use icloud_notes_sync::cmd::push::{PushOptions, PushResult, run_push_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::vault::state::{Account, CloneState, read_clone_state, write_clone_state};
use serde_json::{Value, json};

const PRIVATE: &str = "/database/1/com.apple.notes/production/private";
const SHARED: &str = "/database/1/com.apple.notes/production/shared";
const NOTE: &str = "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1";
const NOTE_FILE: &str = "Notes/Test Note.md";

fn cassette(name: &str) -> Cassette {
    Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/differential/cassettes")
            .join(name),
    )
    .unwrap()
}

/// What a test does when a request arrives: the operation, whether it is
/// about to be answered (`true`) or, for a `records/modify`, was just applied
/// (`false`), and its body.
type Hook = Box<dyn Fn(&str, bool, &Value) + Send + Sync>;

/// A private Notes zone holding `records`: it answers lookups (an entry
/// with a `serverErrorCode` is answered as that error) and applies every
/// `records/modify` operation.
#[derive(Default)]
struct Server {
    records: Mutex<Vec<Value>>,
    /// Every POST to the database as (operation, body).
    posts: Mutex<Vec<(String, Value)>>,
    hook: Option<Hook>,
    changes: Mutex<u32>,
}

impl Server {
    fn with_note() -> Server {
        let lookup = cassette("tiny-lookup.json");
        let note = lookup.interactions[0].response.body.as_ref().unwrap()["records"][0].clone();
        assert_eq!(note["recordName"], NOTE);
        Server {
            records: Mutex::new(vec![note]),
            ..Default::default()
        }
    }

    fn posts_to(&self, operation: &str) -> Vec<Value> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(op, _)| op == operation)
            .map(|(_, body)| body.clone())
            .collect()
    }

    /// The operations of every `records/modify` so far, as (type, record name).
    fn operations(&self) -> Vec<(String, String)> {
        self.posts_to("/records/modify")
            .iter()
            .flat_map(|body| body["operations"].as_array().unwrap().clone())
            .map(|op| {
                (
                    op["operationType"].as_str().unwrap().to_owned(),
                    op["record"]["recordName"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// Answers every lookup of `name` with the error `code`.
    fn fail_lookup_of(&self, name: &str, code: &str) {
        let mut records = self.records.lock().unwrap();
        records.retain(|r| r["recordName"] != name);
        records.push(json!({ "recordName": name, "serverErrorCode": code, "reason": "refused by the test server" }));
    }

    fn modify(&self, body: &Value) -> Value {
        let mut records = self.records.lock().unwrap();
        let mut changes = self.changes.lock().unwrap();
        let mut answers = Vec::new();
        for op in body["operations"].as_array().unwrap() {
            *changes += 1;
            let sent = &op["record"];
            let tag = json!(format!("t{changes}"));
            let record = match op["operationType"].as_str().unwrap() {
                "update" => {
                    let record = records
                        .iter_mut()
                        .find(|r| r["recordName"] == sent["recordName"])
                        .expect("an update names a record the zone has");
                    assert_eq!(record["recordChangeTag"], sent["recordChangeTag"], "a stale change tag");
                    for (name, field) in sent["fields"].as_object().unwrap() {
                        if field["value"].is_null() {
                            record["fields"].as_object_mut().unwrap().remove(name);
                        } else {
                            record["fields"][name] = json!({ "value": field["value"] });
                        }
                    }
                    record["recordChangeTag"] = tag;
                    record.clone()
                }
                "create" => {
                    assert!(
                        !records.iter().any(|r| r["recordName"] == sent["recordName"]),
                        "created twice: {}",
                        sent["recordName"]
                    );
                    let mut record = sent.clone();
                    record["recordChangeTag"] = tag;
                    records.push(record.clone());
                    record
                }
                "delete" | "forceDelete" => {
                    records.retain(|r| r["recordName"] != sent["recordName"]);
                    json!({ "recordName": sent["recordName"], "deleted": true })
                }
                other => panic!("unexpected operation {other}"),
            };
            answers.push(record);
        }
        json!({ "records": answers })
    }
}

impl Transport for Server {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        let path = path.split('?').next().unwrap_or(path);
        let operation = path
            .strip_prefix(PRIVATE)
            .or_else(|| path.strip_prefix(SHARED))
            .unwrap_or_else(|| panic!("unexpected request to {path}"));
        self.posts.lock().unwrap().push((operation.to_owned(), body.clone()));
        if let Some(hook) = &self.hook {
            hook(operation, true, body);
        }
        Ok(match operation {
            "/records/lookup" => {
                let records = self.records.lock().unwrap();
                let found: Vec<Value> = body["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|wanted| records.iter().find(|r| r["recordName"] == wanted["recordName"]))
                    .cloned()
                    .collect();
                json!({ "records": found })
            }
            "/records/modify" => {
                let answer = self.modify(body);
                if let Some(hook) = &self.hook {
                    hook(operation, false, body);
                }
                answer
            }
            "/changes/zone" => json!({ "zones": [{
                "zoneID": { "zoneName": "Notes", "ownerRecordName": "_defaultOwner", "zoneType": "REGULAR_CUSTOM_ZONE" },
                "moreComing": false,
                "syncToken": "after-the-push",
                "records": self.records.lock().unwrap().iter().filter(|r| r.get("serverErrorCode").is_none()).cloned().collect::<Vec<_>>(),
            }] }),
            "/changes/database" => json!({ "zones": [], "moreComing": false, "syncToken": "shared" }),
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

fn serve(server: &Arc<Server>) -> impl Connector + use<> {
    let server = server.clone();
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

/// The `tiny-clone` vault: one plain note, "Notes/Test Note.md".
fn cloned_vault(dir: &Path) -> PathBuf {
    let vault = dir.join("vault");
    let clone = cassette("tiny-clone.json");
    let log = dir.join("clone.json");
    let connector = FnConnector(move || -> Result<Remote, Error> {
        let account = Account {
            apple_id: clone.account.apple_id.clone(),
            dsid: clone.account.dsid.clone(),
        };
        let transport = ReplayTransport::from_cassette(clone.clone(), Some(log.clone()))?;
        Ok(Remote {
            db: Database::new(Box::new(transport)),
            account,
        })
    });
    run_clone_with(&connector, &vault, &mut NoProgress, &CloneOptions::default()).unwrap();
    vault
}

fn try_push(server: &Arc<Server>, vault: &Path) -> Result<PushResult, Error> {
    run_push_with(&serve(server), vault, &mut |_| {}, &PushOptions::default())
}

fn push(server: &Arc<Server>, vault: &Path) -> PushResult {
    try_push(server, vault).unwrap()
}

fn state(vault: &Path) -> CloneState {
    read_clone_state(vault).unwrap().unwrap()
}

fn read(vault: &Path, file: &str) -> String {
    std::fs::read_to_string(vault.join(file)).unwrap()
}

fn append(vault: &Path, file: &str, text: &str) {
    std::fs::write(vault.join(file), read(vault, file) + text).unwrap();
}

/// The entries of `result` as (kind, file, resolution).
fn entries(result: &PushResult) -> Vec<(PlanEntryKind, &str, PlanResolution)> {
    result
        .entries
        .iter()
        .map(|e| (e.entry.kind, e.entry.file.as_str(), e.entry.resolution))
        .collect()
}

// --- a lookup that answers an error -----------------------------------------------

/// iCloud failing to look a note up is not iCloud saying it is gone: an
/// edit is refused for now, not reported as a note that no longer exists.
#[test]
fn an_edit_whose_lookup_answered_an_error_is_refused_not_read_as_gone() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    server.fail_lookup_of(NOTE, "TRY_AGAIN_LATER");
    append(&vault, NOTE_FILE, "\nA line added here.\n");

    let result = push(&server, &vault);
    assert_eq!(
        entries(&result),
        [(PlanEntryKind::Update, NOTE_FILE, PlanResolution::Refused)]
    );
    let reason = result.entries[0].entry.reason.as_deref().unwrap();
    assert!(reason.contains("couldn't look this up (TRY_AGAIN_LATER)"), "{reason}");
    assert!(server.operations().is_empty());
    assert_eq!(state(&vault).notes[NOTE].record_change_tag, "25q");
}

/// The same for a note renamed here.
#[test]
fn a_move_whose_lookup_answered_an_error_is_refused_not_read_as_gone() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    server.fail_lookup_of(NOTE, "TRY_AGAIN_LATER");
    std::fs::rename(vault.join(NOTE_FILE), vault.join("Notes/Renamed.md")).unwrap();

    let result = push(&server, &vault);
    assert_eq!(
        entries(&result),
        [(PlanEntryKind::Move, "Notes/Renamed.md", PlanResolution::Refused)]
    );
    let reason = result.entries[0].entry.reason.as_deref().unwrap();
    assert!(reason.contains("couldn't look this up (TRY_AGAIN_LATER)"), "{reason}");
    assert_eq!(state(&vault).notes[NOTE].file, NOTE_FILE);
}

// --- move pairing --------------------------------------------------------------------

/// A second tracked note, `OTHER` at `file`, whose file is not written.
const OTHER: &str = "a1b2c3d4-0000-4000-8000-0000000000b2";

fn track_other(vault: &Path, file: &str, base: &str) {
    let mut state = state(vault);
    let mut entry = state.notes[NOTE].clone();
    entry.file = file.to_owned();
    state.notes.insert(OTHER.to_owned(), entry);
    write_clone_state(vault, &state).unwrap();
    icloud_notes_sync::vault::base::write_base_copy(vault, OTHER, base).unwrap();
}

/// A note deleted here, and a copy of another note (it carries that note's
/// id) that happens to have the deleted note's file name: the copy is a new
/// note, and the deleted note is deleted, not overwritten with the copy.
#[test]
fn a_copy_of_another_note_is_not_taken_for_a_deleted_note_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    // OTHER's file (at the top of the vault) is gone; the copy of NOTE sits
    // in a new directory under the same name.
    track_other(&vault, "Test Note.md", "The other note\n");
    std::fs::create_dir_all(vault.join("Copies")).unwrap();
    std::fs::write(
        vault.join("Copies/Test Note.md"),
        read(&vault, NOTE_FILE) + "\nedited in the copy\n",
    )
    .unwrap();

    let result = run_push_with(&serve(&server), &vault, &mut |_| {}, &PushOptions { dry_run: true }).unwrap();
    let planned = entries(&result);
    assert!(
        planned.contains(&(PlanEntryKind::Delete, "Test Note.md", PlanResolution::Ready)),
        "{planned:?}"
    );
    assert!(
        planned.contains(&(PlanEntryKind::Create, "Copies/Test Note.md", PlanResolution::Ready)),
        "{planned:?}"
    );
    assert!(
        planned.iter().all(|(kind, ..)| *kind != PlanEntryKind::Move),
        "{planned:?}"
    );
}

// --- a file saved again while it is being pushed ------------------------------------

/// The note changed in iCloud since the last pull, so the push plans a merge
/// and would write the merged text over the file. A save that lands after
/// the plan read the file (here: while iCloud is being asked) is not undone:
/// the entry is left for the next push, which sends both edits.
#[test]
fn a_planned_merge_is_not_written_over_a_later_save() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let saved_later = Arc::new(Mutex::new(false));
    let hook: Hook = {
        let (vault, saved_later) = (vault.clone(), saved_later.clone());
        Box::new(move |operation, _, _| {
            let mut saved = saved_later.lock().unwrap();
            if operation == "/records/lookup" && !*saved {
                *saved = true;
                append(&vault, NOTE_FILE, "A line saved while the push ran.\n");
            }
        })
    };
    let server = Arc::new(Server {
        hook: Some(hook),
        ..Server::with_note()
    });
    // Another device touched the note: only its change tag differs.
    server.records.lock().unwrap()[0]["recordChangeTag"] = json!("26z");
    append(&vault, NOTE_FILE, "\nA line added before the push.\n");

    let result = push(&server, &vault);
    assert_eq!(result.pushed, Some(0), "{result:?}");
    let outcome = result.entries[0].outcome.as_ref().unwrap();
    assert!(outcome.skipped && outcome.succeeded, "{outcome:?}");
    assert!(result.failures().is_empty());
    assert!(server.operations().is_empty(), "nothing was sent");
    let text = read(&vault, NOTE_FILE);
    assert!(text.contains("A line saved while the push ran."), "{text}");
    assert_eq!(state(&vault).notes[NOTE].record_change_tag, "25q");

    let result = push(&server, &vault);
    assert_eq!(result.pushed, Some(1), "{result:?}");
    assert_eq!(server.operations(), [("update".to_owned(), NOTE.to_owned())]);
    let text = read(&vault, NOTE_FILE);
    assert!(text.contains("A line added before the push.") && text.contains("A line saved while the push ran."));
}
