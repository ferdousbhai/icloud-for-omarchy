//! Runs killed part way (docs/DESIGN.md, "A run that fails or is killed part
//! way"): the real `icloud-notes-sync` binary, over icloud-session's mock
//! session, against a fake iCloud that keeps what it is sent (below), killed
//! at each crash point (`src/crash.rs`, `ICLOUD_NOTES_SYNC_CRASH_AT`) and at
//! each time a point is reached in a run, then run again the way the Notes
//! app would carry on. After that the vault and the fake must agree:
//!
//! - no note was created in iCloud twice (the creates in the fake's log are
//!   the notes made here, once each, and iCloud has no more notes than that);
//! - nothing the user did not delete was moved to Recently Deleted, marked
//!   deleted or untracked while iCloud still has it, and every attachment
//!   iCloud has for a note is tracked at a file that is there;
//! - no edit made here is lost: it is in the file and in iCloud;
//! - state.json reads back, stamped with this build's layout and generator,
//!   and nothing is left for a push to do.
//!
//! Which points a run reaches, and how often, comes from a run with
//! `ICLOUD_NOTES_SYNC_CRASH_TRACE`; every point of `crash::POINTS` is
//! reached by at least one of the runs here
//! (`every_crash_point_is_reached_by_some_test`).
#![cfg(debug_assertions)]

use std::collections::{BTreeSet, HashMap};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::crash;
use icloud_notes_sync::doc::document::{build_initial_note_document, encode_note_document};
use icloud_notes_sync::doc::text::{compress_note_document, decode_note_body_text};
use icloud_notes_sync::js::{base64_decode, base64_encode};
use icloud_notes_sync::vault::state::{CURRENT_LAYOUT_VERSION, read_clone_state};
use serde_json::{Value, json};

const TRASH: &str = "TrashFolder-CloudKit";
const DEFAULT_FOLDER: &str = "DefaultFolder-CloudKit";
const WORK_FOLDER: &str = "F0000000-0000-4000-8000-00000000F00D";

// --- the fake iCloud ------------------------------------------------------------

/// One record as iCloud keeps it.
#[derive(Debug, Clone)]
struct Stored {
    /// `{recordName, recordType, fields, recordChangeTag, parent?}`.
    record: Value,
    /// The change counter when it last changed.
    changed_at: u64,
    deleted: bool,
}

/// One operation a `records/modify` applied.
#[derive(Debug, Clone)]
struct Applied {
    op: String,
    record_type: String,
    record_name: String,
    fields: Value,
}

/// The private `Notes` zone, the assets, and what was done to them.
#[derive(Debug, Clone, Default)]
struct Zone {
    records: indexmap::IndexMap<String, Stored>,
    counter: u64,
    assets: HashMap<String, Vec<u8>>,
    next_upload: u64,
    applied: Vec<Applied>,
    page_size: usize,
    /// The shared zones, by owner (the shared database; their assets and
    /// uploads live in the private zone's).
    shared: indexmap::IndexMap<String, Zone>,
    /// Shared zones whose owner stopped sharing them.
    revoked: BTreeSet<String>,
}

fn field_type(name: &str, value: &Value) -> &'static str {
    match name {
        "CreationDate" | "ModificationDate" | "FoldersModificationDate" | "LastModificationDate" => "TIMESTAMP",
        "Folders" => "REFERENCE_LIST",
        "Folder" | "Media" | "Note" | "ParentFolder" => "REFERENCE",
        "Asset" | "TextDataAsset" => "ASSETID",
        _ if name.ends_with("Encrypted") => "ENCRYPTED_BYTES",
        _ if value.is_i64() || value.is_u64() => "INT64",
        _ if value.is_f64() => "DOUBLE",
        _ => "STRING",
    }
}

impl Zone {
    fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Puts `record` (fields already typed) in the zone as changed now.
    fn put(&mut self, mut record: Value) {
        let at = self.tick();
        record["recordChangeTag"] = json!(format!("c{at}"));
        let name = record["recordName"].as_str().unwrap().to_owned();
        self.records.insert(
            name,
            Stored {
                record,
                changed_at: at,
                deleted: false,
            },
        );
    }

    fn live(&self, name: &str) -> Option<&Value> {
        self.records.get(name).filter(|s| !s.deleted).map(|s| &s.record)
    }

    fn live_notes(&self) -> Vec<&Value> {
        self.records
            .values()
            .filter(|s| !s.deleted && s.record["recordType"] == "Note")
            .map(|s| &s.record)
            .filter(|r| r["fields"]["Folder"]["value"]["recordName"] != TRASH && r["fields"]["Deleted"]["value"] != 1)
            .collect()
    }

    /// A request field set (`{name: {value}}`) applied to `record`'s fields.
    fn apply_fields(&mut self, record: &mut Value, fields: &Value, base: &str) {
        let stored = record["fields"].as_object_mut().unwrap();
        for (name, field) in fields.as_object().unwrap() {
            match field.get("value") {
                None | Some(Value::Null) => {
                    stored.remove(name);
                }
                Some(value) => {
                    let kind = stored
                        .get(name)
                        .and_then(|f| f["type"].as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| field_type(name, value).to_owned());
                    let value = if kind == "ASSETID" && value.get("receipt").is_some() {
                        let receipt = value["receipt"].as_str().unwrap();
                        json!({
                            "fileChecksum": value["fileChecksum"],
                            "size": value["size"],
                            "downloadURL": format!("{base}/asset/{receipt}"),
                        })
                    } else {
                        value.clone()
                    };
                    stored.insert(name.clone(), json!({ "value": value, "type": kind }));
                }
            }
        }
    }

    fn answer_record(stored: &Stored) -> Value {
        let mut record = stored.record.clone();
        record["modified"] = json!({ "timestamp": 1_760_000_000_000u64 + stored.changed_at * 1000 });
        record
    }

    fn changes(&self, body: &Value) -> Value {
        let zone = &body["zones"][0];
        let since: Option<u64> = zone["syncToken"]
            .as_str()
            .and_then(|t| t.strip_prefix('t'))
            .and_then(|t| t.parse().ok());
        let mut changed: Vec<&Stored> = self
            .records
            .values()
            .filter(|s| match since {
                Some(since) => s.changed_at > since,
                None => !s.deleted,
            })
            .collect();
        changed.sort_by_key(|s| s.changed_at);
        let page_size = if self.page_size == 0 {
            usize::MAX
        } else {
            self.page_size
        };
        let more = changed.len() > page_size;
        changed.truncate(page_size);
        let token = match (more, changed.last()) {
            (true, Some(last)) => last.changed_at,
            _ => self.counter,
        };
        let records: Vec<Value> = changed
            .iter()
            .map(|s| {
                if s.deleted {
                    json!({ "recordName": s.record["recordName"], "deleted": true })
                } else {
                    Zone::answer_record(s)
                }
            })
            .collect();
        let zone_id = match zone["zoneID"].get("ownerRecordName") {
            Some(_) => zone["zoneID"].clone(),
            None => json!({ "zoneName": "Notes", "ownerRecordName": "_defaultOwner" }),
        };
        json!({ "zones": [{ "zoneID": zone_id,
            "moreComing": more, "syncToken": format!("t{token}"), "records": records }] })
    }

