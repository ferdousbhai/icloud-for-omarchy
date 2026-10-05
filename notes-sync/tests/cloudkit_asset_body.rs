//! Notes too large to store their text inline (`TextDataAsset`): the text is
//! downloaded, on the private path and the shared-zone path (after the
//! `records/lookup` backfill). Originally derived from the tests of
//! icloud-md PR #29 ("Fetch the text of notes too large to store it inline").

mod cloudkit_common;

use std::cell::RefCell;
use std::rc::Rc;

use cloudkit_common::{MockTransport, no_pages};
use icloud_notes_sync::cloudkit::client::TEXT_DATA_ASSET_KEY;
use icloud_notes_sync::cloudkit::{CkError, Database};
use icloud_notes_sync::doc::decode::{
    ClassifyOptions, NoteDecodeResult, TEXT_DATA_ASSET_UNPUBLISHABLE_REASON, classify_note_record,
};
use icloud_notes_sync::doc::document::{build_initial_note_document, encode_note_document};
use icloud_notes_sync::doc::text::compress_note_document;
use indexmap::IndexMap;
use serde_json::{Value, json};

const ASSET_URL: &str = "https://cvws.icloud-content.example/B/asset-1";

/// The compressed NoteStoreProto document a `TextDataAsset` download returns -
/// the same bytes `TextDataEncrypted` would carry inline.
fn note_document_bytes(text: &str) -> Vec<u8> {
    let doc = build_initial_note_document(text, &[7; 16]).expect("builds");
    compress_note_document(&encode_note_document(&doc).expect("encodes"))
}

/// A `changes/zone` listing record for a note too large to keep its text
/// inline: no `TextDataEncrypted`, a `TextDataAsset` instead (the shape of a
/// real ~830 KB note, 2026-09-26).
fn asset_body_record() -> Value {
    json!({
        "recordName": "big-note",
        "recordType": "Note",
        "recordChangeTag": "tag-big",
        "fields": {
            "TitleEncrypted": { "value": icloud_notes_sync::js::base64_encode(b"Today"), "type": "ENCRYPTED_BYTES" },
            "TextDataAsset": {
                "value": { "fileChecksum": "sum", "size": 123, "wrappingKey": "key", "downloadURL": ASSET_URL },
                "type": "ASSETID",
            },
        },
    })
}

type Seen = Rc<RefCell<Vec<Value>>>;

/// The private zone lists one asset-backed note; asset GETs answered by
/// `asset`. Returns the database and the desiredKeys each `changes/zone` sent.
fn install(mut asset: impl FnMut(&str) -> Result<Vec<u8>, CkError> + 'static) -> (Database<MockTransport>, Seen) {
    let desired = Rc::new(RefCell::new(Vec::new()));
    let seen = desired.clone();
    let transport = MockTransport::new(move |path, body| {
        if path.contains("/private/changes/zone") {
            seen.borrow_mut().push(body["zones"][0]["desiredKeys"].clone());
            return Ok(json!({
                "zones": [{ "zoneID": { "zoneName": "Notes" }, "syncToken": "token-new", "moreComing": false, "records": [asset_body_record()] }],
            }));
        }
        panic!("unexpected request in test: {path}")
    })
    .with_assets(move |url| {
        assert_eq!(url, ASSET_URL);
        asset(url)
    });
    (Database::new(transport), desired)
}

#[test]
fn fetch_all_note_records_asks_for_text_data_asset_and_inlines_a_large_notes_text_from_it() {
    let bytes = note_document_bytes("Today\n~~~\n\n9am");
    let served = bytes.clone();
    let (database, desired) = install(move |_| Ok(served.clone()));
    let result = database.fetch_all_note_records(None, &mut no_pages()).unwrap();

    let keys = desired.borrow()[0].clone();
    assert!(keys.as_array().unwrap().iter().any(|k| k == TEXT_DATA_ASSET_KEY));
    assert_eq!(keys.as_array().unwrap().last(), Some(&json!("TextDataAsset")));
    assert_eq!(database.transport.downloads.borrow().len(), 1);
    assert_eq!(result.sync_token.as_deref(), Some("token-new"));
    let record = &result.records[0];
    assert_eq!(
        record.fields["TextDataEncrypted"].value,
        json!(icloud_notes_sync::js::base64_encode(bytes.as_ref()))
    );
    assert_eq!(record.fields["TextDataEncrypted"].type_, "ENCRYPTED_BYTES");
    let NoteDecodeResult::Ok(decoded) = classify_note_record(record, &ClassifyOptions::default()) else {
        panic!("expected ok");
    };
    assert_eq!(decoded.title_line, "Today");
    // Readable, but never pushed: writing it back would mean uploading an asset.
    assert!(!decoded.publishable);
    assert_eq!(
        decoded.unpublishable_reason.as_deref(),
        Some(TEXT_DATA_ASSET_UNPUBLISHABLE_REASON)
    );
    assert!(TEXT_DATA_ASSET_UNPUBLISHABLE_REASON.contains("separate file"));
}

