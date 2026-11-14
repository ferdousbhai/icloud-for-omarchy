//! Adding an attachment to a note from this computer: a link to a file in
//! the note folder's `attachments/` is uploaded and created with the note's
//! edit in one atomic request, or nothing is. And removing one (the second
//! half of this file): a link gone from the note takes the attachment out of
//! it, its records marked deleted in one atomic request with the edit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::client::parse_record;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::plan::{PlanEntryKind, PlanResolution};
use icloud_notes_sync::cmd::pull::{run_pull_with};
use icloud_notes_sync::cmd::push::{PushOptions, PushResult, run_push_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
use icloud_notes_sync::doc::embeds::{
    AttachmentReference, EmbedSlot, decode_note_embed_slots, format_attachment_markdown,
};
use icloud_notes_sync::doc::text::decode_note_body_text;
use icloud_notes_sync::js::base64_decode;
use icloud_notes_sync::vault::base::read_base_copy;
use icloud_notes_sync::vault::state::{Account, AttachmentEntry, CloneState, read_clone_state, write_clone_state};
use serde_json::{Value, json};

const PRIVATE: &str = "/database/1/com.apple.notes/production/private";
const SHARED: &str = "/database/1/com.apple.notes/production/shared";
const NOTE: &str = "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1";
const NOTE_FILE: &str = "Notes/Test Note.md";
const O: char = '\u{fffc}';

fn cassette(name: &str) -> Cassette {
    Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/differential/cassettes")
            .join(name),
    )
    .unwrap()
}

/// A private Notes zone holding `records`, which answers lookups, hands out
/// upload URLs, stores uploads and applies `records/modify` as iCloud does:
/// all of an atomic request, or none of it.
#[derive(Default)]
struct Server {
    records: Mutex<Vec<Value>>,
    /// Every POST to the database as (operation, body).
    posts: Mutex<Vec<(String, Value)>>,
    /// Every upload as (URL, bytes).
    uploads: Mutex<Vec<(String, Vec<u8>)>>,
    /// Uploads fail with this HTTP status.
    fail_upload: Option<u16>,
    /// `records/modify` refuses the record of this type with this code.
    reject: Option<(&'static str, &'static str)>,
    /// `records/modify` fails with this HTTP status.
    fail_modify: Option<u16>,
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

    fn record(&self, name: &str) -> Option<Value> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r["recordName"] == name)
            .cloned()
    }

    fn note_text(&self, name: &str) -> (String, Vec<EmbedSlot>) {
        let record = self.record(name).unwrap();
        let bytes = base64_decode(record["fields"]["TextDataEncrypted"]["value"].as_str().unwrap());
        (
            decode_note_body_text(&bytes).unwrap(),
            decode_note_embed_slots(&bytes).unwrap().unwrap(),
        )
    }

    fn modify(&self, body: &Value) -> Value {
        let operations = body["operations"].as_array().unwrap();
        if let Some((record_type, code)) = self.reject {
            assert_eq!(body["atomic"], true, "only an atomic request is refused whole");
            let answers: Vec<Value> = operations
                .iter()
                .map(|op| {
                    let refused = op["record"]["recordType"] == record_type;
                    json!({
                        "recordName": op["record"]["recordName"],
                        "serverErrorCode": if refused { code } else { "ATOMIC_ERROR" },
                        "reason": if refused { "refused by the test server" } else { "Atomic failure" },
                    })
                })
                .collect();
            return json!({ "records": answers });
        }
        let mut records = self.records.lock().unwrap();
        let mut changes = self.changes.lock().unwrap();
        let mut answers = Vec::new();
        for op in operations {
            *changes += 1;
            let sent = &op["record"];
            let typed = |fields: &Value| -> Value {
                let typed: serde_json::Map<String, Value> = fields
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(_, field)| field.get("value").is_some_and(|v| !v.is_null()))
                    .map(|(name, field)| {
                        let type_ = match &field["value"] {
                            Value::String(_) => "ENCRYPTED_BYTES",
                            Value::Number(_) => "INT64",
                            Value::Array(_) => "REFERENCE_LIST",
                            v if v.get("receipt").is_some() => "ASSETID",
                            _ => "REFERENCE",
                        };
                        (name.clone(), json!({ "value": field["value"], "type": type_ }))
                    })
                    .collect();
                Value::Object(typed)
            };
            let record = match op["operationType"].as_str().unwrap() {
                "update" => {
                    let record = records
                        .iter_mut()
                        .find(|r| r["recordName"] == sent["recordName"])
                        .expect("an update names a record the zone has");
                    assert_eq!(record["recordChangeTag"], sent["recordChangeTag"], "a stale change tag");
                    for (name, field) in typed(&sent["fields"]).as_object().unwrap() {
                        record["fields"][name] = field.clone();
                    }
                    record["recordChangeTag"] = json!(format!("t{changes}"));
                    record.clone()
                }
                "create" => {
                    assert!(
                        !records.iter().any(|r| r["recordName"] == sent["recordName"]),
                        "created twice: {}",
                        sent["recordName"]
                    );
                    let mut record = json!({
                        "recordName": sent["recordName"],
                        "recordType": sent["recordType"],
                        "fields": typed(&sent["fields"]),
                        "recordChangeTag": format!("t{changes}"),
                        "deleted": false,
                    });
                    if let Some(parent) = sent.get("parent") {
                        record["parent"] = parent.clone();
                    }
                    records.push(record.clone());
                    record
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
            "/assets/upload" => {
                let token = &body["tokens"][0];
                json!({ "tokens": [{
                    "recordName": token["recordName"],
                    "fieldName": token["fieldName"],
                    "url": format!("https://upload.test/{}", token["recordName"].as_str().unwrap()),
                }] })
            }
            "/records/modify" => match self.fail_modify {
                Some(status) => {
                    return Err(CkError::Http {
                        status,
                        body: "modify refused".into(),
                    });
                }
                None => self.modify(body),
            },
            // A pull: everything the zone holds, and nobody sharing.
            "/changes/zone" => json!({ "zones": [{
                "zoneID": { "zoneName": "Notes", "ownerRecordName": "_defaultOwner", "zoneType": "REGULAR_CUSTOM_ZONE" },
                "moreComing": false,
                "syncToken": "after-the-push",
                "records": *self.records.lock().unwrap(),
            }] }),
            "/changes/database" => json!({ "zones": [], "moreComing": false, "syncToken": "shared" }),
            _ => panic!("unexpected request to {path}"),
        })
    }

    fn download(&self, url: &str, _dest: &Path) -> Result<u64, CkError> {
        panic!("unexpected download: {url}")
    }

    fn upload(&self, url: &str, bytes: &[u8]) -> Result<Value, CkError> {
        self.uploads.lock().unwrap().push((url.to_owned(), bytes.to_vec()));
        if let Some(status) = self.fail_upload {
            return Err(CkError::Http {
                status,
                body: "upload refused".into(),
            });
        }
        Ok(json!({ "singleFile": asset(bytes.len()) }))
    }
}

