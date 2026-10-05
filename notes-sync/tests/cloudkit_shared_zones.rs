//! Port of icloud-md `src/cloudkit/databaseClient.sharedZonePaging.test.ts`
//! and `databaseClient.sharedZoneSkip.test.ts`.

mod common;

use common::zone;

mod cloudkit_common;

use cloudkit_common::{db, no_pages, scripted};
use icloud_notes_sync::cloudkit::{CkError, SharedDatabaseCursor, SkippedSharedZone};
use indexmap::IndexMap;
use serde_json::{Value, json};

fn zone_entry(owner: &str) -> Value {
    json!({ "zoneID": { "zoneName": "Notes", "ownerRecordName": owner, "zoneType": "REGULAR_CUSTOM_ZONE" } })
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

// --- the shared-database cursor (not in icloud-md) ---------------------------

/// The shared database answers `delta` to an incremental listing; a zone
/// walk answers no changes and `token-<owner>`, except `_ownerB`'s, which
/// answers ZONE_NOT_FOUND when `b_gone`.
fn cursor_transport(delta: Value, b_gone: bool) -> cloudkit_common::MockTransport {
    cloudkit_common::MockTransport::new(move |path, body| {
        if path.contains("/shared/changes/database") {
            assert!(body.get("syncToken").is_some(), "listed from scratch: {body}");
            return Ok(delta.clone());
        }
        let owner = body["zones"][0]["zoneID"]["ownerRecordName"]
            .as_str()
            .unwrap()
            .to_owned();
        if b_gone && owner == "_ownerB" {
            return Ok(json!({ "zones": [{ "serverErrorCode": "ZONE_NOT_FOUND", "reason": "gone" }] }));
        }
        Ok(json!({ "zones": [{ "syncToken": format!("token-{owner}"), "moreComing": false, "records": [] }] }))
    })
}

fn cursor(token: &str, owners: &[&str]) -> SharedDatabaseCursor {
    SharedDatabaseCursor {
        sync_token: token.into(),
        zones: owners.iter().map(|o| zone(o)).collect(),
        listed_at: 1_000,
    }
}

fn walked_owners(database: &icloud_notes_sync::Database<cloudkit_common::MockTransport>) -> Vec<String> {
    database
        .transport
        .bodies("/shared/changes/zone")
        .iter()
        .map(|b| b["zones"][0]["zoneID"]["ownerRecordName"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn an_incremental_listing_walks_only_changed_zones_and_tokenless_ones() {
    let database = icloud_notes_sync::Database::new(cursor_transport(
        json!({ "zones": [zone_entry("_ownerB"), zone_entry("_ownerD")], "syncToken": "db-2" }),
        false,
    ));
    let tokens: IndexMap<String, String> = [("_ownerA", "ta"), ("_ownerB", "tb")]
        .into_iter()
        .map(|(o, t)| (o.to_owned(), t.to_owned()))
        .collect();
    let previous = cursor("db-1", &["_ownerA", "_ownerB", "_ownerC"]);
    let result = database
        .fetch_shared_note_records_since(&tokens, Some(&previous), 2_000, &mut no_pages())
        .unwrap();
    // B changed, C has no stored token, D is new; A is unchanged.
    assert_eq!(walked_owners(&database), ["_ownerB", "_ownerC", "_ownerD"]);
    let zones: Vec<(String, Option<String>, usize)> = result
        .zones
        .iter()
        .map(|z| {
            (
                z.zone_id.owner_record_name.clone().unwrap(),
                z.sync_token.clone(),
                z.records.len(),
            )
        })
        .collect();
    assert_eq!(
        zones,
        [
            ("_ownerA".into(), Some("ta".into()), 0),
            ("_ownerB".into(), Some("token-_ownerB".into()), 0),
            ("_ownerC".into(), Some("token-_ownerC".into()), 0),
            ("_ownerD".into(), Some("token-_ownerD".into()), 0),
        ]
    );
    assert_eq!(
        result.cursor,
        Some(SharedDatabaseCursor {
            listed_at: 1_000,
            ..cursor("db-2", &["_ownerA", "_ownerB", "_ownerC", "_ownerD"])
        })
    );
}

#[test]
fn an_incremental_listing_drops_deleted_zones() {
    let database = icloud_notes_sync::Database::new(cursor_transport(
        json!({ "zones": [{ "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" }, "deleted": true }], "syncToken": "db-2" }),
        false,
    ));
    let tokens: IndexMap<String, String> = [("_ownerA", "ta"), ("_ownerB", "tb")]
        .into_iter()
        .map(|(o, t)| (o.to_owned(), t.to_owned()))
        .collect();
    let result = database
        .fetch_shared_note_records_since(
            &tokens,
            Some(&cursor("db-1", &["_ownerA", "_ownerB"])),
            2_000,
            &mut no_pages(),
        )
        .unwrap();
    assert!(walked_owners(&database).is_empty());
    assert_eq!(result.zones.len(), 1);
    assert_eq!(result.zones[0].zone_id, zone("_ownerB"));
    assert_eq!(result.cursor.unwrap().zones, vec![zone("_ownerB")]);
}

/// A skipped zone holds the cursor at its old token, so the next listing
/// reports the zone again and it is retried.
#[test]
fn a_skipped_zone_keeps_the_previous_cursor_token() {
    let database = icloud_notes_sync::Database::new(cursor_transport(
        json!({ "zones": [zone_entry("_ownerB")], "syncToken": "db-2" }),
        true,
    ));
    let tokens: IndexMap<String, String> = [("_ownerA", "ta"), ("_ownerB", "tb")]
        .into_iter()
        .map(|(o, t)| (o.to_owned(), t.to_owned()))
        .collect();
    let previous = cursor("db-1", &["_ownerA", "_ownerB"]);
    let result = database
        .fetch_shared_note_records_since(&tokens, Some(&previous), 2_000, &mut no_pages())
        .unwrap();
    assert_eq!(walked_owners(&database), ["_ownerB"]);
    assert_eq!(result.skipped_zones.len(), 1);
    assert_eq!(result.cursor, Some(previous));
}
