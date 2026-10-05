mod support;

use icloud_photos::cloudkit::{CloudKit, Kind, LIST_ALL, Record, pair_assets};
use icloud_photos::transport::Error;
use serde_json::{Value, json};
use support::{CK_ROOT, FixtureTransport, fixture, library};

fn records(name: &str) -> Vec<Record> {
    fixture(name)["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Record::parse)
        .collect()
}

#[test]
fn pairs_assets_with_masters_and_decodes_fields() {
    let page = pair_assets(&records("assets_page1.json"));
    assert_eq!(page.assets.len(), 2);
    assert_eq!(page.orphans.len(), 1, "ASSET-003's master is on the next page");
    assert_eq!(page.orphans[0].master_id, "AX/m003+ccc");
    assert!(page.lone_masters.is_empty());

    let live = &page.assets[0];
    assert_eq!(live.id, "ASSET-001");
    assert_eq!(live.master.master_id, "AX/m001+aaa");
    assert_eq!(live.master.filename, "IMG_0001.HEIC", "filenameEnc is base64");
    assert_eq!(live.created, 1_757_000_000, "assetDate ms -> s");
    assert_eq!(
        (live.master.width, live.master.height, live.master.size),
        (4032, 3024, 3_456_789)
    );
    assert_eq!(live.master.kind, Kind::Photo);
    assert!(live.is_live(), "resOriginalVidComplRes marks a Live Photo");
    assert_eq!(live.change_tag.as_deref(), Some("t001"));
    assert!(live.master.thumb.as_ref().unwrap().url.contains("/thumb/"));
    assert!(live.master.medium.as_ref().unwrap().url.contains("/med/"));
    assert!(!live.deleted);

    let page2 = pair_assets(&records("assets_page2.json"));
    assert_eq!(page2.lone_masters.len(), 1);
    assert_eq!(page2.lone_masters[0].kind, Kind::Video);
    assert!(!page2.lone_masters[0].filename.is_empty());
}

#[test]
fn query_request_has_pyicloud_shape() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    ck.query(
        LIST_ALL,
        vec![icloud_photos::cloudkit::int_filter("startRank", 0)],
        None,
    )
    .unwrap();
    let call = &t.calls()[0];
    assert_eq!(
        call.url,
        format!(
            "{CK_ROOT}/database/1/com.apple.photos.cloud/production/private/records/query?remapEnums=true&getCurrentSyncToken=true"
        )
    );
    assert_eq!(call.body["zoneID"], json!({ "zoneName": "PrimarySync" }));
    assert_eq!(call.body["query"]["recordType"], LIST_ALL);
    assert_eq!(
        call.body["query"]["filterBy"][0],
        json!({ "fieldName": "startRank", "comparator": "EQUALS", "fieldValue": { "type": "INT64", "value": 0 } })
    );
    let keys: Vec<&str> = call.body["desiredKeys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for k in [
        "filenameEnc",
        "resOriginalRes",
        "resJPEGThumbRes",
        "resJPEGMedRes",
        "resOriginalVidComplRes",
        "masterRef",
        "assetDate",
    ] {
        assert!(keys.contains(&k), "desiredKeys lacks {k}");
    }
    assert!(call.body.get("continuationMarker").is_none());
}

