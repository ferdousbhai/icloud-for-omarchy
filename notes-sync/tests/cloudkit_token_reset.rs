//! Port of icloud-md `src/cloudkit/databaseClient.tokenReset.test.ts`.

mod cloudkit_common;

use cloudkit_common::{db, no_pages, scripted};
use icloud_notes_sync::cloudkit::CkError;
use indexmap::IndexMap;
use serde_json::{Value, json};

fn note_record(name: &str, tag: &str) -> Value {
    json!({
        "recordName": name,
        "recordType": "Note",
        "recordChangeTag": tag,
        "fields": { "TextDataEncrypted": { "value": "Ym9keQ==", "type": "ENCRYPTED_BYTES" } },
    })
}

fn token_rejection() -> Value {
    json!({
        "zones": [{ "zoneID": { "zoneName": "Notes" }, "serverErrorCode": "BAD_REQUEST", "reason": "Unknown sync continuation type" }],
    })
}

/// The `syncToken` each `changes/zone` request carried.
fn sent_tokens(database: &icloud_notes_sync::Database<cloudkit_common::MockTransport>) -> Vec<Option<String>> {
    database
        .transport
        .bodies("/changes/zone")
        .iter()
        .map(|b| {
            b["zones"][0]
                .get("syncToken")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn zone_error_code(err: CkError) -> String {
    match err {
        CkError::ZoneFetchFailed { server_error_code, .. } => server_error_code,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_rejected_sync_token_falls_back_to_a_full_refetch() {
    let database = db(scripted(vec![
        token_rejection(),
        json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "moreComing": true, "syncToken": "fresh-1", "records": [note_record("note-1", "tag-1")] }] }),
        json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "moreComing": false, "syncToken": "fresh-2", "records": [note_record("note-2", "tag-2")] }] }),
    ]));
    let result = database
        .fetch_all_note_records(Some("stale-token"), &mut no_pages())
        .unwrap();
    assert!(result.resynced_from_scratch);
    assert_eq!(
        result
            .records
            .iter()
            .map(|r| r.record_name.as_str())
            .collect::<Vec<_>>(),
        ["note-1", "note-2"]
    );
    assert_eq!(result.sync_token.as_deref(), Some("fresh-2"));
    assert_eq!(
        sent_tokens(&database),
        vec![Some("stale-token".into()), None, Some("fresh-1".into())]
    );
}

#[test]
fn an_accepted_sync_token_stays_incremental() {
    let database = db(scripted(vec![json!({
        "zones": [{ "zoneID": { "zoneName": "Notes" }, "moreComing": false, "syncToken": "next", "records": [note_record("note-1", "tag-1")] }],
    })]));
    let result = database
        .fetch_all_note_records(Some("still-good"), &mut no_pages())
        .unwrap();
    assert!(!result.resynced_from_scratch);
    assert_eq!(result.sync_token.as_deref(), Some("next"));
}

#[test]
fn bad_request_with_no_stored_token_propagates() {
    let database = db(scripted(vec![token_rejection()]));
    let err = database.fetch_all_note_records(None, &mut no_pages()).unwrap_err();
    assert_eq!(zone_error_code(err), "BAD_REQUEST");
    assert_eq!(sent_tokens(&database).len(), 1);
}

#[test]
fn a_non_bad_request_zone_error_does_not_trigger_the_fallback() {
    let database = db(scripted(vec![json!({
        "zones": [{ "zoneID": { "zoneName": "Notes" }, "serverErrorCode": "THROTTLED", "reason": "slow down" }],
    })]));
    let err = database
        .fetch_all_note_records(Some("stale-token"), &mut no_pages())
        .unwrap_err();
    assert_eq!(zone_error_code(err), "THROTTLED");
    assert_eq!(sent_tokens(&database).len(), 1);
}

#[test]
fn a_bad_request_that_persists_without_the_token_propagates() {
    let database = db(scripted(vec![token_rejection(), token_rejection()]));
    let err = database
        .fetch_all_note_records(Some("stale-token"), &mut no_pages())
        .unwrap_err();
    assert_eq!(zone_error_code(err), "BAD_REQUEST");
    assert_eq!(sent_tokens(&database).len(), 2);
}

#[test]
fn fetch_shared_note_records_carries_a_zone_resync_through() {
    let mut owner_a_requests = 0;
    let database = db(move |path, body| {
        if path.contains("/shared/changes/database") {
            return Ok(json!({
                "moreComing": false,
                "zones": [
                    { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA", "zoneType": "REGULAR_CUSTOM_ZONE" } },
                    { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerB", "zoneType": "REGULAR_CUSTOM_ZONE" } },
                ],
            }));
        }
        if path.contains("/shared/changes/zone") {
            if body["zones"][0]["zoneID"]["ownerRecordName"] == "_ownerA" {
                owner_a_requests += 1;
                if owner_a_requests == 1 {
                    return Ok(json!({
                        "zones": [{
                            "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" },
                            "serverErrorCode": "BAD_REQUEST",
                            "reason": "Unknown sync continuation type",
                        }],
                    }));
                }
                return Ok(json!({
                    "zones": [{
                        "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" },
                        "moreComing": false,
                        "syncToken": "fresh-a",
                        "records": [note_record("note-a", "tag-a")],
                    }],
                }));
            }
            return Ok(json!({
                "zones": [{
                    "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerB" },
                    "moreComing": false,
                    "syncToken": "token-b",
                    "records": [note_record("note-b", "tag-b")],
                }],
            }));
        }
        Err(CkError::Other(format!("Unexpected fetch in test: {path}")))
    });

    let tokens: IndexMap<String, String> = [("_ownerA", "stale-token-a"), ("_ownerB", "good-token-b")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let result = database.fetch_shared_note_records(&tokens, &mut no_pages()).unwrap();

    assert!(result.skipped_zones.is_empty());
    let find = |owner: &str| {
        result
            .zones
            .iter()
            .find(|z| z.zone_id.owner_record_name.as_deref() == Some(owner))
            .unwrap()
    };
    assert!(find("_ownerA").resynced_from_scratch);
    assert_eq!(find("_ownerA").sync_token.as_deref(), Some("fresh-a"));
    assert!(!find("_ownerB").resynced_from_scratch);
    // Shared-database requests never carry `reverse`.
    for body in database.transport.bodies("/shared/changes/zone") {
        assert!(body["zones"][0].get("reverse").is_none());
    }
}
