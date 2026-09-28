//! Downloads: Apple's own JPEG derivatives for thumbnails and the viewer
//! (no image decoding here), and originals on demand, on a small thread pool.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use crate::catalog::{Catalog, PathKind, Row};
use crate::cloudkit::{CloudKit, extension_for, sanitize};
use crate::config::Dirs;
use crate::transport::{Error, Result, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Job {
    /// `resJPEGThumbRes` into `~/.cache/icloud-photos/thumbs/`.
    Thumb,
    /// `resJPEGMedRes` into `~/.cache/icloud-photos/medium/`, for the viewer.
    Medium,
    /// `resOriginalRes` (and the Live Photo video) into the library dir.
    Original,
}

/// Where a job's file goes.
pub struct Targets {
    pub dirs: Dirs,
    pub library: PathBuf,
}

/// Download one rendition of one asset and record its path. Returns the path
/// (for originals, the photo; the Live Photo video lands next to it).
/// A 4xx on the signed URL means it expired: look the master up again for
/// fresh URLs and retry once.
pub fn fetch(t: &dyn Transport, cat: &Catalog, targets: &Targets, id: &str, job: Job) -> Result<PathBuf> {
    let row = cat.asset(id)?.ok_or_else(|| Error::Other(format!("unknown asset {id}")))?;
    if let Some(p) = existing(&row, job) {
        return Ok(p);
    }
    let (url, dest) = plan(cat, targets, &row, job)?;
    let url = url.ok_or_else(|| Error::Other(format!("{} has no {job:?} rendition", row.filename)))?;
    let row = match t.download(&url, &dest) {
        Ok(_) => row,
        Err(e) if e.is_expired_url() => {
            let fresh = refresh(t, cat, &row)?;
            let url = plan(cat, targets, &fresh, job)?.0.ok_or(e)?;
            t.download(&url, &dest)?;
            fresh
        }
        Err(e) => return Err(e),
    };
    let kind = match job {
        Job::Thumb => PathKind::Thumb,
        Job::Medium => PathKind::Medium,
        Job::Original => PathKind::Original,
    };
    cat.set_path(id, kind, Some(&dest))?;
    if job == Job::Original && row.live_url.is_some() && row.live_path.as_ref().is_none_or(|p| !p.exists()) {
        let live = live_dest(&dest, row.live_type.as_deref());
        let url = row.live_url.clone().unwrap_or_default();
        match t.download(&url, &live) {
            Ok(_) => cat.set_path(id, PathKind::Live, Some(&live))?,
            Err(e) if e.is_expired_url() => {
                let fresh = refresh(t, cat, &row)?;
                if let Some(url) = fresh.live_url {
                    t.download(&url, &live)?;
                    cat.set_path(id, PathKind::Live, Some(&live))?;
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(dest)
}

fn existing(row: &Row, job: Job) -> Option<PathBuf> {
    let p = match job {
        Job::Thumb => &row.thumb_path,
        Job::Medium => &row.medium_path,
        Job::Original => &row.local_path,
    };
    let live_ok = job != Job::Original || row.live_url.is_none() || row.live_path.as_ref().is_some_and(|p| p.exists());
    p.as_ref().filter(|p| p.exists() && live_ok).cloned()
}

fn plan(cat: &Catalog, targets: &Targets, row: &Row, job: Job) -> Result<(Option<String>, PathBuf)> {
    let name = sanitize(&row.id);
    Ok(match job {
        Job::Thumb => (row.thumb_url.clone(), targets.dirs.thumbs().join(format!("{name}.jpg"))),
        Job::Medium => (row.medium_url.clone(), targets.dirs.medium().join(format!("{name}.jpg"))),
        Job::Original => (row.orig_url.clone(), original_dest(cat, &targets.library, row)?),
    })
}

fn refresh(t: &dyn Transport, cat: &Catalog, row: &Row) -> Result<Row> {
    let ck = CloudKit::connect(t)?;
    let masters = ck.lookup_masters(&[row.master_id.as_str()])?;
    let m = masters.first().ok_or_else(|| Error::Other(format!("{} is no longer in iCloud", row.filename)))?;
    cat.update_master(m)?;
    cat.asset(&row.id)?.ok_or_else(|| Error::Other(format!("unknown asset {}", row.id)))
}

/// `<library>/<YYYY>/<MM>/<filename>`, with ` (2)` etc. when another asset
/// already owns that name (IMG_0001.JPG repeats across cameras).
pub fn original_dest(cat: &Catalog, library: &Path, row: &Row) -> Result<PathBuf> {
    let (y, m) = year_month(row.created);
    let dir = library.join(format!("{y:04}")).join(format!("{m:02}"));
    let filename = safe_filename(&row.filename);
    let (stem, ext) = match filename.rfind('.') {
        Some(i) if i > 0 => (&filename[..i], &filename[i..]),
        _ => (filename.as_str(), ""),
    };
    for n in 1..10_000 {
        let candidate = if n == 1 { dir.join(&filename) } else { dir.join(format!("{stem} ({n}){ext}")) };
        if !cat.path_taken(&candidate, &row.id)? {
            return Ok(candidate);
        }
    }
    Err(Error::Other(format!("no free name for {filename}")))
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c == '/' || c == '\0' { '_' } else { c }).collect();
    match cleaned.trim() {
        "" | "." | ".." => "photo".into(),
        s => s.to_owned(),
    }
}

/// The Live Photo's video sits next to the photo: IMG_0001.HEIC + IMG_0001.MOV.
pub fn live_dest(photo: &Path, live_type: Option<&str>) -> PathBuf {
    let ext = live_type.map(extension_for).filter(|e| !e.is_empty()).unwrap_or(".MOV");
    photo.with_extension(ext.trim_start_matches('.'))
}

/// UTC year and month of a Unix time (Howard Hinnant's civil_from_days).
pub fn year_month(unix: i64) -> (i64, u32) {
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m)
}

#[derive(Debug)]
pub struct Event {
    pub id: String,
    pub job: Job,
    pub result: Result<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// What the user is looking at: thumbs, the viewer, a clicked download.
    Now,
    /// "Download all originals".
    Background,
}

type Notify = Box<dyn Fn(Event) + Send + Sync>;

struct Queues {
    now: VecDeque<(String, Job)>,
    background: VecDeque<(String, Job)>,
    pending: HashSet<(String, Job)>,
}

struct Inner {
    queues: Mutex<Queues>,
    wake: Condvar,
    transport: Arc<dyn Transport>,
    dirs: Dirs,
    library: Mutex<PathBuf>,
    notify: Notify,
}

/// A fixed pool of download threads, each with its own catalog connection.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<Inner>,
}

impl Downloader {
    pub fn start(transport: Arc<dyn Transport>, dirs: Dirs, library: PathBuf, workers: usize, notify: Notify) -> Downloader {
        let inner = Arc::new(Inner {
            queues: Mutex::new(Queues { now: VecDeque::new(), background: VecDeque::new(), pending: HashSet::new() }),
            wake: Condvar::new(),
            transport,
            dirs,
            library: Mutex::new(library),
            notify,
        });
        for i in 0..workers.max(1) {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name(format!("download-{i}"))
                .spawn(move || worker(inner))
                .expect("spawn download thread");
        }
        Downloader { inner }
    }

    pub fn set_library(&self, dir: PathBuf) {
        *self.inner.library.lock().unwrap_or_else(|e| e.into_inner()) = dir;
    }

    /// Queue a job unless the same one is already queued or running.
    /// `Now` jobs jump ahead of background ones, newest request first.
    pub fn enqueue(&self, id: &str, job: Job, priority: Priority) {
        let mut q = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
        let key = (id.to_owned(), job);
        if q.pending.contains(&key) {
            if priority == Priority::Now {
                // Promote a background job the user now wants.
                if let Some(i) = q.background.iter().position(|k| *k == key) {
                    q.background.remove(i);
                    q.now.push_front(key);
                }
            }
            return;
        }
        q.pending.insert(key.clone());
        match priority {
            Priority::Now => q.now.push_front(key),
            Priority::Background => q.background.push_back(key),
        }
        self.inner.wake.notify_one();
    }

    pub fn clear_background(&self) {
        let mut q = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
        let dropped: Vec<_> = q.background.drain(..).collect();
        for k in dropped {
            q.pending.remove(&k);
        }
    }

    pub fn queued(&self) -> usize {
        let q = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
        q.pending.len()
    }
}

fn worker(inner: Arc<Inner>) {
    let mut cat: Option<Catalog> = None;
    loop {
        let (id, job) = {
            let mut q = inner.queues.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(k) = q.now.pop_front().or_else(|| q.background.pop_front()) {
                    break k;
                }
                q = inner.wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        if cat.is_none() {
            cat = Catalog::open(&inner.dirs.catalog()).ok();
        }
        let result = match &cat {
            Some(cat) => {
                let targets = Targets { dirs: inner.dirs.clone(), library: inner.library.lock().unwrap_or_else(|e| e.into_inner()).clone() };
                fetch(&*inner.transport, cat, &targets, &id, job)
            }
            None => Err(Error::Other("could not open the catalog".into())),
        };
        inner.queues.lock().unwrap_or_else(|e| e.into_inner()).pending.remove(&(id.clone(), job));
        (inner.notify)(Event { id, job, result });
    }
}
