//! Downloads: Apple's own JPEG derivatives for thumbnails and the viewer
//! (no image decoding here), and originals on demand, on a small thread pool.
//!
//! The transport writes a temp file next to `dest` and renames it over
//! `dest`; it creates no directories. So every job makes its directory
//! first, and originals (which live among the user's own files) download to
//! a private temp name that is then moved into place without replacing
//! anything: names are reserved under a lock, checked against the catalog and
//! the disk, and a file that appears meanwhile only moves ours to the next name.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::SystemTime;

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

/// What a job produced. For an original, `path` is the photo; a Live Photo
/// video that failed after the photo was saved is reported in `live_error`
/// (the photo stays recorded, and the next attempt fetches only the video).
#[derive(Debug)]
pub struct Fetched {
    pub path: PathBuf,
    pub live_error: Option<Error>,
}

/// Download one rendition of one asset and record its path. Returns the path
/// (for originals, the photo; the Live Photo video lands next to it).
pub fn fetch(t: &dyn Transport, cat: &Catalog, targets: &Targets, id: &str, job: Job) -> Result<PathBuf> {
    fetch_detailed(t, cat, targets, id, job).map(|f| f.path)
}

/// [`fetch`], keeping a Live Photo video failure apart from the photo's.
/// A 4xx on a signed URL means it expired: the master is looked up again for
/// fresh URLs and the download retried once.
pub fn fetch_detailed(t: &dyn Transport, cat: &Catalog, targets: &Targets, id: &str, job: Job) -> Result<Fetched> {
    fetch_detailed_with(t, cat, targets, id, job, &Vec::new)
}

/// The most masters one `records/lookup` refreshes.
pub const LOOKUP_BATCH: usize = 200;

/// Asset ids that will be downloaded soon (a pool's queue). When one URL
/// turns out to have expired, the others signed at the same time have too:
/// their masters are refreshed in the same `records/lookup`.
pub type Peers<'a> = &'a dyn Fn() -> Vec<String>;

/// [`fetch_detailed`], refreshing `peers()` along with this asset when its
/// URL has expired.
pub fn fetch_detailed_with(
    t: &dyn Transport,
    cat: &Catalog,
    targets: &Targets,
    id: &str,
    job: Job,
    peers: Peers,
) -> Result<Fetched> {
    let mut row = cat
        .asset(id)?
        .ok_or_else(|| Error::Other(format!("unknown asset {id}")))?;
    let (dir, kind, url_of): (PathBuf, PathKind, UrlOf) = match job {
        Job::Thumb => (targets.dirs.thumbs(), PathKind::Thumb, |r| r.thumb_url.as_ref()),
        Job::Medium => (targets.dirs.medium(), PathKind::Medium, |r| r.medium_url.as_ref()),
        Job::Original => return original(t, cat, targets, row, peers),
    };
    let cached = if job == Job::Thumb {
        &row.thumb_path
    } else {
        &row.medium_path
    };
    if let Some(p) = cached.as_ref().filter(|p| p.exists()) {
        if job == Job::Medium {
            touch(p);
        }
        return Ok(Fetched {
            path: p.clone(),
            live_error: None,
        });
    }
    // The cache is ours and named by asset id: replacing a file there is fine.
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("{}.jpg", sanitize(&row.id)));
    download_fresh(t, cat, &mut row, url_of, job, &dest, peers)?;
    cat.set_path(id, kind, Some(&dest))?;
    Ok(Fetched {
        path: dest,
        live_error: None,
    })
}

type UrlOf = fn(&Row) -> Option<&String>;

fn original(t: &dyn Transport, cat: &Catalog, targets: &Targets, mut row: Row, peers: Peers) -> Result<Fetched> {
    let mut hold = Reservation::default();
    let photo = match row.local_path.clone().filter(|p| p.exists()) {
        Some(p) => p,
        None => {
            let dest = {
                let mut taken = reserved();
                let (photo, live) = plan_original(cat, &targets.library, &row, &taken)?;
                hold.add(&mut taken, photo.clone());
                if let Some(live) = live {
                    hold.add(&mut taken, live);
                }
                photo
            };
            let tmp = Temp::beside(&dest)?;
            download_fresh(t, cat, &mut row, |r| r.orig_url.as_ref(), Job::Original, &tmp.0, peers)?;
            let library = targets.library.clone();
            let photo = settle(tmp, dest, &mut hold, |taken| {
                Ok(plan_original(cat, &library, &row, taken)?.0)
            })?;
            cat.set_path(&row.id, PathKind::Original, Some(&photo))?;
            row.local_path = Some(photo.clone());
            photo
        }
    };
    let live_error = if row.live_url.is_some() && row.live_path.as_ref().is_none_or(|p| !p.exists()) {
        live(t, cat, &mut row, &photo, hold, peers).err()
    } else {
        None
    };
    Ok(Fetched {
        path: photo,
        live_error,
    })
}

