//! `carry-over`: a note's pin, creation date, paper style and attachment
//! view written on another note, and nothing else.

use std::path::Path;
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::carry_over::run_carry_over_with;
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::vault::state::{Account, read_clone_state};
use serde_json::{Value, json};

const OLD: &str = "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1";
const NEW: &str = "11111111-2222-3333-4444-555555555555";

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The tiny clone with a second tracked note, "Notes/Copy.md".
fn vault() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential/expected/tiny-clone/vault"),
        tmp.path(),
    );
    let state_file = tmp.path().join(".icloud-notes/state.json");
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
    state["notes"][NEW] = json!({"file": "Notes/Copy.md", "recordChangeTag": "n1", "modificationDate": 5});
    std::fs::write(&state_file, state.to_string()).unwrap();
    std::fs::write(
        tmp.path().join("Notes/Copy.md"),
        format!("---\napple-note-id: {NEW}\n---\nCopy\n"),
    )
    .unwrap();
    tmp
}

/// Answers the lookup with `records` and accepts (or refuses) one update,
/// keeping every `records/modify` body.
struct Server {
    records: Value,
    refuse: bool,
    modified: Mutex<Vec<Value>>,
}

struct Shared(Arc<Server>);

impl Transport for Shared {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        let path = path.split('?').next().unwrap_or(path);
        Ok(match path {
            p if p.ends_with("/private/records/lookup") => json!({ "records": self.0.records }),
            p if p.ends_with("/private/records/modify") => {
                self.0.modified.lock().unwrap().push(body.clone());
                if self.0.refuse {
                    json!({ "records": [{ "recordName": NEW, "serverErrorCode": "CONFLICT", "reason": "changed" }] })
                } else {
                    let mut record = body["operations"][0]["record"].clone();
                    record["recordChangeTag"] = json!("n2");
                    json!({ "records": [record] })
                }
            }
            _ => panic!("unexpected request to {path}"),
        })
    }

    fn download(&self, url: &str, _dest: &Path) -> Result<u64, CkError> {
        panic!("unexpected download: {url}")
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

fn note(name: &str, tag: &str, fields: Value) -> Value {
    json!({ "recordName": name, "recordType": "Note", "recordChangeTag": tag, "fields": fields })
}

fn field(value: i64) -> Value {
    json!({ "value": value, "type": "INT64" })
}

#[test]
fn the_pin_and_the_rest_are_written_on_the_other_note() {
    let dir = vault();
    let server = Arc::new(Server {
        records: json!([
            note(
                OLD,
                "o9",
                json!({
                    "IsPinned": field(1),
                    "CreationDate": field(1_676_698_363_307),
                    "PaperStyleType": field(1),
                    "AttachmentViewType": field(0),
                    "ModificationDate": field(9),
                    "TitleEncrypted": { "value": "b2xk", "type": "ENCRYPTED_BYTES" },
                })
            ),
            note(
                NEW,
                "n1",
                json!({
                    "CreationDate": field(1_794_375_865_745),
                    "PaperStyleType": field(0),
                    "AttachmentViewType": field(0),
                    "ModificationDate": field(5),
                })
            ),
        ]),
        refuse: false,
        modified: Mutex::new(Vec::new()),
    });
    let result = run_carry_over_with(
        &serve(server.clone()),
        dir.path(),
        "Notes/Test Note.md",
        "Notes/Copy.md",
    )
    .unwrap();
    assert_eq!(result.carried, ["IsPinned", "CreationDate", "PaperStyleType"]);
    assert_eq!(
        (result.from.as_str(), result.to.as_str()),
        ("Notes/Test Note.md", "Notes/Copy.md")
    );

    let modified = server.modified.lock().unwrap();
    assert_eq!(modified.len(), 1);
    let record = &modified[0]["operations"][0]["record"];
    assert_eq!(modified[0]["operations"][0]["operationType"], "update");
    assert_eq!(
        (record["recordName"].as_str(), record["recordChangeTag"].as_str()),
        (Some(NEW), Some("n1"))
    );
    // Only what differs: not the view that already matched, nor the text,
    // title or modification date.
    assert_eq!(
        record["fields"],
        json!({
            "CreationDate": { "value": 1_676_698_363_307_i64 },
            "IsPinned": { "value": 1 },
            "PaperStyleType": { "value": 1 },
        })
    );
    let state = read_clone_state(dir.path()).unwrap().unwrap();
    assert_eq!(state.notes[NEW].record_change_tag, "n2");
    assert_eq!(state.notes[OLD].record_change_tag, "25q");
}

#[test]
fn nothing_is_sent_when_the_notes_already_match() {
    let dir = vault();
    let same = json!({ "CreationDate": field(7), "PaperStyleType": field(0) });
    let server = Arc::new(Server {
        records: json!([note(OLD, "o9", same.clone()), note(NEW, "n1", same)]),
        refuse: false,
        modified: Mutex::new(Vec::new()),
    });
    let result = run_carry_over_with(
        &serve(server.clone()),
        dir.path(),
        "Notes/Test Note.md",
        "Notes/Copy.md",
    )
    .unwrap();
    assert!(result.carried.is_empty());
    assert!(server.modified.lock().unwrap().is_empty());
}

#[test]
fn a_refusal_from_icloud_is_an_error_and_leaves_the_state_alone() {
    let dir = vault();
    let server = Arc::new(Server {
        records: json!([
            note(OLD, "o9", json!({ "IsPinned": field(1) })),
            note(NEW, "n1", json!({}))
        ]),
        refuse: true,
        modified: Mutex::new(Vec::new()),
    });
    let err = run_carry_over_with(&serve(server), dir.path(), "Notes/Test Note.md", "Notes/Copy.md").unwrap_err();
    assert!(err.to_string().contains("CONFLICT (changed)"), "{err}");
    assert_eq!(
        read_clone_state(dir.path()).unwrap().unwrap().notes[NEW].record_change_tag,
        "n1"
    );
}

#[test]
fn a_note_gone_from_icloud_is_an_error() {
    let dir = vault();
    let server = Arc::new(Server {
        records: json!([note(NEW, "n1", json!({}))]),
        refuse: false,
        modified: Mutex::new(Vec::new()),
    });
    let err = run_carry_over_with(&serve(server), dir.path(), "Notes/Test Note.md", "Notes/Copy.md").unwrap_err();
    assert!(err.to_string().contains("no longer in iCloud"), "{err}");
}