    fn lookup(&self, body: &Value) -> Value {
        let records: Vec<Value> = body["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let name = r["recordName"].as_str().unwrap();
                match self.records.get(name).filter(|s| !s.deleted) {
                    Some(stored) => Zone::answer_record(stored),
                    None => json!({ "recordName": name, "serverErrorCode": "NOT_FOUND" }),
                }
            })
            .collect();
        json!({ "records": records })
    }

    fn modify(&mut self, body: &Value, base: &str) -> Value {
        let atomic = body["atomic"] == true;
        let ops = body["operations"].as_array().unwrap().clone();
        // Checked first, applied after: an atomic request is all or nothing.
        let refusal = |zone: &Zone, op: &Value| -> Option<&'static str> {
            let record = &op["record"];
            let name = record["recordName"].as_str().unwrap();
            match op["operationType"].as_str().unwrap() {
                "create" => zone.records.get(name).is_some().then_some("RECORD_EXISTS"),
                _ => match zone.live(name) {
                    None => Some("NOT_FOUND"),
                    Some(live) if live["recordChangeTag"] != record["recordChangeTag"] => Some("CONFLICT"),
                    Some(_) => None,
                },
            }
        };
        let refusals: Vec<Option<&str>> = ops.iter().map(|op| refusal(self, op)).collect();
        let any_refused = refusals.iter().any(Option::is_some);
        let mut answers = Vec::new();
        for (op, refused) in ops.iter().zip(refusals) {
            let record = &op["record"];
            let name = record["recordName"].as_str().unwrap().to_owned();
            if let Some(code) = refused {
                answers.push(json!({ "recordName": name, "serverErrorCode": code }));
                continue;
            }
            if atomic && any_refused {
                answers.push(json!({ "recordName": name, "serverErrorCode": "ATOMIC_ERROR" }));
                continue;
            }
            let kind = op["operationType"].as_str().unwrap().to_owned();
            let record_type = match kind.as_str() {
                "create" => record["recordType"].as_str().unwrap().to_owned(),
                _ => self.live(&name).unwrap()["recordType"].as_str().unwrap().to_owned(),
            };
            self.applied.push(Applied {
                op: kind.clone(),
                record_type: record_type.clone(),
                record_name: name.clone(),
                fields: record["fields"].clone(),
            });
            match kind.as_str() {
                "delete" => {
                    let at = self.tick();
                    let stored = self.records.get_mut(&name).unwrap();
                    stored.deleted = true;
                    stored.changed_at = at;
                    answers.push(json!({ "recordName": name, "deleted": true }));
                    continue;
                }
                "create" => {
                    let mut new = json!({ "recordName": name, "recordType": record_type, "fields": {} });
                    if let Some(parent) = record.get("parent") {
                        new["parent"] = parent.clone();
                    }
                    self.apply_fields(&mut new, &record["fields"], base);
                    self.put(new);
                }
                _ => {
                    let mut stored = self.live(&name).unwrap().clone();
                    self.apply_fields(&mut stored, &record["fields"], base);
                    if let Some(parent) = record.get("parent") {
                        stored["parent"] = parent.clone();
                    }
                    self.put(stored);
                }
            }
            answers.push(Zone::answer_record(&self.records[&name]));
        }
        json!({ "records": answers })
    }

    /// The zones still shared with the account, the private one first.
    fn zones(&self) -> Vec<&Zone> {
        let shared = self
            .shared
            .iter()
            .filter(|(o, _)| !self.revoked.contains(*o))
            .map(|(_, z)| z);
        std::iter::once(self).chain(shared).collect()
    }

    fn all_applied(&self) -> Vec<&Applied> {
        std::iter::once(self)
            .chain(self.shared.values())
            .flat_map(|z| &z.applied)
            .collect()
    }

    fn creates(&self, record_type: &str) -> Vec<&Applied> {
        self.all_applied()
            .into_iter()
            .filter(|a| a.op == "create" && a.record_type == record_type)
            .collect()
    }

    /// Every note iCloud has for the account, in its zones and the ones
    /// shared with it.
    fn all_live_notes(&self) -> Vec<&Value> {
        self.zones().into_iter().flat_map(|z| z.live_notes()).collect()
    }

    fn find_live(&self, name: &str) -> Option<&Value> {
        self.zones().into_iter().find_map(|z| z.live(name))
    }

    /// The shared database's zone list: a listing from scratch has the
    /// zones still shared, a resumed one the revoked ones too, as deleted.
    fn shared_listing(&self, body: &Value) -> Value {
        let resumed = body.get("syncToken").is_some();
        let zones: Vec<Value> = self
            .shared
            .keys()
            .filter_map(|owner| {
                let id = json!({ "zoneName": "Notes", "ownerRecordName": owner });
                match self.revoked.contains(owner) {
                    false => Some(json!({ "zoneID": id })),
                    true if resumed => Some(json!({ "zoneID": id, "deleted": true })),
                    true => None,
                }
            })
            .collect();
        json!({ "zones": zones, "moreComing": false, "syncToken": format!("s{}", self.counter) })
    }

    /// The shared zone a request names, if it is still shared.
    fn shared_zone(&mut self, body: &Value) -> Option<&mut Zone> {
        let zone_id = body.get("zoneID").or_else(|| body["zones"][0].get("zoneID"))?;
        let owner = zone_id["ownerRecordName"].as_str()?;
        if self.revoked.contains(owner) {
            return None;
        }
        self.shared.get_mut(owner)
    }
}

/// The fake on a local port, serving the mock session's requests.
struct Fake {
    base: String,
    zone: Arc<Mutex<Zone>>,
}

impl Fake {
    fn start(zone: Zone) -> Fake {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let zone = Arc::new(Mutex::new(zone));
        let (shared, url) = (zone.clone(), base.clone());
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut bytes = Vec::new();
                request.as_reader().read_to_end(&mut bytes).unwrap();
                let path = request.url().split('?').next().unwrap_or("").to_owned();
                let mut zone = shared.lock().unwrap();
                let answer: Result<Vec<u8>, u16> = (|| {
                    const PRIVATE: &str = "/database/1/com.apple.notes/production/private/";
                    const SHARED: &str = "/database/1/com.apple.notes/production/shared/";
                    let json_body = || serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                    let reply = |v: Value| Ok(v.to_string().into_bytes());
                    if let Some(op) = path.strip_prefix(PRIVATE) {
                        return match op {
                            "changes/zone" => reply(zone.changes(&json_body())),
                            "records/lookup" => reply(zone.lookup(&json_body())),
                            "records/modify" => reply(zone.modify(&json_body(), &url)),
                            "assets/upload" => {
                                zone.next_upload += 1;
                                let n = zone.next_upload;
                                reply(json!({ "tokens": [{ "url": format!("{url}/upload/{n}") }] }))
                            }
                            _ => Err(404),
                        };
                    }
                    if let Some(op) = path.strip_prefix(SHARED) {
                        let body = json_body();
                        if op == "changes/database" {
                            return reply(zone.shared_listing(&body));
                        }
                        let Some(shared) = zone.shared_zone(&body) else {
                            return match op {
                                "changes/zone" => reply(json!({ "zones": [{ "zoneID": body["zones"][0]["zoneID"],
                                    "serverErrorCode": "ZONE_NOT_FOUND", "reason": "no longer shared" }] })),
                                _ => Err(404),
                            };
                        };
                        return match op {
                            "changes/zone" => reply(shared.changes(&body)),
                            "records/lookup" => reply(shared.lookup(&body)),
                            "records/modify" => reply(shared.modify(&body, &url)),
                            _ => Err(404),
                        };
                    }
                    if let Some(n) = path.strip_prefix("/upload/") {
                        let receipt = format!("r{n}");
                        let size = bytes.len();
                        zone.assets.insert(format!("/asset/{receipt}"), bytes.clone());
                        return reply(json!({ "singleFile": {
                            "fileChecksum": format!("ck-{receipt}"), "size": size, "wrappingKey": "k",
                            "referenceChecksum": "rc", "receipt": receipt } }));
                    }
                    zone.assets.get(&path).cloned().ok_or(404)
                })();
                drop(zone);
                let response = match answer {
                    Ok(body) => tiny_http::Response::from_data(body),
                    Err(status) => {
                        eprintln!("fake iCloud: nothing at {path}");
                        tiny_http::Response::from_data(b"{}".to_vec()).with_status_code(status)
                    }
                };
                let _ = request.respond(response);
            }
        });
        Fake { base, zone }
    }

    fn zone(&self) -> std::sync::MutexGuard<'_, Zone> {
        self.zone.lock().unwrap()
    }
}

// --- what the account holds -----------------------------------------------------

