mod support;

use icloud_photos::catalog::Catalog;
use icloud_photos::cloudkit::CloudKit;
use icloud_photos::config::Dirs;
use icloud_photos::sync::sync;
use icloud_photos::thumbs::{Job, Targets, fetch, live_dest, original_dest, year_month};
use serde_json::Value;
use support::{FixtureTransport, fixture, library, temp_dir};

fn synced(t: &FixtureTransport, root: &std::path::Path) -> (Catalog, Targets) {
    let dirs = Dirs::under(root);
    let mut cat = Catalog::open(&dirs.catalog()).unwrap();
    sync(&CloudKit::connect(t).unwrap(), &mut cat, &|_| {}).unwrap();
    (cat, Targets { dirs, library: root.join("Pictures/iCloud") })
}

fn url(cat: &Catalog, id: &str, f: impl Fn(&icloud_photos::catalog::Row) -> Option<String>) -> String {
    f(&cat.asset(id).unwrap().unwrap()).unwrap()
}

#[test]
fn thumb_comes_from_apples_derivative_into_the_cache() {
    let root = temp_dir("thumb");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    t.serve(&url(&cat, "ASSET-001", |r| r.thumb_url.clone()), b"\xFF\xD8thumb");

    let path = fetch(&t, &cat, &targets, "ASSET-001", Job::Thumb).unwrap();
    assert_eq!(path, root.join("cache/thumbs/ASSET-001.jpg"));
    assert_eq!(std::fs::read(&path).unwrap(), b"\xFF\xD8thumb");
    assert_eq!(cat.asset("ASSET-001").unwrap().unwrap().thumb_path, Some(path.clone()));
    // Second time: already there, no download.
    let n = t.downloads.lock().unwrap().len();
    fetch(&t, &cat, &targets, "ASSET-001", Job::Thumb).unwrap();
    assert_eq!(t.downloads.lock().unwrap().len(), n);
}

#[test]
fn original_of_a_live_photo_brings_its_video() {
    let root = temp_dir("live");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    t.serve(&url(&cat, "ASSET-001", |r| r.orig_url.clone()), b"heic");
    t.serve(&url(&cat, "ASSET-001", |r| r.live_url.clone()), b"mov");

    let path = fetch(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap();
    assert_eq!(path, root.join("Pictures/iCloud/2025/09/IMG_0001.HEIC"));
    let row = cat.asset("ASSET-001").unwrap().unwrap();
    assert_eq!(row.local_path, Some(path.clone()));
    assert_eq!(row.live_path, Some(root.join("Pictures/iCloud/2025/09/IMG_0001.MOV")));
    assert_eq!(std::fs::read(row.live_path.unwrap()).unwrap(), b"mov");
}

#[test]
fn same_filename_in_the_same_month_gets_a_suffix() {
    let root = temp_dir("collide");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    // ASSET-001 and ASSET-004 are both IMG_0001.HEIC from September 2025.
    t.serve(&url(&cat, "ASSET-001", |r| r.orig_url.clone()), b"one");
    t.serve(&url(&cat, "ASSET-001", |r| r.live_url.clone()), b"mov");
    t.serve(&url(&cat, "ASSET-004", |r| r.orig_url.clone()), b"four");
    fetch(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap();
    let second = fetch(&t, &cat, &targets, "ASSET-004", Job::Original).unwrap();
    assert_eq!(second, root.join("Pictures/iCloud/2025/09/IMG_0001 (2).HEIC"));
    // The first asset keeps its own name on a re-plan.
    let row = cat.asset("ASSET-001").unwrap().unwrap();
    assert_eq!(original_dest(&cat, &targets.library, &row).unwrap(), root.join("Pictures/iCloud/2025/09/IMG_0001.HEIC"));
}

#[test]
fn an_expired_url_is_refreshed_once() {
    let root = temp_dir("expired");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    // Make the stored URL look expired; lookup_m002_fresh.json has /B2/ URLs.
    let row = cat.asset("ASSET-002").unwrap().unwrap();
    let mut m = icloud_photos::cloudkit::MasterInfo {
        master_id: row.master_id.clone(),
        filename: row.filename.clone(),
        size: row.size,
        width: row.w,
        height: row.h,
        kind: row.kind,
        original: None,
        thumb: None,
        medium: None,
        live: None,
    };
    m.medium = Some(icloud_photos::cloudkit::Resource { url: "https://cvws.icloud-content.com/expired/med".into(), size: 1, file_type: None });
    cat.update_master(&m).unwrap();
    let fresh = fixture("lookup_m002_fresh.json");
    let fresh_url = fresh.pointer("/records/0/fields/resJPEGMedRes/value/downloadURL").and_then(Value::as_str).unwrap();
    assert!(fresh_url.contains("/B2/"));
    t.serve(fresh_url, b"medium");

    let path = fetch(&t, &cat, &targets, "ASSET-002", Job::Medium).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"medium");
    assert!(t.ops().contains(&"records/lookup".to_string()));
    assert_eq!(cat.asset("ASSET-002").unwrap().unwrap().medium_url.as_deref(), Some(fresh_url));
}

#[test]
fn missing_rendition_is_an_error_not_a_panic() {
    let root = temp_dir("missing");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    assert!(fetch(&t, &cat, &targets, "ASSET-001", Job::Thumb).is_err(), "404 from the content host");
    assert!(fetch(&t, &cat, &targets, "NOPE", Job::Thumb).is_err());
}

#[test]
fn dates_and_live_names() {
    assert_eq!(year_month(0), (1970, 1));
    assert_eq!(year_month(1_757_000_000), (2025, 9));
    assert_eq!(year_month(951_782_400), (2000, 2), "leap day 2000-02-29");
    assert_eq!(year_month(-1), (1969, 12));
    let p = std::path::Path::new("/x/IMG_1.HEIC");
    assert_eq!(live_dest(p, Some("com.apple.quicktime-movie")), std::path::Path::new("/x/IMG_1.MOV"));
    assert_eq!(live_dest(p, None), std::path::Path::new("/x/IMG_1.MOV"));
}