#[test]
fn list_assets_pages_by_start_rank_until_empty() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    let mut pages = 0;
    ck.list_assets(LIST_ALL, &[], |_| {
        pages += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(pages, 3);
    let ranks: Vec<i64> = t
        .calls()
        .iter()
        .filter_map(|c| c.filter("startRank").and_then(Value::as_i64))
        .collect();
    assert_eq!(ranks, vec![0, 2, 4], "rank advances by the masters on each page");
    assert!(
        t.calls()
            .iter()
            .all(|c| c.filter("direction") == Some(&json!("ASCENDING")))
    );
}

#[test]
fn list_assets_follows_continuation_marker() {
    let t = FixtureTransport::new(|call| {
        let rank = call.filter("startRank").and_then(Value::as_i64);
        Ok(
            match (call.body.get("continuationMarker").and_then(Value::as_str), rank) {
                (None, Some(0)) => {
                    let mut v = fixture("assets_page1.json");
                    v["continuationMarker"] = json!("CONT-1");
                    v
                }
                (Some("CONT-1"), _) => fixture("assets_page2.json"),
                _ => fixture("assets_empty.json"),
            },
        )
    });
    let ck = CloudKit::connect(&t).unwrap();
    let mut seen = 0;
    ck.list_assets(LIST_ALL, &[], |r| {
        seen += r.len();
        Ok(())
    })
    .unwrap();
    let calls = t.calls();
    assert_eq!(calls[1].body["continuationMarker"], "CONT-1");
    assert_eq!(
        calls[1].filter("startRank"),
        Some(&json!(0)),
        "the marker re-sends the same query"
    );
    assert_eq!(seen, 5 + 3);
    // When the marker runs out, paging resumes by rank and ends on an empty page.
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[2].filter("startRank"),
        Some(&json!(4)),
        "rank counts every master already seen"
    );
}

#[test]
fn albums_flatten_folders_and_skip_root_and_deleted() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    let albums = ck.albums().unwrap();
    let names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["Italy 2025", "Family"]);
    let folder_call = t.calls().into_iter().find(|c| c.filter("parentId").is_some()).unwrap();
    assert_eq!(folder_call.filter("parentId"), Some(&json!("A-TRIPS-FOLDER")));
}

#[test]
fn album_members_use_relations_or_assets() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    let family = ck.album_members("A-FAMILY").unwrap();
    assert_eq!(
        family.iter().map(|r| r.asset_id.as_str()).collect::<Vec<_>>(),
        vec!["ASSET-001", "ASSET-002"]
    );
    assert_eq!(family[0].id, "ASSET-001-IN-A-FAMILY");
    let italy = ck.album_members("A-ITALY").unwrap();
    assert_eq!(italy.len(), 1);
    assert_eq!(italy[0].asset_id, "ASSET-004");
    assert!(italy[0].id.is_empty());
}

#[test]
fn album_members_keep_one_entry_per_asset_preferring_the_relation() {
    let rel = |asset: &str| {
        json!({ "recordName": format!("{asset}-IN-A"), "recordType": "CPLContainerRelation",
                "fields": { "containerId": { "value": "A" }, "itemId": { "value": asset } } })
    };
    let asset = |name: &str| json!({ "recordName": name, "recordType": "CPLAsset", "fields": {} });
    let page = json!({ "records": [
        asset("X1"), rel("X1"), asset("X2"), asset("X2"), rel("X3"), asset("X3"), rel("X1"),
        { "recordName": "M", "recordType": "CPLMaster", "fields": {} },
    ] });
    let t = FixtureTransport::new(move |call| {
        Ok(if call.filter("startRank") == Some(&json!(0)) {
            page.clone()
        } else {
            json!({ "records": [] })
        })
    });
    let ck = CloudKit::connect(&t).unwrap();
    let members = ck.album_members("A").unwrap();
    let got: Vec<(&str, &str)> = members.iter().map(|r| (r.asset_id.as_str(), r.id.as_str())).collect();
    assert_eq!(got, vec![("X1", "X1-IN-A"), ("X2", ""), ("X3", "X3-IN-A")]);
}

#[test]
fn owned_and_borrowed_parsing_agree() {
    let v = fixture("assets_page1.json");
    for r in v["records"].as_array().unwrap() {
        let (a, b) = (Record::parse(r).unwrap(), Record::from_value(r.clone()).unwrap());
        assert_eq!(
            (&a.name, &a.record_type, &a.change_tag, &a.fields),
            (&b.name, &b.record_type, &b.change_tag, &b.fields)
        );
    }
}

