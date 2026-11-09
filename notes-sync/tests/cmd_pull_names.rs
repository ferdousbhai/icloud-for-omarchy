//! `clone` and `pull` against records whose names, titles or zones would
//! put files outside the vault, abort the run, or overwrite another zone's
//! note: such a record is skipped with a notice, or named safely, and the
//! rest syncs.

use std::path::{Path, PathBuf};

use icloud_notes_sync::cloudkit::Database;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cmd::clone::{CloneOptions, CloneSummary, run_clone_with};
use icloud_notes_sync::cmd::pull::{PullOptions, PullSummary, run_pull_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress, NoticeLevel, SyncNotice};
use icloud_notes_sync::doc::document::{build_initial_note_document, encode_note_document};
use icloud_notes_sync::doc::proto::topotext::{self, AttachmentInfo, AttributeRun};
use icloud_notes_sync::doc::proto::{Message, versioned_document};
use icloud_notes_sync::doc::text::compress_note_document;
use icloud_notes_sync::js::base64_encode;
use icloud_notes_sync::vault::state::{Account, read_clone_state};
use serde_json::{Value, json};

/// The tiny clone's one note, in `Notes/Test Note.md`.
const OWN: &str = "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1";
const OWNER: &str = "_ownerA";

fn cassette() -> Cassette {
    Cassette::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential/cassettes/tiny-clone.json")).unwrap()
}

fn replay(cassette: Cassette) -> impl Connector {
    FnConnector(move || -> Result<Remote, Error> {
        let account = Account {
            apple_id: cassette.account.apple_id.clone(),
            dsid: cassette.account.dsid.clone(),
        };
        let transport = ReplayTransport::from_cassette(cassette.clone(), None)?;
        Ok(Remote {
            db: Database::new(Box::new(transport)),
            account,
        })
    })
}

fn text_data(text: &str) -> String {
    let doc = build_initial_note_document(text, &[7; 16]).unwrap();
    base64_encode(&compress_note_document(&encode_note_document(&doc).unwrap()))
}

fn private_records(c: &Cassette) -> Vec<Value> {
    c.interactions[0].response.body.as_ref().unwrap()["zones"][0]["records"]
        .as_array()
        .unwrap()
        .clone()
}

/// The tiny clone's note record, renamed, with `text` and change tag `tag`.
fn note(record_name: &str, text: &str, tag: &str) -> Value {
    let mut record = private_records(&cassette())
        .into_iter()
        .find(|r| r["recordName"] == OWN)
        .unwrap();
    record["recordName"] = json!(record_name);
    record["recordChangeTag"] = json!(tag);
    record["fields"]["TextDataEncrypted"]["value"] = json!(text_data(text));
    record
}

fn in_folder(mut record: Value, folder: &str) -> Value {
    record["fields"]["Folder"]["value"]["recordName"] = json!(folder);
    record["fields"]["Folders"]["value"][0]["recordName"] = json!(folder);
    record
}

fn folder(record_name: &str, title: &str) -> Value {
    let mut record = private_records(&cassette())
        .into_iter()
        .find(|r| r["recordName"] == "DefaultFolder-CloudKit")
        .unwrap();
    record["recordName"] = json!(record_name);
    record["fields"]["TitleEncrypted"]["value"] = json!(base64_encode(title.as_bytes()));
    record
}

/// The tiny cassette, its private zone listing `private` and, with `shared`,
/// one shared zone (owned by [`OWNER`]) listing those.
fn listing(private: Vec<Value>, shared: Option<Vec<Value>>) -> Cassette {
    listing_in(cassette(), private, shared)
}

/// [`listing`] over cassette `c`.
fn listing_in(mut c: Cassette, private: Vec<Value>, shared: Option<Vec<Value>>) -> Cassette {
    c.interactions[0].response.body.as_mut().unwrap()["zones"][0]["records"] = json!(private);
    if let Some(shared) = shared {
        let zone = json!({ "zoneName": "Notes", "ownerRecordName": OWNER });
        c.interactions[1].response.body = Some(json!({
            "zones": [{ "zoneID": zone }], "moreComing": false, "syncToken": "db-1",
        }));
        let mut walk = c.interactions[0].clone();
        walk.request.path = Some("/database/1/com.apple.notes/production/shared/changes/zone".into());
        walk.response.body = Some(json!({
            "zones": [{ "zoneID": zone, "moreComing": false, "syncToken": "shared-1", "records": shared }],
        }));
        c.interactions.push(walk);
    }
    c
}

