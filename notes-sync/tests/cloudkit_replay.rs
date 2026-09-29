//! ReplayTransport against the differential cassettes: the request logs it
//! writes must equal the Node driver's (minus its `/validate` setup call),
//! byte for byte. Plus request-shape checks for records/modify and error
//! mapping for non-2xx answers and asset downloads.

use std::path::Path;

use icloud_notes_sync::cloudkit::transport::{
    Cassette, CassetteAccount, CassetteRequest, CassetteResponse, Interaction, ReplayTransport, RequestLog,
};
use icloud_notes_sync::cloudkit::{
    CkError, CreateExtras, Database, RecordUpdate, RecordUpdateResult, UpdateFieldValue, UpdateFields, ZoneId,
    note_zone,
};
use indexmap::IndexMap;
use serde_json::{Value, json};

const DIFF: &str = "tests/differential";

/// The Node log without `service: "setup"`, serialized the way both sides do.
fn node_log_without_setup(dir: &str) -> String {
    let text = std::fs::read_to_string(Path::new(DIFF).join("expected").join(dir).join("requests.json")).unwrap();
    let mut log: RequestLog = serde_json::from_str(&text).unwrap();
    log.requests.retain(|r| r.service != "setup");
    serde_json::to_string_pretty(&log).unwrap() + "\n"
}

/// [`node_log_without_setup`] for a 0.6.2 log, with the one request change
/// upstream PR #29 makes (on by default in-process): `TextDataAsset` appended
/// to every `changes/zone` `desiredKeys`.
fn node_log_with_asset_key(dir: &str) -> String {
    let mut log: Value = serde_json::from_str(&node_log_without_setup(dir)).unwrap();
    for request in log["requests"].as_array_mut().unwrap() {
        let zones = request.get_mut("body").and_then(|b| b.get_mut("zones"));
        for zone in zones.and_then(Value::as_array_mut).into_iter().flatten() {
            if let Some(keys) = zone["desiredKeys"].as_array_mut() {
                keys.push(json!("TextDataAsset"));
            }
        }
    }
    serde_json::to_string_pretty(&log).unwrap() + "\n"
}

#[test]
fn tiny_clone_requests_match_the_node_driver() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("out/requests.json");
    let transport = ReplayTransport::open(
        &Path::new(DIFF).join("cassettes/tiny-clone.json"),
        Some(log_path.clone()),
    )
    .unwrap();
    let database = Database::new(transport);

    let mut pages = Vec::new();
    let private = database.fetch_all_note_records(None, &mut |n| pages.push(n)).unwrap();
    assert!(!private.resynced_from_scratch);
    assert!(private.sync_token.is_some());
    assert!(private.records.iter().any(|r| r.record_type == "Note"));
    assert_eq!(pages.iter().sum::<usize>(), private.records.len());

    let shared = database
        .fetch_shared_note_records(&IndexMap::new(), &mut |_| {})
        .unwrap();
    assert!(shared.zones.is_empty() && shared.skipped_zones.is_empty());

    assert_eq!(
        std::fs::read_to_string(&log_path).unwrap(),
        node_log_with_asset_key("tiny-clone")
    );
    assert_eq!(database.transport.request_log().requests.len(), 2);
}

#[test]
fn tiny_lookup_requests_match_the_node_driver() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("requests.json");
    let transport = ReplayTransport::open(
        &Path::new(DIFF).join("cassettes/tiny-lookup.json"),
        Some(log_path.clone()),
    )
    .unwrap();
    let database = Database::new(transport);
    let records = database
        .lookup_records(&note_zone(None), &["03667d1d-eee8-4e98-82fb-8c5cd02fd9d1".to_owned()])
        .unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].fields.contains_key("TextDataEncrypted"));
    assert_eq!(
        std::fs::read_to_string(&log_path).unwrap(),
        node_log_without_setup("tiny-push-dry-run")
    );
}

fn cassette(interactions: Vec<Interaction>) -> Cassette {
    Cassette {
        version: 1,
        account: CassetteAccount {
            dsid: "10000000001".into(),
            apple_id: "harness@example.com".into(),
        },
        ckdatabasews_url: None,
        validate: None,
        interactions,
    }
}