/// The Live Photo's video, named after the photo's (already unique) stem.
fn live(
    t: &dyn Transport,
    cat: &Catalog,
    row: &mut Row,
    photo: &Path,
    mut hold: Reservation,
    peers: Peers,
) -> Result<PathBuf> {
    let dest = {
        let mut taken = reserved();
        // The photo is in the catalog now; re-plan the video with the lock held.
        hold.release(&mut taken);
        let dest = plan_live(cat, row, photo, &taken)?;
        hold.add(&mut taken, dest.clone());
        dest
    };
    let tmp = Temp::beside(&dest)?;
    download_fresh(t, cat, row, |r| r.live_url.as_ref(), Job::Original, &tmp.0, peers)?;
    let live = settle(tmp, dest, &mut hold, |taken| plan_live(cat, row, photo, taken))?;
    cat.set_path(&row.id, PathKind::Live, Some(&live))?;
    row.live_path = Some(live.clone());
    Ok(live)
}

/// Download `url_of(row)` to `dest`; on an expired URL, refresh `row` (and
/// the `peers` queued behind it) from iCloud and try once more.
fn download_fresh(
    t: &dyn Transport,
    cat: &Catalog,
    row: &mut Row,
    url_of: UrlOf,
    job: Job,
    dest: &Path,
    peers: Peers,
) -> Result<()> {
    let url = url_of(row)
        .cloned()
        .ok_or_else(|| Error::Other(format!("{} has no {job:?} rendition", row.filename)))?;
    match t.download(&url, dest) {
        Ok(_) => Ok(()),
        Err(e) if e.is_expired_url() => {
            // Another download thread may have refreshed it meanwhile.
            let stored = cat.asset(&row.id)?;
            *row = match stored {
                Some(r) if url_of(&r).is_some_and(|u| *u != url) => r,
                _ => refresh(t, cat, row, peers)?,
            };
            let url = url_of(row).cloned().ok_or(e)?;
            t.download(&url, dest).map(drop)
        }
        Err(e) => Err(e),
    }
}

/// Fresh URLs for `row`'s master and, in the same lookup, for the masters
/// of `peers()` (up to [`LOOKUP_BATCH`] in all).
fn refresh(t: &dyn Transport, cat: &Catalog, row: &Row, peers: Peers) -> Result<Row> {
    let mut masters = vec![row.master_id.clone()];
    let mut seen: HashSet<String> = masters.iter().cloned().collect();
    for id in peers() {
        if masters.len() >= LOOKUP_BATCH {
            break;
        }
        if let Ok(Some(r)) = cat.asset(&id)
            && seen.insert(r.master_id.clone())
        {
            masters.push(r.master_id);
        }
    }
    let ck = CloudKit::connect(t)?;
    let ids: Vec<&str> = masters.iter().map(String::as_str).collect();
    let mut found = false;
    for m in ck.lookup_masters(&ids)? {
        found |= m.master_id == row.master_id;
        cat.update_master(&m)?;
    }
    if !found {
        return Err(Error::Other(format!("{} is no longer in iCloud", row.filename)));
    }
    cat.asset(&row.id)?
        .ok_or_else(|| Error::Other(format!("unknown asset {}", row.id)))
}

/// Library paths chosen for downloads that are not in the catalog yet,
/// shared by every download thread.
static RESERVED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn reserved() -> MutexGuard<'static, BTreeSet<PathBuf>> {
    RESERVED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Paths this job reserved; released when it ends.
#[derive(Default)]
struct Reservation(Vec<PathBuf>);

impl Reservation {
    fn add(&mut self, taken: &mut BTreeSet<PathBuf>, p: PathBuf) {
        taken.insert(p.clone());
        self.0.push(p);
    }

    fn release(&mut self, taken: &mut BTreeSet<PathBuf>) {
        for p in self.0.drain(..) {
            taken.remove(&p);
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            self.release(&mut reserved());
        }
    }
}

/// A download's private temp file in the destination directory, removed
/// unless it was moved into place.
struct Temp(PathBuf);