/// A note document holding `text` (its first line the title), as iCloud
/// keeps it.
fn note_text_field(text: &str) -> String {
    let doc = build_initial_note_document(text, &[7; 16]).unwrap();
    base64_encode(&compress_note_document(&encode_note_document(&doc).unwrap()))
}

fn note_record(name: &str, text: &str, folder: &str) -> Value {
    let title = text.lines().next().unwrap_or_default();
    let reference = json!({ "recordName": folder, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
    json!({
        "recordName": name,
        "recordType": "Note",
        "parent": { "recordName": folder },
        "fields": {
            "TitleEncrypted": { "value": base64_encode(title.as_bytes()), "type": "ENCRYPTED_BYTES" },
            "SnippetEncrypted": { "value": base64_encode(title.as_bytes()), "type": "ENCRYPTED_BYTES" },
            "CreationDate": { "value": 1_752_563_940_000u64, "type": "TIMESTAMP" },
            "ModificationDate": { "value": 1_752_564_000_000u64, "type": "TIMESTAMP" },
            "Deleted": { "value": 0, "type": "INT64" },
            "Folder": { "value": reference, "type": "REFERENCE" },
            "Folders": { "value": [reference], "type": "REFERENCE_LIST" },
            "TextDataEncrypted": { "value": note_text_field(text), "type": "ENCRYPTED_BYTES" },
        }
    })
}

fn folder_record(name: &str, title: &str) -> Value {
    json!({ "recordName": name, "recordType": "Folder",
        "fields": { "TitleEncrypted": { "value": base64_encode(title.as_bytes()), "type": "ENCRYPTED_BYTES" } } })
}

/// The `attach-clone` account (the default folder, a plain note and two
/// notes with a file attachment each), plus a folder `Work` and `extra`
/// plain notes split between the two folders.
fn account(extra: usize) -> Zone {
    let cassette: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential/cassettes/attach-clone.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut zone = Zone {
        page_size: 10,
        ..Default::default()
    };
    for interaction in cassette["interactions"].as_array().unwrap() {
        let answer = &interaction["response"]["body"];
        let listed = answer["zones"][0]["records"]
            .as_array()
            .or(answer["records"].as_array());
        for record in listed.into_iter().flatten() {
            let mut record = record.clone();
            if let Some(object) = record.as_object_mut() {
                object.remove("recordChangeTag");
                object.remove("modified");
            }
            zone.put(record);
        }
        if let (Some(url), Some(bytes)) = (
            interaction["request"]["url"].as_str(),
            interaction["response"]["bodyBase64"].as_str(),
        ) {
            let path = url::Url::parse(url).unwrap().path().to_owned();
            zone.assets.insert(path, base64_decode(bytes));
        }
    }
    zone.put(folder_record(WORK_FOLDER, "Work"));
    for i in 0..extra {
        let folder = if i % 3 == 0 { WORK_FOLDER } else { DEFAULT_FOLDER };
        zone.put(note_record(
            &format!("E0000000-0000-4000-8000-{i:012}"),
            &format!("Extra {i}\nline of note {i}"),
            folder,
        ));
    }
    zone
}

// --- running the binary -----------------------------------------------------------

/// A vault directory and the runtime directory its lock goes in.
struct Place {
    _tmp: tempfile::TempDir,
    vault: PathBuf,
    runtime: PathBuf,
}

impl Place {
    fn new() -> Place {
        let tmp = tempfile::tempdir().unwrap();
        let (vault, runtime) = (tmp.path().join("vault"), tmp.path().join("run"));
        std::fs::create_dir_all(&runtime).unwrap();
        Place {
            _tmp: tmp,
            vault,
            runtime,
        }
    }

    /// A place holding a copy of `from`'s vault.
    fn copy_of(from: &Place) -> Place {
        let place = Place::new();
        if from.vault.exists() {
            copy_dir(&from.vault, &place.vault);
        }
        place
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            let mtime = entry.metadata().unwrap().modified().unwrap();
            std::fs::File::options()
                .write(true)
                .open(&target)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    }
}

/// How a run is cut short.
#[derive(Debug, Clone, Default)]
struct Crash {
    at: Option<(String, u64)>,
    trace: Option<PathBuf>,
}

fn run(fake: &Fake, place: &Place, command: &str, crash: &Crash) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"));
    // `clone-filename`: a clone whose notes carry their title in the file name.
    let filename_mode = command == "clone-filename";
    cmd.arg("--json").arg(if filename_mode { "clone" } else { command });
    if command.starts_with("clone") {
        cmd.args(["--account", "mock"]);
    }
    if filename_mode {
        cmd.arg("--filename-as-title");
    }
    cmd.arg(&place.vault)
        .env_remove("ICLOUD_NOTES_SYNC_CASSETTE")
        .env_remove(crash::CRASH_AT_ENV)
        .env_remove(crash::CRASH_TRACE_ENV)
        .env_remove("ICLOUD_NOTES_SYNC_DETERMINISTIC")
        .env("ICLOUD_SESSION_MOCK", "1")
        .env("ICLOUD_SESSION_MOCK_URL", &fake.base)
        .env("XDG_RUNTIME_DIR", &place.runtime)
        .env("HOME", place.runtime.join("home"));
    if let Some((point, n)) = &crash.at {
        cmd.env(crash::CRASH_AT_ENV, format!("{point}:{n}"));
    }
    if let Some(trace) = &crash.trace {
        cmd.env(crash::CRASH_TRACE_ENV, trace);
    }
    cmd.output().expect("the binary runs")
}

fn says(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn succeeded(output: &Output, what: &str) {
    assert!(output.status.success(), "{what}: {}", says(output));
}

/// Every point a run reaches, with how many times it was reached by then:
/// each place the run can be cut short.
fn crash_sites(fake: &Fake, place: &Place, command: &str) -> Vec<(String, u64)> {
    let trace = place.runtime.join("trace");
    let output = run(
        fake,
        place,
        command,
        &Crash {
            trace: Some(trace.clone()),
            ..Default::default()
        },
    );
    succeeded(&output, &format!("{command}, traced"));
    let mut seen: HashMap<String, u64> = HashMap::new();
    std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .map(|point| {
            let n = seen.entry(point.to_owned()).or_default();
            *n += 1;
            (point.to_owned(), *n)
        })
        .collect()
}

// --- what must hold afterwards ----------------------------------------------------

/// What the user did here, which a run cut short must not undo.
#[derive(Debug, Clone, Default)]
struct Expect {
    /// Notes iCloud has to have, by record name, before the run (all of
    /// them stay unless listed in `deleted`).
    kept: Vec<String>,
    /// Notes the user deleted here.
    deleted: Vec<String>,
    /// Folders the user deleted here.
    deleted_folders: Vec<String>,
    /// What happens in iCloud after the run is cut short and before it is
    /// carried on with (`kept` is what iCloud has after it).
    between: Option<fn(&mut Zone)>,
    /// How many notes the user made here: each is created once.
    new_notes: usize,
    /// How many attachments the user added here: each is created once.
    new_attachments: usize,
    /// How many folders the user made here: each is created once.
    new_folders: usize,
    /// Text the user typed here: (file, text) - in the file, and in the
    /// note iCloud has.
    edits: Vec<(String, String)>,
    /// A filename-title vault's titles: (file, title) - the note at that
    /// file has the title in iCloud.
    titles: Vec<(String, String)>,
}

fn body_of(record: &Value) -> String {
    let text = record["fields"]["TextDataEncrypted"]["value"]
        .as_str()
        .unwrap_or_default();
    decode_note_body_text(&base64_decode(text)).unwrap_or_default()
}

fn note_id_of(text: &str) -> Option<String> {
    text.lines()
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.strip_prefix("apple-note-id: "))
        .map(|id| id.trim().trim_matches('"').to_owned())
}

fn markdown_files(dir: &Path, rel: &str, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir.join(rel)).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "attachments" {
            continue;
        }
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if entry.file_type().unwrap().is_dir() {
            markdown_files(dir, &path, out);
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
}

fn check(fake: &Fake, place: &Place, expect: &Expect, label: &str) {
    let zone = fake.zone();
    let vault = &place.vault;
    // state.json reads back, stamped.
    let raw: Value =
        serde_json::from_str(&std::fs::read_to_string(vault.join(".icloud-notes/state.json")).unwrap()).unwrap();
    assert_eq!(raw["layoutVersion"], CURRENT_LAYOUT_VERSION, "{label}");
    assert_eq!(raw["generator"], icloud_notes_sync::GENERATOR, "{label}");
    assert!(raw.get("pendingCreates").is_none(), "{label}: {raw:#}");
    let state = read_clone_state(vault).unwrap().unwrap();

    // Nothing created twice.
    let created_notes = zone.creates("Note");
    assert_eq!(
        created_notes.len(),
        expect.new_notes,
        "{label}: notes created in iCloud: {created_notes:#?}"
    );
    let created_folders = zone.creates("Folder");
    assert_eq!(
        created_folders.len(),
        expect.new_folders,
        "{label}: folders created in iCloud: {created_folders:#?}"
    );
    let created_attachments = zone.creates("Attachment");
    assert_eq!(
        created_attachments.len(),
        expect.new_attachments,
        "{label}: attachments created in iCloud: {created_attachments:#?}"
    );
    let live = zone.all_live_notes();
    assert_eq!(
        live.len(),
        expect.kept.len() - expect.deleted.len() + expect.new_notes,
        "{label}: notes in iCloud"
    );

    // Nothing the user kept deleted, trashed or untracked.
    for applied in zone.all_applied() {
        let trashed = applied.fields["Folder"]["value"]["recordName"] == TRASH;
        let marked = applied.fields["Deleted"]["value"] == 1 && applied.record_type == "Note";
        if applied.op == "delete" || trashed || marked {
            assert!(
                expect.deleted.contains(&applied.record_name) || expect.deleted_folders.contains(&applied.record_name),
                "{label}: {applied:?} deletes what the user kept"
            );
        }
    }
    for record in &live {
        let name = record["recordName"].as_str().unwrap();
        let entry = state
            .notes
            .get(name)
            .unwrap_or_else(|| panic!("{label}: {name} is in iCloud and not tracked"));
        let text = std::fs::read_to_string(vault.join(&entry.file))
            .unwrap_or_else(|e| panic!("{label}: {name}'s file {}: {e}", entry.file));
        assert_eq!(note_id_of(&text).as_deref(), Some(name), "{label}: {}", entry.file);
    }
    for name in &expect.deleted {
        assert!(!state.notes.contains_key(name), "{label}: {name} was deleted here");
    }
    for name in &expect.deleted_folders {
        assert!(zone.live(name).is_none(), "{label}: the folder {name} was deleted here");
        let folders = state.folders.clone().unwrap_or_default();
        assert!(
            !folders.contains_key(name),
            "{label}: the folder {name} is still tracked"
        );
    }
    let attachments = state.attachments.clone().unwrap_or_default();
    for stored in zone.records.values().filter(|s| !s.deleted) {
        let record = &stored.record;
        let note = record["fields"]["Note"]["value"]["recordName"].as_str();
        if record["recordType"] != "Attachment" || record["fields"]["Deleted"]["value"] == 1 {
            continue;
        }
        if !note.is_some_and(|note| live.iter().any(|r| r["recordName"] == note)) {
            continue;
        }
        let name = record["recordName"].as_str().unwrap();
        let entry = attachments
            .get(name)
            .unwrap_or_else(|| panic!("{label}: the attachment {name} is in iCloud and not tracked"));
        assert!(vault.join(&entry.file).is_file(), "{label}: {} is gone", entry.file);
    }

    // No edit lost.
    for (file, text) in &expect.edits {
        let on_disk = std::fs::read_to_string(vault.join(file)).unwrap_or_default();
        assert!(on_disk.contains(text), "{label}: {file} lost {text:?}:\n{on_disk}");
        let id = note_id_of(&on_disk).unwrap_or_else(|| panic!("{label}: {file} has no id"));
        let record = zone
            .find_live(&id)
            .unwrap_or_else(|| panic!("{label}: {file}'s note {id} is not in iCloud"));
        assert!(body_of(record).contains(text), "{label}: iCloud lost {text:?}");
    }

    for (file, title) in &expect.titles {
        let id = state
            .notes
            .iter()
            .find(|(_, e)| e.file == *file)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| panic!("{label}: nothing is tracked at {file}"));
        let record = zone
            .find_live(&id)
            .unwrap_or_else(|| panic!("{label}: {id} is not in iCloud"));
        let body = body_of(record);
        assert_eq!(
            body.lines().next(),
            Some(title.as_str()),
            "{label}: {file} in iCloud: {body:?}"
        );
    }

    // Every note file is a tracked note's: nothing left to be created.
    // (The edited copy of a note no longer shared is kept, and not synced.)
    let unshared: BTreeSet<&str> = raw["unshared"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut files = Vec::new();
    markdown_files(vault, "", &mut files);
    files.retain(|file| {
        let text = std::fs::read_to_string(vault.join(file)).unwrap_or_default();
        !note_id_of(&text).is_some_and(|id| unshared.contains(id.as_str()) && !state.notes.contains_key(&id))
    });
    let tracked: BTreeSet<&str> = state.notes.values().map(|e| e.file.as_str()).collect();
    for file in &files {
        assert!(tracked.contains(file.as_str()), "{label}: {file} is not tracked");
    }
    assert_eq!(files.len(), state.notes.len(), "{label}: {files:?}");
    drop(zone);
    let preview = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(["--quiet", "status"])
        .arg(vault)
        .env_remove("ICLOUD_NOTES_SYNC_CASSETTE")
        .env("ICLOUD_SESSION_MOCK", "1")
        .env("ICLOUD_SESSION_MOCK_URL", &fake.base)
        .env("XDG_RUNTIME_DIR", &place.runtime)
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&preview.stdout).unwrap_or(Value::Null);
    assert_eq!(
        status["entries"],
        json!([]),
        "{label}: left to push: {}",
        says(&preview)
    );
}