/// What the test server answers an upload of `size` bytes with.
fn asset(size: usize) -> Value {
    json!({
        "fileChecksum": format!("checksum-{size}"),
        "size": size,
        "wrappingKey": "d3JhcA==",
        "referenceChecksum": "cmVm",
        "receipt": format!("receipt-{size}"),
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

    fn upload(&self, url: &str, bytes: &[u8]) -> Result<Value, CkError> {
        self.0.upload(url, bytes)
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
    run_clone_with(
        &connector,
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    vault
}

fn push(server: &Arc<Server>, vault: &Path) -> PushResult {
    run_push_with(&serve(server), vault, &mut |_| {}, &PushOptions::default()).unwrap()
}

fn preview(server: &Arc<Server>, vault: &Path) -> PushResult {
    run_push_with(&serve(server), vault, &mut |_| {}, &PushOptions { dry_run: true }).unwrap()
}

fn state(vault: &Path) -> CloneState {
    read_clone_state(vault).unwrap().unwrap()
}

fn append(vault: &Path, file: &str, text: &str) {
    let path = vault.join(file);
    let mut current = std::fs::read_to_string(&path).unwrap();
    current.push_str(text);
    std::fs::write(path, current).unwrap();
}

fn attach(vault: &Path, name: &str, bytes: &[u8]) {
    let dir = vault.join("Notes/attachments");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 1, 2, 3]);
    bytes
}

fn is_uppercase_uuid(name: &str) -> bool {
    name.len() == 36
        && name.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('A'..='F').contains(&c),
        })
}

fn slot(id: &str, uti: &str) -> EmbedSlot {
    EmbedSlot::Attachment(AttachmentReference {
        attachment_identifier: id.into(),
        type_uti: uti.into(),
    })
}

fn reference(name: &str) -> Value {
    json!({ "recordName": name, "action": "VALIDATE", "zoneID": { "zoneName": "Notes" } })
}

/// The one refusal of a plan that has nothing else in it.
fn refusal(result: &PushResult) -> &str {
    assert_eq!(result.entries.len(), 1, "{result:?}");
    let entry = &result.entries[0].entry;
    assert_eq!(entry.resolution, PlanResolution::Refused, "{entry:?}");
    entry.reason.as_deref().unwrap()
}

/// Nothing was written to iCloud, and the vault tracks no attachment.
fn assert_nothing_made(server: &Server, vault: &Path) {
    assert!(server.posts_to("/records/modify").is_empty());
    assert_eq!(server.records.lock().unwrap().len(), 1);
    assert!(state(vault).attachments.unwrap_or_default().is_empty());
    assert_eq!(state(vault).notes[NOTE].record_change_tag, "25q");
}

#[test]
fn a_linked_file_is_uploaded_and_created_with_the_edit_in_one_atomic_request() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let image = png(640, 480);
    attach(&vault, "pic.png", &image);
    append(&vault, NOTE_FILE, "\nSee:\n![pic.png](attachments/pic.png)\n");
    let note_text = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
    let state_before = std::fs::read(vault.join(".icloud-notes/state.json")).unwrap();

    // A preview plans it and says so; it uploads, creates and tracks nothing.
    let planned = preview(&server, &vault);
    assert_eq!(planned.entries.len(), 1);
    let entry = &planned.entries[0].entry;
    assert_eq!(
        (entry.kind, entry.resolution),
        (PlanEntryKind::Update, PlanResolution::Ready)
    );
    assert_eq!(entry.remark.as_deref(), Some("adds the attachment pic.png"));
    assert!(planned.entries[0].outcome.is_none());
    assert!(server.uploads.lock().unwrap().is_empty());
    assert!(server.posts_to("/assets/upload").is_empty());
    assert_nothing_made(&server, &vault);
    assert_eq!(
        std::fs::read(vault.join(".icloud-notes/state.json")).unwrap(),
        state_before
    );

    let result = push(&server, &vault);
    assert_eq!(result.pushed, Some(1));
    let outcome = result.entries[0].outcome.as_ref().unwrap();
    assert!(outcome.succeeded, "{outcome:?}");
    assert_eq!(outcome.message, "Pushed Notes/Test Note.md, with its new attachment");

    // The upload: a token for the new Media record's Asset, then the bytes.
    let modify = server.posts_to("/records/modify");
    assert_eq!(modify.len(), 1, "one request carries everything");
    let operations = modify[0]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 3);
    let media = operations[1]["record"]["recordName"].as_str().unwrap().to_owned();
    let attachment = operations[2]["record"]["recordName"].as_str().unwrap().to_owned();
    assert!(is_uppercase_uuid(&media) && is_uppercase_uuid(&attachment) && media != attachment);
    assert_eq!(
        server.posts_to("/assets/upload"),
        [json!({
            "zoneID": { "zoneName": "Notes" },
            "tokens": [{ "recordType": "Media", "recordName": media, "fieldName": "Asset" }],
        })]
    );
    assert_eq!(
        *server.uploads.lock().unwrap(),
        [(format!("https://upload.test/{media}"), image.clone())]
    );

    // The request: atomic; the note's edit, then the Media and Attachment
    // records, both children of the note.
    assert_eq!(modify[0]["atomic"], true);
    assert_eq!(modify[0]["zoneID"], json!({ "zoneName": "Notes" }));
    let note = &operations[0];
    assert_eq!(note["operationType"], "update");
    assert_eq!(note["record"]["recordName"], NOTE);
    assert_eq!(note["record"]["recordType"], "Note");
    assert_eq!(note["record"]["recordChangeTag"], "25q");
    assert_eq!(
        operations[1],
        json!({
            "operationType": "create",
            "record": {
                "recordName": media,
                "recordType": "Media",
                "fields": {
                    "Asset": { "value": asset(image.len()) },
                    "FilenameEncrypted": { "value": "cGljLnBuZw==" },
                    "MinimumSupportedNotesVersion": { "value": 0 },
                    "Deleted": { "value": 0 },
                },
                "parent": { "recordName": NOTE },
            },
        })
    );
    let now = operations[2]["record"]["fields"]["CreationDate"]["value"].clone();
    assert!(now.as_i64().unwrap() > 1_700_000_000_000);
    assert_eq!(
        operations[2],
        json!({
            "operationType": "create",
            "record": {
                "recordName": attachment,
                "recordType": "Attachment",
                "fields": {
                    "UTI": { "value": "public.png" },
                    "UTIEncrypted": { "value": "cHVibGljLnBuZw==" },
                    "TitleEncrypted": { "value": "cGljLnBuZw==" },
                    "Media": { "value": reference(&media) },
                    "Note": { "value": reference(NOTE) },
                    "FileSize": { "value": image.len() },
                    "CreationDate": { "value": now },
                    "LastModificationDate": { "value": now },
                    "MinimumSupportedNotesVersion": { "value": 0 },
                    "Deleted": { "value": 0 },
                    "Orientation": { "value": 0 },
                    "ImageFilterType": { "value": 0 },
                    "CroppingQuadTopLeftX": { "value": 0.0 },
                    "CroppingQuadTopLeftY": { "value": 1.0 },
                    "CroppingQuadTopRightX": { "value": 1.0 },
                    "CroppingQuadTopRightY": { "value": 1.0 },
                    "CroppingQuadBottomRightX": { "value": 1.0 },
                    "CroppingQuadBottomRightY": { "value": 0.0 },
                    "CroppingQuadBottomLeftX": { "value": 0.0 },
                    "CroppingQuadBottomLeftY": { "value": 0.0 },
                    "Width": { "value": 640.0 },
                    "Height": { "value": 480.0 },
                },
                "parent": { "recordName": NOTE },
            },
        })
    );

    // The note's text: a placeholder where the link is, carrying the
    // attachment.
    let (text, slots) = server.note_text(NOTE);
    assert!(text.ends_with(&format!("\nSee:\n{O}\n")), "{text:?}");
    assert_eq!(slots, [slot(&attachment, "public.png")]);

    // A pull of that record writes the link back exactly as it is here.
    let record = parse_record(&server.record(NOTE).unwrap()).unwrap();
    let NoteDecodeResult::Ok(decoded) = classify_note_record(&record, &ClassifyOptions::default()) else {
        panic!("the pushed note decodes");
    };
    assert!(decoded.publishable);
    let link = format_attachment_markdown(
        &AttachmentReference {
            attachment_identifier: attachment.clone(),
            type_uti: "public.png".into(),
        },
        "attachments/pic.png",
    );
    assert_eq!(
        Some(decoded.markdown_text.replace(O, &link)),
        read_base_copy(&vault, NOTE).unwrap()
    );

    // The vault tracks it: the link is the attachment, the file its download.
    let after = state(&vault);
    assert_eq!(
        after.attachments.unwrap().into_iter().collect::<Vec<_>>(),
        [(
            attachment.clone(),
            AttachmentEntry {
                file: "Notes/attachments/pic.png".into(),
                media_record_name: media.clone(),
                media_file_checksum: format!("checksum-{}", image.len()),
                note_record_name: NOTE.into(),
            }
        )]
    );
    assert_eq!(after.notes[NOTE].record_change_tag, "t1");
    assert_eq!(std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap(), note_text);
    assert_eq!(std::fs::read(vault.join("Notes/attachments/pic.png")).unwrap(), image);
    let base = read_base_copy(&vault, NOTE).unwrap().unwrap();
    assert!(base.ends_with("![pic.png](attachments/pic.png)\n"), "{base:?}");
    assert!(
        std::fs::read_dir(vault.join(".icloud-notes/history").join(NOTE).join("epochs"))
            .unwrap()
            .count()
            > 0
    );

    // Nothing is left to push, and nothing more is asked of iCloud.
    let requests = server.posts.lock().unwrap().len();
    let clean = preview(&server, &vault);
    assert!(clean.entries.is_empty(), "{clean:?}");
    assert_eq!(clean.unchanged, 1);
    assert_eq!(server.posts.lock().unwrap().len(), requests);

    // An edit beside the link goes up as any edit; the link stays the
    // attachment, and no file is uploaded again.
    append(&vault, NOTE_FILE, "A line after it.\n");
    let result = push(&server, &vault);
    assert_eq!(
        result.entries[0].outcome.as_ref().unwrap().message,
        "Pushed Notes/Test Note.md"
    );
    assert_eq!(server.uploads.lock().unwrap().len(), 1);
    let (text, slots) = server.note_text(NOTE);
    assert!(text.ends_with(&format!("\nSee:\n{O}\nA line after it.\n")), "{text:?}");
    assert_eq!(slots, [slot(&attachment, "public.png")]);
}