#[test]
fn zone_changes_parse_records_tombstones_and_token() {
    let t = FixtureTransport::new(|_| Ok(fixture("zone_changes_page2.json")));
    let ck = CloudKit::connect(&t).unwrap();
    let ch = ck.zone_changes(Some("TOKEN-MID")).unwrap();
    assert_eq!(ch.sync_token, "TOKEN-NEW");
    assert!(!ch.more_coming);
    assert!(
        ch.records[0].record_type.is_none() && ch.records[0].deleted,
        "tombstone"
    );
    let body = &t.calls()[0].body;
    assert_eq!(body["zones"][0]["zoneID"]["zoneName"], "PrimarySync");
    assert_eq!(body["zones"][0]["syncToken"], "TOKEN-MID");
    assert!(t.calls()[0].url.contains("/private/changes/zone?"));
}

#[test]
fn zone_level_error_is_an_error() {
    let t = FixtureTransport::new(|_| Ok(fixture("zone_changes_expired.json")));
    let ck = CloudKit::connect(&t).unwrap();
    match ck.zone_changes(Some("OLD")) {
        Err(Error::CloudKit { code, .. }) => assert_eq!(code, "CHANGE_TOKEN_EXPIRED"),
        other => panic!("expected a CloudKit error, got {other:?}"),
    }
}

#[test]
fn zone_sync_token_picks_primary_sync() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    assert_eq!(ck.zone_sync_token().unwrap().as_deref(), Some("TOKEN-FULL"));
}

#[test]
fn delete_matches_the_browser_capture() {
    let t = FixtureTransport::new(|_| Ok(fixture("write/delete_response.json")));
    let ck = CloudKit::connect(&t).unwrap();
    let m = ck.delete_asset("ASSET-002", Some("t002")).unwrap();
    assert_eq!(m.change_tag.as_deref(), Some("t002-after-delete"));
    let call = &t.calls()[0];
    assert_eq!(call.op, "records/modify");
    let browser = fixture("write/delete_request_browser.json");
    assert_eq!(call.body["atomic"], browser["atomic"]);
    assert_eq!(call.body["operations"], browser["operations"]);
    assert_eq!(call.body["zoneID"]["zoneName"], browser["zoneID"]["zoneName"]);
}

#[test]
fn delete_conflict_is_reported() {
    let t = FixtureTransport::new(|_| Ok(fixture("write/delete_conflict_response.json")));
    let ck = CloudKit::connect(&t).unwrap();
    match ck.delete_asset("ASSET-002", Some("stale")) {
        Err(Error::CloudKit { code, .. }) => assert_eq!(code, "CONFLICT"),
        other => panic!("expected CONFLICT, got {other:?}"),
    }
}

#[test]
fn delete_assets_batches_and_maps_each_record_by_name() {
    let t = FixtureTransport::new(|call| {
        // Answer out of order, with one per-record error.
        let ops = call.body["operations"].as_array().unwrap();
        let mut recs: Vec<Value> = ops
            .iter()
            .map(|op| {
                let name = op["record"]["recordName"].as_str().unwrap();
                if name == "B" {
                    json!({ "recordName": name, "serverErrorCode": "CONFLICT", "reason": "oplock" })
                } else {
                    json!({ "recordName": name, "recordType": "CPLAsset", "recordChangeTag": format!("{name}-new") })
                }
            })
            .collect();
        recs.reverse();
        Ok(json!({ "records": recs }))
    });
    let ck = CloudKit::connect(&t).unwrap();
    let names: Vec<String> = (0..450)
        .map(|i| if i == 1 { "B".into() } else { format!("A{i}") })
        .collect();
    let items: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), Some("t"))).collect();
    let out = ck.delete_assets(&items);
    assert_eq!(out.len(), 450);
    assert_eq!(out[0].as_ref().unwrap().change_tag.as_deref(), Some("A0-new"));
    assert!(matches!(&out[1], Err(Error::CloudKit { code, .. }) if code == "CONFLICT"));
    assert_eq!(out[449].as_ref().unwrap().name, "A449");
    let calls = t.calls();
    let sizes: Vec<usize> = calls
        .iter()
        .map(|c| c.body["operations"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, vec![200, 200, 50]);
    assert_eq!(calls[0].body["atomic"], false);
    assert_eq!(
        calls[0].body["operations"][0]["record"]["fields"]["isDeleted"]["value"],
        1
    );
}

