mod support;

use icloud_photos::catalog::Catalog;
use icloud_photos::catalog::PathKind;
use icloud_photos::cloudkit::CloudKit;
use icloud_photos::config::Dirs;
use icloud_photos::sync::sync;
use icloud_photos::thumbs::{
    Job, Targets, fetch, fetch_detailed, live_ext, original_dest, prune_cache, rename_noreplace,
};
use icloud_photos::transport::{Result, Transport};
use serde_json::Value;
use support::{FixtureTransport, fixture, library, temp_dir};

fn synced(t: &FixtureTransport, root: &std::path::Path) -> (Catalog, Targets) {
    let dirs = Dirs::under(root);
    let mut cat = Catalog::open(&dirs.catalog()).unwrap();
    sync(&CloudKit::connect(t).unwrap(), &mut cat, &|_| {}).unwrap();
    (
        cat,
        Targets {
            dirs,
            library: root.join("Pictures/iCloud"),
        },
    )
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
    assert_eq!(
        original_dest(&cat, &targets.library, &row).unwrap(),
        root.join("Pictures/iCloud/2025/09/IMG_0001.HEIC")
    );
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
    m.medium = Some(icloud_photos::cloudkit::Resource {
        url: "https://cvws.icloud-content.com/expired/med".into(),
        size: 1,
        file_type: None,
    });
    cat.update_master(&m).unwrap();
    let fresh = fixture("lookup_m002_fresh.json");
    let fresh_url = fresh
        .pointer("/records/0/fields/resJPEGMedRes/value/downloadURL")
        .and_then(Value::as_str)
        .unwrap();
    assert!(fresh_url.contains("/B2/"));
    t.serve(fresh_url, b"medium");

    let path = fetch(&t, &cat, &targets, "ASSET-002", Job::Medium).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"medium");
    assert!(t.ops().contains(&"records/lookup".to_string()));
    assert_eq!(
        cat.asset("ASSET-002").unwrap().unwrap().medium_url.as_deref(),
        Some(fresh_url)
    );
}

#[test]
fn missing_rendition_is_an_error_not_a_panic() {
    let root = temp_dir("missing");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    assert!(
        fetch(&t, &cat, &targets, "ASSET-001", Job::Thumb).is_err(),
        "404 from the content host"
    );
    assert!(fetch(&t, &cat, &targets, "NOPE", Job::Thumb).is_err());
}

#[test]
fn live_names() {
    assert_eq!(live_ext(Some("com.apple.quicktime-movie")), ".MOV");
    assert_eq!(live_ext(Some("public.mpeg-4")), ".MP4");
    assert_eq!(live_ext(None), ".MOV");
}

fn serve_originals(t: &FixtureTransport, cat: &Catalog) {
    t.serve(&url(cat, "ASSET-001", |r| r.orig_url.clone()), b"one");
    t.serve(&url(cat, "ASSET-001", |r| r.live_url.clone()), b"mov");
    t.serve(&url(cat, "ASSET-004", |r| r.orig_url.clone()), b"four");
}