/// What `attach` does twice: the second link goes after the first. The
/// first attachment keeps its placeholder; the new one is the one after it.
#[test]
fn a_second_attachment_goes_in_after_the_first_which_keeps_its_place() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    attach(&vault, "one.txt", b"first file");
    append(&vault, NOTE_FILE, "\n[one.txt](attachments/one.txt)");
    assert_eq!(push(&server, &vault).pushed, Some(1));
    let (_, slots) = server.note_text(NOTE);
    let first = slots[0].clone();

    attach(&vault, "two.pdf", b"%PDF-1.7 second file");
    attach(&vault, "three.gif", b"GIF89a\x02\x00\x03\x00");
    append(
        &vault,
        NOTE_FILE,
        "\n[two.pdf](attachments/two.pdf)\n![three.gif](attachments/three.gif)\n",
    );
    let planned = preview(&server, &vault);
    assert_eq!(
        planned.entries[0].entry.remark.as_deref(),
        Some("adds the attachments two.pdf, three.gif")
    );
    let result = push(&server, &vault);
    assert_eq!(
        result.entries[0].outcome.as_ref().unwrap().message,
        "Pushed Notes/Test Note.md, with its 2 new attachments"
    );

    let (text, slots) = server.note_text(NOTE);
    assert!(text.ends_with(&format!("`\n\n{O}\n{O}\n{O}\n")), "{text:?}");
    assert_eq!(slots.len(), 3);
    assert_eq!(slots[0], first, "the first attachment is where it was");
    let modify = server.posts_to("/records/modify");
    let operations = modify[1]["operations"].as_array().unwrap();
    let kinds: Vec<(&str, &str)> = operations
        .iter()
        .map(|op| {
            (
                op["operationType"].as_str().unwrap(),
                op["record"]["recordType"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("update", "Note"),
            ("create", "Media"),
            ("create", "Attachment"),
            ("create", "Media"),
            ("create", "Attachment"),
        ]
    );
    let name = |i: usize| operations[i]["record"]["recordName"].as_str().unwrap();
    assert_eq!(
        slots[1..],
        [slot(name(2), "com.adobe.pdf"), slot(name(4), "com.compuserve.gif")]
    );
    let fields = &operations[4]["record"]["fields"];
    assert_eq!(fields["Width"], json!({ "value": 2.0 }));
    assert_eq!(fields["Height"], json!({ "value": 3.0 }));
    assert!(operations[2]["record"]["fields"].get("Width").is_none());
    assert_eq!(server.uploads.lock().unwrap().len(), 3);
    assert_eq!(state(&vault).attachments.unwrap().len(), 3);
    assert!(preview(&server, &vault).entries.is_empty());
}

#[test]
fn a_failed_upload_makes_and_tracks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server {
        fail_upload: Some(503),
        ..Server::with_note()
    });
    attach(&vault, "plan.pdf", b"%PDF-1.7");
    append(&vault, NOTE_FILE, "\n[plan.pdf](attachments/plan.pdf)\n");
    let note_text = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
    let base = read_base_copy(&vault, NOTE).unwrap();

    let err = run_push_with(&serve(&server), &vault, &mut |_| {}, &PushOptions::default()).unwrap_err();
    assert!(
        matches!(&err, Error::CloudKit(CkError::RequestFailed { message, status: Some(503) })
            if message == "Asset upload failed: HTTP 503"),
        "{err:?}"
    );
    assert_eq!(server.uploads.lock().unwrap().len(), 1);
    assert_nothing_made(&server, &vault);
    assert_eq!(std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap(), note_text);
    assert_eq!(
        std::fs::read(vault.join("Notes/attachments/plan.pdf")).unwrap(),
        b"%PDF-1.7"
    );
    assert_eq!(read_base_copy(&vault, NOTE).unwrap(), base);

    // Still to push: the next run tries again.
    let planned = preview(&server, &vault);
    assert_eq!(planned.entries[0].entry.resolution, PlanResolution::Ready);
}

#[test]
fn a_rejected_request_makes_and_tracks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server {
        reject: Some(("Media", "BAD_REQUEST")),
        ..Server::with_note()
    });
    attach(&vault, "plan.pdf", b"%PDF-1.7");
    append(&vault, NOTE_FILE, "\n[plan.pdf](attachments/plan.pdf)\n");
    let note_text = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
    let base = read_base_copy(&vault, NOTE).unwrap();

    let result = push(&server, &vault);
    assert_eq!(result.pushed, Some(0));
    let outcome = result.entries[0].outcome.as_ref().unwrap();
    assert!(!outcome.succeeded);
    // The record that was refused says why, not the ones refused with it.
    assert_eq!(
        outcome.message,
        "Notes/Test Note.md: server rejected the update: BAD_REQUEST (refused by the test server)"
    );
    assert_eq!(result.failures().len(), 1);

    assert_eq!(server.posts_to("/records/modify").len(), 1);
    assert_eq!(server.records.lock().unwrap().len(), 1, "nothing half-made");
    assert_eq!(server.note_text(NOTE).1, []);
    let after = state(&vault);
    assert!(after.attachments.unwrap_or_default().is_empty());
    assert_eq!(after.notes[NOTE].record_change_tag, "25q");
    assert_eq!(std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap(), note_text);
    assert_eq!(read_base_copy(&vault, NOTE).unwrap(), base);
    assert_eq!(
        preview(&server, &vault).entries[0].entry.resolution,
        PlanResolution::Ready
    );
}