#[test]
fn delete_assets_stops_sending_after_a_lapsed_sign_in() {
    let t = FixtureTransport::new(|_| Err(Error::SignInRequired));
    let ck = CloudKit::connect(&t).unwrap();
    let names: Vec<String> = (0..250).map(|i| format!("A{i}")).collect();
    let items: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
    let out = ck.delete_assets(&items);
    assert!(out.iter().all(|r| matches!(r, Err(e) if e.is_sign_in())));
    assert_eq!(t.calls().len(), 1);
}

#[test]
fn sign_in_required_propagates() {
    let t = FixtureTransport::new(|_| Err(Error::SignInRequired));
    let ck = CloudKit::connect(&t).unwrap();
    assert!(ck.albums().unwrap_err().is_sign_in());
}

fn master(fields: Value) -> Record {
    Record::parse(&json!({ "recordName": "AX/m", "recordType": "CPLMaster", "fields": fields })).unwrap()
}

fn res(url: &str) -> Value {
    json!({ "value": { "downloadURL": url, "size": 1 }, "type": "ASSETID" })
}

#[test]
fn kind_comes_from_the_uti_not_from_video_renditions() {
    use icloud_photos::cloudkit::MasterInfo;
    let s = |v: &str| json!({ "value": v, "type": "STRING" });
    // A Live Photo master carries resVid* for its video half: still a photo.
    let live = MasterInfo::from_record(&master(json!({
        "resOriginalRes": res("https://x/o"), "resOriginalFileType": s("public.heic"),
        "resOriginalVidComplRes": res("https://x/l"), "resOriginalVidComplFileType": s("com.apple.quicktime-movie"),
        "resVidSmallRes": res("https://x/vs"), "resVidSmallFileType": s("com.apple.quicktime-movie"),
        "resVidMedRes": res("https://x/vm"), "resVidMedFileType": s("com.apple.quicktime-movie"),
    })));
    assert_eq!(live.kind, Kind::Photo);
    assert!(live.live.is_some());
    // Even with no UTI at all, a paired video means a Live Photo.
    let bare_live = MasterInfo::from_record(&master(json!({
        "resOriginalVidComplRes": res("https://x/l"), "resVidSmallRes": res("https://x/vs"),
    })));
    assert_eq!(bare_live.kind, Kind::Photo);
    for (uti, kind) in [
        ("public.jpeg", Kind::Photo),
        ("public.heic", Kind::Photo),
        ("public.png", Kind::Photo),
        ("public.mpeg-4", Kind::Video),
        ("com.apple.quicktime-movie", Kind::Video),
    ] {
        let m = MasterInfo::from_record(&master(json!({
            "itemType": s(uti), "resOriginalRes": res("https://x/o"), "resVidSmallRes": res("https://x/vs"),
        })));
        assert_eq!(m.kind, kind, "{uti}");
    }
    // resOriginalFileType decides when itemType is missing.
    let mp4 = MasterInfo::from_record(&master(
        json!({ "resOriginalRes": res("https://x/o"), "resOriginalFileType": s("public.mpeg-4") }),
    ));
    assert_eq!(mp4.kind, Kind::Video);
}

#[test]
fn hidden_assets_count_as_removed() {
    let rec = Record::parse(&json!({
        "recordName": "A1", "recordType": "CPLAsset",
        "fields": { "masterRef": { "value": { "recordName": "M1" } }, "isHidden": { "value": 1 } },
    }))
    .unwrap();
    assert!(rec.is_hidden());
    assert!(icloud_photos::cloudkit::AssetPart::from_record(&rec).unwrap().deleted);
    assert!(icloud_photos::cloudkit::DESIRED_KEYS.contains(&"isHidden"));
}
