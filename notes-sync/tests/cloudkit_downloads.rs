//! `client::DownloadQueue`: attachment downloads run in line, in order, over
//! a transport without a shared downloader (the recorded scenarios), and on
//! `DOWNLOAD_WORKERS` background threads over one that has one (live).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use icloud_notes_sync::cloudkit::client::{DOWNLOAD_WORKERS, DownloadQueue};
use icloud_notes_sync::cloudkit::transport::SharedDownloader;
use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use serde_json::Value;

#[derive(Default)]
struct Stats {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    done: Mutex<Vec<(String, PathBuf)>>,
    callers: Mutex<Vec<std::thread::ThreadId>>,
}

/// Downloads take a few ms; a URL ending in `/403` answers HTTP 403.
struct FakeTransport {
    shared: bool,
    stats: Arc<Stats>,
}

fn fetch(stats: &Stats, url: &str, dest: &Path) -> Result<u64, CkError> {
    stats.callers.lock().unwrap().push(std::thread::current().id());
    let now = stats.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    stats.peak.fetch_max(now, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(5));
    stats.in_flight.fetch_sub(1, Ordering::SeqCst);
    if url.ends_with("/403") {
        return Err(CkError::Http {
            status: 403,
            body: String::new(),
        });
    }
    stats.done.lock().unwrap().push((url.to_owned(), dest.to_owned()));
    Ok(1)
}

impl Transport for FakeTransport {
    fn post_json(&self, path: &str, _body: &Value) -> Result<Value, CkError> {
        panic!("unexpected POST {path}")
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        fetch(&self.stats, url, dest)
    }

    fn shared_downloader(&self) -> Option<SharedDownloader> {
        let stats = self.stats.clone();
        self.shared
            .then(|| Arc::new(move |url: &str, dest: &Path| fetch(&stats, url, dest)) as SharedDownloader)
    }
}

fn database(shared: bool) -> (Database<FakeTransport>, Arc<Stats>) {
    let stats = Arc::new(Stats::default());
    (
        Database::new(FakeTransport {
            shared,
            stats: stats.clone(),
        }),
        stats,
    )
}

fn job(i: usize) -> (String, PathBuf) {
    (
        format!("https://cvws.icloud-content.com/B/{i}"),
        PathBuf::from(format!("/vault/a/{i}")),
    )
}

#[test]
fn without_a_shared_downloader_downloads_run_in_line_in_order() {
    let (db, stats) = database(false);
    let mut queue = DownloadQueue::new(&db);
    for i in 0..5 {
        let (url, dest) = job(i);
        queue.push(url, dest).unwrap();
        assert_eq!(stats.done.lock().unwrap().len(), i + 1, "ran when queued");
    }
    queue.finish().unwrap();
    assert_eq!(*stats.done.lock().unwrap(), (0..5).map(job).collect::<Vec<_>>());
    let me = std::thread::current().id();
    assert!(stats.callers.lock().unwrap().iter().all(|t| *t == me));
}

#[test]
fn with_a_shared_downloader_downloads_overlap_up_to_the_worker_count() {
    let (db, stats) = database(true);
    let mut queue = DownloadQueue::new(&db);
    for i in 0..24 {
        let (url, dest) = job(i);
        queue.push(url, dest).unwrap();
    }
    queue.finish().unwrap();
    let mut done = stats.done.lock().unwrap().clone();
    done.sort_by_key(|(_, dest)| dest.clone());
    let mut want: Vec<_> = (0..24).map(job).collect();
    want.sort_by_key(|(_, dest)| dest.clone());
    assert_eq!(done, want);
    let peak = stats.peak.load(Ordering::SeqCst);
    assert!((2..=DOWNLOAD_WORKERS).contains(&peak), "peak {peak}");
}

#[test]
fn a_destination_already_queued_is_fetched_once() {
    for shared in [false, true] {
        let (db, stats) = database(shared);
        let mut queue = DownloadQueue::new(&db);
        let (url, dest) = job(1);
        queue.push(url.clone(), dest.clone()).unwrap();
        queue.push(url, dest).unwrap();
        queue.finish().unwrap();
        assert_eq!(stats.done.lock().unwrap().len(), 1, "shared: {shared}");
    }
}

fn assert_attachment_download_failed(e: CkError) {
    match e {
        CkError::RequestFailed(m) => assert_eq!(m, "Attachment download failed: HTTP 403"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failed_download_fails_the_push_in_line() {
    let (db, _) = database(false);
    let mut queue = DownloadQueue::new(&db);
    let e = queue
        .push("https://cvws.icloud-content.com/B/403".into(), "/vault/x".into())
        .unwrap_err();
    assert_attachment_download_failed(e);
}

#[test]
fn a_failed_background_download_surfaces_at_a_later_push_or_at_finish() {
    let (db, stats) = database(true);
    let mut queue = DownloadQueue::new(&db);
    queue
        .push("https://cvws.icloud-content.com/B/403".into(), "/vault/x".into())
        .unwrap();
    let mut failed = None;
    for i in 0..50 {
        std::thread::sleep(Duration::from_millis(2));
        let (url, dest) = job(i);
        if let Err(e) = queue.push(url, dest) {
            failed = Some(e);
            break;
        }
    }
    let e = match failed {
        Some(e) => {
            drop(queue);
            e
        }
        None => queue.finish().unwrap_err(),
    };
    assert_attachment_download_failed(e);
    assert!(stats.done.lock().unwrap().len() < 50, "stopped starting new downloads");
}
