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
use icloud_notes_sync::cmd::sync::run_sync_with;
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

/// A field as iCloud answers it: the value sent, with its type.
fn typed(value: &Value) -> Value {
    let type_ = match value {
        Value::String(_) => "ENCRYPTED_BYTES",
        Value::Number(_) => "INT64",
        Value::Array(_) => "REFERENCE_LIST",
        _ => "REFERENCE",
    };
    json!({ "value": value, "type": type_ })
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
                            record["fields"][name] = typed(&field["value"]);
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
                    // iCloud keeps no field that was sent empty.
                    let fields = record["fields"].as_object_mut().unwrap();
                    fields.retain(|_, field| !field["value"].is_null());
                    for field in fields.values_mut() {
                        *field = typed(&field["value"]);
                    }
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

    let result = run_push_with(
        &serve(&server),
        &vault,
        &mut |_| {},
        &PushOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
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

// --- most of the vault's files gone ----------------------------------------------------

/// Tracks `count` more notes (no record in iCloud), each with its file and
/// base copy; returns their files.
fn track_more(vault: &Path, count: usize) -> Vec<String> {
    let mut state = state(vault);
    let mut files = Vec::new();
    for n in 0..count {
        let record_name = format!("a1b2c3d4-0000-4000-8000-{n:012x}");
        let file = format!("Notes/More {n}.md");
        let mut entry = state.notes[NOTE].clone();
        entry.file = file.clone();
        state.notes.insert(record_name.clone(), entry);
        std::fs::write(vault.join(&file), format!("Note {n}\n")).unwrap();
        icloud_notes_sync::vault::base::write_base_copy(vault, &record_name, &format!("Note {n}\n")).unwrap();
        files.push(file);
    }
    write_clone_state(vault, &state).unwrap();
    files
}

/// The note files of most of the vault gone while its state is still there
/// is what an accident looks like. Nothing is deleted, nothing else is
/// pushed, and the error says so; `--yes` (`allow_deletes`) deletes them.
#[test]
fn a_push_that_would_delete_most_of_the_vault_is_refused_until_told_to() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let mut files = track_more(&vault, 5);
    files.push(NOTE_FILE.to_owned());
    for file in &files[1..] {
        std::fs::remove_file(vault.join(file)).unwrap();
    }

    let error = try_push(&server, &vault).unwrap_err();
    assert!(
        matches!(error, Error::TooManyDeletions { deletions: 5, total: 6 }),
        "{error:?}"
    );
    assert_eq!((error.code().as_str(), error.exit_code()), ("too_many_deletions", 2));
    assert!(error.hint().unwrap().contains("push --yes"));
    assert!(server.operations().is_empty(), "nothing was sent");
    assert_eq!(state(&vault).notes.len(), 6, "every note is still tracked");

    // The preview lists them and says what a push would do.
    let preview = run_push_with(
        &serve(&server),
        &vault,
        &mut |_| {},
        &PushOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(preview.entries.len(), 5);
    assert!(
        preview.notices.iter().any(|n| n.message.contains("5 of the 6 notes")),
        "{:?}",
        preview.notices
    );

    // A sync reports it as its push half's failure, and still pulls.
    let outcome = run_sync_with(
        &serve(&server),
        &vault,
        &mut NoProgress,
        &mut |_| {},
        &PushOptions::default(),
    );
    assert!(matches!(outcome.push, Err(Error::TooManyDeletions { .. })));
    assert!(matches!(outcome.pull, Some(Ok(_))));
    assert_eq!(outcome.exit_code(), 2);
    assert!(server.operations().is_empty());

    let allowed = PushOptions {
        allow_deletes: true,
        ..Default::default()
    };
    let result = run_push_with(&serve(&server), &vault, &mut |_| {}, &allowed).unwrap();
    assert_eq!(result.pushed, Some(5));
    assert_eq!(server.operations(), [("update".to_owned(), NOTE.to_owned())]);
    assert_eq!(state(&vault).notes.len(), 1);
}

/// A few deletions, or deletions that are not most of the vault, go up as
/// they always did.
#[test]
fn deleting_some_notes_is_not_refused() {
    for (more, deleted) in [(2, 3), (7, 4)] {
        let dir = tempfile::tempdir().unwrap();
        let vault = cloned_vault(dir.path());
        let server = Arc::new(Server::with_note());
        let mut files = track_more(&vault, more);
        files.push(NOTE_FILE.to_owned());
        for file in &files[files.len() - deleted..] {
            std::fs::remove_file(vault.join(file)).unwrap();
        }
        let result = push(&server, &vault);
        assert_eq!(result.pushed, Some(deleted), "{more} more, {deleted} deleted");
        assert_eq!(state(&vault).notes.len(), more + 1 - deleted);
    }
}

// --- a push killed between iCloud's answer and its own record of it -----------------

/// A server that takes the whole run down right after it applied the first
/// `records/modify` holding a create of `record_type`: iCloud has the
/// record, the push never hears of it. (A panic stands in for the kill: no
/// code of the push runs after the request.)
fn dies_after_creating(record_type: &'static str) -> Arc<Server> {
    let died = Mutex::new(false);
    let hook: Hook = Box::new(move |operation, before, body| {
        let creates = body["operations"].as_array().is_some_and(|ops| {
            ops.iter()
                .any(|op| op["operationType"] == "create" && op["record"]["recordType"] == record_type)
        });
        if operation == "/records/modify" && !before && creates {
            let mut died = died.lock().unwrap();
            if !*died {
                *died = true;
                drop(died);
                std::panic::resume_unwind(Box::new("killed"));
            }
        }
    });
    Arc::new(Server {
        hook: Some(hook),
        ..Server::with_note()
    })
}

fn killed_push(server: &Arc<Server>, vault: &Path) {
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| try_push(server, vault)));
    assert!(killed.is_err(), "the push was cut off");
}