// --- the scenarios ------------------------------------------------------------------

/// A starting point: the fake's account and a vault, both as `prepare`
/// left them, and what the run to cut short must keep.
struct Start {
    zone: Zone,
    place: Place,
    expect: Expect,
}

/// The points a run reaches once per note (or per write) in a loop.
const PER_NOTE_POINTS: &[&str] = &[
    crash::TEMP_FILE,
    crash::NOTE_FILE,
    crash::BASE_COPY,
    crash::HISTORY_SNAPSHOT,
    crash::HISTORY_EPOCH,
];

/// The places worth cutting a run short at: every place but the per-note
/// points' (`PER_NOTE_POINTS`), and of those, the first two times and the
/// last time each is reached, and the times within a few places of one of
/// the others (a checkpoint, a request, a rename...): the notes in between
/// go through the same code with nothing else around them.
/// `CRASH_POINTS_ALL=1` keeps every place.
fn distinct_sites(sites: Vec<(String, u64)>) -> Vec<(String, u64)> {
    if std::env::var_os("CRASH_POINTS_ALL").is_some() {
        return sites;
    }
    const NEAR: usize = 4;
    let per_note = |point: &str| PER_NOTE_POINTS.contains(&point);
    let last: HashMap<&str, u64> = sites
        .iter()
        .map(|(p, n)| (p.as_str(), *n))
        .fold(HashMap::new(), |mut m, (p, n)| {
            m.insert(p, n);
            m
        });
    let anchors: Vec<usize> = (0..sites.len()).filter(|&i| !per_note(&sites[i].0)).collect();
    sites
        .iter()
        .enumerate()
        .filter(|(i, (point, n))| {
            !per_note(point)
                || *n <= 2
                || last.get(point.as_str()) == Some(n)
                || anchors.iter().any(|a| a.abs_diff(*i) <= NEAR)
        })
        .map(|(_, site)| site.clone())
        .collect()
}