impl Temp {
    fn beside(dest: &Path) -> Result<Temp> {
        let dir = dest
            .parent()
            .ok_or_else(|| Error::Other(format!("{} has no directory", dest.display())))?;
        std::fs::create_dir_all(dir)?;
        Ok(Temp(dir.join(format!(
            ".icloud-photos-{}.part",
            uuid::Uuid::new_v4().simple()
        ))))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Move the download into place without replacing anything; if a file
/// appeared at `dest` meanwhile, take the next free name.
fn settle(
    tmp: Temp,
    mut dest: PathBuf,
    hold: &mut Reservation,
    mut repick: impl FnMut(&BTreeSet<PathBuf>) -> Result<PathBuf>,
) -> Result<PathBuf> {
    for _ in 0..32 {
        match rename_noreplace(&tmp.0, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let mut taken = reserved();
                dest = repick(&taken)?;
                hold.add(&mut taken, dest.clone());
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Other(format!("no free name for {}", dest.display())))
}

/// rename(2) that fails with `AlreadyExists` instead of replacing `to`:
/// renameat2(RENAME_NOREPLACE), or link + unlink where the filesystem lacks it.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let (f, t) = (
        CString::new(from.as_os_str().as_bytes())?,
        CString::new(to.as_os_str().as_bytes())?,
    );
    // SAFETY: both are valid NUL-terminated paths that outlive the call.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {
            std::fs::hard_link(from, to)?;
            std::fs::remove_file(from)
        }
        _ => Err(e),
    }
}

/// Free for a new file: not reserved by a running download, not recorded for
/// another asset, and nothing (not even a dangling link) on disk.
fn is_free(cat: &Catalog, row: &Row, taken: &BTreeSet<PathBuf>, p: &Path) -> Result<bool> {
    Ok(!taken.contains(p) && !cat.path_taken(p, &row.id)? && std::fs::symlink_metadata(p).is_err())
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

fn numbered(dir: &Path, stem: &str, n: u32, ext: &str) -> PathBuf {
    if n == 1 {
        dir.join(format!("{stem}{ext}"))
    } else {
        dir.join(format!("{stem} ({n}){ext}"))
    }
}

/// `<library>/<YYYY>/<MM>/<filename>`, with ` (2)` etc. when the name is
/// taken (IMG_0001.JPG repeats across cameras). For a Live Photo the video's
/// name (same stem) must be free too. Returns (photo, video).
fn plan_original(
    cat: &Catalog,
    library: &Path,
    row: &Row,
    taken: &BTreeSet<PathBuf>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let (y, m, _) = icloud_session::time::civil_from_days(row.created.div_euclid(86_400));
    let dir = library.join(format!("{y:04}")).join(format!("{m:02}"));
    let filename = safe_filename(&row.filename);
    let (stem, ext) = split_name(&filename);
    let live_ext = row.live_url.is_some().then(|| live_ext(row.live_type.as_deref()));
    for n in 1..10_000 {
        let photo = numbered(&dir, stem, n, ext);
        if !is_free(cat, row, taken, &photo)? {
            continue;
        }
        let live = live_ext.map(|e| numbered(&dir, stem, n, e));
        if let Some(l) = &live
            && (*l == photo || !is_free(cat, row, taken, l)?)
        {
            continue;
        }
        return Ok((photo, live));
    }
    Err(Error::Other(format!("no free name for {filename}")))
}

/// The video of a Live Photo whose photo is saved at `photo`: the photo's
/// stem with the video's extension, numbered if that is taken.
fn plan_live(cat: &Catalog, row: &Row, photo: &Path, taken: &BTreeSet<PathBuf>) -> Result<PathBuf> {
    let dir = photo.parent().unwrap_or(Path::new("."));
    let stem = photo
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "photo".into());
    let ext = live_ext(row.live_type.as_deref());
    for n in 1..10_000 {
        let candidate = numbered(dir, &stem, n, ext);
        if candidate != photo && is_free(cat, row, taken, &candidate)? {
            return Ok(candidate);
        }
    }
    Err(Error::Other(format!(
        "no free name for the video of {}",
        photo.display()
    )))
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    match cleaned.trim() {
        "" | "." | ".." => "photo".into(),
        s => s.to_owned(),
    }
}

/// The Live Photo video's extension (`.MOV` unless its UTI says otherwise).
pub fn live_ext(live_type: Option<&str>) -> &'static str {
    live_type.map(extension_for).filter(|e| !e.is_empty()).unwrap_or(".MOV")
}

/// Mark a cached rendition as just used (the medium cache is pruned oldest first).
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// The medium cache's size cap; the thumbnail cache is small and kept whole.
pub const MEDIUM_CACHE_CAP: u64 = 2 << 30;

