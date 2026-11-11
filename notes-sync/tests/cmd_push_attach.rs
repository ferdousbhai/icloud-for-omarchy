//! Adding an attachment to a note from this computer: a link to a file in
//! the note folder's `attachments/` is uploaded and created with the note's
//! edit in one atomic request, or nothing is.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::client::parse_record;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::plan::{PlanEntryKind, PlanResolution};
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
            "/records/modify" => self.modify(body),
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
        &mut |_| {},
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

    // Removing the link of what is now an attachment is refused, as ever.
    let path = vault.join(NOTE_FILE);
    let without = std::fs::read_to_string(&path)
        .unwrap()
        .replace("![pic.png](attachments/pic.png)\n", "");
    std::fs::write(&path, without).unwrap();
    let reason = refusal(&preview(&server, &vault)).to_owned();
    assert!(reason.contains("was removed, changed or moved"), "{reason}");
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
        "\n[two.pdf](attachments/two.pdf)\n[three.gif](attachments/three.gif)\n",
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