fn post(path: &str, status: u16, body: Value) -> Interaction {
    Interaction {
        note: None,
        request: CassetteRequest {
            method: "POST".into(),
            path: Some(format!("/database/1/com.apple.notes/production/{path}")),
            url: None,
            body: None,
        },
        response: CassetteResponse {
            status,
            body: Some(body),
            body_base64: None,
        },
        repeat: false,
    }
}

fn get(url: &str, status: u16, base64: &str) -> Interaction {
    Interaction {
        note: None,
        request: CassetteRequest {
            method: "GET".into(),
            path: None,
            url: Some(url.into()),
            body: None,
        },
        response: CassetteResponse {
            status,
            body: None,
            body_base64: Some(base64.into()),
        },
        repeat: false,
    }
}

fn modify_ok(name: &str, tag: &str) -> Value {
    json!({ "records": [{ "recordName": name, "recordType": "Note", "recordChangeTag": tag, "fields": {} }] })
}

#[test]
fn records_modify_bodies_keep_icloud_md_key_order() {
    let transport = ReplayTransport::from_cassette(
        cassette(vec![
            post(
                "shared/records/modify",
                200,
                json!({ "records": [
                    { "recordName": "n1", "recordType": "Note", "recordChangeTag": "t2", "fields": {} },
                    { "recordName": "a1", "serverErrorCode": "CONFLICT", "reason": "record to update already exists with a different change tag" },
                ] }),
            ),
            post("private/records/modify", 200, modify_ok("new-note", "t1")),
            post("private/records/modify", 200, modify_ok("n2", "t9")),
        ]),
        None,
    )
    .unwrap();
    let database = Database::new(transport);

    let mut fields = UpdateFields::new();
    fields.insert("TextDataEncrypted".into(), UpdateFieldValue::new(json!("eJw=")));
    fields.insert("TextDataAsset".into(), UpdateFieldValue::null());
    fields.insert("FirstAttachmentThumbnail".into(), UpdateFieldValue::EMPTY);
    let shared = note_zone(Some("_owner"));
    let results = database
        .update_records(
            &shared,
            &[
                RecordUpdate {
                    record_name: "n1".into(),
                    record_type: "Note".into(),
                    record_change_tag: "t1".into(),
                    fields: fields.clone(),
                    parent_record_name: Some("Folder-1".into()),
                },
                RecordUpdate {
                    record_name: "a1".into(),
                    record_type: "Attachment".into(),
                    record_change_tag: "ta".into(),
                    fields: UpdateFields::new(),
                    parent_record_name: None,
                },
            ],
        )
        .unwrap();
    assert!(results[0].is_ok());
    assert!(
        matches!(&results[1], RecordUpdateResult::Rejected { server_error_code, .. } if server_error_code == "CONFLICT")
    );

    let created = database
        .create_note_record(
            &note_zone(None),
            "new-note",
            &fields,
            &CreateExtras {
                parent_record_name: Some("DefaultFolder-CloudKit".into()),
                create_short_guid: true,
            },
        )
        .unwrap();
    assert!(created.is_ok());

    // updateNoteRecord forces recordType "Note".
    let single = database
        .update_note_record(
            &note_zone(None),
            &RecordUpdate {
                record_name: "n2".into(),
                record_type: "Whatever".into(),
                record_change_tag: "t8".into(),
                fields: UpdateFields::new(),
                parent_record_name: None,
            },
        )
        .unwrap();
    assert!(single.is_ok());

    let bodies: Vec<String> = database
        .transport
        .request_log()
        .requests
        .iter()
        .map(|r| serde_json::to_string(r.body.as_ref().unwrap()).unwrap())
        .collect();
    assert_eq!(
        bodies,
        [
            r#"{"operations":[{"operationType":"update","record":{"recordName":"n1","recordType":"Note","recordChangeTag":"t1","fields":{"TextDataEncrypted":{"value":"eJw="},"TextDataAsset":{"value":null},"FirstAttachmentThumbnail":{}},"parent":{"recordName":"Folder-1"}}},{"operationType":"update","record":{"recordName":"a1","recordType":"Attachment","recordChangeTag":"ta","fields":{}}}],"zoneID":{"zoneName":"Notes","ownerRecordName":"_owner"}}"#,
            r#"{"operations":[{"operationType":"create","record":{"recordName":"new-note","recordType":"Note","fields":{"TextDataEncrypted":{"value":"eJw="},"TextDataAsset":{"value":null},"FirstAttachmentThumbnail":{}},"parent":{"recordName":"DefaultFolder-CloudKit"},"createShortGUID":true}}],"zoneID":{"zoneName":"Notes"}}"#,
            r#"{"operations":[{"operationType":"update","record":{"recordName":"n2","recordType":"Note","recordChangeTag":"t8","fields":{}}}],"zoneID":{"zoneName":"Notes"}}"#,
        ]
    );
    let paths: Vec<_> = database
        .transport
        .request_log()
        .requests
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(
        paths,
        [
            "/database/1/com.apple.notes/production/shared/records/modify",
            "/database/1/com.apple.notes/production/private/records/modify",
            "/database/1/com.apple.notes/production/private/records/modify",
        ]
    );
}

