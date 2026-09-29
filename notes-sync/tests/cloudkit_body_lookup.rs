//! Port of icloud-md `src/cloudkit/databaseClient.bodyLookup.test.ts`.

mod cloudkit_common;

use std::cell::RefCell;
use std::rc::Rc;

use cloudkit_common::{db, no_pages};
use icloud_notes_sync::cloudkit::{CkError, Database, DatabaseScope, NoteZone, SkippedSharedZone, ZoneId, note_zone};
use indexmap::IndexMap;
use serde_json::{Value, json};

fn bodyless_listing_record(name: &str) -> Value {
    json!({
        "recordName": name,
        "recordType": "Note",
        "recordChangeTag": format!("tag-{name}"),
        "fields": { "TitleEncrypted": { "value": "dGl0bGU=", "type": "ENCRYPTED_BYTES" } },
    })
}

fn full_lookup_entry(name: &str) -> Value {
    json!({
        "recordName": name,
        "recordType": "Note",
        "recordChangeTag": format!("tag-{name}-lookup"),
        "fields": { "TextDataEncrypted": { "value": "Ym9keQ==", "type": "ENCRYPTED_BYTES" } },
    })
}

type LookupFn = Box<dyn Fn(&[String]) -> Vec<Value>>;
type Batches = Rc<RefCell<Vec<Vec<String>>>>;

/// One shared zone `_ownerA` listing two body-less notes; `records/lookup`
/// answered by `lookup`. Returns the database and the lookup batches seen.
fn install(lookup: LookupFn) -> (Database<cloudkit_common::MockTransport>, Batches) {
    let batches = Rc::new(RefCell::new(Vec::new()));
    let seen = batches.clone();
    let database = db(move |path, body| {
        if path.contains("/shared/changes/database") {
            return Ok(json!({
                "zones": [{ "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA", "zoneType": "REGULAR_CUSTOM_ZONE" } }],
            }));
        }
        if path.contains("/shared/changes/zone") {
            return Ok(json!({
                "zones": [{
                    "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" },
                    "syncToken": "token-a-new",
                    "moreComing": false,
                    "records": [bodyless_listing_record("note-1"), bodyless_listing_record("note-2")],
                }],
            }));
        }
        if path.contains("/shared/records/lookup") {
            let requested: Vec<String> = body["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["recordName"].as_str().unwrap().to_owned())
                .collect();
            seen.borrow_mut().push(requested.clone());
            return Ok(json!({ "records": lookup(&requested) }));
        }
        Err(CkError::Other(format!("Unexpected fetch in test: {path}")))
    });
    (database, batches)
}

fn owner_a() -> ZoneId {
    ZoneId {
        zone_name: "Notes".into(),
        owner_record_name: Some("_ownerA".into()),
    }
}

#[test]
fn fetch_shared_note_records_fills_bodies_via_lookup() {
    let (database, _) = install(Box::new(|names| names.iter().map(|n| full_lookup_entry(n)).collect()));
    let result = database
        .fetch_shared_note_records(&IndexMap::new(), &mut no_pages())
        .unwrap();
    assert_eq!(result.skipped_zones, vec![]);
    assert_eq!(result.zones.len(), 1);
    assert_eq!(result.zones[0].sync_token.as_deref(), Some("token-a-new"));
    for record in &result.zones[0].records {
        assert_eq!(record.fields["TextDataEncrypted"].value, json!("Ym9keQ=="));
    }
}

#[test]
fn fetch_shared_note_records_skips_a_zone_with_bodies_still_missing() {
    let (database, _) = install(Box::new(|names| {
        names
            .iter()
            .map(|n| {
                if n == "note-2" {
                    json!({ "recordName": n, "serverErrorCode": "NOT_FOUND", "reason": "record not found" })
                } else {
                    full_lookup_entry(n)
                }
            })
            .collect()
    }));
    let result = database
        .fetch_shared_note_records(&IndexMap::new(), &mut no_pages())
        .unwrap();
    assert_eq!(result.zones, vec![]);
    assert_eq!(
        result.skipped_zones,
        vec![SkippedSharedZone::MissingNoteBodies {
            zone_id: owner_a(),
            missing_record_names: vec!["note-2".into()],
        }]
    );
}

#[test]
fn lookup_records_chunks_into_200_record_requests() {
    let (database, batches) = install(Box::new(|names| names.iter().map(|n| full_lookup_entry(n)).collect()));
    let names: Vec<String> = (0..450).map(|i| format!("note-{i}")).collect();
    let zone = NoteZone {
        database: DatabaseScope::Shared,
        zone_id: owner_a(),
    };
    let records = database.lookup_records(&zone, &names).unwrap();
    assert_eq!(
        batches.borrow().iter().map(Vec::len).collect::<Vec<_>>(),
        vec![200, 200, 50]
    );
    assert_eq!(records.iter().map(|r| r.record_name.clone()).collect::<Vec<_>>(), names);
}

#[test]
fn lookup_records_with_no_names_makes_no_request() {
    let (database, batches) = install(Box::new(|_| vec![]));
    let records = database.lookup_records(&note_zone(Some("_ownerA")), &[]).unwrap();
    assert!(records.is_empty());
    assert!(batches.borrow().is_empty());
    assert!(database.transport.requests.borrow().is_empty());
}
