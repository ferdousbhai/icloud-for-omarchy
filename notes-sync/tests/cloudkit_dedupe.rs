//! A record CloudKit lists twice in one zone walk is kept once (deliberate
//! difference from icloud-md 0.6.2, docs/PORT_PLAN.md §1): same page, across
//! pages, the shared-zone path with its `records/lookup` backfill, a shared
//! zone listed twice, and which occurrence wins.

mod cloudkit_common;

use cloudkit_common::{MockTransport, db, no_pages, scripted};
use icloud_notes_sync::Database;
use icloud_notes_sync::cloudkit::CloudKitRecord;
use icloud_notes_sync::cloudkit::client::dedupe_zone_records;
use indexmap::IndexMap;
use serde_json::{Value, json};

fn note(name: &str, tag: &str, modified: Option<i64>, body: &str) -> Value {
    let mut record = json!({
        "recordName": name,
        "recordType": "Note",
        "recordChangeTag": tag,
        "fields": { "TextDataEncrypted": { "value": body, "type": "ENCRYPTED_BYTES" } },
    });
    if let Some(ms) = modified {
        record["modified"] = json!({ "timestamp": ms, "deviceID": "2" });
    }
    record
}

fn tombstone(name: &str) -> Value {
    json!({ "recordName": name, "deleted": true })
}

fn page(records: Vec<Value>, more_coming: bool, token: &str) -> Value {
    json!({ "zones": [{
        "zoneID": { "zoneName": "Notes", "ownerRecordName": "_defaultOwner" },
        "moreComing": more_coming,
        "syncToken": token,
        "records": records,
    }] })
}

fn names(records: &[CloudKitRecord]) -> Vec<&str> {
    records.iter().map(|r| r.record_name.as_str()).collect()
}

fn tag(records: &[CloudKitRecord], name: &str) -> String {
    records
        .iter()
        .find(|r| r.record_name == name)
        .and_then(|r| r.record_change_tag.clone())
        .unwrap()
}

#[test]
fn same_record_twice_in_one_page_is_kept_once() {
    let database = db(scripted(vec![page(
        vec![
            note("A", "1", None, "YQ=="),
            note("B", "1", None, "Yg=="),
            note("A", "2", None, "YQ=="),
        ],
        false,
        "t1",
    )]));
    let changes = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    assert_eq!(names(&changes.records), ["A", "B"]);
    // No timestamps to compare: the later occurrence wins.
    assert_eq!(tag(&changes.records, "A"), "2");
}

#[test]
fn a_record_on_two_pages_is_kept_once_at_its_first_position() {
    let database = db(scripted(vec![
        page(
            vec![note("A", "1", Some(100), "YQ=="), note("B", "1", Some(100), "Yg==")],
            true,
            "t1",
        ),
        page(
            vec![note("C", "1", Some(100), "Yw=="), note("A", "1", Some(100), "YQ==")],
            false,
            "t2",
        ),
    ]));
    let mut pages = Vec::new();
    let changes = database.fetch_all_note_records(None, &mut |n| pages.push(n)).unwrap();
    assert_eq!(names(&changes.records), ["A", "B", "C"]);
    assert_eq!(changes.sync_token.as_deref(), Some("t2"));
    // Progress still reports what each page carried.
    assert_eq!(pages, [2, 2]);
}

#[test]
fn a_later_modified_timestamp_wins_whatever_the_page_order() {
    let database = db(scripted(vec![
        page(vec![note("A", "new", Some(200), "bmV3")], true, "t1"),
        page(vec![note("A", "old", Some(100), "b2xk")], false, "t2"),
    ]));
    let changes = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    assert_eq!(names(&changes.records), ["A"]);
    assert_eq!(tag(&changes.records, "A"), "new");

    let database = db(scripted(vec![
        page(vec![note("A", "old", Some(100), "b2xk")], true, "t1"),
        page(vec![note("A", "new", Some(200), "bmV3")], false, "t2"),
    ]));
    let changes = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    assert_eq!(tag(&changes.records, "A"), "new");
}

#[test]
fn differing_change_tags_with_equal_timestamps_take_the_later_page() {
    let database = db(scripted(vec![
        page(vec![note("A", "zz", Some(100), "YQ==")], true, "t1"),
        page(vec![note("A", "aa", Some(100), "YQ==")], false, "t2"),
    ]));
    let changes = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    // Tags are opaque: "aa" < "zz" doesn't matter, the later read does.
    assert_eq!(tag(&changes.records, "A"), "aa");
}

#[test]
fn a_later_tombstone_replaces_the_live_copy() {
    let database = db(scripted(vec![
        page(vec![note("A", "1", Some(100), "YQ==")], true, "t1"),
        page(vec![tombstone("A")], false, "t2"),
    ]));
    let changes = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    assert_eq!(changes.records.len(), 1);
    assert!(changes.records[0].is_deleted());
}