/// How a run cut short is carried on with.
#[derive(Debug, Clone, Copy)]
enum Then {
    /// The same command again (a clone that left state.json: a pull), then a
    /// sync.
    Again,
    /// A sync straight away, then another: what the Notes app does next.
    Sync,
}

/// Kills `command` at every place it can be cut short, carries on each
/// way in `thens`, and checks what must hold. Runs the cases side by side.
fn crash_everywhere(start: &Start, command: &str, thens: &[Then]) {
    let fake = Fake::start(start.zone.clone());
    let traced = Place::copy_of(&start.place);
    let sites = crash_sites(&fake, &traced, command);
    assert!(!sites.is_empty(), "{command} reached no crash point");
    if std::env::var_os("CRASH_POINTS_VERBOSE").is_some() {
        eprintln!("{command}: {} places to cut it short: {sites:?}", sites.len());
    }
    // The run that is not cut short keeps it all too.
    if let Some(between) = start.expect.between {
        between(&mut fake.zone());
    }
    for then in thens {
        carry_on(&fake, &traced, command, *then);
    }
    check(&fake, &traced, &start.expect, &format!("{command} not cut short"));

    let only = std::env::var("CRASH_POINTS_ONLY").ok();
    let sites = distinct_sites(sites);
    let cases: Vec<((String, u64), Then)> = sites
        .iter()
        .filter(|(p, n)| only.as_ref().is_none_or(|o| *o == format!("{p}:{n}")))
        .flat_map(|site| thens.iter().map(move |then| (site.clone(), *then)))
        .collect();
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let next = Mutex::new(0usize);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = {
                        let mut next = next.lock().unwrap();
                        let i = *next;
                        *next += 1;
                        i
                    };
                    let Some(((point, n), then)) = cases.get(i) else {
                        break;
                    };
                    let label = format!("{command} killed at {point}:{n}, then {then:?}");
                    let result = std::panic::catch_unwind(|| one_case(start, command, point, *n, *then, &label));
                    if let Err(panic) = result {
                        let message = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default();
                        failures.lock().unwrap().push(message);
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}

fn one_case(start: &Start, command: &str, point: &str, n: u64, then: Then, label: &str) {
    one_case_place(start, command, point, n, then, label);
}

/// [`one_case`], handing back the vault it checked.
fn one_case_place(start: &Start, command: &str, point: &str, n: u64, then: Then, label: &str) -> Place {
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let killed = run(
        &fake,
        &place,
        command,
        &Crash {
            at: Some((point.to_owned(), n)),
            ..Default::default()
        },
    );
    assert_eq!(
        killed.status.signal(),
        Some(6),
        "{label}: not killed: {}",
        says(&killed)
    );
    if let Some(between) = start.expect.between {
        between(&mut fake.zone());
    }
    let outputs = carry_on(&fake, &place, command, then);
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        check(&fake, &place, &start.expect, label)
    }));
    if let Err(panic) = checked {
        if std::env::var_os("CRASH_POINTS_DUMP").is_some() {
            let mut files = Vec::new();
            markdown_files(&place.vault, "", &mut files);
            eprintln!(
                "=== {label}\nfiles: {files:?}\nstate: {}\n{}",
                std::fs::read_to_string(place.vault.join(".icloud-notes/state.json")).unwrap_or_default(),
                outputs.iter().map(says).collect::<Vec<_>>().join("\n")
            );
        }
        std::panic::resume_unwind(panic);
    }
    place
}

fn carry_on(fake: &Fake, place: &Place, command: &str, then: Then) -> Vec<Output> {
    let cloned = [".icloud-notes", ".icloud-md"]
        .iter()
        .any(|dir| place.vault.join(dir).join("state.json").exists());
    let next = match then {
        Then::Again if command == "clone" && cloned => "pull",
        Then::Again => command,
        Then::Sync if !cloned => "clone",
        Then::Sync => "sync",
    };
    let first = run(fake, place, next, &Crash::default());
    succeeded(&first, next);
    let second = run(fake, place, "sync", &Crash::default());
    succeeded(&second, "sync");
    vec![first, second]
}

/// The account cloned into a vault.
fn cloned(extra: usize) -> Start {
    cloned_from(account(extra))
}

/// `zone` cloned into a vault.
fn cloned_from(zone: Zone) -> Start {
    cloned_by(zone, "clone")
}

/// `zone` cloned into a vault by `clone` (or `clone-filename`).
fn cloned_by(zone: Zone, clone: &str) -> Start {
    let fake = Fake::start(zone);
    let place = Place::new();
    succeeded(&run(&fake, &place, clone, &Crash::default()), clone);
    let zone = fake.zone().clone();
    let kept = zone
        .all_live_notes()
        .iter()
        .map(|r| r["recordName"].as_str().unwrap().to_owned())
        .collect();
    let mut zone = zone;
    zone.applied.clear();
    for shared in zone.shared.values_mut() {
        shared.applied.clear();
    }
    Start {
        zone,
        place,
        expect: Expect {
            kept,
            ..Default::default()
        },
    }
}

fn tracked_file(place: &Place, record_name: &str) -> String {
    read_clone_state(&place.vault).unwrap().unwrap().notes[record_name]
        .file
        .clone()
}

#[test]
fn a_clone_cut_short_anywhere_is_finished_by_the_next_run() {
    let zone = account(30);
    let kept = zone
        .all_live_notes()
        .iter()
        .map(|r| r["recordName"].as_str().unwrap().to_owned())
        .collect();
    let start = Start {
        zone,
        place: Place::new(),
        expect: Expect {
            kept,
            ..Default::default()
        },
    };
    crash_everywhere(&start, "clone", &[Then::Again, Then::Sync]);
}

fn kept_notes(zone: &Zone) -> Vec<String> {
    zone.all_live_notes()
        .iter()
        .map(|r| r["recordName"].as_str().unwrap().to_owned())
        .collect()
}

/// Another device added 30 notes (more than a checkpoint's 25), edited
/// two and deleted one: nothing moves, so the pull saves what it applied
/// as it goes.
fn changed_in_icloud(start: &mut Start) {
    let zone = &mut start.zone;
    for i in 0..30 {
        zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Added {i}\nwritten on the phone"),
            if i % 2 == 0 { WORK_FOLDER } else { DEFAULT_FOLDER },
        ));
    }
    for (name, text) in [
        ("E0000000-0000-4000-8000-000000000001", "Extra 1\nedited on the phone"),
        (
            "E0000000-0000-4000-8000-000000000003",
            "Extra 3\nedited on the phone too",
        ),
    ] {
        let mut record = zone.live(name).unwrap().clone();
        record["fields"]["TextDataEncrypted"]["value"] = json!(note_text_field(text));
        zone.put(record);
    }
    let gone = "E0000000-0000-4000-8000-000000000002";
    let at = zone.tick();
    let stored = zone.records.get_mut(gone).unwrap();
    stored.deleted = true;
    stored.changed_at = at;
    start.expect.kept = kept_notes(zone);
}