#[test]
fn links_that_cannot_be_attachments_are_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let original = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
    std::fs::write(dir.path().join("secret.txt"), "outside the vault").unwrap();
    std::fs::write(vault.join("Notes/beside.txt"), "outside attachments/").unwrap();
    attach(&vault, "ok.txt", b"fine");
    attach(&vault, "empty.txt", b"");
    std::fs::create_dir_all(vault.join("Notes/attachments/sub")).unwrap();
    std::fs::write(vault.join("Notes/attachments/sub/deep.txt"), "deeper").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret.txt"), vault.join("Notes/attachments/link.txt")).unwrap();
    std::os::unix::fs::symlink("ok.txt", vault.join("Notes/attachments/inside-link.txt")).unwrap();
    let huge = std::fs::File::create(vault.join("Notes/attachments/huge.zip")).unwrap();
    huge.set_len(100 * 1024 * 1024 + 1).unwrap();

    let refused = |link: &str| -> String {
        std::fs::write(vault.join(NOTE_FILE), format!("{original}\n{link}\n")).unwrap();
        let result = preview(&server, &vault);
        let reason = refusal(&result).to_owned();
        // A real push refuses it the same way and sends nothing.
        let pushed = push(&server, &vault);
        assert_eq!(pushed.pushed, Some(0));
        assert_eq!(refusal(&pushed), reason);
        assert!(server.uploads.lock().unwrap().is_empty());
        assert_nothing_made(&server, &vault);
        reason
    };
    let restore = " Run \"icloud-notes restore Notes/Test Note.md\" to discard your local edit.";

    assert_eq!(
        refused("[gone.txt](attachments/gone.txt)"),
        format!(
            "the linked file \"attachments/gone.txt\" doesn't exist - put the file in this folder's attachments/ \
             directory, or remove the link.{restore}"
        )
    );
    let outside = |href: &str| {
        format!(
            "the link \"{href}\" doesn't name a file directly in this folder's attachments/ directory - only a \
             file there can be added as an attachment.{restore}"
        )
    };
    for href in [
        "attachments/../beside.txt",
        "attachments/..%2Fbeside.txt",
        "attachments/../../secret.txt",
        "attachments/sub/deep.txt",
        "attachments/.",
        "attachments/%ZZ.txt",
    ] {
        assert_eq!(refused(&format!("[x]({href})")), outside(href), "{href}");
    }
    for name in ["link.txt", "inside-link.txt", "sub"] {
        assert_eq!(
            refused(&format!("[{name}](attachments/{name})")),
            format!(
                "the linked file \"attachments/{name}\" is not a regular file (a directory or a symbolic link \
                 can't be added as an attachment).{restore}"
            )
        );
    }
    assert_eq!(
        refused("[empty.txt](attachments/empty.txt)"),
        format!("the linked file \"attachments/empty.txt\" is empty.{restore}")
    );
    assert_eq!(
        refused("[huge.zip](attachments/huge.zip)"),
        format!(
            "the linked file \"attachments/huge.zip\" is larger than 100 MB, the most this tool adds as an \
             attachment.{restore}"
        )
    );
    // Not the link pull would write for the file.
    assert_eq!(
        refused("[my file](attachments/ok.txt)"),
        format!(
            "the link to \"attachments/ok.txt\" isn't written the way this tool writes an attachment's link - \
             write it as [ok.txt](attachments/ok.txt).{restore}"
        )
    );
    assert_eq!(
        refused("[ok.txt](attachments/ok.txt) and [ok.txt](attachments/ok.txt)"),
        format!("\"attachments/ok.txt\" is linked more than once - a file is attached once.{restore}")
    );
    assert!(refused("[ok.txt](attachments/ok.txt").contains("isn't written the way this tool writes attachment"));

    // The `attachments` directory itself leading out of the vault.
    std::fs::rename(vault.join("Notes/attachments"), dir.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), vault.join("Notes/attachments")).unwrap();
    assert_eq!(refused("[ok.txt](attachments/ok.txt)"), outside("attachments/ok.txt"));
}

/// A file that is another note's attachment (the folder's notes share one
/// `attachments/`) is that note's: removing either note would take it.
#[test]
fn a_file_that_is_already_an_attachment_is_not_attached_again() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    attach(&vault, "theirs.png", &png(1, 1));
    let mut tracked = state(&vault);
    tracked.attachments.get_or_insert_default().insert(
        "ATT-OTHER".into(),
        AttachmentEntry {
            file: "Notes/attachments/theirs.png".into(),
            media_record_name: "MEDIA-OTHER".into(),
            media_file_checksum: "x".into(),
            note_record_name: "another-note".into(),
        },
    );
    write_clone_state(&vault, &tracked).unwrap();
    append(&vault, NOTE_FILE, "\n![theirs.png](attachments/theirs.png)\n");
    let reason = refusal(&preview(&server, &vault)).to_owned();
    assert!(
        reason.starts_with(
            "\"attachments/theirs.png\" is already an attachment of another note - to attach the file again, copy \
             it under another name."
        ),
        "{reason}"
    );
    assert!(server.posts_to("/records/modify").is_empty());
}

/// A note in someone else's shared folder can be edited, but its
/// attachments are made in the owner's zone: not from here.
#[test]
fn a_shared_note_gets_no_attachment_from_here() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let mut shared = state(&vault);
    let mut folder = shared.folders.as_ref().unwrap()["DefaultFolder-CloudKit"].clone();
    folder.shared_zone_owner = Some("_sharer".into());
    folder.permission = Some("READ_WRITE".into());
    shared.folders.as_mut().unwrap().insert("F-SHARED".into(), folder);
    let note = shared.notes.get_mut(NOTE).unwrap();
    note.shared_zone_owner = Some("_sharer".into());
    note.folder_record_name = Some("F-SHARED".into());
    write_clone_state(&vault, &shared).unwrap();
    attach(&vault, "plan.pdf", b"%PDF-1.7");
    append(&vault, NOTE_FILE, "\n[plan.pdf](attachments/plan.pdf)\n");

    let reason = refusal(&push(&server, &vault)).to_owned();
    assert!(
        reason.starts_with("attachments can't be added to a shared note from here - add them in Notes itself."),
        "{reason}"
    );
    assert!(server.uploads.lock().unwrap().is_empty());
    assert!(server.posts_to("/records/modify").is_empty());
    assert!(state(&vault).attachments.unwrap_or_default().is_empty());
}

