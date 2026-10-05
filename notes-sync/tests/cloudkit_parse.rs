//! Port of icloud-md `src/cloudkit/databaseClient.test.ts`.

mod common;

use common::zone;

use icloud_notes_sync::cloudkit::client::{
    first_zone, merge_looked_up_records, parse_note_update_response, parse_record_update_response,
    parse_shared_zone_list,
};
use icloud_notes_sync::cloudkit::{CkError, CloudKitRecord, FieldValue, RecordUpdateResult, SharedZoneListPage};
use indexmap::IndexMap;
use serde_json::json;

fn unexpected(err: CkError) -> String {
    match err {
        CkError::UnexpectedResponse(m) => m,
        other => panic!("expected UnexpectedResponse, got {other:?}"),
    }
}

#[test]
fn parse_shared_zone_list_extracts_zone_name_and_owner_per_zone() {
    let body = json!({
        "moreComing": false,
        "syncToken": "AQAAAZ9XZ9gy",
        "zones": [
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_35c0dc4416a1c75e7d98713af3f50348", "zoneType": "REGULAR_CUSTOM_ZONE" } },
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_3ae5f00b01edbda385d0db894253c622", "zoneType": "REGULAR_CUSTOM_ZONE" } },
        ],
    });
    assert_eq!(
        parse_shared_zone_list(&body).unwrap(),
        SharedZoneListPage {
            zone_ids: vec![
                zone("_35c0dc4416a1c75e7d98713af3f50348"),
                zone("_3ae5f00b01edbda385d0db894253c622")
            ],
            deleted_zone_ids: vec![],
            more_coming: false,
            sync_token: Some("AQAAAZ9XZ9gy".into()),
        }
    );
}

#[test]
fn parse_shared_zone_list_surfaces_more_coming() {
    let body = json!({ "moreComing": true, "syncToken": "page-1", "zones": [] });
    assert_eq!(
        parse_shared_zone_list(&body).unwrap(),
        SharedZoneListPage {
            zone_ids: vec![],
            deleted_zone_ids: vec![],
            more_coming: true,
            sync_token: Some("page-1".into()),
        }
    );
}

#[test]
fn parse_shared_zone_list_handles_no_shared_zones() {
    assert_eq!(
        parse_shared_zone_list(&json!({ "moreComing": false, "zones": [] })).unwrap(),
        SharedZoneListPage::default()
    );
}

#[test]
fn parse_shared_zone_list_rejects_missing_zones_array() {
    let err = unexpected(parse_shared_zone_list(&json!({ "syncToken": "x" })).unwrap_err());
    assert!(err.contains("missing zones array"), "{err}");
}

#[test]
fn parse_shared_zone_list_skips_tombstoned_zones() {
    let body = json!({
        "zones": [
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_live", "zoneType": "REGULAR_CUSTOM_ZONE" } },
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_revoked", "zoneType": "REGULAR_CUSTOM_ZONE" }, "deleted": true },
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_purged", "zoneType": "REGULAR_CUSTOM_ZONE" }, "purged": true },
        ],
    });
    let page = parse_shared_zone_list(&body).unwrap();
    assert_eq!(page.zone_ids, vec![zone("_live")]);
    assert_eq!(page.deleted_zone_ids, vec![zone("_revoked"), zone("_purged")]);
}

#[test]
fn first_zone_throws_on_zone_level_server_error() {
    let body = json!({
        "zones": [{
            "zoneID": { "zoneName": "Notes", "ownerRecordName": "_owner", "zoneType": "REGULAR_CUSTOM_ZONE" },
            "reason": "Reverse sync of share db is unsupported",
            "serverErrorCode": "BAD_REQUEST",
        }],
    });
    let err = first_zone(&body).unwrap_err();
    assert!(matches!(err, CkError::ZoneFetchFailed { .. }));
    assert_eq!(
        err.to_string(),
        "changes/zone failed for a zone: BAD_REQUEST (Reverse sync of share db is unsupported)"
    );
}

fn zone_error(code: &str, reason: &str) -> CkError {
    let body = json!({
        "zones": [{ "zoneID": { "zoneName": "Notes", "zoneType": "REGULAR_CUSTOM_ZONE" }, "reason": reason, "serverErrorCode": code }],
    });
    first_zone(&body).unwrap_err()
}

#[test]
fn first_zone_zone_not_found_is_typed_and_drops_the_retry_hint() {
    let err = zone_error("ZONE_NOT_FOUND", "Zone does not exist");
    let CkError::ZoneFetchFailed { server_error_code, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(server_error_code, "ZONE_NOT_FOUND");
    // The hint text lives in cmd::errors (icloud-md put it on the error class).
    let hint = icloud_notes_sync::cmd::Error::from(err).hint().unwrap_or_default();
    assert!(!hint.contains("transient") && !hint.contains("try again"), "{hint}");
    assert!(hint.contains("revoked or deleted"), "{hint}");
}

#[test]
fn first_zone_keeps_the_transient_hint_for_other_codes() {
    let err = zone_error("INTERNAL_ERROR", "internal server error");
    let CkError::ZoneFetchFailed { server_error_code, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(server_error_code, "INTERNAL_ERROR");
    let hint = icloud_notes_sync::cmd::Error::from(err).hint().unwrap_or_default();
    assert!(hint.contains("transient"), "{hint}");
}

#[test]
fn first_zone_parses_a_deletion_tombstone() {
    let body = json!({
        "zones": [{
            "zoneID": { "zoneName": "Notes", "zoneType": "REGULAR_CUSTOM_ZONE" },
            "syncToken": "AQAAAZ9XZ9gy",
            "records": [{ "recordName": "deleted-note-1", "deleted": true }],
        }],
    });
    let zone = first_zone(&body).unwrap();
    assert_eq!(
        zone.records.unwrap(),
        vec![CloudKitRecord {
            record_name: "deleted-note-1".into(),
            record_type: "Note".into(),
            deleted: Some(true),
            ..Default::default()
        }]
    );
}

fn field(value: &str) -> FieldValue {
    FieldValue {
        value: json!(value),
        type_: "ENCRYPTED_BYTES".into(),
    }
}

fn make_record(name: &str, fields: &[(&str, &str)], tag: Option<&str>) -> CloudKitRecord {
    CloudKitRecord {
        record_name: name.into(),
        record_type: "Note".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), field(v)))
            .collect::<IndexMap<_, _>>(),
        record_change_tag: tag.map(Into::into),
        ..Default::default()
    }
}

