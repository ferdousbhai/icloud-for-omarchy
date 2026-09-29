//! The whole non-UI app against the dev fake server over real HTTP, through
//! the same `MockTransport` that `ICLOUD_SESSION_MOCK=1` selects.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use icloud_photos::catalog::{Catalog, SYNC_TOKEN_KEY};
use icloud_photos::cloudkit::CloudKit;
use icloud_photos::config::Dirs;
use icloud_photos::sync::{Mode, sync};
use icloud_photos::thumbs::{Downloader, Job, Priority, Targets, fetch};
use icloud_photos::transport::{MockTransport, Transport};
use icloud_photos::upload::Uploader;
use support::fake_server::FakeServer;
use support::temp_dir;

#[test]
fn browse_download_delete_upload_and_resync() {
    let server = FakeServer::start(0, 60);
    let t = MockTransport::new(&server.url);
    let root = temp_dir("e2e");
    let dirs = Dirs::under(&root);
    let mut cat = Catalog::open(&dirs.catalog()).unwrap();
    let ck = CloudKit::connect(&t).unwrap();

    // Full sync pages through 60 assets, 50 per page.
    let r = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!((r.mode, r.assets, r.albums), (Mode::Full, 60, 3));
    assert_eq!(cat.count().unwrap(), 60);
    assert_eq!(
        cat.albums().unwrap().iter().map(|a| a.count).collect::<Vec<_>>(),
        vec![20, 12, 0]
    );
    let newest = cat.assets(None).unwrap()[0].clone();
    assert_eq!(newest.id, server.asset_ids()[0]);

    // A thumb is a real JPEG from the fake content host.
    let targets = Targets {
        dirs: dirs.clone(),
        library: root.join("Pictures/iCloud"),
    };
    let thumb = fetch(&t, &cat, &targets, &newest.id, Job::Thumb).unwrap();
    assert_eq!(&std::fs::read(&thumb).unwrap()[..2], b"\xFF\xD8");

    // Delete moves it to Recently Deleted with the current change tag.
    let m = ck.delete_asset(&newest.id, newest.change_tag.as_deref()).unwrap();
    cat.mark_deleted(&newest.id, m.change_tag.as_deref()).unwrap();
    // A stale tag conflicts.
    let second = cat.assets(None).unwrap()[0].clone();
    assert!(ck.delete_asset(&second.id, Some("tag-999")).is_err());

    // Something deleted on another device, and an upload from here.
    server.delete_elsewhere(&second.id);
    let file = root.join("IMG_UPLOAD.JPG");
    std::fs::write(&file, b"\xFF\xD8 pretend jpeg").unwrap();
    let up = Uploader::connect(&t).unwrap();
    let uploaded = up
        .upload(&file, "0f0f0f0f-0000-4000-8000-000000000001", &|_| {})
        .unwrap();
    assert!(!uploaded.duplicate);
    up.wait_for_ingest(&[uploaded.job_id.clone().unwrap()], Duration::from_secs(5), &|_| {})
        .unwrap();
    // The same bytes again are a duplicate.
    let again = up
        .upload(&file, "0f0f0f0f-0000-4000-8000-000000000002", &|_| {})
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.asset_id, uploaded.asset_id);

    // Incremental sync sees all three changes via changes/zone.
    let r = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!(r.mode, Mode::Incremental, "{:?}", r.fell_back);
    assert_eq!(cat.count().unwrap(), 60 - 2 + 1);
    assert!(cat.asset(&second.id).unwrap().unwrap().deleted);
    assert_eq!(
        cat.asset(&uploaded.asset_id).unwrap().unwrap().filename,
        "IMG_UPLOAD.JPG"
    );
    assert_eq!(cat.meta(SYNC_TOKEN_KEY).unwrap().as_deref(), Some("tok-4"));

    // Nothing new: an empty incremental sync.
    let r = sync(&ck, &mut cat, &|_| {}).unwrap();
    assert_eq!((r.mode, r.assets, r.removed), (Mode::Incremental, 0, 0));
}

#[test]
fn signed_out_is_sign_in_required_until_reauthenticated() {
    let server = FakeServer::start(0, 3);
    let t = MockTransport::new(&server.url);
    let mut cat = Catalog::open_in_memory().unwrap();
    let ck = CloudKit::connect(&t).unwrap();
    server.sign_out();
    assert!(sync(&ck, &mut cat, &|_| {}).unwrap_err().is_sign_in());
    t.reauthenticate().unwrap();
    assert_eq!(sync(&ck, &mut cat, &|_| {}).unwrap().assets, 3);
}

#[test]
fn the_download_pool_fetches_in_parallel_and_reports_each_job() {
    let server = FakeServer::start(0, 12);
    let t: Arc<dyn Transport> = Arc::new(MockTransport::new(&server.url));
    let root = temp_dir("pool");
    let dirs = Dirs::under(&root);
    let mut cat = Catalog::open(&dirs.catalog()).unwrap();
    sync(&CloudKit::connect(&*t).unwrap(), &mut cat, &|_| {}).unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    let pool = Downloader::start(
        t,
        dirs,
        root.join("lib"),
        3,
        Box::new(move |e| tx.lock().unwrap().send(e).unwrap()),
    );
    let ids = cat.missing_thumbs().unwrap();
    assert_eq!(ids.len(), 12);
    for id in &ids {
        pool.enqueue(id, Job::Thumb, Priority::Now);
        pool.enqueue(id, Job::Thumb, Priority::Now); // de-duplicated
    }
    pool.enqueue(&ids[0], Job::Original, Priority::Background);
    let mut done = 0;
    while done < 13 {
        let e = rx.recv_timeout(Duration::from_secs(30)).expect("download event");
        assert!(e.result.is_ok(), "{:?}", e.result);
        done += 1;
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "each job ran once"
    );
    assert!(cat.missing_thumbs().unwrap().is_empty());
    let orig = cat.asset(&ids[0]).unwrap().unwrap().local_path.unwrap();
    assert!(orig.starts_with(root.join("lib")) && orig.exists());
}