fn creates(server: &Server, record_type: &str) -> usize {
    server
        .posts_to("/records/modify")
        .iter()
        .flat_map(|body| body["operations"].as_array().unwrap().clone())
        .filter(|op| op["operationType"] == "create" && op["record"]["recordType"] == record_type)
        .count()
}

/// A new note's create reaches iCloud and the push is killed before it
/// records anything: the file has no id yet and state doesn't track it. The
/// push after it finds the create it had announced in state, asks iCloud,
/// and tracks the note it finds instead of creating it a second time.
#[test]
fn a_note_created_just_before_the_push_was_killed_is_not_created_twice() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = dies_after_creating("Note");
    std::fs::write(vault.join("Notes/Fresh.md"), "Fresh\n\nA note made here.\n").unwrap();

    killed_push(&server, &vault);
    assert_eq!(creates(&server, "Note"), 1);
    let left = state(&vault);
    assert_eq!(left.notes.len(), 1, "the new note is not tracked yet");
    let pending = left.extra["pendingCreates"]
        .as_object()
        .expect("the create was announced");
    let (record_name, create) = pending.iter().next().unwrap();
    assert_eq!(create["file"], "Notes/Fresh.md");

    let result = push(&server, &vault);
    assert!(result.failures().is_empty(), "{result:?}");
    assert_eq!(creates(&server, "Note"), 1, "not created again");
    let after = state(&vault);
    assert_eq!(after.notes[record_name].file, "Notes/Fresh.md");
    assert!(!after.extra.contains_key("pendingCreates"));
    assert!(
        read(&vault, "Notes/Fresh.md").contains(record_name.as_str()),
        "the file carries its id"
    );

    // And from here on it is a note like any other: nothing left to push.
    let again = push(&server, &vault);
    assert_eq!(again.entries.len(), 0, "{again:?}");
    assert_eq!(creates(&server, "Note"), 1);
}

/// The same for a new folder: the folder made just before the kill is the
/// folder the next push puts the note in, not one of two of that name.
#[test]
fn a_folder_created_just_before_the_push_was_killed_is_not_created_twice() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = dies_after_creating("Folder");
    std::fs::create_dir_all(vault.join("Projects")).unwrap();
    std::fs::write(vault.join("Projects/Plan.md"), "Plan\n\nFirst steps.\n").unwrap();

    killed_push(&server, &vault);
    assert_eq!((creates(&server, "Folder"), creates(&server, "Note")), (1, 0));

    let result = push(&server, &vault);
    assert!(result.failures().is_empty(), "{result:?}");
    assert_eq!((creates(&server, "Folder"), creates(&server, "Note")), (1, 1));
    let after = state(&vault);
    assert!(!after.extra.contains_key("pendingCreates"));
    let (folder, _) = after
        .folders
        .as_ref()
        .unwrap()
        .iter()
        .find(|(_, f)| f.dir_name == "Projects")
        .expect("the folder is recorded");
    let note = after.notes.values().find(|e| e.file == "Projects/Plan.md").unwrap();
    assert_eq!(note.folder_record_name.as_ref(), Some(folder));
}

/// A create announced and never sent (killed before the request): iCloud
/// has no such record, and the note is created as it would have been.
#[test]
fn a_create_announced_and_never_sent_is_created_by_the_next_push() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    std::fs::write(vault.join("Notes/Fresh.md"), "Fresh\n\nA note made here.\n").unwrap();
    let mut announced = state(&vault);
    announced.extra.insert(
        "pendingCreates".into(),
        json!({ "a1b2c3d4-0000-4000-8000-00000000dead": { "file": "Notes/Fresh.md" } }),
    );
    write_clone_state(&vault, &announced).unwrap();

    let result = push(&server, &vault);
    assert_eq!(result.pushed, Some(1), "{result:?}");
    assert_eq!(creates(&server, "Note"), 1);
    let after = state(&vault);
    assert!(!after.extra.contains_key("pendingCreates"));
    assert!(!after.notes.contains_key("a1b2c3d4-0000-4000-8000-00000000dead"));
    assert!(after.notes.values().any(|e| e.file == "Notes/Fresh.md"));
}
