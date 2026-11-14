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

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    /// Private `records/lookup` requests answered so far, and the first one
    /// to answer 503 from then on (0: none).
    lookups: usize,
    fail_lookups_from: usize,
    /// Runs against it download attachments one at a time, in order
    /// (`crash::DOWNLOADS_IN_LINE_ENV`), so a run is cut short at the same
    /// place every time.
    downloads_in_line: bool,
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
                            "records/lookup" => {
                                zone.lookups += 1;
                                if zone.fail_lookups_from != 0 && zone.lookups >= zone.fail_lookups_from {
                                    return Err(503);
                                }
                                reply(zone.lookup(&json_body()))
                            }
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
        .env_remove(crash::DOWNLOADS_IN_LINE_ENV)
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
    if fake.zone().downloads_in_line {
        cmd.env(crash::DOWNLOADS_IN_LINE_ENV, "1");
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
    /// What the user does in the vault then.
    between_here: Option<fn(&Path)>,
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

    // Nothing a killed run left half-written stays (the runs are over).
    let leftovers: Vec<String> = files_under(vault)
        .into_keys()
        .filter(|rel| {
            let name = rel.rsplit('/').next().unwrap_or_default();
            name.starts_with('.') && name.ends_with(".tmp")
        })
        .collect();
    assert!(leftovers.is_empty(), "{label}: left half-written: {leftovers:?}");

    // No base copy (or sent copy) is left of a note nothing tracks.
    for name in std::fs::read_dir(vault.join(".icloud-notes/base"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = name.file_name().to_string_lossy().into_owned();
        let id = name
            .trim_start_matches('.')
            .trim_end_matches(".sent")
            .trim_end_matches(".md");
        assert!(
            state.notes.contains_key(id),
            "{label}: base/{name} is of no tracked note"
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
    /// A pull by itself, then a sync: `icloud-notes pull` run before the
    /// app's next sync, so a pull meets what a push cut short left.
    Pull,
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
    if let Some(between) = start.expect.between_here {
        between(&traced.vault);
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
    if let Some(between) = start.expect.between_here {
        between(&place.vault);
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
        Then::Sync | Then::Pull if !cloned => "clone",
        Then::Sync => "sync",
        Then::Pull => "pull",
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
    crash_everywhere(&start, "push", &[Then::Again, Then::Sync, Then::Pull]);
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
    crash_everywhere(&folders_changed_here(), "push", &[Then::Again, Then::Sync, Then::Pull]);
}

/// A folder directory deleted here and another renamed, then a pull before
/// any push: neither directory comes back, so the push that follows
/// deletes the one folder and renames the other. The pull used to make
/// both again, empty: the deleted folder stayed in iCloud, and the renamed
/// one became a new folder beside the old.
#[test]
fn a_folder_deleted_and_one_renamed_here_are_pushed_so_after_a_pull() {
    let start = folders_changed_here();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let pull = run(&fake, &place, "pull", &Crash::default());
    succeeded(&pull, "pull");
    assert!(!place.vault.join("Archive").exists(), "{}", says(&pull));
    assert!(!place.vault.join("Work").exists(), "{}", says(&pull));
    succeeded(&run(&fake, &place, "sync", &Crash::default()), "sync");
    check(&fake, &place, &start.expect, "a pull before the push");
}

/// The same cut short anywhere, and the deleted folder renamed on the
/// phone before the next run.
#[test]
fn a_push_of_folder_changes_cut_short_before_the_folder_was_renamed_in_icloud_is_finished_by_the_next_run() {
    fn rename_on_the_phone(zone: &mut Zone) {
        // Unless the push got to delete it.
        if let Some(folder) = zone.live(ARCHIVE_FOLDER) {
            let mut folder = folder.clone();
            folder["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(b"Old"));
            zone.put(folder);
        }
    }
    let mut start = folders_changed_here();
    start.expect.between = Some(rename_on_the_phone);
    crash_everywhere(&start, "push", &[Then::Again, Then::Sync, Then::Pull]);
}

/// The same with the deleted folder renamed on the phone before the pull:
/// its directory stays gone under the new name too. The pull's move of its
/// notes (whose files are gone) to the new name used to make the directory
/// before finding no file to move, and the push then kept the folder.
#[test]
fn a_folder_deleted_here_and_renamed_in_icloud_is_pushed_so_after_a_pull() {
    let start = folders_changed_here();
    let fake = Fake::start(start.zone.clone());
    {
        let mut zone = fake.zone();
        let mut folder = zone.live(ARCHIVE_FOLDER).unwrap().clone();
        folder["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(b"Old"));
        zone.put(folder);
    }
    let place = Place::copy_of(&start.place);
    let pull = run(&fake, &place, "pull", &Crash::default());
    succeeded(&pull, "pull");
    assert!(!place.vault.join("Old").exists(), "{}", says(&pull));
    succeeded(&run(&fake, &place, "sync", &Crash::default()), "sync");
    check(
        &fake,
        &place,
        &start.expect,
        "a pull after the folder was renamed in iCloud",
    );
}

/// How the photo note's file left `Work` here before its photo was replaced
/// on the phone.
#[derive(Debug, Clone, Copy)]
enum PhotoNoteMoved {
    /// `Work` renamed to `Office`, `attachments/` and all.
    FolderRenamed,
    /// Only the note's file moved to a new `Office`; its photo stayed.
    FileMoved,
    /// The same, with another file of the photo's name in `Office/attachments`.
    FileMovedNameTaken,
}

/// The photo note in `Work`, moved out of it here as `how` says, its photo
/// replaced on the phone, then a pull and a sync: the new photo is
/// downloaded beside the moved file (the old copy moved along, or a free
/// name taken), `Work` is not made again, and the push renames the folder
/// or moves the note, as was done here.
fn photo_of_a_note_moved_here_replaced_in_icloud(how: PhotoNoteMoved) {
    const PHOTO_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
    let mut zone = account(3);
    let mut photo_note = zone.live(PHOTO_NOTE).unwrap().clone();
    let reference = json!({ "recordName": WORK_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
    photo_note["fields"]["Folder"]["value"] = reference.clone();
    photo_note["fields"]["Folders"]["value"] = json!([reference]);
    zone.put(photo_note);
    let mut start = cloned_from(zone);
    let file = tracked_file(&start.place, PHOTO_NOTE);
    let name = Path::new(&file).file_name().unwrap().to_string_lossy().into_owned();
    assert!(file.starts_with("Work/"), "{file}");
    let (photo_name, _) = photo(&start.zone);
    let vault = start.place.vault.clone();
    match how {
        PhotoNoteMoved::FolderRenamed => {
            std::fs::rename(vault.join("Work"), vault.join("Office")).unwrap();
        }
        PhotoNoteMoved::FileMoved | PhotoNoteMoved::FileMovedNameTaken => {
            std::fs::create_dir_all(vault.join("Office/attachments")).unwrap();
            std::fs::rename(vault.join(&file), vault.join("Office").join(&name)).unwrap();
            start.expect.new_folders = 1;
        }
    }
    if let PhotoNoteMoved::FileMovedNameTaken = how {
        std::fs::write(vault.join("Office/attachments").join(&photo_name), "not the photo").unwrap();
    }
    let fake = Fake::start(start.zone.clone());
    {
        let mut zone = fake.zone();
        zone.assets.insert("/B/other-image".into(), b"another photo".to_vec());
        let media_name = "066C8A2E-796F-403F-AD75-A5267CBD0E18";
        let mut media = zone.live(media_name).unwrap().clone();
        media["fields"]["Asset"]["value"] = json!({ "fileChecksum": "ck-other", "size": 13,
            "downloadURL": "https://cvws.icloud-content.com/B/other-image" });
        zone.put(media);
        let photo_note = zone.live(PHOTO_NOTE).unwrap().clone();
        zone.put(photo_note);
    }
    let place = Place::copy_of(&start.place);
    let label = format!("{how:?}");
    let pull = run(&fake, &place, "pull", &Crash::default());
    succeeded(&pull, "pull");
    let state = read_clone_state(&place.vault).unwrap().unwrap();
    let attachments = state.attachments.clone().unwrap_or_default();
    let entry = attachments
        .values()
        .find(|a| a.note_record_name == PHOTO_NOTE)
        .unwrap_or_else(|| panic!("{label}: the photo is not tracked"));
    assert!(entry.file.starts_with("Office/attachments/"), "{label}: {}", entry.file);
    assert_eq!(
        std::fs::read(place.vault.join(&entry.file)).unwrap(),
        b"another photo",
        "{label}"
    );
    match how {
        PhotoNoteMoved::FolderRenamed => assert!(!place.vault.join("Work").exists(), "{label}: {}", says(&pull)),
        _ => assert!(
            !place.vault.join("Work/attachments").join(&photo_name).exists(),
            "{label}: the old photo was left in Work"
        ),
    }
    if let PhotoNoteMoved::FileMovedNameTaken = how {
        assert_ne!(entry.file, format!("Office/attachments/{photo_name}"), "{label}");
        assert_eq!(
            std::fs::read_to_string(place.vault.join("Office/attachments").join(&photo_name)).unwrap(),
            "not the photo",
            "{label}"
        );
    }
    let text = std::fs::read_to_string(place.vault.join("Office").join(&name)).unwrap();
    assert!(!text.contains("<<<<<<<"), "{label}: {text}");
    let link = format!(
        "](attachments/{}",
        Path::new(&entry.file).file_name().unwrap().to_string_lossy()
    );
    assert!(
        text.contains(&link) || text.contains(&link.replace(' ', "%20")),
        "{label}: {text}"
    );
    succeeded(&run(&fake, &place, "sync", &Crash::default()), "sync");
    check(&fake, &place, &start.expect, &label);
    assert_eq!(tracked_file(&place, PHOTO_NOTE), format!("Office/{name}"), "{label}");
    if let PhotoNoteMoved::FolderRenamed = how {
        let zone = fake.zone();
        let folder = zone.live(WORK_FOLDER).unwrap();
        let title = folder["fields"]["TitleEncrypted"]["value"].as_str().unwrap();
        assert_eq!(base64_decode(title), b"Office", "{label}: the folder was not renamed");
    }
}

#[test]
fn a_photo_replaced_in_icloud_of_a_note_whose_folder_was_renamed_here_goes_beside_it() {
    photo_of_a_note_moved_here_replaced_in_icloud(PhotoNoteMoved::FolderRenamed);
}

#[test]
fn a_photo_replaced_in_icloud_of_a_note_moved_here_goes_beside_it() {
    photo_of_a_note_moved_here_replaced_in_icloud(PhotoNoteMoved::FileMoved);
}

#[test]
fn a_photo_replaced_in_icloud_of_a_note_moved_here_takes_a_free_name_beside_it() {
    photo_of_a_note_moved_here_replaced_in_icloud(PhotoNoteMoved::FileMovedNameTaken);
}

/// The same three, with the pull cut short anywhere (moving the photo
/// beside the moved file, rewriting its link, downloading the new one).
#[test]
fn a_pull_moving_the_photo_of_a_note_moved_here_cut_short_anywhere_is_finished_by_the_next_run() {
    const PHOTO_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
    for how in [
        PhotoNoteMoved::FolderRenamed,
        PhotoNoteMoved::FileMoved,
        PhotoNoteMoved::FileMovedNameTaken,
    ] {
        let mut zone = account(3);
        let mut photo_note = zone.live(PHOTO_NOTE).unwrap().clone();
        let reference = json!({ "recordName": WORK_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
        photo_note["fields"]["Folder"]["value"] = reference.clone();
        photo_note["fields"]["Folders"]["value"] = json!([reference]);
        zone.put(photo_note);
        let mut start = cloned_from(zone);
        let file = tracked_file(&start.place, PHOTO_NOTE);
        let name = Path::new(&file).file_name().unwrap().to_string_lossy().into_owned();
        let (photo_name, _) = photo(&start.zone);
        let vault = start.place.vault.clone();
        match how {
            PhotoNoteMoved::FolderRenamed => std::fs::rename(vault.join("Work"), vault.join("Office")).unwrap(),
            PhotoNoteMoved::FileMoved | PhotoNoteMoved::FileMovedNameTaken => {
                std::fs::create_dir_all(vault.join("Office/attachments")).unwrap();
                std::fs::rename(vault.join(&file), vault.join("Office").join(&name)).unwrap();
                start.expect.new_folders = 1;
            }
        }
        if let PhotoNoteMoved::FileMovedNameTaken = how {
            std::fs::write(vault.join("Office/attachments").join(&photo_name), "not the photo").unwrap();
        }
        let zone = &mut start.zone;
        zone.downloads_in_line = true;
        zone.assets.insert("/B/other-image".into(), b"another photo".to_vec());
        let media_name = "066C8A2E-796F-403F-AD75-A5267CBD0E18";
        let mut media = zone.live(media_name).unwrap().clone();
        media["fields"]["Asset"]["value"] = json!({ "fileChecksum": "ck-other", "size": 13,
            "downloadURL": "https://cvws.icloud-content.com/B/other-image" });
        zone.put(media);
        let photo_note = zone.live(PHOTO_NOTE).unwrap().clone();
        zone.put(photo_note);
        crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
        if let PhotoNoteMoved::FileMovedNameTaken = how {
            // The file that was in the way is still there, as it was.
            let fake = Fake::start(start.zone.clone());
            for (point, n) in distinct_sites(crash_sites(&fake, &Place::copy_of(&start.place), "pull")) {
                let label = format!("{how:?}: pull killed at {point}:{n}");
                let place = one_case_place(&start, "pull", &point, n, Then::Again, &label);
                assert_eq!(
                    std::fs::read_to_string(place.vault.join("Office/attachments").join(&photo_name)).unwrap(),
                    "not the photo",
                    "{label}"
                );
            }
        }
    }
}

/// The photo note moved to `Work` on the phone, where a file of the photo's
/// name is in `attachments/` here: a pull cut short anywhere moving it (the
/// photo to a free name, its link following) leaves that file as it was
/// and nothing to push.
#[test]
fn a_pull_moving_a_photo_to_a_free_name_cut_short_anywhere_is_finished_by_the_next_run() {
    const PHOTO_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
    let mut start = cloned(3);
    let (photo_name, _) = photo(&start.zone);
    let in_the_way = start.place.vault.join("Work/attachments").join(&photo_name);
    std::fs::create_dir_all(in_the_way.parent().unwrap()).unwrap();
    std::fs::write(&in_the_way, "not the photo").unwrap();
    let zone = &mut start.zone;
    let mut photo_note = zone.live(PHOTO_NOTE).unwrap().clone();
    let reference = json!({ "recordName": WORK_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
    photo_note["fields"]["Folder"]["value"] = reference.clone();
    photo_note["fields"]["Folders"]["value"] = json!([reference]);
    zone.put(photo_note);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
    let fake = Fake::start(start.zone.clone());
    for (point, n) in distinct_sites(crash_sites(&fake, &Place::copy_of(&start.place), "pull")) {
        let label = format!("pull killed at {point}:{n}");
        let place = one_case_place(&start, "pull", &point, n, Then::Again, &label);
        assert_eq!(
            std::fs::read_to_string(place.vault.join("Work/attachments").join(&photo_name)).unwrap(),
            "not the photo",
            "{label}"
        );
    }
}

/// A folder directory deleted here with a note whose photo was replaced on
/// the phone before the pull: the new photo is not downloaded into the
/// deleted directory, which would bring it back for the push to keep the
/// folder.
#[test]
fn a_folder_deleted_here_with_a_note_whose_attachment_changed_in_icloud_is_pushed_so_after_a_pull() {
    const PHOTO_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
    let mut zone = account(3);
    zone.put(folder_record(ARCHIVE_FOLDER, "Archive"));
    let mut photo = zone.live(PHOTO_NOTE).unwrap().clone();
    let reference = json!({ "recordName": ARCHIVE_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
    photo["fields"]["Folder"]["value"] = reference.clone();
    photo["fields"]["Folders"]["value"] = json!([reference]);
    zone.put(photo);
    let mut start = cloned_from(zone);
    assert!(tracked_file(&start.place, PHOTO_NOTE).starts_with("Archive/"));
    std::fs::remove_dir_all(start.place.vault.join("Archive")).unwrap();
    start.expect.deleted = vec![PHOTO_NOTE.into()];
    start.expect.deleted_folders = vec![ARCHIVE_FOLDER.into()];
    let fake = Fake::start(start.zone.clone());
    {
        let mut zone = fake.zone();
        zone.assets.insert("/B/other-image".into(), b"another photo".to_vec());
        let media_name = "066C8A2E-796F-403F-AD75-A5267CBD0E18";
        let mut media = zone.live(media_name).unwrap().clone();
        media["fields"]["Asset"]["value"] = json!({ "fileChecksum": "ck-other", "size": 13,
            "downloadURL": "https://cvws.icloud-content.com/B/other-image" });
        zone.put(media);
        let photo = zone.live(PHOTO_NOTE).unwrap().clone();
        zone.put(photo);
    }
    let place = Place::copy_of(&start.place);
    let pull = run(&fake, &place, "pull", &Crash::default());
    succeeded(&pull, "pull");
    assert!(!place.vault.join("Archive").exists(), "{}", says(&pull));
    succeeded(&run(&fake, &place, "sync", &Crash::default()), "sync");
    check(
        &fake,
        &place,
        &start.expect,
        "a pull after the photo was replaced in iCloud",
    );
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
    crash_everywhere(&start, "sync", &[Then::Again, Then::Pull]);
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

/// The same with an edit made along with the move, cut short after iCloud
/// took the edit too: the next push alone settles it, the note not read as
/// changed elsewhere.
#[test]
fn a_move_with_an_edit_a_push_cut_short_made_is_not_refused_by_the_next_push() {
    let start = cloned(3);
    let moving = tracked_file(&start.place, "E0000000-0000-4000-8000-000000000001");
    let moved = format!("Work/{}", Path::new(&moving).file_name().unwrap().to_string_lossy());
    let text = std::fs::read_to_string(start.place.vault.join(&moving)).unwrap();
    std::fs::remove_file(start.place.vault.join(&moving)).unwrap();
    std::fs::write(start.place.vault.join(&moved), text + "\nedited with the move\n").unwrap();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let crash = Crash {
        at: Some((crash::ICLOUD_MODIFY.into(), 2)),
        ..Default::default()
    };
    assert_eq!(run(&fake, &place, "push", &crash).status.signal(), Some(6));
    assert!(
        body_of(fake.zone().live("E0000000-0000-4000-8000-000000000001").unwrap()).contains("edited with the move")
    );
    let again = run(&fake, &place, "push", &Crash::default());
    succeeded(&again, "push");
    let result: Value = serde_json::from_slice(&again.stdout).unwrap();
    let entries = &result["data"]["entries"];
    assert_eq!(entries.as_array().map(Vec::len), Some(1), "{}", says(&again));
    assert_eq!(entries[0]["resolution"], "ready", "{}", says(&again));
    assert_eq!(tracked_file(&place, "E0000000-0000-4000-8000-000000000001"), moved);
    let on_disk = std::fs::read_to_string(place.vault.join(&moved)).unwrap();
    assert_eq!(on_disk.matches("edited with the move").count(), 1, "{on_disk}");
}

/// A push killed right after iCloud took an edit that adds an attachment,
/// then a pull by itself: the attachment is taken at the file the note was
/// made with. The pull used to download it beside that file
/// (`extra 2.txt`) and merge the second link into the note as a conflict.
#[test]
fn an_attachment_a_push_cut_short_added_is_taken_at_its_file_by_a_pull() {
    let start = cloned(3);
    let note = tracked_file(&start.place, "E0000000-0000-4000-8000-000000000001");
    let dir = Path::new(&note).parent().unwrap().to_owned();
    let vault = &start.place.vault;
    std::fs::create_dir_all(vault.join(&dir).join("attachments")).unwrap();
    std::fs::write(vault.join(&dir).join("attachments/extra.txt"), "an extra file\n").unwrap();
    let text = std::fs::read_to_string(vault.join(&note)).unwrap();
    std::fs::write(vault.join(&note), text + "\n[extra.txt](attachments/extra.txt)\n").unwrap();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let crash = Crash {
        at: Some((crash::ICLOUD_MODIFY.into(), 1)),
        ..Default::default()
    };
    assert_eq!(run(&fake, &place, "push", &crash).status.signal(), Some(6));
    let pull = run(&fake, &place, "pull", &Crash::default());
    succeeded(&pull, "pull");
    let on_disk = std::fs::read_to_string(place.vault.join(&note)).unwrap();
    assert!(!on_disk.contains("<<<<<<<"), "{on_disk}");
    assert_eq!(on_disk.matches("](attachments/").count(), 1, "{on_disk}");
    let names: Vec<String> = std::fs::read_dir(place.vault.join(&dir).join("attachments"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("extra"))
        .collect();
    assert_eq!(names, ["extra.txt"], "{}", says(&pull));
    let mut expect = start.expect.clone();
    expect.new_attachments = 1;
    succeeded(&run(&fake, &place, "sync", &Crash::default()), "sync");
    check(&fake, &place, &expect, "pull after the attachment's push was cut short");
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

/// The account with the recording's file given the photo's name: two
/// attachments of one name in one folder, the second downloaded as
/// `_7130093 2.jpeg`.
fn same_attachment_names() -> Zone {
    let mut zone = account(3);
    let media_name = "0B8509A3-A5FC-470B-A777-03BFFFDFB5F9";
    let mut media = zone.live(media_name).unwrap().clone();
    media["fields"]["FilenameEncrypted"]["value"] = json!(base64_encode(b"_7130093.jpeg"));
    zone.put(media);
    zone
}

/// A clone killed after it downloaded both: the clone after it took the
/// first file for its attachment and downloaded the second again under a
/// third name, so the note file left beside it no longer read as the one it
/// wrote, and the next push created that file as a second note.
#[test]
fn a_clone_cut_short_with_two_attachments_of_one_name_is_finished_by_the_next_run() {
    let zone = same_attachment_names();
    let start = Start {
        expect: Expect {
            kept: kept_notes(&zone),
            ..Default::default()
        },
        zone,
        place: Place::new(),
    };
    crash_everywhere(&start, "clone", &[Then::Again, Then::Sync]);
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
    crash_everywhere(&shares_changed(false), "sync", &[Then::Again, Then::Pull]);
}

/// The read-only share revoked while the run was cut short: the sync after
/// it finds the share gone.
#[test]
fn a_sync_cut_short_before_a_share_was_revoked_is_finished_by_the_next_run() {
    crash_everywhere(&shares_changed(true), "sync", &[Then::Again, Then::Pull]);
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
    crash_everywhere(&retitled_both_sides(), "sync", &[Then::Again, Then::Pull]);
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
    // History to move: three notes changed in iCloud and pulled (a
    // snapshot and an epoch each), and the app's conflict backups.
    for i in [0, 2, 3] {
        let name = format!("E0000000-0000-4000-8000-{i:012}");
        let mut record = start.zone.live(&name).unwrap().clone();
        record["fields"]["TextDataEncrypted"]["value"] =
            json!(note_text_field(&format!("Extra {i}\nline of note {i}\nfrom the phone")));
        start.zone.put(record);
    }
    let fake = Fake::start(start.zone.clone());
    succeeded(&run(&fake, &start.place, "pull", &Crash::default()), "pull");
    start.zone = fake.zone().clone();
    start.zone.applied.clear();
    for (name, text) in [("Extra 0.md", "a copy kept by the app"), ("Extra 2.md", "another")] {
        let path = vault.join(".icloud-notes/conflict-backups").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
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

/// Every file under `dir` (relative path, bytes).
fn files_under(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, rel: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(root.join(rel)).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(path, std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, "", &mut out);
    out
}

/// The history and conflict backups the old layout held: each once in
/// `.icloud-notes/`, none left in `.icloud-md/`, each in every backup.
fn check_moved(place: &Place, before: &BTreeMap<String, Vec<u8>>, label: &str) {
    let vault = &place.vault;
    let now = files_under(&vault.join(".icloud-notes"));
    let left = files_under(&vault.join(".icloud-md"));
    assert_eq!(
        left.keys().collect::<Vec<_>>(),
        ["state.json"],
        "{label}: left in .icloud-md/"
    );
    for (rel, bytes) in before {
        assert_eq!(now.get(rel), Some(bytes), "{label}: .icloud-notes/{rel}");
        let copies = now
            .iter()
            .filter(|(r, b)| *b == bytes && r.starts_with(rel.split('/').next().unwrap()))
            .count();
        assert_eq!(copies, 1, "{label}: {rel} is in .icloud-notes/ {copies} times");
        for backup in std::fs::read_dir(vault).unwrap().flatten() {
            let name = backup.file_name().to_string_lossy().into_owned();
            if name.starts_with(".icloud-md.bak-") {
                assert_eq!(
                    std::fs::read(backup.path().join(rel)).ok().as_ref(),
                    Some(bytes),
                    "{label}: {name}/{rel}"
                );
            }
        }
    }
}

fn migration_cut_short_anywhere(version: u32) {
    let start = older_layout(version);
    let before: BTreeMap<String, Vec<u8>> = files_under(&start.place.vault.join(".icloud-md"))
        .into_iter()
        .filter(|(rel, _)| rel.starts_with("history/") || rel.starts_with("conflict-backups/"))
        .collect();
    assert!(before.keys().any(|r| r.contains("/epochs/")) && before.keys().any(|r| r.starts_with("conflict-backups/")));
    let fake = Fake::start(start.zone.clone());
    let traced = Place::copy_of(&start.place);
    let sites = crash_sites(&fake, &traced, "push");
    let migrating: Vec<(String, u64)> = sites
        .into_iter()
        .take_while(|(point, _)| point != crash::ICLOUD_MODIFY)
        .collect();
    let mut failures = Vec::new();
    for (point, n) in &migrating {
        for then in [Then::Again, Then::Sync, Then::Pull] {
            let label = format!("push of a layout {version} vault killed at {point}:{n}, then {then:?}");
            let result = std::panic::catch_unwind(|| {
                let place = one_case_place(&start, "push", point, *n, then, &label);
                check_migrated(&place, &label);
                check_moved(&place, &before, &label);
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

/// A note renamed here and edited in iCloud: the push refuses the move
/// (changed since the last pull), the pull merges the edit into the renamed
/// file, and the sync then pushes the move, where it used to wait for the
/// next sync. A push by itself still refuses it.
#[test]
fn a_note_renamed_here_and_edited_in_icloud_is_moved_and_merged_in_one_sync() {
    let start = retitled_both_sides();
    let id = "E0000000-0000-4000-8000-000000000007";
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let pushed = run(&fake, &place, "push", &Crash::default());
    let result: Value = serde_json::from_slice(&pushed.stdout).unwrap();
    let refused = result["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["file"] == "Notes/Extra seven.md" && e["resolution"] == "conflict");
    assert!(refused, "{}", says(&pushed));

    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let synced = run(&fake, &place, "sync", &Crash::default());
    succeeded(&synced, "sync");
    assert_eq!(tracked_file(&place, id), "Notes/Extra seven.md", "{}", says(&synced));
    let record = fake.zone().live(id).unwrap().clone();
    assert!(body_of(&record).starts_with("Extra seven\n"), "{}", body_of(&record));
    assert!(body_of(&record).contains("remote line"));
    let text = std::fs::read_to_string(place.vault.join("Notes/Extra seven.md")).unwrap();
    assert!(text.contains("remote line"), "{text}");
}

/// The sync that pushes such a move again reports each entry once: a change
/// refused by both pushes (here, a file with conflict markers) used to be
/// listed twice in the push half.
#[test]
fn a_sync_that_pushes_twice_lists_each_refusal_once() {
    let start = retitled_both_sides();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let file = place.vault.join("Notes/Extra 2.md");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text + "<<<<<<< ours\nmine\n=======\ntheirs\n>>>>>>> theirs\n").unwrap();
    let synced = run(&fake, &place, "sync", &Crash::default());
    let result: Value = serde_json::from_slice(&synced.stdout).unwrap();
    let push = &result["data"]["push"]["result"];
    let entries = push["entries"].as_array().unwrap();
    let refused: Vec<&Value> = entries.iter().filter(|e| e["file"] == "Notes/Extra 2.md").collect();
    assert_eq!(refused.len(), 1, "{}", says(&synced));
    assert!(
        entries
            .iter()
            .any(|e| e["file"] == "Notes/Extra seven.md" && e["resolution"] == "ready"),
        "{}",
        says(&synced)
    );
}

/// Notes added on the phone, two of them with one title in one folder and
/// one of the same title in another: a pull cut short anywhere is finished
/// by the next run with each note once, in its own file.
#[test]
fn a_pull_of_notes_with_one_title_cut_short_anywhere_is_finished_by_the_next_run() {
    let mut start = cloned(3);
    for i in 0..3 {
        start.zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Twin\nwritten on the phone, {i}"),
            if i == 2 { WORK_FOLDER } else { DEFAULT_FOLDER },
        ));
    }
    start.expect.kept = kept_notes(&start.zone);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}

/// The folder the pull was renaming (`Work` to `Office`) renamed again on
/// the phone (to `Bureau`) while the pull was cut short: the directory the
/// pull had made for the first name is no new folder for the next push.
#[test]
fn a_pull_cut_short_before_a_folder_it_was_renaming_was_renamed_again_is_finished_by_the_next_run() {
    let mut start = cloned(6);
    moved_in_icloud(&mut start);
    fn rename_again(zone: &mut Zone) {
        let mut folder = zone.live(WORK_FOLDER).unwrap().clone();
        folder["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(b"Bureau"));
        zone.put(folder);
    }
    start.expect.between = Some(rename_again);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}

/// After the push was cut short, and before the next run, the user typed
/// more into the new note and into the edited one: those edits go up too.
#[test]
fn a_push_cut_short_before_more_was_typed_here_is_finished_by_the_next_run() {
    let mut start = cloned(9);
    changed_here(&mut start);
    let edited = tracked_file(&start.place, "E0000000-0000-4000-8000-000000000001");
    assert_eq!(edited, "Notes/Extra 1.md");
    fn type_more(vault: &Path) {
        for file in ["Notes/Fresh.md", "Notes/Extra 1.md"] {
            let text = std::fs::read_to_string(vault.join(file)).unwrap();
            std::fs::write(vault.join(file), text + "\ntyped after the push\n").unwrap();
        }
    }
    start.expect.between_here = Some(type_more);
    for file in ["Notes/Fresh.md", "Notes/Extra 1.md"] {
        start.expect.edits.push((file.into(), "typed after the push".into()));
    }
    crash_everywhere(&start, "push", &[Then::Again, Then::Sync, Then::Pull]);
}

/// The share a push was creating a note in revoked while the push was cut
/// short after iCloud made the note: every later push looked the note up in
/// a zone that is no longer shared and failed on the answer, for good.
#[test]
fn a_create_in_a_share_revoked_after_the_push_was_cut_short_does_not_stop_every_later_push() {
    let mut start = shares_changed(false);
    start.zone.revoked.clear();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let crash = Crash {
        at: Some((crash::ICLOUD_MODIFY.into(), 1)),
        ..Default::default()
    };
    // The first request is the shared create (the update of an own note
    // goes after it).
    let killed = run(&fake, &place, "push", &crash);
    assert_eq!(killed.status.signal(), Some(6), "{}", says(&killed));
    assert_eq!(fake.zone().creates("Note").len(), 1);
    fake.zone().revoked.insert(WRITABLE_OWNER.into());
    let _ = run(&fake, &place, "sync", &Crash::default());
    let again = run(&fake, &place, "sync", &Crash::default());
    succeeded(&again, "the sync after");
    let raw: Value =
        serde_json::from_str(&std::fs::read_to_string(place.vault.join(".icloud-notes/state.json")).unwrap()).unwrap();
    assert!(raw.get("pendingCreates").is_none(), "{raw:#}");
    // What was typed into it is kept, a note of the account's own now.
    let mut files = Vec::new();
    markdown_files(&place.vault, "", &mut files);
    let kept = files.iter().find(|file| {
        std::fs::read_to_string(place.vault.join(file)).is_ok_and(|text| text.contains("made here in a share"))
    });
    let kept = kept.unwrap_or_else(|| panic!("the new note's text is gone: {files:?}"));
    let id = note_id_of(&std::fs::read_to_string(place.vault.join(kept)).unwrap()).unwrap();
    assert!(
        fake.zone()
            .live(&id)
            .is_some_and(|r| body_of(r).contains("made here in a share"))
    );
}

/// After the push was cut short, and before the next run, a note the push
/// was editing was edited on the phone too, further down: both edits end
/// up in the file and in iCloud.
#[test]
fn a_push_cut_short_before_an_edit_on_the_phone_is_finished_by_the_next_run() {
    const LONG: &str = "E0000000-0000-4000-8000-000000000003";
    let mut zone = account(9);
    let mut record = zone.live(LONG).unwrap().clone();
    record["fields"]["TextDataEncrypted"]["value"] =
        json!(note_text_field("Extra 3\nline of note 3\n\nmiddle\n\nlast line"));
    zone.put(record);
    let mut start = cloned_from(zone);
    changed_here(&mut start);
    fn edit_on_the_phone(zone: &mut Zone) {
        let mut record = zone.live(LONG).unwrap().clone();
        let text = body_of(&record).replace("last line", "last line, from the phone");
        record["fields"]["TextDataEncrypted"]["value"] = json!(note_text_field(&text));
        zone.put(record);
    }
    let edited = tracked_file(&start.place, LONG);
    let text = std::fs::read_to_string(start.place.vault.join(&edited)).unwrap();
    std::fs::write(
        start.place.vault.join(&edited),
        text.replacen("line of note 3", "line of note 3, typed here", 1),
    )
    .unwrap();
    start.expect.edits.push((edited.clone(), "typed here".into()));
    start.expect.edits.push((edited, "from the phone".into()));
    start.expect.between = Some(edit_on_the_phone);
    crash_everywhere(&start, "push", &[Then::Again, Then::Sync, Then::Pull]);
}

/// Two notes of one title added on the phone, the second deleted on the
/// phone while a pull writing them was cut short.
fn deleted_while_pulled() -> Start {
    let mut start = cloned(3);
    for i in 0..2 {
        start.zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Twin\nwritten on the phone, {i}"),
            DEFAULT_FOLDER,
        ));
    }
    fn delete_one(zone: &mut Zone) {
        let at = zone.tick();
        let stored = zone.records.get_mut("A0000000-0000-4000-8000-000000000001").unwrap();
        stored.deleted = true;
        stored.changed_at = at;
    }
    let mut after = start.zone.clone();
    delete_one(&mut after);
    start.expect.kept = kept_notes(&after);
    start.expect.between = Some(delete_one);
    start
}

/// The file the pull wrote for the deleted note, untouched since, used to be
/// pushed as a new note by the next run: a note deleted on the phone came
/// back. It goes, as the file of a deleted note does.
#[test]
fn a_pull_cut_short_before_a_note_it_wrote_was_deleted_is_finished_by_the_next_run() {
    crash_everywhere(&deleted_while_pulled(), "pull", &[Then::Again, Then::Sync]);
}

/// Notes added on the phone, and one of them edited there while a pull
/// writing them was cut short: the file the pull wrote for it, untouched
/// since, is the note's file, brought up to the edit, not a copy beside the
/// note for the next push to create a second time.
#[test]
fn a_pull_cut_short_before_a_note_it_wrote_was_edited_on_the_phone_is_finished_by_the_next_run() {
    let mut start = cloned(3);
    for i in 0..2 {
        start.zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Twin\nwritten on the phone, {i}"),
            if i == 0 { DEFAULT_FOLDER } else { WORK_FOLDER },
        ));
    }
    fn edit_them(zone: &mut Zone) {
        for i in 0..2 {
            let name = format!("A0000000-0000-4000-8000-{i:012}");
            let mut record = zone.live(&name).unwrap().clone();
            record["fields"]["TextDataEncrypted"]["value"] = json!(note_text_field(&format!(
                "Twin\nwritten on the phone, {i}\nand edited there"
            )));
            zone.put(record);
        }
    }
    start.expect.kept = kept_notes(&start.zone);
    start.expect.between = Some(edit_them);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}

/// The same with the file edited here before the next run: it is kept, and
/// pushed as a new note, edits and all.
#[test]
fn a_file_a_pull_cut_short_wrote_and_edited_here_since_is_kept_when_its_note_is_deleted() {
    let mut start = deleted_while_pulled();
    fn type_here(vault: &Path) {
        let file = vault.join("Notes/Twin 2.md");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("written on the phone, 1"), "{text}");
        std::fs::write(&file, text + "\ntyped here\n").unwrap();
    }
    start.expect.between_here = Some(type_here);
    start.expect.new_notes = 1;
    start.expect.edits.push(("Notes/Twin 2.md".into(), "typed here".into()));
    for then in [Then::Again, Then::Sync] {
        one_case(
            &start,
            "pull",
            crash::NOTE_FILE,
            2,
            then,
            &format!("pull killed after the deleted note's file, then {then:?}"),
        );
    }
}

/// Two untouched copies of the file a pull cut short wrote for a note
/// deleted on the phone since (a file-sync tool's copy, say): both go. The
/// first one's removal used to take the base copy with it, and the second
/// was pushed as a new note.
#[test]
fn two_untouched_copies_of_a_file_a_pull_cut_short_wrote_for_a_deleted_note_both_go() {
    let mut start = deleted_while_pulled();
    fn copy_here(vault: &Path) {
        std::fs::copy(vault.join("Notes/Twin 2.md"), vault.join("Notes/Twin copy.md")).unwrap();
    }
    start.expect.between_here = Some(copy_here);
    for then in [Then::Again, Then::Sync] {
        let place = one_case_place(
            &start,
            "pull",
            crash::NOTE_FILE,
            2,
            then,
            &format!("pull killed after the deleted note's file, copied, then {then:?}"),
        );
        assert!(!place.vault.join("Notes/Twin copy.md").exists());
        assert!(!place.vault.join("Notes/Twin 2.md").exists());
    }
}

/// A note deleted here and its file put back from a backup afterwards is no
/// file a pull left: it is pushed as a new note, as before.
#[test]
fn a_file_put_back_after_its_note_was_deleted_here_is_pushed_as_a_new_note() {
    let start = cloned(3);
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    let file = tracked_file(&place, "E0000000-0000-4000-8000-000000000001");
    let backup = std::fs::read(place.vault.join(&file)).unwrap();
    std::fs::remove_file(place.vault.join(&file)).unwrap();
    succeeded(&run(&fake, &place, "push", &Crash::default()), "push");
    std::fs::write(place.vault.join(&file), &backup).unwrap();
    succeeded(&run(&fake, &place, "push", &Crash::default()), "push");
    assert_eq!(fake.zone().creates("Note").len(), 1);
    assert!(place.vault.join(&file).is_file());
}

/// The push a sync runs again after the pull failing: the half still lists
/// what the first push did, with the second one's error.
#[test]
fn a_sync_whose_second_push_fails_keeps_what_the_first_one_did() {
    let start = retitled_both_sides();
    let fake = Fake::start(start.zone.clone());
    let place = Place::copy_of(&start.place);
    succeeded(&run(&fake, &place, "sync", &Crash::default()), "sync");
    let lookups = fake.zone().lookups;

    let mut zone = start.zone.clone();
    zone.fail_lookups_from = lookups;
    let fake = Fake::start(zone);
    let place = Place::copy_of(&start.place);
    let synced = run(&fake, &place, "sync", &Crash::default());
    assert_eq!(synced.status.code(), Some(5), "{}", says(&synced));
    let result: Value = serde_json::from_slice(&synced.stdout).unwrap();
    let push = &result["data"]["push"];
    assert_eq!(push["ok"], false, "{}", says(&synced));
    assert_eq!(push["exit_code"], 5);
    assert_eq!(push["error"]["code"], "cloudkit_request_failed");
    let entries = push["result"]["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["file"] == "Notes/Extra one.md" && e["outcome"]["succeeded"] == true),
        "{}",
        says(&synced)
    );
    assert!(
        entries.iter().any(|e| e["file"] == "Notes/Extra seven.md"),
        "{}",
        says(&synced)
    );
    assert!(!push["lines"].as_array().unwrap().is_empty());
}

/// Notes added on the phone, and while a pull writing them was cut short,
/// one of them moved to another folder and the folder the others are in
/// renamed: the files the pull had written for them are not left in their
/// old directory, for the next push to create a second time.
#[test]
fn a_pull_cut_short_before_notes_it_wrote_were_moved_on_the_phone_is_finished_by_the_next_run() {
    let mut start = cloned(3);
    for i in 0..4 {
        start.zone.put(note_record(
            &format!("A0000000-0000-4000-8000-{i:012}"),
            &format!("Added {i}\nwritten on the phone"),
            WORK_FOLDER,
        ));
    }
    fn move_them(zone: &mut Zone) {
        let mut moved = zone.live("A0000000-0000-4000-8000-000000000000").unwrap().clone();
        let reference =
            json!({ "recordName": DEFAULT_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
        moved["fields"]["Folder"]["value"] = reference.clone();
        moved["fields"]["Folders"]["value"] = json!([reference]);
        zone.put(moved);
        let mut folder = zone.live(WORK_FOLDER).unwrap().clone();
        folder["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(b"Office"));
        zone.put(folder);
    }
    start.expect.kept = kept_notes(&start.zone);
    start.expect.between = Some(move_them);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}

/// While a clone was cut short, a note it wrote moved to another folder and
/// another one deleted on the phone: the next run leaves neither file it
/// wrote for them behind, to be created as a note again.
#[test]
fn a_clone_cut_short_before_notes_it_wrote_were_moved_or_deleted_on_the_phone_is_finished_by_the_next_run() {
    let zone = account(6);
    fn change_them(zone: &mut Zone) {
        let mut moved = zone.live("E0000000-0000-4000-8000-000000000000").unwrap().clone();
        let reference =
            json!({ "recordName": DEFAULT_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
        moved["fields"]["Folder"]["value"] = reference.clone();
        moved["fields"]["Folders"]["value"] = json!([reference]);
        zone.put(moved);
        let at = zone.tick();
        let stored = zone.records.get_mut("E0000000-0000-4000-8000-000000000001").unwrap();
        stored.deleted = true;
        stored.changed_at = at;
    }
    let mut after = zone.clone();
    change_them(&mut after);
    let start = Start {
        zone,
        place: Place::new(),
        expect: Expect {
            kept: kept_notes(&after),
            between: Some(change_them),
            ..Default::default()
        },
    };
    crash_everywhere(&start, "clone", &[Then::Again, Then::Sync]);
}

/// A note the owner of a shared folder added, and the share revoked while a
/// pull writing it was cut short.
fn revoked_while_pulled() -> Start {
    let mut start = cloned_from(shared_account());
    let folder = format!("SF{}", &WRITABLE_OWNER[1..9]);
    start.zone.shared.get_mut(WRITABLE_OWNER).unwrap().put(note_record(
        "C0000000-0000-4000-8000-000000000009",
        "Owner's new\nfrom the owner",
        &folder,
    ));
    fn revoke(zone: &mut Zone) {
        zone.revoked.insert(WRITABLE_OWNER.into());
    }
    let mut after = start.zone.clone();
    revoke(&mut after);
    start.expect.kept = kept_notes(&after);
    start.expect.between = Some(revoke);
    start
}

/// Nothing of that note goes up into the account's own notes, neither the
/// note nor a folder for the directory it was in.
#[test]
fn a_pull_cut_short_before_a_share_it_wrote_a_note_from_was_revoked_is_finished_by_the_next_run() {
    crash_everywhere(&revoked_while_pulled(), "pull", &[Then::Again, Then::Sync]);
}

/// The file the pull wrote for the owner's note edited here before the next
/// run: kept, with the edit, as a file that is not synced (`unshared`), as
/// an edited note of a share revoked is; never uploaded as the account's own.
#[test]
fn a_file_a_pull_cut_short_wrote_from_a_share_revoked_since_and_edited_here_is_kept_and_not_uploaded() {
    let mut start = revoked_while_pulled();
    fn type_here(vault: &Path) {
        let mut files = Vec::new();
        markdown_files(vault, "", &mut files);
        for file in files {
            let text = std::fs::read_to_string(vault.join(&file)).unwrap();
            if text.contains("from the owner") {
                std::fs::write(vault.join(&file), text + "\ntyped here\n").unwrap();
            }
        }
    }
    start.expect.between_here = Some(type_here);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
    for then in [Then::Again, Then::Sync] {
        let label = format!("pull killed after the owner's note's file, then {then:?}");
        let place = one_case_place(&start, "pull", crash::NOTE_FILE, 1, then, &label);
        let mut files = Vec::new();
        markdown_files(&place.vault, "", &mut files);
        let kept = files
            .iter()
            .find(|file| std::fs::read_to_string(place.vault.join(file)).is_ok_and(|text| text.contains("typed here")));
        let kept = kept.unwrap_or_else(|| panic!("{label}: the edited file is gone: {files:?}"));
        let id = note_id_of(&std::fs::read_to_string(place.vault.join(kept)).unwrap());
        assert_eq!(id.as_deref(), Some("C0000000-0000-4000-8000-000000000009"), "{label}");
        let raw: Value =
            serde_json::from_str(&std::fs::read_to_string(place.vault.join(".icloud-notes/state.json")).unwrap())
                .unwrap();
        assert!(
            raw["unshared"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|i| i == id.as_deref().unwrap())),
            "{label}: {raw:#}"
        );
    }
}

/// A note with a photo added on the phone in `Work`, and `Work` renamed
/// while a pull writing it was cut short: the photo the pull had downloaded
/// into `Work/attachments/` goes along with the directory, which leaves no
/// `Work/` behind for the next push to make a folder of.
#[test]
fn a_pull_cut_short_before_the_folder_of_a_note_with_a_photo_it_wrote_was_renamed_is_finished_by_the_next_run() {
    const PHOTO_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
    let mut zone = account(3);
    zone.downloads_in_line = true;
    let photo_note = zone.live(PHOTO_NOTE).unwrap().clone();
    zone.records.get_mut(PHOTO_NOTE).unwrap().deleted = true;
    let mut start = cloned_from(zone);
    let mut added = photo_note;
    let reference = json!({ "recordName": WORK_FOLDER, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } });
    added["fields"]["Folder"]["value"] = reference.clone();
    added["fields"]["Folders"]["value"] = json!([reference]);
    start.zone.put(added);
    fn rename(zone: &mut Zone) {
        let mut folder = zone.live(WORK_FOLDER).unwrap().clone();
        folder["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(b"Office"));
        zone.put(folder);
    }
    start.expect.kept = kept_notes(&start.zone);
    start.expect.between = Some(rename);
    crash_everywhere(&start, "pull", &[Then::Again, Then::Sync]);
}