#[test]
fn folder_create_without_extras_sends_the_bare_shape() {
    let transport = ReplayTransport::from_cassette(
        cassette(vec![post("private/records/modify", 200, modify_ok("f1", "t"))]),
        None,
    )
    .unwrap();
    let database = Database::new(transport);
    let mut fields = UpdateFields::new();
    fields.insert("TitleEncrypted".into(), UpdateFieldValue::new(json!("Rm9v")));
    database
        .create_folder_record(&note_zone(None), "f1", &fields, &CreateExtras::default())
        .unwrap();
    let body = database.transport.request_log().requests[0].body.clone().unwrap();
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"operations":[{"operationType":"create","record":{"recordName":"f1","recordType":"Folder","fields":{"TitleEncrypted":{"value":"Rm9v"}}}}],"zoneID":{"zoneName":"Notes"}}"#
    );
}

#[test]
fn non_2xx_answers_become_request_failed_with_icloud_md_wording() {
    let transport = ReplayTransport::from_cassette(
        cassette(vec![
            post("private/changes/zone", 500, json!({})),
            post("shared/changes/database", 503, json!({})),
        ]),
        None,
    )
    .unwrap();
    let database = Database::new(transport);
    let err = database.fetch_all_note_records(None, &mut |_| {}).unwrap_err();
    assert!(matches!(&err, CkError::RequestFailed(_)));
    assert_eq!(err.to_string(), "changes/zone request failed (private db): HTTP 500");
    let err = database.fetch_shared_zone_ids().unwrap_err();
    assert_eq!(err.to_string(), "changes/database request failed (shared db): HTTP 503");
}

#[test]
fn an_unmatched_request_gets_599_and_is_logged_unmatched() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("requests.json");
    let transport = ReplayTransport::from_cassette(cassette(vec![]), Some(log_path.clone())).unwrap();
    let database = Database::new(transport);
    let err = database
        .lookup_records(&note_zone(None), &["x".to_owned()])
        .unwrap_err();
    assert_eq!(err.to_string(), "records/lookup request failed (private db): HTTP 599");
    let log: RequestLog = serde_json::from_str(&std::fs::read_to_string(&log_path).unwrap()).unwrap();
    assert_eq!(log.requests.len(), 1);
    assert_eq!(log.requests[0].matched, None);
    assert_eq!(log.requests[0].service, "ckdatabasews");
    assert_eq!(
        log.requests[0]
            .query
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>(),
        ["ckjsBuildVersion=2310ProjectDev27", "ckjsVersion=2.6.4"]
    );
}

#[test]
fn body_pinned_interactions_match_on_json_equality() {
    let mut first = post(
        "private/changes/zone",
        200,
        json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "moreComing": false, "syncToken": "B", "records": [] }] }),
    );
    // Pinned to the second walk only (the token-free one).
    first.request.body = Some(json!({ "zones": [{
        "zoneID": { "zoneName": "Notes" },
        "desiredKeys": icloud_notes_sync::cloudkit::client::note_desired_keys(),
        "desiredRecordTypes": icloud_notes_sync::cloudkit::client::NOTE_DESIRED_RECORD_TYPES,
        "reverse": true,
    }] }));
    let rejection = post(
        "private/changes/zone",
        200,
        json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "serverErrorCode": "BAD_REQUEST", "reason": "Invalid continuation format" }] }),
    );
    let transport = ReplayTransport::from_cassette(cassette(vec![first, rejection]), None).unwrap();
    let database = Database::new(transport);
    let result = database.fetch_all_note_records(Some("corrupt"), &mut |_| {}).unwrap();
    assert!(result.resynced_from_scratch);
    assert_eq!(result.sync_token.as_deref(), Some("B"));
    let matched: Vec<_> = database
        .transport
        .request_log()
        .requests
        .iter()
        .map(|r| r.matched)
        .collect();
    assert_eq!(matched, [Some(1), Some(0)]);
}

