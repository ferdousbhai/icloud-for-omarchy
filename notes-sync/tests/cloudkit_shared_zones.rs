//! Port of icloud-md `src/cloudkit/databaseClient.sharedZonePaging.test.ts`
//! and `databaseClient.sharedZoneSkip.test.ts`.

mod cloudkit_common;

use cloudkit_common::{db, no_pages, scripted};
use icloud_notes_sync::cloudkit::{CkError, SkippedSharedZone, ZoneId};
use indexmap::IndexMap;
use serde_json::{Value, json};

fn zone_entry(owner: &str) -> Value {
    json!({ "zoneID": { "zoneName": "Notes", "ownerRecordName": owner, "zoneType": "REGULAR_CUSTOM_ZONE" } })
}

fn zone(owner: &str) -> ZoneId {
    ZoneId {
        zone_name: "Notes".into(),
        owner_record_name: Some(owner.into()),
    }
}

// --- sharedZonePaging --------------------------------------------------------

#[test]
fn fetch_shared_zone_ids_follows_more_coming_across_pages() {
    let database = db(scripted(vec![
        json!({ "moreComing": true, "syncToken": "page-1", "zones": [zone_entry("_ownerA")] }),
        json!({ "moreComing": true, "syncToken": "page-2", "zones": [zone_entry("_ownerB")] }),
        json!({ "moreComing": false, "syncToken": "page-3", "zones": [zone_entry("_ownerC")] }),
    ]));
    let zone_ids = database.fetch_shared_zone_ids().unwrap();
    assert_eq!(zone_ids, vec![zone("_ownerA"), zone("_ownerB"), zone("_ownerC")]);
    assert_eq!(
        database.transport.bodies("/shared/changes/database"),
        vec![
            json!({}),
            json!({ "syncToken": "page-1" }),
            json!({ "syncToken": "page-2" })
        ]
    );
}

#[test]
fn fetch_shared_zone_ids_fails_when_more_coming_has_no_sync_token() {
    let database = db(scripted(vec![
        json!({ "moreComing": true, "zones": [zone_entry("_ownerA")] }),
    ]));
    match database.fetch_shared_zone_ids().unwrap_err() {
        CkError::RequestFailed(m) => assert!(m.contains("moreComing"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn fetch_shared_zone_ids_fails_when_the_server_repeats_the_sync_token() {
    let database = db(scripted(vec![
        json!({ "moreComing": true, "syncToken": "page-1", "zones": [zone_entry("_ownerA")] }),
        json!({ "moreComing": true, "syncToken": "page-1", "zones": [zone_entry("_ownerA")] }),
    ]));
    match database.fetch_shared_zone_ids().unwrap_err() {
        CkError::RequestFailed(m) => assert!(m.contains("same page"), "{m}"),
        other => panic!("{other:?}"),
    }
}

// --- sharedZoneSkip ----------------------------------------------------------

/// Two shared zones: `_ownerA` answers one full note, `_ownerB` a zone-level
/// error with `owner_b_code` inside an HTTP 200.
fn two_zones(owner_b_code: &'static str) -> cloudkit_common::MockTransport {
    cloudkit_common::MockTransport::new(move |path, body| {
        if path.contains("/shared/changes/database") {
            return Ok(json!({ "zones": [zone_entry("_ownerA"), zone_entry("_ownerB")] }));
        }
        if path.contains("/shared/changes/zone") {
            if body["zones"][0]["zoneID"]["ownerRecordName"] == "_ownerA" {
                return Ok(json!({
                    "zones": [{
                        "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" },
                        "syncToken": "token-a",
                        "moreComing": false,
                        "records": [{
                            "recordName": "note-a",
                            "recordType": "Note",
                            "recordChangeTag": "tag-a",
                            "fields": { "TextDataEncrypted": { "value": "Ym9keQ==", "type": "ENCRYPTED_BYTES" } },
                        }],
                    }],
                }));
            }
            return Ok(json!({
                "zones": [{
                    "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerB" },
                    "reason": "Zone does not exist",
                    "serverErrorCode": owner_b_code,
                }],
            }));
        }
        Err(CkError::Other(format!("Unexpected fetch in test: {path}")))
    })
}

#[test]
fn fetch_shared_note_records_skips_a_zone_not_found_zone() {
    let database = icloud_notes_sync::Database::new(two_zones("ZONE_NOT_FOUND"));
    let result = database
        .fetch_shared_note_records(&IndexMap::new(), &mut no_pages())
        .unwrap();
    assert_eq!(result.zones.len(), 1);
    assert_eq!(result.zones[0].zone_id.owner_record_name.as_deref(), Some("_ownerA"));
    assert_eq!(result.zones[0].sync_token.as_deref(), Some("token-a"));
    assert_eq!(result.zones[0].records[0].record_name, "note-a");
    assert_eq!(
        result.skipped_zones,
        vec![SkippedSharedZone::ZoneNotFound {
            zone_id: zone("_ownerB"),
            server_error_code: "ZONE_NOT_FOUND".into(),
        }]
    );
}

#[test]
fn fetch_shared_note_records_stays_fatal_for_other_zone_errors() {
    let database = icloud_notes_sync::Database::new(two_zones("INTERNAL_ERROR"));
    match database
        .fetch_shared_note_records(&IndexMap::new(), &mut no_pages())
        .unwrap_err()
    {
        CkError::ZoneFetchFailed { server_error_code, .. } => assert_eq!(server_error_code, "INTERNAL_ERROR"),
        other => panic!("{other:?}"),
    }
}