#[test]
fn merge_looked_up_records_swaps_listing_records_for_full_versions() {
    let mut records = vec![
        make_record("A", &[("TitleEncrypted", "dA==")], Some("tag-list-a")),
        make_record("B", &[("TitleEncrypted", "dQ==")], Some("tag-list-b")),
    ];
    let looked_up = vec![make_record(
        "A",
        &[("TitleEncrypted", "dA=="), ("TextDataEncrypted", "Ym9keQ==")],
        Some("tag-full-a"),
    )];
    merge_looked_up_records(&mut records, looked_up);

    assert_eq!(records[0].fields["TextDataEncrypted"].value, json!("Ym9keQ=="));
    assert_eq!(records[0].record_change_tag.as_deref(), Some("tag-full-a"));
    assert!(!records[1].fields.contains_key("TextDataEncrypted"));
    assert_eq!(records[1].record_change_tag.as_deref(), Some("tag-list-b"));
}

#[test]
fn merge_looked_up_records_keeps_listing_change_tag_when_lookup_lacks_one() {
    let mut records = vec![make_record("A", &[], Some("tag-list-a"))];
    let looked_up = vec![make_record("A", &[("TextDataEncrypted", "Ym9keQ==")], None)];
    merge_looked_up_records(&mut records, looked_up);
    assert_eq!(records[0].record_change_tag.as_deref(), Some("tag-list-a"));
}

#[test]
fn parse_note_update_response_returns_the_updated_record() {
    let body = json!({
        "records": [{
            "recordName": "F90C80BA-2D47-4CB1-B000-000000000000",
            "recordType": "Note",
            "recordChangeTag": "25d",
            "fields": {
                "ModificationDate": { "value": 1783880004527_i64, "type": "TIMESTAMP" },
                "TextDataEncrypted": { "value": "eJw=", "type": "ENCRYPTED_BYTES" },
            },
            "parent": { "recordName": "DefaultFolder-CloudKit" },
        }],
    });
    let RecordUpdateResult::Ok(record) = parse_note_update_response(&body).unwrap() else {
        panic!("expected ok")
    };
    assert_eq!(record.record_change_tag.as_deref(), Some("25d"));
    assert_eq!(record.parent_record_name.as_deref(), Some("DefaultFolder-CloudKit"));
    assert_eq!(record.fields["ModificationDate"].value, json!(1783880004527_i64));
}

fn conflict_body() -> serde_json::Value {
    json!({
        "records": [{
            "recordName": "F90C80BA-2D47-4CB1-B000-000000000000",
            "reason": "record to update already exists with a different change tag",
            "serverErrorCode": "CONFLICT",
        }],
    })
}

#[test]
fn parse_note_update_response_surfaces_per_record_errors_as_refusals() {
    let RecordUpdateResult::Rejected {
        server_error_code,
        reason,
    } = parse_note_update_response(&conflict_body()).unwrap()
    else {
        panic!("expected a refusal")
    };
    assert_eq!(server_error_code, "CONFLICT");
    assert!(reason.unwrap_or_default().contains("change tag"));
}

#[test]
fn parse_note_update_response_rejects_bodies_without_records() {
    let err = unexpected(parse_note_update_response(&json!({})).unwrap_err());
    assert!(err.contains("missing records array"), "{err}");
}

#[test]
fn parse_record_update_response_returns_one_result_per_record_in_order() {
    let body = json!({
        "records": [
            {
                "recordName": "note-1",
                "recordType": "Note",
                "recordChangeTag": "25d",
                "fields": { "TextDataEncrypted": { "value": "eJw=", "type": "ENCRYPTED_BYTES" } },
            },
            {
                "recordName": "attachment-1",
                "reason": "record to update already exists with a different change tag",
                "serverErrorCode": "CONFLICT",
            },
        ],
    });
    let results = parse_record_update_response(&body).unwrap();
    assert_eq!(results.len(), 2);
    let RecordUpdateResult::Ok(record) = &results[0] else {
        panic!("expected ok")
    };
    assert_eq!(record.record_name, "note-1");
    assert!(
        matches!(&results[1], RecordUpdateResult::Rejected { server_error_code, .. } if server_error_code == "CONFLICT")
    );
}

#[test]
fn parse_record_update_response_returns_empty_for_empty_records() {
    assert_eq!(parse_record_update_response(&json!({ "records": [] })).unwrap(), vec![]);
}

#[test]
fn parse_record_update_response_rejects_bodies_without_records() {
    let err = unexpected(parse_record_update_response(&json!({})).unwrap_err());
    assert!(err.contains("missing records array"), "{err}");
}