#[test]
fn asset_downloads_are_served_from_the_cassette() {
    let tmp = tempfile::tempdir().unwrap();
    let transport = ReplayTransport::from_cassette(
        cassette(vec![
            get("https://cvws.icloud-content.com/B/abc", 200, "aGVsbG8="),
            get("https://cvws.icloud-content.com/B/expired", 403, ""),
        ]),
        None,
    )
    .unwrap();
    let database = Database::new(transport);
    let dest = tmp.path().join("Notes/attachments/hello.txt");
    let n = database
        .fetch_asset("https://cvws.icloud-content.com/B/abc?e=123&s=sig", &dest)
        .unwrap();
    assert_eq!(n, 5);
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello");

    let err = database
        .fetch_asset("https://cvws.icloud-content.com/B/expired?e=1", &tmp.path().join("x"))
        .unwrap_err();
    assert_eq!(err.to_string(), "Attachment download failed: HTTP 403");
    assert!(!tmp.path().join("x").exists());

    let log = database.transport.request_log();
    assert_eq!(log.requests[0].service, "other");
    assert_eq!(log.requests[0].path, "https://cvws.icloud-content.com/B/abc");
    assert_eq!(log.requests[0].query.get("e").map(String::as_str), Some("123"));
    assert!(log.requests[0].body.is_none());
}

#[test]
fn shared_zone_requests_carry_the_owner_and_no_reverse() {
    let transport = ReplayTransport::from_cassette(
        cassette(vec![
            post(
                "shared/changes/database",
                200,
                json!({ "moreComing": false, "zones": [{ "zoneID": { "zoneName": "Notes", "ownerRecordName": "_o", "zoneType": "REGULAR_CUSTOM_ZONE" } }] }),
            ),
            post(
                "shared/changes/zone",
                200,
                json!({ "zones": [{ "zoneID": { "zoneName": "Notes", "ownerRecordName": "_o" }, "moreComing": false, "syncToken": "s2", "records": [
                    { "recordName": "n", "recordType": "Note", "fields": { "TitleEncrypted": { "value": "dA==", "type": "ENCRYPTED_BYTES" } }, "recordChangeTag": "c1" },
                    { "recordName": "gone", "deleted": true },
                ] }] }),
            ),
            post(
                "shared/records/lookup",
                200,
                json!({ "records": [{ "recordName": "n", "recordType": "Note", "fields": { "TextDataEncrypted": { "value": "eJw=", "type": "ENCRYPTED_BYTES" } } }] }),
            ),
        ]),
        None,
    )
    .unwrap();
    let database = Database::new(transport);
    let mut tokens = IndexMap::new();
    tokens.insert("_o".to_owned(), "s1".to_owned());
    let result = database.fetch_shared_note_records(&tokens, &mut |_| {}).unwrap();
    assert_eq!(result.zones.len(), 1);
    let zone = &result.zones[0];
    assert_eq!(
        zone.zone_id,
        ZoneId {
            zone_name: "Notes".into(),
            owner_record_name: Some("_o".into())
        }
    );
    assert_eq!(zone.records[0].record_change_tag.as_deref(), Some("c1"));
    assert!(zone.records[1].is_deleted());

    let log = database.transport.request_log();
    let zone_body = log.requests[1].body.as_ref().unwrap();
    let keys: Vec<_> = zone_body["zones"][0].as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["zoneID", "desiredKeys", "desiredRecordTypes", "syncToken"]);
    assert_eq!(zone_body["zones"][0]["syncToken"], "s1");
    assert_eq!(
        serde_json::to_string(log.requests[2].body.as_ref().unwrap()).unwrap(),
        r#"{"records":[{"recordName":"n"}],"zoneID":{"zoneName":"Notes","ownerRecordName":"_o"}}"#
    );
}