/// A vault at `<tmp>/a/vault`, cloned from `cassette`: a record name
/// climbing out of `.icloud-notes/history/` lands in `<tmp>`.
fn clone_from(cassette: Cassette) -> (tempfile::TempDir, PathBuf, CloneSummary) {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("a/vault");
    let summary = run_clone_with(
        &replay(cassette),
        &vault,
        &mut NoProgress,
        &mut |_| {},
        &CloneOptions::default(),
    )
    .unwrap();
    (tmp, vault, summary)
}

fn pull(vault: &Path, cassette: Cassette) -> PullSummary {
    run_pull_with(
        &replay(cassette),
        vault,
        &mut NoProgress,
        &mut |_| {},
        &PullOptions::default(),
    )
    .unwrap()
}

fn read(vault: &Path, file: &str) -> String {
    std::fs::read_to_string(vault.join(file)).unwrap()
}

/// Every file under `dir`, relative to it, sorted.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Nothing was written outside the vault.
fn assert_contained(tmp: &Path) {
    let outside: Vec<String> = files_under(tmp)
        .into_iter()
        .filter(|f| !f.starts_with("a/vault/"))
        .collect();
    assert!(outside.is_empty(), "{outside:?}");
}

fn warned(notices: &[SyncNotice], needle: &str) -> bool {
    notices
        .iter()
        .any(|n| n.level == NoticeLevel::Warn && n.message.contains(needle))
}

const CLIMBING: &str = "../../../escape";

// --- record names that aren't one plain file name ------------------------------

#[test]
fn pull_skips_a_note_whose_record_name_is_a_path_and_pulls_the_rest() {
    let (tmp, vault, _) = clone_from(cassette());
    let summary = pull(
        &vault,
        listing(
            vec![
                note(CLIMBING, "Escape\n\nbody", "9a"),
                note(OWN, "Test Note\n\nEdited remotely.", "2b"),
            ],
            None,
        ),
    );
    assert_contained(tmp.path());
    assert!(warned(&summary.notices, "../../../escape"), "{:?}", summary.notices);
    assert_eq!((summary.added, summary.updated), (0, 1));
    assert!(read(&vault, "Notes/Test Note.md").contains("Edited remotely."));
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(!state.notes.contains_key(CLIMBING));
    assert_eq!(state.notes[OWN].record_change_tag, "2b");
}

#[test]
fn pull_skips_a_shared_note_whose_record_name_is_a_path() {
    let (tmp, vault, _) = clone_from(cassette());
    let summary = pull(
        &vault,
        listing(vec![], Some(vec![note(CLIMBING, "Escape\n\nbody", "9a")])),
    );
    assert_contained(tmp.path());
    assert!(warned(&summary.notices, "../../../escape"), "{:?}", summary.notices);
    assert_eq!(summary.added, 0);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(!state.notes.contains_key(CLIMBING));
}

#[test]
fn clone_skips_a_note_whose_record_name_is_a_path() {
    let mut records = private_records(&cassette());
    records.push(note(CLIMBING, "Escape\n\nbody", "9a"));
    let (tmp, vault, summary) = clone_from(listing(records, Some(vec![note("..", "Dots\n\nbody", "9b")])));
    assert_contained(tmp.path());
    assert!(warned(&summary.notices, "../../../escape"), "{:?}", summary.notices);
    assert_eq!((summary.written, summary.written_shared), (1, 0));
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes.keys().collect::<Vec<_>>(), [OWN]);
}