#[test]
fn without_timestamps_the_later_page_wins() {
    let database = db(scripted(vec![
        page(
            vec![note("A", "1", None, "YQ=="), note("B", "1", None, "Yg==")],
            true,
            "t1",
        ),
        page(vec![note("A", "2", None, "YQ==")], false, "t2"),
    ]));
    let changes = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    assert_eq!(names(&changes.records), ["A", "B"]);
    assert_eq!(tag(&changes.records, "A"), "2");
}

#[test]
fn the_pure_rule() {
    let parse = |v: Value| -> CloudKitRecord { serde_json::from_value(v).unwrap() };
    let records = vec![
        parse(
            json!({ "recordName": "A", "recordType": "Note", "fields": {}, "recordChangeTag": "1",
                      "modified": { "timestamp": 300 } }),
        ),
        parse(json!({ "recordName": "B", "recordType": "Note", "fields": {}, "recordChangeTag": "1" })),
        parse(
            json!({ "recordName": "A", "recordType": "Note", "fields": {}, "recordChangeTag": "2",
                      "modified": { "timestamp": 200 } }),
        ),
        parse(json!({ "recordName": "B", "recordType": "Note", "fields": {}, "recordChangeTag": "2" })),
    ];
    let out = dedupe_zone_records(records);
    assert_eq!(names(&out), ["A", "B"]);
    assert_eq!(tag(&out, "A"), "1"); // newer modified wins over later position
    assert_eq!(tag(&out, "B"), "2"); // no timestamps: later position wins
    assert!(dedupe_zone_records(Vec::new()).is_empty());
}

// --- private vs shared ------------------------------------------------------

/// The private zone and one shared zone (`_ownerA`) both list a record named
/// `SAME`; the shared zone lists it on both of its pages without a body, and
/// the `records/lookup` backfill answers it twice as well.
fn private_and_shared() -> MockTransport {
    let mut shared_page = 0;
    MockTransport::new(move |path, body| {
        if path.contains("/private/changes/zone") {
            return Ok(page(vec![note("SAME", "p1", Some(100), "cHJpdmF0ZQ==")], false, "tp"));
        }
        if path.contains("/shared/changes/database") {
            return Ok(json!({ "moreComing": false, "syncToken": "db1", "zones": [
                { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" } },
            ] }));
        }
        if path.contains("/shared/changes/zone") {
            shared_page += 1;
            let bodyless = json!({ "recordName": "SAME", "recordType": "Note", "recordChangeTag": "s1",
                                   "fields": {}, "modified": { "timestamp": 100 } });
            return Ok(json!({ "zones": [{
                "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" },
                "moreComing": shared_page == 1,
                "syncToken": format!("ts{shared_page}"),
                "records": [bodyless],
            }] }));
        }
        if path.contains("/shared/records/lookup") {
            assert_eq!(body["records"], json!([{ "recordName": "SAME" }]), "looked up once");
            return Ok(json!({ "records": [
                note("SAME", "s1", Some(100), "c2hhcmVk"),
                note("SAME", "s1", Some(100), "c2hhcmVk"),
            ] }));
        }
        panic!("unexpected request {path}")
    })
}

#[test]
fn the_same_record_name_in_private_and_shared_zones_is_two_records() {
    let database = Database::new(private_and_shared());
    let private = database.fetch_all_note_records(None, &mut no_pages()).unwrap();
    let shared = database
        .fetch_shared_note_records(&IndexMap::new(), &mut no_pages())
        .unwrap();
    assert_eq!(names(&private.records), ["SAME"]);
    assert_eq!(tag(&private.records, "SAME"), "p1");
    assert!(shared.skipped_zones.is_empty(), "{:?}", shared.skipped_zones);
    assert_eq!(shared.zones.len(), 1);
    assert_eq!(names(&shared.zones[0].records), ["SAME"]);
    assert_eq!(tag(&shared.zones[0].records, "SAME"), "s1");
    assert!(shared.zones[0].records[0].fields.contains_key("TextDataEncrypted"));
    assert_eq!(shared.zones[0].sync_token.as_deref(), Some("ts2"));
}

#[test]
fn a_shared_zone_listed_on_two_pages_is_fetched_once() {
    let database = db(scripted(vec![
        json!({ "moreComing": true, "syncToken": "p1", "zones": [
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" } },
        ] }),
        json!({ "moreComing": false, "syncToken": "p2", "zones": [
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerA" } },
            { "zoneID": { "zoneName": "Notes", "ownerRecordName": "_ownerB" } },
        ] }),
    ]));
    let zone_ids = database.fetch_shared_zone_ids().unwrap();
    let owners: Vec<_> = zone_ids
        .iter()
        .map(|z| z.owner_record_name.as_deref().unwrap())
        .collect();
    assert_eq!(owners, ["_ownerA", "_ownerB"]);
}