/// A new note's file that links to files: the note, and a Media and an
/// Attachment record per file, are created in one atomic request.
#[test]
fn a_new_note_is_created_with_its_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    attach(&vault, "hello.txt", b"hello");
    std::fs::write(
        vault.join("Notes/Trip.md"),
        "Trip\n\nThe ticket: [hello.txt](attachments/hello.txt)\n",
    )
    .unwrap();

    let planned = preview(&server, &vault);
    let entry = &planned.entries[0].entry;
    assert_eq!(
        (entry.kind, entry.resolution),
        (PlanEntryKind::Create, PlanResolution::Ready)
    );
    assert_eq!(entry.remark.as_deref(), Some("adds the attachment hello.txt"));
    assert!(server.uploads.lock().unwrap().is_empty() && server.posts_to("/records/modify").is_empty());

    let result = push(&server, &vault);
    assert_eq!(
        result.entries[0].outcome.as_ref().unwrap().message,
        "Created Notes/Trip.md, with its attachment"
    );
    let modify = server.posts_to("/records/modify");
    assert_eq!(modify.len(), 1);
    assert_eq!(modify[0]["atomic"], true);
    let operations = modify[0]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 3);
    assert!(operations.iter().all(|op| op["operationType"] == "create"));
    let name = |i: usize| operations[i]["record"]["recordName"].as_str().unwrap().to_owned();
    let (note, media, attachment) = (name(0), name(1), name(2));
    assert_eq!(operations[0]["record"]["recordType"], "Note");
    assert_eq!(operations[1]["record"]["recordType"], "Media");
    assert_eq!(operations[1]["record"]["parent"], json!({ "recordName": note }));
    assert_eq!(operations[2]["record"]["recordType"], "Attachment");
    assert_eq!(operations[2]["record"]["parent"], json!({ "recordName": note }));
    let fields = &operations[2]["record"]["fields"];
    assert_eq!(fields["Note"]["value"], reference(&note));
    assert_eq!(fields["Media"]["value"], reference(&media));
    assert_eq!(fields["UTI"]["value"], "public.plain-text");
    assert_eq!(
        server.posts_to("/assets/upload")[0]["tokens"],
        json!([{ "recordType": "Media", "recordName": media, "fieldName": "Asset" }])
    );
    assert_eq!(
        *server.uploads.lock().unwrap(),
        [(format!("https://upload.test/{media}"), b"hello".to_vec())]
    );

    let (text, slots) = server.note_text(&note);
    assert_eq!(text, format!("Trip\n\nThe ticket: {O}\n"));
    assert_eq!(slots, [slot(&attachment, "public.plain-text")]);

    let after = state(&vault);
    assert_eq!(after.notes[&note].file, "Notes/Trip.md");
    let tracked = &after.attachments.as_ref().unwrap()[&attachment];
    assert_eq!(tracked.file, "Notes/attachments/hello.txt");
    assert_eq!(tracked.note_record_name, note);
    assert_eq!(tracked.media_record_name, media);
    assert!(
        std::fs::read_to_string(vault.join("Notes/Trip.md"))
            .unwrap()
            .contains(&format!("apple-note-id: {note}"))
    );
    assert!(preview(&server, &vault).entries.is_empty());

    // Refused whole: the note isn't created without its attachment.
    let refusing = Arc::new(Server {
        reject: Some(("Attachment", "BAD_REQUEST")),
        ..Server::with_note()
    });
    attach(&vault, "more.txt", b"more");
    std::fs::write(
        vault.join("Notes/Second.md"),
        "Second\n[more.txt](attachments/more.txt)\n",
    )
    .unwrap();
    let result = push(&refusing, &vault);
    assert_eq!(result.pushed, Some(0));
    assert_eq!(
        result.entries[0].outcome.as_ref().unwrap().message,
        "Notes/Second.md: server rejected the create: BAD_REQUEST (refused by the test server)"
    );
    assert_eq!(refusing.records.lock().unwrap().len(), 1);
    let after = state(&vault);
    assert_eq!(after.notes.len(), 2);
    assert_eq!(after.attachments.unwrap().len(), 1);
    assert!(
        !std::fs::read_to_string(vault.join("Notes/Second.md"))
            .unwrap()
            .contains("apple-note-id")
    );

    // A new note linking to a file that isn't there is refused before
    // anything is asked of iCloud.
    std::fs::remove_file(vault.join("Notes/Second.md")).unwrap();
    std::fs::write(vault.join("Notes/Third.md"), "Third\n[no.txt](attachments/no.txt)\n").unwrap();
    let requests = server.posts.lock().unwrap().len();
    assert_eq!(
        refusal(&preview(&server, &vault)),
        "the linked file \"attachments/no.txt\" doesn't exist - put the file in this folder's attachments/ \
         directory, or remove the link"
    );
    assert_eq!(server.posts.lock().unwrap().len(), requests);
}

// --- removing an attachment -----------------------------------------------------------

/// Pushes `files` as new attachments of the note (one line each, after
/// "Files:"), and returns each one's (Attachment, Media) record names.
fn attached(server: &Arc<Server>, vault: &Path, files: &[(&str, &[u8])]) -> Vec<(String, String)> {
    let mut text = String::from("\nFiles:\n");
    for (name, bytes) in files {
        attach(vault, name, bytes);
        text.push_str(&link_of(name));
        text.push('\n');
    }
    text.push_str("The end.\n");
    append(vault, NOTE_FILE, &text);
    let result = push(server, vault);
    assert_eq!(result.pushed, Some(1), "{result:?}");
    let tracked = state(vault).attachments.unwrap();
    files
        .iter()
        .map(|(name, _)| {
            let file = format!("Notes/attachments/{name}");
            let (attachment, entry) = tracked.iter().find(|(_, entry)| entry.file == file).unwrap();
            (attachment.clone(), entry.media_record_name.clone())
        })
        .collect()
}

/// The link pull writes for `name` (as `icloud-notes attach` writes it).
fn link_of(name: &str) -> String {
    let image = [".png", ".gif", ".jpeg"].iter().any(|e| name.ends_with(e));
    format!("{}[{name}](attachments/{name})", if image { "!" } else { "" })
}

fn edit_note(vault: &Path, edit: impl FnOnce(String) -> String) {
    let path = vault.join(NOTE_FILE);
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = edit(text.clone());
    assert_ne!(edited, text, "the edit changes the note");
    std::fs::write(path, edited).unwrap();
}

/// What the removal of one record sends: `Deleted` set, nothing else.
fn marked_deleted(record_type: &str, name: &str, tag: &str) -> Value {
    json!({
        "operationType": "update",
        "record": {
            "recordName": name,
            "recordType": record_type,
            "recordChangeTag": tag,
            "fields": { "Deleted": { "value": 1 } },
            "parent": { "recordName": NOTE },
        },
    })
}

fn deleted_field(server: &Server, name: &str) -> Value {
    server.record(name).unwrap()["fields"]["Deleted"]["value"].clone()
}

fn exists(vault: &Path, file: &str) -> bool {
    vault.join(file).exists()
}