/// Another device moved a note to another folder, renamed the folder
/// `Work`, replaced the photo of a note with another, and added a few notes.
fn moved_in_icloud(start: &mut Start) {
    let zone = &mut start.zone;
    let mut moved = zone.live("E0000000-0000-4000-8000-000000000004").unwrap().clone();
    let reference = json!({ "recordName": WORK_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
    moved["fields"]["Folder"]["value"] = reference.clone();
    moved["fields"]["Folders"]["value"] = json!([reference]);
    zone.put(moved);
    let mut folder = zone.live(WORK_FOLDER).unwrap().clone();
    folder["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(b"Office"));
    zone.put(folder);
    zone.assets.insert("/B/other-image".into(), b"another photo".to_vec());
    let media_name = "066C8A2E-796F-403F-AD75-A5267CBD0E18";
    let mut media = zone.live(media_name).unwrap().clone();
    media["fields"]["Asset"]["value"] = json!({ "fileChecksum": "ck-other", "size": 13,
        "downloadURL": "https://cvws.icloud-content.com/B/other-image" });
    zone.put(media);
    let photo = zone.live("a1b2c3d4-0000-4000-8000-00000000196e").unwrap().clone();
    zone.put(photo);
    for i in 0..4 {
        zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Added {i}\nwritten on the phone"),
            if i % 2 == 0 { WORK_FOLDER } else { DEFAULT_FOLDER },
        ));
    }
    start.expect.kept = kept_notes(zone);
}

#[test]
fn a_pull_cut_short_anywhere_is_finished_by_the_next_run() {
    let mut start = cloned(6);
    changed_in_icloud(&mut start);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}

#[test]
fn a_pull_that_moves_notes_cut_short_anywhere_is_finished_by_the_next_run() {
    let mut start = cloned(6);
    moved_in_icloud(&mut start);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}

/// What the user did in the vault: three new notes (one with a new
/// attachment, one in a new folder), an edit, an edit that adds an
/// attachment, a deleted note, a note moved to another folder, a removed
/// attachment link, and an edit to a note another device changed too.
fn changed_here(start: &mut Start) {
    let place = &start.place;
    let vault = &place.vault;
    let write = |file: &str, text: &str| {
        let path = vault.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let edit = |file: &str, old: &str, new: &str| {
        let text = std::fs::read_to_string(vault.join(file)).unwrap();
        assert!(text.contains(old), "{file}: {text}");
        std::fs::write(vault.join(file), text.replacen(old, new, 1)).unwrap();
    };
    let expect = &mut start.expect;
    write("Notes/Fresh.md", "Fresh\n\nmade here\n");
    write("Notes/attachments/plan.txt", "the plan\n");
    write("Notes/With file.md", "With file\n\n[plan.txt](attachments/plan.txt)\n");
    write("Projects/Kickoff.md", "Kickoff\n\nin a new folder\n");
    expect.new_notes = 3;
    expect.new_folders = 1;
    expect.edits.push(("Notes/Fresh.md".into(), "made here".into()));
    expect
        .edits
        .push(("Projects/Kickoff.md".into(), "in a new folder".into()));

    let edited = tracked_file(place, "E0000000-0000-4000-8000-000000000001");
    edit(&edited, "line of note 1", "line of note 1, typed here");
    expect.edits.push((edited, "typed here".into()));

    let gains = tracked_file(place, "E0000000-0000-4000-8000-000000000005");
    let dir = Path::new(&gains).parent().unwrap().to_string_lossy().into_owned();
    write(&format!("{dir}/attachments/extra.txt"), "an extra file\n");
    edit(
        &gains,
        "line of note 5",
        "line of note 5\n\n[extra.txt](attachments/extra.txt)",
    );
    expect.new_attachments = 2;

    let deleted = "E0000000-0000-4000-8000-000000000002";
    std::fs::remove_file(vault.join(tracked_file(place, deleted))).unwrap();
    expect.deleted.push(deleted.into());

    let moving = tracked_file(place, "E0000000-0000-4000-8000-000000000004");
    let moved = format!("Work/{}", Path::new(&moving).file_name().unwrap().to_string_lossy());
    std::fs::rename(vault.join(&moving), vault.join(&moved)).unwrap();
    expect.edits.push((moved, "line of note 4".into()));

    // The photo note loses its attachment.
    let photo = tracked_file(place, "a1b2c3d4-0000-4000-8000-00000000196e");
    let text = std::fs::read_to_string(vault.join(&photo)).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains("](attachments/")).collect();
    std::fs::write(vault.join(&photo), kept.join("\n") + "\n").unwrap();

    // Changed on both sides: merged.
    let both = tracked_file(place, "E0000000-0000-4000-8000-000000000007");
    edit(&both, "line of note 7", "line of note 7\nand a line typed here");
    expect.edits.push((both.clone(), "a line typed here".into()));
    let zone = &mut start.zone;
    let mut record = zone.live("E0000000-0000-4000-8000-000000000007").unwrap().clone();
    record["fields"]["TextDataEncrypted"]["value"] =
        json!(note_text_field("Extra 7\nline from the phone first\nline of note 7"));
    zone.put(record);
    expect.edits.push((both, "line from the phone first".into()));
}

#[test]
fn a_push_cut_short_anywhere_is_finished_by_the_next_run() {
    let mut start = cloned(9);
    changed_here(&mut start);
    crash_everywhere(&start, "push", &[Then::Again, Then::Sync]);
}

const ARCHIVE_FOLDER: &str = "F0000000-0000-4000-8000-0000000000AC";

/// The `Work` folder's directory renamed to `Office`, the `Archive`
/// folder's directory deleted with its two notes.
fn folders_changed_here() -> Start {
    let mut zone = account(6);
    zone.put(folder_record(ARCHIVE_FOLDER, "Archive"));
    for i in 0..2 {
        zone.put(note_record(
            &format!("B0000000-0000-4000-8000-{i:012}"),
            &format!("Archived {i}\nold"),
            ARCHIVE_FOLDER,
        ));
    }
    let mut start = cloned_from(zone);
    let vault = &start.place.vault;
    std::fs::rename(vault.join("Work"), vault.join("Office")).unwrap();
    std::fs::remove_dir_all(vault.join("Archive")).unwrap();
    start.expect.deleted = (0..2).map(|i| format!("B0000000-0000-4000-8000-{i:012}")).collect();
    start.expect.deleted_folders = vec![ARCHIVE_FOLDER.into()];
    start
        .expect
        .edits
        .push(("Office/Extra 0.md".into(), "line of note 0".into()));
    start
}

#[test]
fn a_push_of_folder_changes_cut_short_anywhere_is_finished_by_the_next_run() {
    crash_everywhere(&folders_changed_here(), "push", &[Then::Again, Then::Sync]);
}

#[test]
fn a_sync_cut_short_anywhere_is_finished_by_the_next_run() {
    let mut start = cloned(9);
    changed_here(&mut start);
    let zone = &mut start.zone;
    for i in 0..3 {
        zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Added {i}\nwritten on the phone"),
            WORK_FOLDER,
        ));
    }
    start
        .expect
        .kept
        .extend((0..3).map(|i| format!("A0000000-0000-4000-8000-{i:012}")));
    crash_everywhere(&start, "sync", &[Then::Again]);
}

/// Every point is reached by the runs the tests above cut short, so every
/// point has a test.
#[test]
fn every_crash_point_is_reached_by_some_test() {
    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut trace = |start: &Start, command: &str| {
        let fake = Fake::start(start.zone.clone());
        let place = Place::copy_of(&start.place);
        reached.extend(crash_sites(&fake, &place, command).into_iter().map(|(p, _)| p));
    };
    trace(
        &Start {
            zone: account(30),
            place: Place::new(),
            expect: Expect::default(),
        },
        "clone",
    );
    let mut pulled = cloned(6);
    changed_in_icloud(&mut pulled);
    trace(&pulled, "pull");
    let mut moved = cloned(6);
    moved_in_icloud(&mut moved);
    trace(&moved, "pull");
    trace(&older_layout(2), "push");
    let mut pushed = cloned(9);
    changed_here(&mut pushed);
    trace(&pushed, "push");
    trace(&folders_changed_here(), "push");
    let missing: Vec<&str> = crash::POINTS
        .iter()
        .copied()
        .filter(|p| !reached.contains(*p))
        .collect();
    assert!(missing.is_empty(), "no test reaches {missing:?}");
}

// --- what the runs above found, one case each ---------------------------------------

/// A pull killed after it wrote a new note's file and before it recorded
/// the note: the file carries the id of a note iCloud has, which nothing
/// here tracks. The sync after it (push first) used to create it as a new
/// note, a second copy; now the push leaves it and the pull takes it up.
#[test]
fn a_note_a_pull_cut_short_wrote_is_not_created_again_by_the_next_sync() {
    let mut start = cloned(3);
    start.zone.put(note_record(
        "A0000000-0000-4000-8000-000000000000",
        "Added\nwritten on the phone",
        DEFAULT_FOLDER,
    ));
    start.expect.kept = kept_notes(&start.zone);
    one_case(
        &start,
        "pull",
        crash::NOTE_FILE,
        1,
        Then::Sync,
        "pull killed at its first note",
    );
}