#[test]
fn a_fresh_install_gets_its_directories() {
    let root = temp_dir("fresh");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    // Like icloud-session, the fixture transport creates no directories.
    t.serve("https://x/probe", b"x");
    let err = t
        .download("https://x/probe", &root.join("missing/probe.jpg"))
        .unwrap_err();
    assert!(
        matches!(err, icloud_photos::transport::Error::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound),
        "{err}"
    );

    t.serve(&url(&cat, "ASSET-001", |r| r.thumb_url.clone()), b"thumb");
    t.serve(&url(&cat, "ASSET-001", |r| r.medium_url.clone()), b"medium");
    serve_originals(&t, &cat);
    assert!(!root.join("cache").exists() && !targets.library.exists());
    assert_eq!(
        fetch(&t, &cat, &targets, "ASSET-001", Job::Thumb).unwrap(),
        root.join("cache/thumbs/ASSET-001.jpg")
    );
    assert_eq!(
        fetch(&t, &cat, &targets, "ASSET-001", Job::Medium).unwrap(),
        root.join("cache/medium/ASSET-001.jpg")
    );
    assert_eq!(
        fetch(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap(),
        targets.library.join("2025/09/IMG_0001.HEIC")
    );
    let leftovers: Vec<_> = std::fs::read_dir(targets.library.join("2025/09"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 2, "photo and video, no temp files: {leftovers:?}");
}

#[test]
fn files_already_on_disk_are_never_overwritten() {
    let root = temp_dir("disk");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    serve_originals(&t, &cat);
    // A lost catalog, or the user's own files: neither is in the catalog.
    let month = targets.library.join("2025/09");
    std::fs::create_dir_all(&month).unwrap();
    std::fs::write(month.join("IMG_0001.HEIC"), b"mine").unwrap();
    std::fs::write(month.join("IMG_0001 (2).MOV"), b"my video").unwrap();

    let photo = fetch(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap();
    // (2) is out: its video name is taken. The pair moves to (3) together.
    assert_eq!(photo, month.join("IMG_0001 (3).HEIC"));
    assert_eq!(
        cat.asset("ASSET-001").unwrap().unwrap().live_path,
        Some(month.join("IMG_0001 (3).MOV"))
    );
    assert_eq!(std::fs::read(month.join("IMG_0001.HEIC")).unwrap(), b"mine");
    assert_eq!(std::fs::read(month.join("IMG_0001 (2).MOV")).unwrap(), b"my video");
    // The next same-named asset still avoids every one of them.
    assert_eq!(
        fetch(&t, &cat, &targets, "ASSET-004", Job::Original).unwrap(),
        month.join("IMG_0001 (2).HEIC")
    );
}

#[test]
fn a_live_video_never_lands_on_another_assets_file() {
    let root = temp_dir("live-collide");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    serve_originals(&t, &cat);
    let month = targets.library.join("2025/09");
    // Another asset already owns IMG_0001.MOV (say, a video of that name).
    cat.set_path("ASSET-003", PathKind::Original, Some(&month.join("IMG_0001.MOV")))
        .unwrap();
    fetch(&t, &cat, &targets, "ASSET-004", Job::Original).unwrap();
    let photo = fetch(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap();
    let row = cat.asset("ASSET-001").unwrap().unwrap();
    assert_eq!(photo, month.join("IMG_0001 (2).HEIC"));
    assert_eq!(
        row.live_path,
        Some(month.join("IMG_0001 (2).MOV")),
        "the video shares the photo's unique stem"
    );
}

/// Holds every download long enough for concurrent jobs to overlap.
struct Slow<'a>(&'a FixtureTransport);

impl Transport for Slow<'_> {
    fn service_url(&self, key: &str) -> Result<String> {
        self.0.service_url(key)
    }
    fn post_json(&self, url: &str, body: &Value) -> Result<Value> {
        self.0.post_json(url, body)
    }
    fn post_file(&self, url: &str, content_type: &str, path: &std::path::Path) -> Result<Value> {
        self.0.post_file(url, content_type, path)
    }
    fn download(&self, url: &str, dest: &std::path::Path) -> Result<u64> {
        std::thread::sleep(std::time::Duration::from_millis(150));
        self.0.download(url, dest)
    }
}

#[test]
fn concurrent_downloads_of_the_same_name_get_different_files() {
    let root = temp_dir("concurrent");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    serve_originals(&t, &cat);
    let slow = Slow(&t);
    let (a, b) = std::thread::scope(|s| {
        let job = |id: &'static str| {
            let (slow, targets) = (&slow, &targets);
            s.spawn(move || {
                let cat = Catalog::open(&targets.dirs.catalog()).unwrap();
                fetch(slow, &cat, targets, id, Job::Original).unwrap()
            })
        };
        let (a, b) = (job("ASSET-001"), job("ASSET-004"));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(a, b);
    assert_eq!(std::fs::read(&a).unwrap(), b"one");
    assert_eq!(std::fs::read(&b).unwrap(), b"four");
}

#[test]
fn a_failed_live_video_keeps_the_photo_and_retries_only_the_video() {
    let root = temp_dir("live-retry");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    let (orig, live) = (
        url(&cat, "ASSET-001", |r| r.orig_url.clone()),
        url(&cat, "ASSET-001", |r| r.live_url.clone()),
    );
    t.serve(&orig, b"heic");

    let first = fetch_detailed(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap();
    assert!(first.live_error.is_some(), "the video 404s");
    let row = cat.asset("ASSET-001").unwrap().unwrap();
    assert_eq!(row.local_path, Some(first.path.clone()), "the photo is recorded anyway");
    assert_eq!(row.live_path, None);

    t.serve(&live, b"mov");
    let n = t.downloads.lock().unwrap().len();
    let second = fetch_detailed(&t, &cat, &targets, "ASSET-001", Job::Original).unwrap();
    assert!(second.live_error.is_none());
    assert_eq!(second.path, first.path);
    assert_eq!(
        t.downloads.lock().unwrap()[n..],
        [live],
        "only the video is fetched again"
    );
    assert_eq!(
        cat.asset("ASSET-001").unwrap().unwrap().live_path,
        Some(first.path.with_extension("MOV"))
    );
}

#[test]
fn rename_noreplace_refuses_to_replace() {
    let dir = temp_dir("noreplace");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::write(&a, b"new").unwrap();
    std::fs::write(&b, b"old").unwrap();
    assert_eq!(
        rename_noreplace(&a, &b).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&b).unwrap(), b"old");
    std::fs::remove_file(&b).unwrap();
    rename_noreplace(&a, &b).unwrap();
    assert_eq!(std::fs::read(&b).unwrap(), b"new");
    assert!(!a.exists());
}

#[test]
fn prune_drops_removed_assets_and_the_oldest_mediums() {
    let root = temp_dir("prune");
    let t = FixtureTransport::new(library);
    let (cat, targets) = synced(&t, &root);
    for id in ["ASSET-001", "ASSET-002", "ASSET-004"] {
        t.serve(&url(&cat, id, |r| r.thumb_url.clone()), b"thumb");
        t.serve(&url(&cat, id, |r| r.medium_url.clone()), &[0u8; 100]);
        fetch(&t, &cat, &targets, id, Job::Thumb).unwrap();
        fetch(&t, &cat, &targets, id, Job::Medium).unwrap();
    }
    let medium = |id: &str| root.join(format!("cache/medium/{id}.jpg"));
    let age = |id: &str, secs: u64| {
        let f = std::fs::File::options().write(true).open(medium(id)).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
            .unwrap();
    };
    age("ASSET-002", 300);
    age("ASSET-004", 100);
    cat.mark_deleted("ASSET-001", None).unwrap();

    // ASSET-001 left the library; then 200 bytes of medium JPEGs over a
    // 150-byte cap lose the oldest.
    let removed = prune_cache(&cat, &targets.dirs, 150).unwrap();
    assert_eq!(removed, 3);
    let gone = cat.asset("ASSET-001").unwrap().unwrap();
    assert_eq!((gone.thumb_path, gone.medium_path), (None, None));
    assert!(!root.join("cache/thumbs/ASSET-001.jpg").exists() && !medium("ASSET-001").exists());
    assert!(!medium("ASSET-002").exists());
    assert_eq!(cat.asset("ASSET-002").unwrap().unwrap().medium_path, None);
    assert!(
        root.join("cache/thumbs/ASSET-002.jpg").exists(),
        "thumbs of live assets stay"
    );
    assert!(medium("ASSET-004").exists());
}