#[test]
fn a_link_gone_from_the_note_removes_the_attachment_in_one_atomic_request() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let image = png(4, 3);
    let ids = attached(&server, &vault, &[("pic.png", &image)]);
    let (attachment, media) = (&ids[0].0, &ids[0].1);
    let note_tag = state(&vault).notes[NOTE].record_change_tag.clone();
    let attachment_tag = server.record(attachment).unwrap()["recordChangeTag"].clone();
    let media_tag = server.record(media).unwrap()["recordChangeTag"].clone();
    let (text_before, _) = server.note_text(NOTE);
    assert!(
        text_before.ends_with(&format!("\nFiles:\n{O}\nThe end.\n")),
        "{text_before:?}"
    );

    edit_note(&vault, |text| text.replace("![pic.png](attachments/pic.png)\n", ""));
    let note_text = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
    let state_before = std::fs::read(vault.join(".icloud-notes/state.json")).unwrap();
    let modifies_before = server.posts_to("/records/modify").len();

    // A preview plans it and says so; it changes nothing anywhere.
    let planned = preview(&server, &vault);
    assert_eq!(planned.entries.len(), 1);
    let entry = &planned.entries[0].entry;
    assert_eq!(
        (entry.kind, entry.resolution),
        (PlanEntryKind::Update, PlanResolution::Ready)
    );
    assert_eq!(entry.remark.as_deref(), Some("removes the attachment pic.png"));
    assert_eq!(server.posts_to("/records/modify").len(), modifies_before);
    assert_eq!(
        std::fs::read(vault.join(".icloud-notes/state.json")).unwrap(),
        state_before
    );
    assert!(exists(&vault, "Notes/attachments/pic.png"));

    let lookups_before = server.posts_to("/records/lookup").len();
    let result = push(&server, &vault);
    assert_eq!(result.pushed, Some(1));
    let outcome = result.entries[0].outcome.as_ref().unwrap();
    assert!(outcome.succeeded, "{outcome:?}");
    assert_eq!(
        outcome.message,
        "Pushed Notes/Test Note.md, without its attachment pic.png"
    );

    // Looked up: the note, then the attachment's two records in one lookup.
    let lookups = server.posts_to("/records/lookup");
    assert_eq!(lookups.len(), lookups_before + 2);
    assert_eq!(
        lookups[lookups_before + 1]["records"],
        json!([{ "recordName": attachment }, { "recordName": media }])
    );

    // The request: atomic; the note's text without the placeholder, then
    // the Attachment and the Media record marked deleted, each at its tag.
    let modify = server.posts_to("/records/modify");
    assert_eq!(modify.len(), modifies_before + 1, "one request carries everything");
    let request = modify.last().unwrap();
    assert_eq!(request["atomic"], true);
    assert_eq!(request["zoneID"], json!({ "zoneName": "Notes" }));
    let operations = request["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 3);
    assert_eq!(operations[0]["operationType"], "update");
    assert_eq!(operations[0]["record"]["recordName"], NOTE);
    assert_eq!(operations[0]["record"]["recordType"], "Note");
    assert_eq!(operations[0]["record"]["recordChangeTag"], json!(note_tag));
    assert_eq!(
        operations[1],
        marked_deleted("Attachment", attachment, attachment_tag.as_str().unwrap())
    );
    assert_eq!(
        operations[2],
        marked_deleted("Media", media, media_tag.as_str().unwrap())
    );

    // In iCloud: no placeholder and no embed in the text; both records
    // deleted, and otherwise as they were.
    let (text, slots) = server.note_text(NOTE);
    assert!(text.ends_with("\nFiles:\nThe end.\n"), "{text:?}");
    assert!(!text.contains(O));
    assert_eq!(slots, []);
    assert_eq!(deleted_field(&server, attachment), 1);
    assert_eq!(deleted_field(&server, media), 1);
    assert_eq!(
        server.record(attachment).unwrap()["fields"]["UTI"]["value"],
        "public.png"
    );
    assert!(server.record(media).unwrap()["fields"]["Asset"]["value"]["receipt"].is_string());

    // Here: untracked, its file gone, the note's file as the user left it.
    let after = state(&vault);
    assert!(after.attachments.unwrap_or_default().is_empty());
    assert_ne!(after.notes[NOTE].record_change_tag, note_tag);
    assert!(!exists(&vault, "Notes/attachments/pic.png"));
    assert_eq!(std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap(), note_text);
    let base = read_base_copy(&vault, NOTE).unwrap().unwrap();
    assert!(base.ends_with("\nFiles:\nThe end.\n"), "{base:?}");

    // Nothing is left to push, and nothing more is asked of iCloud.
    let requests = server.posts.lock().unwrap().len();
    let clean = preview(&server, &vault);
    assert!(clean.entries.is_empty(), "{clean:?}");
    assert_eq!(server.posts.lock().unwrap().len(), requests);

    // A pull of the same change (the note at the tag just written, the
    // Attachment and Media records now `Deleted: 1`) does nothing: no file
    // comes back, nothing is tracked again, nothing is reported.
    let notes_after_push = state(&vault).notes;
    let summary = run_pull_with(
        &serve(&server),
        &vault,
        &mut NoProgress,
        &mut |_| {}
    )
    .unwrap();
    assert_eq!(
        (summary.added, summary.updated, summary.merged, summary.removed),
        (0, 0, 0, 0)
    );
    assert_eq!(summary.attachments_downloaded, 0);
    assert!(summary.changes.is_empty(), "{:?}", summary.changes);
    assert!(summary.conflicts.is_empty() && summary.notices.is_empty());
    assert_eq!(std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap(), note_text);
    assert!(!exists(&vault, "Notes/attachments/pic.png"));
    let pulled = state(&vault);
    assert!(pulled.attachments.clone().unwrap_or_default().is_empty());
    assert_eq!(pulled.sync_token.as_deref(), Some("after-the-push"));
    assert_eq!(pulled.notes, notes_after_push);
    assert!(preview(&server, &vault).entries.is_empty());
}