/// A pull killed after it made the directory of a folder new to the vault
/// and wrote a note there: the push of the sync after it used to make the
/// directory a folder in iCloud, a second one beside the folder the pull was
/// laying out, and the note in it a second copy.
#[test]
fn a_folder_a_pull_cut_short_was_laying_out_is_not_made_again_by_the_next_sync() {
    let mut start = cloned(3);
    start
        .zone
        .put(folder_record("F0000000-0000-4000-8000-0000000000A1", "Projects"));
    start.zone.put(note_record(
        "A0000000-0000-4000-8000-000000000000",
        "Added\nin a new folder",
        "F0000000-0000-4000-8000-0000000000A1",
    ));
    start.expect.kept = kept_notes(&start.zone);
    one_case(
        &start,
        "pull",
        crash::NOTE_FILE,
        1,
        Then::Sync,
        "pull killed in a new folder",
    );
}

/// A push killed right after iCloud moved a note to the folder its file
/// was moved to: the next push found the record changed since the last
/// pull and refused the move, leaving the note tracked at a file that is
/// gone until a pull and another push.
#[test]
fn a_move_a_push_cut_short_made_is_not_refused_by_the_next_push() {
    let start = cloned(3);
    let moving = tracked_file(&start.place, "E0000000-0000-4000-8000-000000000001");
    let moved = format!("Work/{}", Path::new(&moving).file_name().unwrap().to_string_lossy());
    std::fs::rename(start.place.vault.join(&moving), start.place.vault.join(&moved)).unwrap();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let crash = Crash {
        at: Some((crash::ICLOUD_MODIFY.into(), 1)),
        ..Default::default()
    };
    assert_eq!(run(&fake, &place, "push", &crash).status.signal(), Some(6));
    let again = run(&fake, &place, "push", &Crash::default());
    succeeded(&again, "push");
    let result: Value = serde_json::from_slice(&again.stdout).unwrap();
    let entries = &result["data"]["entries"];
    assert_eq!(entries.as_array().map(Vec::len), Some(1), "{}", says(&again));
    assert_eq!(entries[0]["resolution"], "ready", "{}", says(&again));
    assert_eq!(tracked_file(&place, "E0000000-0000-4000-8000-000000000001"), moved);
}

/// The photo note's attachment: its file name and what it holds.
fn photo(zone: &Zone) -> (String, Vec<u8>) {
    ("_7130093.jpeg".into(), zone.assets["/B/image-asset"].clone())
}

/// A clone into a directory whose `attachments/` already holds a file of
/// an attachment's name, which is not that attachment: the file stays as it
/// is, and the attachment is downloaded beside it.
#[test]
fn a_clone_never_replaces_a_file_in_attachments_that_is_not_the_attachment() {
    let zone = account(0);
    let (name, _) = photo(&zone);
    let fake = Fake::start(zone);
    let place = Place::new();
    let mine = place.vault.join("Notes/attachments").join(&name);
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, b"my own picture").unwrap();
    succeeded(&run(&fake, &place, "clone", &Crash::default()), "clone");
    assert_eq!(std::fs::read(&mine).unwrap(), b"my own picture");
    let state = read_clone_state(&place.vault).unwrap().unwrap();
    let entry = &state.attachments.unwrap()["7ED80274-4400-4C02-87EA-F542F056FF02"];
    assert_eq!(entry.file, "Notes/attachments/_7130093 2.jpeg");
    let (_, bytes) = photo(&fake.zone());
    assert_eq!(std::fs::read(place.vault.join(&entry.file)).unwrap(), bytes);
}

/// The same with the file byte for byte the attachment (what a clone
/// killed after the download left): it is taken for the attachment, under
/// its name, and nothing is downloaded beside it.
#[test]
fn a_clone_takes_a_file_in_attachments_that_is_the_attachment_for_it() {
    let zone = account(0);
    let (name, bytes) = photo(&zone);
    let fake = Fake::start(zone);
    let place = Place::new();
    let left = place.vault.join("Notes/attachments").join(&name);
    std::fs::create_dir_all(left.parent().unwrap()).unwrap();
    std::fs::write(&left, &bytes).unwrap();
    succeeded(&run(&fake, &place, "clone", &Crash::default()), "clone");
    let state = read_clone_state(&place.vault).unwrap().unwrap();
    let entry = &state.attachments.unwrap()["7ED80274-4400-4C02-87EA-F542F056FF02"];
    assert_eq!(entry.file, format!("Notes/attachments/{name}"));
    assert!(!place.vault.join("Notes/attachments/_7130093 2.jpeg").exists());
}

// --- shared zones -------------------------------------------------------------------

const WRITABLE_OWNER: &str = "_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const READ_ONLY_OWNER: &str = "_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A sharer's zone: a shared folder (`permission` its share's), with two
/// notes in it.
fn shared_zone(owner: &str, title: &str, permission: &str, notes: &str) -> Zone {
    let mut zone = Zone {
        page_size: 10,
        ..Default::default()
    };
    let folder = format!("SF{}", &owner[1..9]);
    let share = format!("SH{}", &owner[1..9]);
    let mut record = folder_record(&folder, title);
    record["share"] = json!({ "recordName": share });
    zone.put(record);
    zone.put(json!({
        "recordName": share, "recordType": "cloudkit.share", "fields": {},
        "currentUserParticipant": { "permission": permission },
        "participants": [{ "type": "OWNER", "userIdentity": { "nameComponents": { "givenName": title, "familyName": "Owner" } } }],
    }));
    for i in 0..2 {
        zone.put(note_record(
            &format!("{notes}0000000-0000-4000-8000-{i:012}"),
            &format!("{title} note {i}\nshared line {i}"),
            &folder,
        ));
    }
    zone
}

/// The account, with a folder shared with it to write in and one to read.
fn shared_account() -> Zone {
    let mut zone = account(3);
    zone.shared.insert(
        WRITABLE_OWNER.into(),
        shared_zone(WRITABLE_OWNER, "Plans", "READ_WRITE", "C"),
    );
    zone.shared.insert(
        READ_ONLY_OWNER.into(),
        shared_zone(READ_ONLY_OWNER, "Recipes", "READ_ONLY", "D"),
    );
    zone
}

fn writable_file(start: &Start, i: usize) -> String {
    tracked_file(&start.place, &format!("C0000000-0000-4000-8000-{i:012}"))
}

/// In the folder shared to write in: a note edited here, a new note made
/// here, a note edited by its owner and a note the owner added. The
/// read-only share: a note edited here, and the share revoked (`revoke`).
fn shares_changed(revoke_between: bool) -> Start {
    let mut start = cloned_from(shared_account());
    let vault = start.place.vault.clone();
    let edited = writable_file(&start, 0);
    let text = std::fs::read_to_string(vault.join(&edited)).unwrap();
    std::fs::write(
        vault.join(&edited),
        text.replacen("shared line 0", "shared line 0, typed here", 1),
    )
    .unwrap();
    start.expect.edits.push((edited.clone(), "typed here".into()));
    let new = format!(
        "{}/Shared new.md",
        Path::new(&edited).parent().unwrap().to_string_lossy()
    );
    std::fs::write(vault.join(&new), "Shared new\n\nmade here in a share\n").unwrap();
    start.expect.new_notes = 1;
    start.expect.edits.push((new, "made here in a share".into()));
    let read_only = tracked_file(&start.place, "D0000000-0000-4000-8000-000000000000");
    let text = std::fs::read_to_string(vault.join(&read_only)).unwrap();
    std::fs::write(vault.join(&read_only), text + "\nmy own line\n").unwrap();

    let writable = start.zone.shared.get_mut(WRITABLE_OWNER).unwrap();
    let mut record = writable.live("C0000000-0000-4000-8000-000000000001").unwrap().clone();
    record["fields"]["TextDataEncrypted"]["value"] = json!(note_text_field("Plans note 1\nedited by the owner"));
    writable.put(record);
    let folder = format!("SF{}", &WRITABLE_OWNER[1..9]);
    writable.put(note_record(
        "C0000000-0000-4000-8000-000000000009",
        "Owner's new\nfrom the owner",
        &folder,
    ));
    fn revoke(zone: &mut Zone) {
        zone.revoked.insert(READ_ONLY_OWNER.into());
    }
    if revoke_between {
        start.expect.between = Some(revoke);
    } else {
        revoke(&mut start.zone);
    }
    let mut after = start.zone.clone();
    revoke(&mut after);
    start.expect.kept = kept_notes(&after);
    start
}