/// The table from vault_attachments.rs (`TABLE_B64`), as a Mergeable blob.
const TABLE_B64: &str = "eJzt1E9oFFccwPGZyWZ39iWNr5OahofQdgxJjHZdB5tDxUOTqKTEoJtNemghxHW0u2x2ZXdFI3opiBcPihRLyUULObVNm4stNFQwSslhD+k/Dy0UNNAiEimoB7H27SbVbLKhh1JP32GH3/x+7/d5895jd23D+fq2ZRvSUF/etoQtgvrZbDZUX+ptO6BabcNx3Vej/3KpoG06VtTUsUbHGh1rdbR1DOr4gqoXQgRKM+vMUq+nNtumarMtZ6P7WncsPnIg7Xdn00dHMz3JnJ8oJLOZPv9QIZ6NJQ+/X1A/mx+Y35tiwhQnRJ8TXPj2G/1RsjxhaeHl2G6WK6auWKoc2y3VJGw99kRf63Tf0+cOyzZLt/OS3p7cOhuev3dhr3F5z0yxMXq+S1dNR16fbL6/M198PD98LRG5dHXKaReOsKKlbQXU02Mq10K6FnxWW9ZZW6Uz9Kym6lPCtvQhBRxLmhWZVZHVVGSB//tE7vRfnPbnHtxtGZ+9NTbUsLB4Ilc2dY7sOntj/5mBybbQw9S2pX0Kvafwin3W61rdqhMpdYoqnfVrnEhtRRasyEIVma06vMV31On5Gla8I6xrLy57xz+9pa/muhW9eraoXNa7O9bjWG9F/+PZWioS2+IYVWZZw1Ss0amyxsaVa+x67mt07eRBP1NIFsbcpkSu2o/YDeT99CE3mMjFssfybnhwsLenN3PQP+6GE7nF3rxbl/DT6aWk4+VEdjQycuRI2o90x3rikf6B/qOjB/xclYGBQi6ZOdyxftVA6S3L+zPZgp+PLP3NrB7o7V5jAIFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAVApPnmycaYlt/u6jYzt+/OlU17vvePLED198dfOT7WO/zQ21npuO/uHJ65PN93fmi4/nh68lIpeuTnnSP9V2OvDhGxOfnpt69PFfG+Ke3Dobnr93Ya9xec9MsTF6vsuTEyd794nxz4Y7fy/KP9XufZ68sqlzZNfZG/vPDEy2hR6mtnnyvdZXZFPdo19v/mLEPo8PbvTknf6L0/7cg7st47O3xoYaFt7cIJRYtUrHsi19m38DuMWS1w==";
const TABLE_UTI: &str = "com.apple.notes.table";

/// A note body "Table\n" + one embedded table named `table_record_name`.
fn text_data_with_table(table_record_name: &str) -> String {
    text_data_with_embed(table_record_name, TABLE_UTI)
}