/// A pull that has to apply the note again (as another device's pull does:
/// it sees the new text and the records marked deleted) writes it without
/// the attachment, and drops the file it had.
#[test]
fn a_device_that_pulls_the_removal_loses_the_link_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    attached(&server, &vault, &[("plan.pdf", b"%PDF-1.7")]);
    // The other device: this vault as it was before the removal.
    let other = dir.path().join("other");
    copy_dir(&vault, &other);

    edit_note(&vault, |text| text.replace("[plan.pdf](attachments/plan.pdf)\n", ""));
    assert_eq!(push(&server, &vault).pushed, Some(1));

    let summary = run_pull_with(
        &serve(&server),
        &other,
        &mut NoProgress,
        &mut |_| {}
    )
    .unwrap();
    assert_eq!(summary.updated, 1, "{:?}", summary.changes);
    assert!(summary.notices.is_empty(), "{:?}", summary.notices);
    let text = std::fs::read_to_string(other.join(NOTE_FILE)).unwrap();
    assert!(!text.contains("plan.pdf"), "{text}");
    assert_eq!(text, std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap());
    assert!(!exists(&other, "Notes/attachments/plan.pdf"));
    assert!(state(&other).attachments.unwrap_or_default().is_empty());
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// One push: two of three attachments removed, another file added, and the
/// text around them edited. The attachment that stays keeps its character.
#[test]
fn removals_go_up_with_edits_and_additions_and_leave_the_others_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let ids = attached(
        &server,
        &vault,
        &[
            ("one.txt", b"first"),
            ("two.pdf", b"%PDF second"),
            ("three.png", &png(2, 2)),
        ],
    );
    let (_, slots_before) = server.note_text(NOTE);
    assert_eq!(slots_before.len(), 3);
    let uploads_before = server.uploads.lock().unwrap().len();

    attach(&vault, "four.txt", b"fourth");
    edit_note(&vault, |text| {
        text.replace("[one.txt](attachments/one.txt)\n", "")
            .replace(
                "![three.png](attachments/three.png)\n",
                "[four.txt](attachments/four.txt)\n",
            )
            .replace("Files:", "Files, edited:")
            .replace("The end.", "The very end.")
    });

    let planned = preview(&server, &vault);
    assert_eq!(
        planned.entries[0].entry.remark.as_deref(),
        Some("adds the attachment four.txt; removes the attachments one.txt, three.png")
    );
    let result = push(&server, &vault);
    assert_eq!(
        result.entries[0].outcome.as_ref().unwrap().message,
        "Pushed Notes/Test Note.md, with its new attachment, without its attachments one.txt, three.png"
    );

    let modify = server.posts_to("/records/modify");
    let request = modify.last().unwrap();
    assert_eq!(request["atomic"], true);
    let operations = request["operations"].as_array().unwrap();
    let sent: Vec<(&str, &str, &str)> = operations
        .iter()
        .map(|op| {
            (
                op["operationType"].as_str().unwrap(),
                op["record"]["recordType"].as_str().unwrap(),
                op["record"]["recordName"].as_str().unwrap(),
            )
        })
        .collect();
    let new_media = sent[5].2.to_owned();
    let new_attachment = sent[6].2.to_owned();
    assert_eq!(
        sent,
        [
            ("update", "Note", NOTE),
            ("update", "Attachment", ids[0].0.as_str()),
            ("update", "Media", ids[0].1.as_str()),
            ("update", "Attachment", ids[2].0.as_str()),
            ("update", "Media", ids[2].1.as_str()),
            ("create", "Media", new_media.as_str()),
            ("create", "Attachment", new_attachment.as_str()),
        ]
    );
    for op in &operations[1..5] {
        assert_eq!(op["record"]["fields"], json!({ "Deleted": { "value": 1 } }));
    }
    assert_eq!(server.uploads.lock().unwrap().len(), uploads_before + 1);

    let (text, slots) = server.note_text(NOTE);
    assert!(
        text.ends_with(&format!("\nFiles, edited:\n{O}\n{O}\nThe very end.\n")),
        "{text:?}"
    );
    assert_eq!(
        slots,
        [slots_before[1].clone(), slot(&new_attachment, "public.plain-text")]
    );

    let tracked = state(&vault).attachments.unwrap();
    let mut files: Vec<&str> = tracked.values().map(|a| a.file.as_str()).collect();
    files.sort();
    assert_eq!(files, ["Notes/attachments/four.txt", "Notes/attachments/two.pdf"]);
    assert!(tracked.contains_key(&ids[1].0) && tracked.contains_key(&new_attachment));
    assert!(!exists(&vault, "Notes/attachments/one.txt"));
    assert!(!exists(&vault, "Notes/attachments/three.png"));
    assert!(exists(&vault, "Notes/attachments/two.pdf"));
    assert!(exists(&vault, "Notes/attachments/four.txt"));
    for (attachment, media) in [&ids[0], &ids[2]] {
        assert_eq!(deleted_field(&server, attachment), 1);
        assert_eq!(deleted_field(&server, media), 1);
    }
    assert_eq!(deleted_field(&server, &ids[1].0), 0);
    assert_eq!(deleted_field(&server, &ids[1].1), 0);
    assert!(preview(&server, &vault).entries.is_empty());

    // The last attachments removed too, with nothing else changed.
    edit_note(&vault, |text| {
        text.replace("[two.pdf](attachments/two.pdf)\n", "")
            .replace("[four.txt](attachments/four.txt)\n", "")
    });
    assert_eq!(
        preview(&server, &vault).entries[0].entry.remark.as_deref(),
        Some("removes the attachments two.pdf, four.txt")
    );
    assert_eq!(push(&server, &vault).pushed, Some(1));
    let (text, slots) = server.note_text(NOTE);
    assert!(text.ends_with("\nFiles, edited:\nThe very end.\n"), "{text:?}");
    assert_eq!(slots, []);
    assert!(state(&vault).attachments.unwrap_or_default().is_empty());
    assert!(
        std::fs::read_dir(vault.join("Notes/attachments"))
            .unwrap()
            .next()
            .is_none()
    );
}

/// A link to the attachment's file that is still in the note, changed or
/// somewhere else, is no removal: refused, with nothing sent or deleted.
#[test]
fn a_link_changed_or_moved_is_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    attached(&server, &vault, &[("one.txt", b"first"), ("two.pdf", b"%PDF second")]);
    let original = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
    let modifies = server.posts_to("/records/modify").len();
    let (one, two) = ("[one.txt](attachments/one.txt)\n", "[two.pdf](attachments/two.pdf)\n");
    let edits: [&dyn Fn(String) -> String; 3] = [
        // Another link text.
        &|text| text.replace(one, "[the first file](attachments/one.txt)\n"),
        // Moved after the other one.
        &|text| text.replace(&format!("{one}{two}"), &format!("{two}{one}")),
        // One removed for real, the other one's link changed.
        &|text| text.replace(one, "").replace(two, "[second](attachments/two.pdf)\n"),
    ];
    for edit in edits {
        std::fs::write(vault.join(NOTE_FILE), edit(original.clone())).unwrap();
        let result = push(&server, &vault);
        let reason = refusal(&result).to_owned();
        assert!(reason.contains("was changed or moved"), "{reason}");
        assert!(reason.contains("put the link back as it was"), "{reason}");
        assert_eq!(server.posts_to("/records/modify").len(), modifies);
        assert_eq!(state(&vault).attachments.unwrap().len(), 2);
        assert!(exists(&vault, "Notes/attachments/one.txt") && exists(&vault, "Notes/attachments/two.pdf"));
    }
}

/// A note in someone else's shared folder: its attachments live in the
/// owner's zone, and are not removed from here.
#[test]
fn a_shared_note_loses_no_attachment_from_here() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let ids = attached(&server, &vault, &[("plan.pdf", b"%PDF-1.7")]);
    let modifies = server.posts_to("/records/modify").len();
    let mut shared = state(&vault);
    let mut folder = shared.folders.as_ref().unwrap()["DefaultFolder-CloudKit"].clone();
    folder.shared_zone_owner = Some("_sharer".into());
    folder.permission = Some("READ_WRITE".into());
    shared.folders.as_mut().unwrap().insert("F-SHARED".into(), folder);
    let note = shared.notes.get_mut(NOTE).unwrap();
    note.shared_zone_owner = Some("_sharer".into());
    note.folder_record_name = Some("F-SHARED".into());
    write_clone_state(&vault, &shared).unwrap();
    edit_note(&vault, |text| text.replace("[plan.pdf](attachments/plan.pdf)\n", ""));

    let reason = refusal(&push(&server, &vault)).to_owned();
    assert!(
        reason.starts_with(
            "attachments can't be removed from a shared note from here - remove them in Notes itself. Run \
             \"icloud-notes restore Notes/Test Note.md\""
        ),
        "{reason}"
    );
    assert_eq!(server.posts_to("/records/modify").len(), modifies);
    assert_eq!(deleted_field(&server, &ids[0].0), 0);
    assert!(state(&vault).attachments.unwrap().contains_key(&ids[0].0));
    assert!(exists(&vault, "Notes/attachments/plan.pdf"));
}

/// `attachments/` is one directory for all the notes of a folder. The
/// removed attachment's file stays when another note of the folder links
/// to it, or when it is tracked for another attachment.
#[test]
fn a_file_another_note_still_uses_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let ids = attached(
        &server,
        &vault,
        &[("linked.txt", b"one"), ("tracked.txt", b"two"), ("mine.txt", b"three")],
    );
    // Another note of the folder links to the first file; the second is
    // tracked as another note's attachment as well.
    std::fs::write(
        vault.join("Notes/Other.md"),
        "Other\n\n[see it](attachments/linked.txt)\n",
    )
    .unwrap();
    let mut tracked = state(&vault);
    tracked.attachments.as_mut().unwrap().insert(
        "ATT-OTHER".into(),
        AttachmentEntry {
            file: "Notes/attachments/tracked.txt".into(),
            media_record_name: "MEDIA-OTHER".into(),
            media_file_checksum: "x".into(),
            note_record_name: "another-note".into(),
        },
    );
    write_clone_state(&vault, &tracked).unwrap();
    edit_note(&vault, |text| {
        text.replace("[linked.txt](attachments/linked.txt)\n", "")
            .replace("[tracked.txt](attachments/tracked.txt)\n", "")
            .replace("[mine.txt](attachments/mine.txt)\n", "")
    });

    let result = push(&server, &vault);
    let outcome = result
        .entries
        .iter()
        .find(|e| e.entry.file == NOTE_FILE)
        .and_then(|e| e.outcome.as_ref())
        .unwrap();
    assert!(outcome.succeeded, "{outcome:?}");
    for (attachment, media) in &ids {
        assert_eq!(deleted_field(&server, attachment), 1);
        assert_eq!(deleted_field(&server, media), 1);
    }
    let after = state(&vault).attachments.unwrap();
    assert_eq!(after.keys().collect::<Vec<_>>(), ["ATT-OTHER"]);
    assert!(exists(&vault, "Notes/attachments/linked.txt"), "linked from Other.md");
    assert!(exists(&vault, "Notes/attachments/tracked.txt"), "another attachment's");
    assert!(!exists(&vault, "Notes/attachments/mine.txt"));
}