#[test]
fn a_sync_with_shares_cut_short_anywhere_is_finished_by_the_next_run() {
    crash_everywhere(&shares_changed(false), "sync", &[Then::Again]);
}

/// The read-only share revoked while the run was cut short: the sync after
/// it finds the share gone.
#[test]
fn a_sync_cut_short_before_a_share_was_revoked_is_finished_by_the_next_run() {
    crash_everywhere(&shares_changed(true), "sync", &[Then::Again]);
}

// --- a vault whose file names are the titles ----------------------------------------

/// Retitled here (a file renamed), retitled in iCloud, and both at once
/// with an edit on the other side.
fn retitled_both_sides() -> Start {
    let mut start = cloned_by(account(8), "clone-filename");
    let vault = start.place.vault.clone();
    let rename = |from: &str, to: &str| std::fs::rename(vault.join(from), vault.join(to)).unwrap();
    rename("Notes/Extra 1.md", "Notes/Extra one.md");
    rename("Notes/Extra 7.md", "Notes/Extra seven.md");
    let text = std::fs::read_to_string(vault.join("Notes/Extra 5.md")).unwrap();
    std::fs::write(
        vault.join("Notes/Extra 5.md"),
        text.replacen("line of note 5", "line of note 5, typed here", 1),
    )
    .unwrap();
    let zone = &mut start.zone;
    for (name, text) in [
        ("E0000000-0000-4000-8000-000000000004", "Extra four\nline of note 4"),
        ("E0000000-0000-4000-8000-000000000005", "Extra five\nline of note 5"),
        (
            "E0000000-0000-4000-8000-000000000007",
            "Extra 7\nline of note 7\nremote line",
        ),
    ] {
        let mut record = zone.live(name).unwrap().clone();
        record["fields"]["TextDataEncrypted"]["value"] = json!(note_text_field(text));
        zone.put(record);
    }
    let expect = &mut start.expect;
    for (file, title) in [
        ("Notes/Extra one.md", "Extra one"),
        ("Notes/Extra four.md", "Extra four"),
        ("Notes/Extra five.md", "Extra five"),
        ("Notes/Extra seven.md", "Extra seven"),
    ] {
        expect.titles.push((file.into(), title.into()));
    }
    expect.edits.push(("Notes/Extra five.md".into(), "typed here".into()));
    expect.edits.push(("Notes/Extra seven.md".into(), "remote line".into()));
    start
}

#[test]
fn a_sync_of_retitled_notes_cut_short_anywhere_is_finished_by_the_next_run() {
    crash_everywhere(&retitled_both_sides(), "sync", &[Then::Again]);
}

/// In a filename-title vault, a note retitled in iCloud and edited here:
/// the push merges the two and sends the edit, and the file used to keep
/// its old name for good (the pull after it had nothing to apply). It takes
/// the new title as its name, as a pull renames the file of a retitled note.
#[test]
fn a_note_retitled_in_icloud_and_edited_here_takes_the_new_title_as_its_file_name() {
    let start = retitled_both_sides();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    succeeded(&run(&fake, &place, "push", &Crash::default()), "push");
    assert_eq!(
        tracked_file(&place, "E0000000-0000-4000-8000-000000000005"),
        "Notes/Extra five.md"
    );
    let text = std::fs::read_to_string(place.vault.join("Notes/Extra five.md")).unwrap();
    assert!(text.contains("typed here"), "{text}");
    assert!(!place.vault.join("Notes/Extra 5.md").exists());
    let record = fake
        .zone()
        .live("E0000000-0000-4000-8000-000000000005")
        .unwrap()
        .clone();
    assert!(body_of(&record).starts_with("Extra five\n"), "{}", body_of(&record));
}

// --- layout migrations ---------------------------------------------------------------

/// The cloned vault turned back into an older layout's (`version` 2 or 3:
/// the state in `.icloud-md/`; 2: no ids in the note files, no title mode),
/// with a note edited since.
fn older_layout(version: u32) -> Start {
    let mut start = cloned(4);
    let vault = start.place.vault.clone();
    let edited = tracked_file(&start.place, "E0000000-0000-4000-8000-000000000001");
    let text = std::fs::read_to_string(vault.join(&edited)).unwrap();
    std::fs::write(
        vault.join(&edited),
        text.replacen("line of note 1", "line of note 1, typed here", 1),
    )
    .unwrap();
    start.expect.edits.push((edited, "typed here".into()));
    std::fs::rename(vault.join(".icloud-notes"), vault.join(".icloud-md")).unwrap();
    let path = vault.join(".icloud-md/state.json");
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    state["layoutVersion"] = json!(version);
    if version == 2 {
        state.as_object_mut().unwrap().remove("titleMode");
        let mut files = Vec::new();
        markdown_files(&vault, "", &mut files);
        for file in files {
            let text = std::fs::read_to_string(vault.join(&file)).unwrap();
            let body = text
                .splitn(3, "---\n")
                .nth(2)
                .unwrap()
                .strip_prefix('\n')
                .unwrap()
                .to_owned();
            std::fs::write(vault.join(&file), body).unwrap();
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap() + "\n").unwrap();
    start
}

/// What a migration leaves besides the state: a tombstone where the state
/// was, and one complete backup of it.
fn check_migrated(place: &Place, label: &str) {
    let vault = &place.vault;
    let tombstone: Value =
        serde_json::from_str(&std::fs::read_to_string(vault.join(".icloud-md/state.json")).unwrap()).unwrap();
    assert_eq!(
        tombstone,
        json!({ "layoutVersion": 4, "movedTo": ".icloud-notes" }),
        "{label}"
    );
    let backups: Vec<String> = std::fs::read_dir(vault)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".icloud-md.bak-"))
        .collect();
    assert!(!backups.is_empty(), "{label}: no backup");
    for backup in &backups {
        assert!(!backup.ends_with(".partial"), "{label}: {backups:?}");
        let state: Value =
            serde_json::from_str(&std::fs::read_to_string(vault.join(backup).join("state.json")).unwrap()).unwrap();
        assert!(state["notes"].is_object(), "{label}: {backup}");
    }
}

fn migration_cut_short_anywhere(version: u32) {
    let start = older_layout(version);
    let fake = Fake::start(start.zone.clone());
    let traced = Place::copy_of(&start.place);
    let sites = crash_sites(&fake, &traced, "push");
    let migrating: Vec<(String, u64)> = sites
        .into_iter()
        .take_while(|(point, _)| point != crash::ICLOUD_MODIFY)
        .collect();
    let mut failures = Vec::new();
    for (point, n) in &migrating {
        for then in [Then::Again, Then::Sync] {
            let label = format!("push of a layout {version} vault killed at {point}:{n}, then {then:?}");
            let result = std::panic::catch_unwind(|| {
                let place = one_case_place(&start, "push", point, *n, then, &label);
                check_migrated(&place, &label);
            });
            if let Err(panic) = result {
                failures.push(panic.downcast_ref::<String>().cloned().unwrap_or_else(|| label.clone()));
            }
        }
    }
    assert!(!migrating.is_empty());
    assert!(
        failures.is_empty(),
        "{} failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn a_migration_from_layout_2_cut_short_anywhere_is_finished_by_the_next_run() {
    migration_cut_short_anywhere(2);
}

#[test]
fn a_migration_from_layout_3_cut_short_anywhere_is_finished_by_the_next_run() {
    migration_cut_short_anywhere(3);
}