/// A note body "Table\n" + one embedded attachment `record_name` of type `type_uti`.
fn text_data_with_embed(record_name: &str, type_uti: &str) -> String {
    let runs = vec![
        AttributeRun::with_length(6),
        AttributeRun {
            length: Some(1),
            attachment_info: Some(AttachmentInfo {
                attachment_identifier: Some(record_name.into()),
                type_uti: Some(type_uti.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];
    let s = topotext::String {
        string: Some("Table\n\u{fffc}".into()),
        attribute_run: runs,
        ..Default::default()
    };
    let wrapper = versioned_document::Document {
        version: vec![versioned_document::Version {
            minimum_supported_version: Some(0),
            data: Some(s.encode().unwrap()),
            ..Default::default()
        }],
        ..Default::default()
    };
    base64_encode(&compress_note_document(&wrapper.encode().unwrap()))
}

#[test]
fn pull_leaves_a_table_whose_record_name_is_a_path_as_a_marker() {
    let (tmp, vault, _) = clone_from(cassette());
    let mut with_table = note("a1b2c3d4-0000-4000-8000-0000000000aa", "x", "4d");
    with_table["fields"]["TextDataEncrypted"]["value"] = json!(text_data_with_table(CLIMBING));
    let mut c = listing(vec![with_table], None);
    let mut lookup = c.interactions[0].clone();
    lookup.request.path = Some("/database/1/com.apple.notes/production/private/records/lookup".into());
    lookup.repeat = true;
    lookup.response.body = Some(json!({ "records": [{
        "recordName": CLIMBING, "recordType": "Attachment", "recordChangeTag": "t1",
        "fields": {
            "UTI": { "value": TABLE_UTI, "type": "STRING" },
            "MergeableDataEncrypted": { "value": TABLE_B64, "type": "ENCRYPTED_BYTES" },
        },
    }]}));
    c.interactions.push(lookup);
    let summary = pull(&vault, c);
    assert_contained(tmp.path());
    assert_eq!(summary.added, 1, "{summary:?}");
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(!state.table_attachments.unwrap_or_default().contains_key(CLIMBING));
    let file = &state.notes["a1b2c3d4-0000-4000-8000-0000000000aa"].file;
    assert!(read(&vault, file).contains("<apple-embed"), "{}", read(&vault, file));
}

// --- titles that can't be file names ------------------------------------------

#[test]
fn a_nul_in_a_note_title_or_folder_title_does_not_abort_the_pull() {
    let (tmp, vault, _) = clone_from(cassette());
    let summary = pull(
        &vault,
        listing(
            vec![
                folder("F-NUL", "Fol\0der\u{1}"),
                in_folder(
                    note("a1b2c3d4-0000-4000-8000-000000000001", "Bad\0Ti\u{7}tle\n\nbody", "3c"),
                    "F-NUL",
                ),
            ],
            None,
        ),
    );
    assert_contained(tmp.path());
    assert_eq!(summary.added, 1, "{summary:?}");
    assert!(vault.join("Folder/BadTitle.md").exists());
}

// --- one zone's record name in another zone ----------------------------------

#[test]
fn a_shared_note_named_like_an_own_note_leaves_the_own_note_alone() {
    let (tmp, vault, _) = clone_from(cassette());
    let before = read(&vault, "Notes/Test Note.md");
    let summary = pull(
        &vault,
        listing(vec![], Some(vec![note(OWN, "Hijacked\n\nby the sharer", "zz")])),
    );
    assert_contained(tmp.path());
    assert!(warned(&summary.notices, OWNER), "{:?}", summary.notices);
    assert_eq!(read(&vault, "Notes/Test Note.md"), before);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes[OWN].shared_zone_owner, None);
    assert_eq!(state.notes[OWN].file, "Notes/Test Note.md");
    assert_ne!(state.notes[OWN].record_change_tag, "zz");
}

#[test]
fn a_shared_deletion_named_like_an_own_note_deletes_nothing() {
    let (_tmp, vault, _) = clone_from(cassette());
    let deleted = json!({ "recordName": OWN, "recordType": "Note", "deleted": true });
    let summary = pull(&vault, listing(vec![], Some(vec![deleted])));
    assert!(vault.join("Notes/Test Note.md").exists(), "{summary:?}");
    assert_eq!(summary.removed, 0);
    assert!(read_clone_state(&vault).unwrap().unwrap().notes.contains_key(OWN));
}

#[test]
fn clone_keeps_the_own_note_when_a_shared_one_has_its_name() {
    let (_tmp, vault, summary) = clone_from(listing(
        private_records(&cassette()),
        Some(vec![note(OWN, "Hijacked\n\nby the sharer", "zz")]),
    ));
    assert!(warned(&summary.notices, OWNER), "{:?}", summary.notices);
    assert_eq!(summary.written_shared, 0);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes[OWN].shared_zone_owner, None);
    assert!(!read(&vault, "Notes/Test Note.md").contains("Hijacked"));
    let base = read(&vault, &format!(".icloud-notes/base/{OWN}.md"));
    assert!(!base.contains("Hijacked"), "{base}");
}

// --- one zone's attachment record name in another zone -------------------------

/// The attach cassette's image attachment, in note [`IMAGE_NOTE`].
const IMAGE: &str = "7ED80274-4400-4C02-87EA-F542F056FF02";
const IMAGE_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
const SHARED_NOTE: &str = "a1b2c3d4-0000-4000-8000-0000000000bb";

fn attach_cassette() -> Cassette {
    Cassette::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential/cassettes/attach-clone.json"))
        .unwrap()
}

/// A shared-zone `records/lookup` answering `records`, as often as asked.
fn shared_lookup(c: &mut Cassette, records: Value) {
    let mut lookup = c.interactions[0].clone();
    lookup.request.path = Some("/database/1/com.apple.notes/production/shared/records/lookup".into());
    lookup.repeat = true;
    lookup.response.body = Some(json!({ "records": records }));
    c.interactions.push(lookup);
}

/// The sharer's Attachment named [`IMAGE`]: a table, or a file of type
/// `uti` whose Media is the sharer's own.
fn sharers_attachment(uti: &str) -> Vec<Value> {
    if uti == TABLE_UTI {
        return vec![json!({
            "recordName": IMAGE, "recordType": "Attachment", "recordChangeTag": "s1",
            "fields": {
                "UTI": { "value": TABLE_UTI, "type": "STRING" },
                "MergeableDataEncrypted": { "value": TABLE_B64, "type": "ENCRYPTED_BYTES" },
            },
        })];
    }
    vec![
        json!({
            "recordName": IMAGE, "recordType": "Attachment", "recordChangeTag": "s1",
            "fields": {
                "UTI": { "value": uti, "type": "STRING" },
                "Media": { "value": { "recordName": "SHARER-MEDIA", "action": "VALIDATE" }, "type": "REFERENCE" },
            },
        }),
        json!({
            "recordName": "SHARER-MEDIA", "recordType": "Media", "recordChangeTag": "s2",
            "fields": { "Asset": { "value": {
                "fileChecksum": "sharers-checksum", "size": 7,
                "downloadURL": "https://cvws.icloud-content.com/B/sharers-asset",
            }, "type": "ASSETID" } },
        }),
    ]
}

/// The attach cassette, its private zone listing `private`, with a shared
/// zone listing `shared` whose lookups answer [`sharers_attachment`].
fn with_sharers_attachment(private: Vec<Value>, shared: Vec<Value>, uti: &str) -> Cassette {
    let mut c = listing_in(attach_cassette(), private, Some(shared));
    shared_lookup(&mut c, json!(sharers_attachment(uti)));
    let mut download = attach_cassette().interactions.pop().unwrap();
    download.request.url = Some("https://cvws.icloud-content.com/B/sharers-asset".into());
    download.response.body_base64 = Some(base64_encode(b"sharer!"));
    download.repeat = true;
    c.interactions.push(download);
    c
}

fn pull_with_sharers_attachment(vault: &Path, shared: Vec<Value>, uti: &str) -> PullSummary {
    pull(vault, with_sharers_attachment(vec![], shared, uti))
}

/// The sharer's note [`SHARED_NOTE`], embedding [`IMAGE`] as type `uti`.
fn sharers_note_embedding(uti: &str) -> Value {
    let mut shared = note(SHARED_NOTE, "x", "s0");
    shared["fields"]["TextDataEncrypted"]["value"] = json!(text_data_with_embed(IMAGE, uti));
    shared
}

#[test]
fn a_shared_attachment_named_like_an_own_one_leaves_the_own_file_alone() {
    let (tmp, vault, _) = clone_from(attach_cassette());
    let own = read_clone_state(&vault).unwrap().unwrap().attachments.unwrap()[IMAGE].clone();
    assert_eq!(own.note_record_name, IMAGE_NOTE);
    let own_bytes = std::fs::read(vault.join(&own.file)).unwrap();

    let summary = pull_with_sharers_attachment(&vault, vec![sharers_note_embedding("public.jpeg")], "public.jpeg");
    assert_contained(tmp.path());
    assert_eq!(summary.added, 1, "{summary:?}");
    assert_eq!(std::fs::read(vault.join(&own.file)).unwrap(), own_bytes);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.attachments.unwrap()[IMAGE], own);
    let shared_file = &state.notes[SHARED_NOTE].file;
    assert!(
        read(&vault, shared_file).contains("<apple-embed"),
        "{}",
        read(&vault, shared_file)
    );

    // The sharer deleting their note deletes nothing of the account's.
    let deleted = json!({ "recordName": SHARED_NOTE, "recordType": "Note", "deleted": true });
    let summary = pull_with_sharers_attachment(&vault, vec![deleted], "public.jpeg");
    assert_eq!(summary.removed, 1, "{summary:?}");
    assert_eq!(std::fs::read(vault.join(&own.file)).unwrap(), own_bytes);
    assert_eq!(
        read_clone_state(&vault).unwrap().unwrap().attachments.unwrap()[IMAGE],
        own
    );
}

#[test]
fn clone_keeps_the_own_attachment_when_a_shared_one_has_its_name() {
    let (_tmp, own_vault, _) = clone_from(attach_cassette());
    let own = read_clone_state(&own_vault).unwrap().unwrap().attachments.unwrap()[IMAGE].clone();
    let own_bytes = std::fs::read(own_vault.join(&own.file)).unwrap();

    let (_tmp, vault, summary) = clone_from(with_sharers_attachment(
        private_records(&attach_cassette()),
        vec![sharers_note_embedding("public.jpeg")],
        "public.jpeg",
    ));
    assert_eq!(summary.written_shared, 1, "{summary:?}");
    assert_eq!(std::fs::read(vault.join(&own.file)).unwrap(), own_bytes);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.attachments.unwrap()[IMAGE], own);
    assert!(read(&vault, &state.notes[SHARED_NOTE].file).contains("<apple-embed"));
}