/// Drop cached renditions of assets that left the library (deleted, hidden,
/// tombstoned), then the oldest medium JPEGs beyond `medium_cap` bytes.
/// Only files inside the cache directory are removed. Returns files removed.
pub fn prune_cache(cat: &Catalog, dirs: &Dirs, medium_cap: u64) -> Result<usize> {
    let mut removed = 0;
    let remove = |p: &Path| -> bool {
        p.starts_with(&dirs.cache)
            && match std::fs::remove_file(p) {
                Ok(()) => true,
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            }
    };
    for (id, thumb, medium) in cat.cached_renditions_of_removed()? {
        if let Some(p) = thumb
            && remove(&p)
        {
            cat.set_path(&id, PathKind::Thumb, None)?;
            removed += 1;
        }
        if let Some(p) = medium
            && remove(&p)
        {
            cat.set_path(&id, PathKind::Medium, None)?;
            removed += 1;
        }
    }

    let mut files: Vec<(SystemTime, u64, PathBuf)> = match std::fs::read_dir(dirs.medium()) {
        Ok(entries) => entries
            .filter_map(|e| {
                let e = e.ok()?;
                let meta = e.metadata().ok().filter(|m| m.is_file())?;
                Some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), e.path()))
            })
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, size, path) in files {
        if total <= medium_cap {
            break;
        }
        if remove(&path) {
            total = total.saturating_sub(size);
            cat.forget_medium(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[derive(Debug)]
pub struct Event {
    pub id: String,
    pub job: Job,
    pub result: Result<PathBuf>,
    /// The original was saved but its Live Photo video was not.
    pub live_error: Option<Error>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// What the user is looking at: thumbs, the viewer, a clicked download.
    Now,
    /// "Download all originals".
    Background,
}

type Notify = Box<dyn Fn(Event) + Send + Sync>;

/// The most `Now` jobs kept queued: beyond it the oldest thumbnail request
/// is dropped (a screenful of tiles is far fewer).
pub const MAX_NOW_JOBS: usize = 256;

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
    pub fn start(
        transport: Arc<dyn Transport>,
        dirs: Dirs,
        library: PathBuf,
        workers: usize,
        notify: Notify,
    ) -> Downloader {
        let inner = Arc::new(Inner {
            queues: Mutex::new(Queues {
                now: VecDeque::new(),
                background: VecDeque::new(),
                pending: HashSet::new(),
            }),
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
        // Thumbnails asked for long ago (scrolled past) give way to new ones.
        if q.now.len() > MAX_NOW_JOBS
            && let Some(i) = q.now.iter().rposition(|(_, j)| *j == Job::Thumb)
            && let Some(stale) = q.now.remove(i)
        {
            q.pending.remove(&stale);
        }
        self.inner.wake.notify_one();
    }

    /// Drop a queued job that is no longer wanted (its tile scrolled away).
    /// A job already running finishes.
    pub fn cancel(&self, id: &str, job: Job) {
        let mut q = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
        let is = |k: &(String, Job)| k.1 == job && k.0 == id;
        let removed = match q.now.iter().position(is) {
            Some(i) => q.now.remove(i),
            None => match q.background.iter().position(is) {
                Some(i) => q.background.remove(i),
                None => None,
            },
        };
        if let Some(k) = removed {
            q.pending.remove(&k);
        }
    }

    /// Jobs waiting (not running), for tests and progress.
    pub fn queued(&self) -> usize {
        let q = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
        q.now.len() + q.background.len()
    }

    pub fn clear_background(&self) {
        let mut q = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
        let dropped: Vec<_> = q.background.drain(..).collect();
        for k in dropped {
            q.pending.remove(&k);
        }
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
                let targets = Targets {
                    dirs: inner.dirs.clone(),
                    library: inner.library.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                };
                let peers = || {
                    let q = inner.queues.lock().unwrap_or_else(|e| e.into_inner());
                    let mut ids: Vec<String> = Vec::new();
                    for (id, _) in q.now.iter().chain(q.background.iter()) {
                        if ids.len() >= LOOKUP_BATCH {
                            break;
                        }
                        if !ids.contains(id) {
                            ids.push(id.clone());
                        }
                    }
                    ids
                };
                fetch_detailed_with(&*inner.transport, cat, &targets, &id, job, &peers)
            }
            None => Err(Error::Other("could not open the catalog".into())),
        };
        let (result, live_error) = match result {
            Ok(f) => (Ok(f.path), f.live_error),
            Err(e) => (Err(e), None),
        };
        inner
            .queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&(id.clone(), job));
        (inner.notify)(Event {
            id,
            job,
            result,
            live_error,
        });
    }
}
