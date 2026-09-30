mod support;

use icloud_photos::catalog::{Catalog, SYNC_TOKEN_KEY};
use icloud_photos::cloudkit::CloudKit;
use icloud_photos::sync::{Mode, sync};
use icloud_photos::transport::{Error, Result};
use serde_json::Value;
use support::{Call, FixtureTransport, fixture, library};

fn with_changes(call: &Call) -> Result<Value> {
    if call.op == "changes/zone" {
        return Ok(match call.body.pointer("/zones/0/syncToken").and_then(Value::as_str) {
            Some("TOKEN-FULL") => fixture("zone_changes_page1.json"),
            Some("TOKEN-MID") => fixture("zone_changes_page2.json"),
            _ => fixture("zone_changes_expired.json"),
        });
    }
    library(call)
}

fn ids(cat: &Catalog, album: Option<&str>) -> Vec<String> {
    cat.assets(album).unwrap().into_iter().map(|r| r.id).collect()
}

#[test]
fn full_sync_fills_the_catalog() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    let report = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!(report.mode, Mode::Full);
    assert_eq!(report.assets, 4);
    assert_eq!(report.albums, 2);
    // Newest first.
    assert_eq!(
        ids(&cat, None),
        vec!["ASSET-003", "ASSET-002", "ASSET-004", "ASSET-001"]
    );
    assert_eq!(cat.meta(SYNC_TOKEN_KEY).unwrap().as_deref(), Some("TOKEN-FULL"));
    // ASSET-003 was split from its master across pages and joined without a lookup.
    assert!(!t.ops().contains(&"records/lookup".to_string()));
    let video = cat.asset("ASSET-003").unwrap().unwrap();
    assert_eq!(video.filename, "IMG_0003.MOV");
    assert_eq!(video.kind, icloud_photos::cloudkit::Kind::Video);
    let live = cat.asset("ASSET-001").unwrap().unwrap();
    assert!(live.is_live && live.live_url.is_some());

    let albums = cat.albums().unwrap();
    let family = albums.iter().find(|a| a.name == "Family").unwrap();
    assert_eq!(family.count, 2);
    assert_eq!(ids(&cat, Some("A-FAMILY")), vec!["ASSET-002", "ASSET-001"]);
    assert_eq!(ids(&cat, Some("A-ITALY")), vec!["ASSET-004"]);
    // The token is read before the listing, so nothing changed meanwhile is lost.
    let ops = t.ops();
    assert_eq!(ops.first().map(String::as_str), Some("zones/list"));
}

#[test]
fn incremental_sync_applies_changes_and_advances_the_token() {
    let t = FixtureTransport::new(with_changes);
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    sync(&ck, &mut cat, &|_| {}).unwrap();
    let before = t.calls().len();

    let report = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!(report.mode, Mode::Incremental);
    assert!(report.fell_back.is_none());
    let ops: Vec<String> = t.ops()[before..].to_vec();
    assert_eq!(
        ops,
        vec!["changes/zone", "changes/zone", "records/lookup"],
        "two pages, then one master lookup for ASSET-006"
    );
    assert_eq!(cat.meta(SYNC_TOKEN_KEY).unwrap().as_deref(), Some("TOKEN-NEW"));

    // ASSET-005 arrived with its master; ASSET-006 needed a lookup;
    // ASSET-002 moved to Recently Deleted; ASSET-003 was tombstoned;
    // ASSET-004 was hidden.
    assert_eq!(ids(&cat, None), vec!["ASSET-006", "ASSET-005", "ASSET-001"]);
    assert_eq!(cat.asset("ASSET-006").unwrap().unwrap().filename, "IMG_0006.JPG");
    let gone = cat.asset("ASSET-002").unwrap().unwrap();
    assert!(gone.deleted);
    assert_eq!(gone.change_tag.as_deref(), Some("t002-deleted"));
    // Album renamed, relation added.
    let family = cat.albums().unwrap().into_iter().find(|a| a.id == "A-FAMILY").unwrap();
    assert_eq!(family.name, "Family & friends");
    assert_eq!(ids(&cat, Some("A-FAMILY")), vec!["ASSET-005", "ASSET-001"]);
    assert!(
        ids(&cat, Some("A-ITALY")).is_empty(),
        "the hidden ASSET-004 leaves its album too"
    );
}