#[test]
fn fetch_all_note_records_fails_rather_than_passing_a_large_note_on_bodyless_when_its_download_fails() {
    // Body-less, the note would be skipped as unsyncable while the syncToken
    // moved past it - a clean-looking sync that silently lost the note.
    let (database, _) = install(|_| {
        Err(CkError::Http {
            status: 410,
            body: "Gone".into(),
        })
    });
    let err = database.fetch_all_note_records(None, &mut no_pages()).unwrap_err();
    assert!(err.to_string().contains("HTTP 410"), "{err}");
    assert_eq!(err.to_string(), "Attachment download failed: HTTP 410");
}

#[test]
fn a_note_that_already_has_inline_text_is_not_downloaded_again() {
    let mut record = asset_body_record();
    record["fields"]["TextDataEncrypted"] = json!({ "value": "Ym9keQ==", "type": "ENCRYPTED_BYTES" });
    let transport = MockTransport::new(move |_, _| {
        Ok(json!({
            "zones": [{ "zoneID": { "zoneName": "Notes" }, "syncToken": "t", "moreComing": false, "records": [record.clone()] }],
        }))
    });
    let database = Database::new(transport);
    let result = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    assert_eq!(result.records[0].fields["TextDataEncrypted"].value, json!("Ym9keQ=="));
    assert!(database.transport.downloads.borrow().is_empty());
}

#[test]
fn a_shared_notes_asset_body_is_inlined_after_the_lookup_backfill() {
    let bytes = note_document_bytes("Shared big\nline");
    let served = bytes.clone();
    let transport = MockTransport::new(move |path, _| {
        if path.contains("/shared/changes/database") {
            return Ok(json!({
                "zones": [{ "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA", "zoneType": "REGULAR_CUSTOM_ZONE" } }],
            }));
        }
        if path.contains("/shared/changes/zone") {
            return Ok(json!({
                "zones": [{
                    "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" },
                    "syncToken": "token-a",
                    "moreComing": false,
                    "records": [{ "recordName": "big-note", "recordType": "Note", "recordChangeTag": "tag-big", "fields": {} }],
                }],
            }));
        }
        if path.contains("/shared/records/lookup") {
            return Ok(json!({ "records": [asset_body_record()] }));
        }
        panic!("unexpected request in test: {path}")
    })
    .with_assets(move |_| Ok(served.clone()));
    let database = Database::new(transport);
    let result = database
        .fetch_shared_note_records(&IndexMap::new(), &mut no_pages())
        .unwrap();
    assert!(result.skipped_zones.is_empty());
    assert_eq!(result.zones.len(), 1);
    let record = &result.zones[0].records[0];
    assert_eq!(
        record.fields["TextDataEncrypted"].value,
        json!(icloud_notes_sync::js::base64_encode(bytes.as_ref()))
    );
    assert_eq!(database.transport.downloads.borrow().as_slice(), [ASSET_URL]);
}

#[test]
fn a_record_still_holding_its_asset_but_no_inline_text_stays_missing_body() {
    // What push sees: it re-reads the record with records/lookup, which never
    // inlines, so an asset-backed note stays body-less there and is refused.
    let record: icloud_notes_sync::CloudKitRecord =
        icloud_notes_sync::cloudkit::client::parse_record(&asset_body_record()).unwrap();
    assert_eq!(
        classify_note_record(&record, &ClassifyOptions::default()),
        NoteDecodeResult::Unsyncable(icloud_notes_sync::doc::decode::UnsyncableReason::MissingBody)
    );
}