/// iCloud refusing any record of the request, or the request failing,
/// leaves everything as it was: the next push tries again.
#[test]
fn a_removal_that_fails_leaves_state_and_file_alone() {
    for failure in ["rejected", "http"] {
        let dir = tempfile::tempdir().unwrap();
        let vault = cloned_vault(dir.path());
        let good = Arc::new(Server::with_note());
        let ids = attached(&good, &vault, &[("plan.pdf", b"%PDF-1.7")]);
        let server = Arc::new(Server {
            records: Mutex::new(good.records.lock().unwrap().clone()),
            reject: (failure == "rejected").then_some(("Attachment", "CONFLICT")),
            fail_modify: (failure == "http").then_some(503),
            ..Default::default()
        });
        edit_note(&vault, |text| text.replace("[plan.pdf](attachments/plan.pdf)\n", ""));
        let note_text = std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap();
        let base = read_base_copy(&vault, NOTE).unwrap();
        let before = state(&vault);

        let pushed = run_push_with(&serve(&server), &vault, &mut |_| {}, &PushOptions::default());
        match failure {
            "rejected" => {
                let result = pushed.unwrap();
                assert_eq!(result.pushed, Some(0));
                let outcome = result.entries[0].outcome.as_ref().unwrap();
                assert!(!outcome.succeeded);
                assert_eq!(
                    outcome.message,
                    "Notes/Test Note.md: rejected by the server as a conflicting change (refused by the test \
                     server) - run \"pull\" first"
                );
            }
            _ => assert!(pushed.is_err()),
        }
        assert_eq!(server.posts_to("/records/modify").len(), 1);
        let after = state(&vault);
        assert_eq!(after.attachments, before.attachments);
        assert_eq!(after.notes, before.notes);
        assert_eq!(server.note_text(NOTE).1.len(), 1, "the note keeps its attachment");
        assert_eq!(deleted_field(&server, &ids[0].0), 0);
        assert_eq!(deleted_field(&server, &ids[0].1), 0);
        assert_eq!(
            std::fs::read(vault.join("Notes/attachments/plan.pdf")).unwrap(),
            b"%PDF-1.7"
        );
        assert_eq!(std::fs::read_to_string(vault.join(NOTE_FILE)).unwrap(), note_text);
        assert_eq!(read_base_copy(&vault, NOTE).unwrap(), base);
        let again = preview(&good, &vault);
        assert_eq!(again.entries[0].entry.resolution, PlanResolution::Ready);
        assert_eq!(
            again.entries[0].entry.remark.as_deref(),
            Some("removes the attachment plan.pdf")
        );
    }
}

/// Records another device deleted already (or that are gone) get no
/// operation: the note's text still loses the placeholder.
#[test]
fn records_that_are_deleted_already_are_not_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let ids = attached(&server, &vault, &[("one.txt", b"first"), ("two.txt", b"second")]);
    {
        let mut records = server.records.lock().unwrap();
        // The first one's Attachment record is marked deleted, its Media
        // record is gone; the second one's are as they were.
        records.retain(|r| r["recordName"] != ids[0].1.as_str());
        let attachment = records
            .iter_mut()
            .find(|r| r["recordName"] == ids[0].0.as_str())
            .unwrap();
        attachment["fields"]["Deleted"]["value"] = json!(1);
    }
    edit_note(&vault, |text| {
        text.replace("[one.txt](attachments/one.txt)\n", "")
            .replace("[two.txt](attachments/two.txt)\n", "")
    });
    assert_eq!(push(&server, &vault).pushed, Some(1));

    let modify = server.posts_to("/records/modify");
    let request = modify.last().unwrap();
    assert_eq!(request["atomic"], true);
    let names: Vec<&str> = request["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["record"]["recordName"].as_str().unwrap())
        .collect();
    assert_eq!(names, [NOTE, ids[1].0.as_str(), ids[1].1.as_str()]);
    assert_eq!(server.note_text(NOTE).1, []);
    assert!(state(&vault).attachments.unwrap_or_default().is_empty());
    assert!(!exists(&vault, "Notes/attachments/one.txt") && !exists(&vault, "Notes/attachments/two.txt"));
}

/// The record the vault tracks as the attachment has to be this note's
/// file attachment in iCloud: anything else is not marked deleted.
#[test]
fn a_record_that_is_not_this_notes_attachment_is_not_touched() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let ids = attached(&server, &vault, &[("one.txt", b"first")]);
    let modifies = server.posts_to("/records/modify").len();
    {
        let mut records = server.records.lock().unwrap();
        let attachment = records
            .iter_mut()
            .find(|r| r["recordName"] == ids[0].0.as_str())
            .unwrap();
        attachment["fields"]["Note"]["value"]["recordName"] = json!("another-note");
    }
    edit_note(&vault, |text| text.replace("[one.txt](attachments/one.txt)\n", ""));
    let reason = refusal(&push(&server, &vault)).to_owned();
    assert!(
        reason.starts_with("the attachment \"one.txt\" can't be removed from here: its record in iCloud is not"),
        "{reason}"
    );
    assert_eq!(server.posts_to("/records/modify").len(), modifies);
    assert!(exists(&vault, "Notes/attachments/one.txt"));
    assert_eq!(state(&vault).attachments.unwrap().len(), 1);
}

/// A note renamed and edited in one go: the removal goes up with the edit
/// that follows the move, as an addition does.
#[test]
fn a_renamed_notes_removal_goes_up_with_its_edit() {
    let dir = tempfile::tempdir().unwrap();
    let vault = cloned_vault(dir.path());
    let server = Arc::new(Server::with_note());
    let ids = attached(&server, &vault, &[("one.txt", b"first")]);
    edit_note(&vault, |text| text.replace("[one.txt](attachments/one.txt)\n", ""));
    std::fs::rename(vault.join(NOTE_FILE), vault.join("Notes/Renamed.md")).unwrap();

    let result = push(&server, &vault);
    let outcome = result.entries[0].outcome.as_ref().unwrap();
    assert!(outcome.succeeded, "{outcome:?}");
    assert!(outcome.message.ends_with(", with its edits"), "{}", outcome.message);
    let modify = server.posts_to("/records/modify");
    let request = modify.last().unwrap();
    assert_eq!(request["atomic"], true);
    assert_eq!(request["operations"].as_array().unwrap().len(), 3);
    assert_eq!(server.note_text(NOTE).1, []);
    assert_eq!(deleted_field(&server, &ids[0].0), 1);
    assert_eq!(deleted_field(&server, &ids[0].1), 1);
    assert!(state(&vault).attachments.unwrap_or_default().is_empty());
    assert!(!exists(&vault, "Notes/attachments/one.txt"));
    assert!(preview(&server, &vault).entries.is_empty());
}