#[test]
fn changes_request_is_hidden_and_hidden_assets_leave_the_library() {
    use icloud_photos::catalog::PathKind;
    let t = FixtureTransport::new(with_changes);
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    sync(&ck, &mut cat, &|_| {}).unwrap();
    let tmp = support::temp_dir("hidden");
    let dir = tmp.path();
    let local = dir.join("IMG_0001.HEIC");
    std::fs::write(&local, b"heic").unwrap();
    cat.set_path("ASSET-004", PathKind::Original, Some(&local)).unwrap();
    let before = t.calls().len();

    sync(&ck, &mut cat, &|_| {}).unwrap();
    let changes: Vec<_> = t.calls()[before..]
        .iter()
        .filter(|c| c.op == "changes/zone")
        .cloned()
        .collect();
    let keys = changes[0]
        .body
        .pointer("/zones/0/desiredKeys")
        .and_then(Value::as_array)
        .unwrap();
    assert!(
        keys.iter().any(|k| k == "isHidden"),
        "changes/zone must ask for isHidden"
    );
    let hidden = cat.asset("ASSET-004").unwrap().unwrap();
    assert!(hidden.deleted, "hidden assets are out of All Photos");
    assert!(!ids(&cat, None).contains(&"ASSET-004".to_string()));
    assert_eq!(hidden.local_path.as_deref(), Some(local.as_path()));
    assert!(local.exists(), "hiding never deletes the downloaded file");
}

#[test]
fn incremental_failure_falls_back_to_a_full_listing() {
    let t = FixtureTransport::new(with_changes);
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    cat.set_meta(SYNC_TOKEN_KEY, Some("SOMETHING-OLD")).unwrap();
    let report = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!(report.mode, Mode::Full);
    assert!(report.fell_back.unwrap().contains("CHANGE_TOKEN_EXPIRED"));
    assert_eq!(cat.meta(SYNC_TOKEN_KEY).unwrap().as_deref(), Some("TOKEN-FULL"));
    assert_eq!(cat.count().unwrap(), 4);
}

#[test]
fn http_error_on_changes_endpoint_also_falls_back() {
    let t = FixtureTransport::new(|call| {
        if call.op == "changes/zone" {
            return Err(Error::Http {
                status: 400,
                body: "BAD_REQUEST".into(),
            });
        }
        library(call)
    });
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    cat.set_meta(SYNC_TOKEN_KEY, Some("TOKEN-FULL")).unwrap();
    let report = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!(report.mode, Mode::Full);
}

#[test]
fn sign_in_required_does_not_fall_back() {
    let t = FixtureTransport::new(|call| {
        if call.op == "changes/zone" {
            Err(Error::SignInRequired)
        } else {
            library(call)
        }
    });
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    cat.set_meta(SYNC_TOKEN_KEY, Some("TOKEN-FULL")).unwrap();
    assert!(sync(&ck, &mut cat, &|_| {}).unwrap_err().is_sign_in());
    assert_eq!(t.ops(), vec!["changes/zone"]);
    assert_eq!(
        cat.meta(SYNC_TOKEN_KEY).unwrap().as_deref(),
        Some("TOKEN-FULL"),
        "token untouched"
    );
}

#[test]
fn full_sync_marks_assets_gone_from_icloud() {
    let t = FixtureTransport::new(library);
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    sync(&ck, &mut cat, &|_| {}).unwrap();
    // Pretend a stale asset from an earlier run exists locally.
    let mut stale = cat.asset("ASSET-001").unwrap().unwrap();
    stale.id = "ASSET-STALE".into();
    let stale_asset = icloud_photos::cloudkit::Asset {
        id: stale.id.clone(),
        change_tag: None,
        created: stale.created,
        deleted: false,
        master: icloud_photos::cloudkit::MasterInfo {
            master_id: "AX/stale".into(),
            filename: "old.jpg".into(),
            size: 1,
            width: 1,
            height: 1,
            kind: icloud_photos::cloudkit::Kind::Photo,
            original: None,
            thumb: None,
            medium: None,
            live: None,
        },
    };
    cat.upsert_asset(&stale_asset).unwrap();
    cat.set_meta(SYNC_TOKEN_KEY, None).unwrap();
    let report = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!(report.removed, 1);
    assert!(cat.asset("ASSET-STALE").unwrap().unwrap().deleted);
}

#[test]
fn a_failed_full_sync_writes_nothing() {
    let t = FixtureTransport::new(|call| {
        if call.record_type() == Some("CPLContainerRelationLiveByAssetDate") {
            return Err(Error::Http {
                status: 503,
                body: "try later".into(),
            });
        }
        library(call)
    });
    let ck = CloudKit::connect(&t).unwrap();
    let mut cat = Catalog::open_in_memory().unwrap();
    assert!(sync(&ck, &mut cat, &|_| {}).is_err());
    assert_eq!(cat.count().unwrap(), 0);
    assert_eq!(cat.meta(SYNC_TOKEN_KEY).unwrap(), None);
}