#[test]
fn a_shared_table_named_like_an_own_attachment_leaves_it_alone() {
    let (_tmp, vault, _) = clone_from(attach_cassette());
    let own = read_clone_state(&vault).unwrap().unwrap().attachments.unwrap()[IMAGE].clone();
    let summary = pull_with_sharers_attachment(&vault, vec![sharers_note_embedding(TABLE_UTI)], TABLE_UTI);
    assert_eq!(summary.added, 1, "{summary:?}");
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(!state.table_attachments.unwrap_or_default().contains_key(IMAGE));
    assert!(!vault.join(format!(".icloud-notes/history/{IMAGE}")).exists());
    assert_eq!(state.attachments.unwrap()[IMAGE], own);
}

#[test]
fn a_shared_table_named_like_an_own_table_leaves_it_alone() {
    let (_tmp, vault, _) = clone_from(cassette());
    let own_note = "a1b2c3d4-0000-4000-8000-0000000000aa";
    let mut with_table = note(own_note, "x", "4d");
    with_table["fields"]["TextDataEncrypted"]["value"] = json!(text_data_with_embed(IMAGE, TABLE_UTI));
    let mut c = listing(vec![with_table], None);
    let mut lookup = c.interactions[0].clone();
    lookup.request.path = Some("/database/1/com.apple.notes/production/private/records/lookup".into());
    lookup.repeat = true;
    lookup.response.body = Some(json!({ "records": sharers_attachment(TABLE_UTI) }));
    c.interactions.push(lookup);
    pull(&vault, c);
    let history = vault.join(format!(".icloud-notes/history/{IMAGE}"));
    let snapshots = std::fs::read_dir(&history).unwrap().count();

    let mut table = sharers_attachment(TABLE_UTI);
    table[0]["recordChangeTag"] = json!("s9");
    let mut c = listing(vec![], Some(vec![sharers_note_embedding(TABLE_UTI)]));
    shared_lookup(&mut c, json!(table));
    let summary = pull(&vault, c);
    assert_eq!(summary.added, 1, "{summary:?}");
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.table_attachments.unwrap()[IMAGE].note_record_name, own_note);
    assert_eq!(std::fs::read_dir(&history).unwrap().count(), snapshots);
    let shared_file = &state.notes[SHARED_NOTE].file;
    assert!(
        read(&vault, shared_file).contains("<apple-embed"),
        "{}",
        read(&vault, shared_file)
    );
}
