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
#[derive(Debug, Clone)]
pub struct Targets {
    pub dirs: Dirs,
    /// Where originals go: the library folder, or an export folder.
    pub library: PathBuf,
    /// `library` is an export folder (`download --out`): originals are
    /// copied there from the library when it has them, downloaded there
    /// otherwise, and the catalog keeps pointing at the library only.
    pub export: bool,
}

/// What a job produced. For an original, `path` is the photo and `live` its
/// Live Photo video; a video that failed after the photo was saved is
/// reported in `live_error` (the photo stays recorded, and the next attempt
/// fetches only the video).
#[derive(Debug)]
pub struct Fetched {
    pub path: PathBuf,
    pub live: Option<PathBuf>,
    pub live_error: Option<Error>,
}

impl Fetched {
    fn file(path: PathBuf) -> Fetched {
        Fetched {
            path,
            live: None,
            live_error: None,
        }
    }
}

/// Download one rendition of one asset and record its path. Returns the path
/// (for originals, the photo; the Live Photo video lands next to it).
/// A 4xx on a signed URL means it expired: the master is looked up again for
/// fresh URLs and the download retried once.
pub fn fetch(t: &dyn Transport, cat: &Catalog, targets: &Targets, id: &str, job: Job) -> Result<PathBuf> {
    fetch_detailed_with(t, cat, targets, id, job, &Vec::new).map(|f| f.path)
}

/// The most masters one `records/lookup` refreshes.
pub const LOOKUP_BATCH: usize = 200;

/// Asset ids that will be downloaded soon (a pool's queue). When one URL
/// turns out to have expired, the others signed at the same time have too:
/// their masters are refreshed in the same `records/lookup`.
pub type Peers<'a> = &'a dyn Fn() -> Vec<String>;

/// [`fetch`], keeping a Live Photo's video (and its failure) apart from the
/// photo, and refreshing `peers()` along with this asset when its URL has
/// expired.
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
        Job::Medium => (targets.dirs.medium(), PathKind::Medium, medium_url),
        Job::Original if targets.export => return export(t, cat, targets, row, peers),
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
        return Ok(Fetched::file(p.clone()));
    }
    // The cache is ours and named by asset id: replacing a file there is fine.
    std::fs::create_dir_all(&dir)?;
    // A PNG original standing in for the preview keeps its own extension.
    let ext = if job == Job::Medium && row.medium_url.is_none() && row.orig_type.as_deref() == Some("public.png") {
        "png"
    } else {
        "jpg"
    };
    let dest = dir.join(format!("{}.{ext}", sanitize(&row.id)));
    download_fresh(t, cat, &mut row, url_of, job, &dest, peers)?;
    cat.set_path(id, kind, Some(&dest))?;
    Ok(Fetched::file(dest))
}

type UrlOf = fn(&Row) -> Option<&String>;

/// The viewer's image: `resJPEGMedRes`, or for a JPEG/PNG original iCloud
/// made no preview of (a small one, such as a screenshot), the original.
fn medium_url(r: &Row) -> Option<&String> {
    r.medium_url
        .as_ref()
        .or(r.orig_url.as_ref().filter(|_| r.viewable_original()))
}

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
            let photo = settle(tmp, dest, &mut hold, |taken| {
                Ok(plan_original(cat, &targets.library, &row, taken)?.0)
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
        live: row.live_path.filter(|p| p.exists()),
        live_error,
    })
}

/// `download --out`: the original (and a Live Photo's video) in the export
/// folder `targets.library`, copied from the library when it is there and
/// downloaded otherwise. Nothing is replaced, and the catalog is not told:
/// its paths are the library's. A file of the right name (numbered or not)
/// and size already in the export folder is taken to be an earlier export
/// of the same photo.
fn export(t: &dyn Transport, cat: &Catalog, targets: &Targets, mut row: Row, peers: Peers) -> Result<Fetched> {
    let len = |p: &Path| std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len());
    let mut hold = Reservation::default();
    let source = row.local_path.clone().filter(|p| p.exists());
    let size = source.as_deref().map_or((row.size > 0).then_some(row.size as u64), len);
    // Among the names this photo can have been exported under (its own, then
    // " (2)" and so on while they are in use), the one holding its bytes.
    let earlier = size.and_then(|size| {
        (1..10_000)
            .map(|n| original_names(&targets.library, &row, n))
            .take_while(|(photo, live)| [Some(photo), live.as_ref()].into_iter().flatten().any(|p| p.exists()))
            .find_map(|(photo, _)| (len(&photo) == Some(size)).then_some(photo))
    });
    let reused = earlier.is_some();
    let photo = if let Some(photo) = earlier {
        photo
    } else {
        let dest = {
            let mut taken = reserved();
            let photo = plan_original(cat, &targets.library, &row, &taken)?.0;
            hold.add(&mut taken, photo.clone());
            photo
        };
        let tmp = Temp::beside(&dest)?;
        match &source {
            Some(have) => drop(std::fs::copy(have, &tmp.0)?),
            None => download_fresh(t, cat, &mut row, |r| r.orig_url.as_ref(), Job::Original, &tmp.0, peers)?,
        }
        settle(tmp, dest, &mut hold, |taken| {
            Ok(plan_original(cat, &targets.library, &row, taken)?.0)
        })?
    };
    if row.live_url.is_none() {
        return Ok(Fetched::file(photo));
    }
    let video = (|| {
        let source = row.live_path.clone().filter(|p| p.exists());
        let first = photo.with_extension(live_ext(row.live_type.as_deref()).trim_start_matches('.'));
        let same = match source.as_deref() {
            Some(have) => len(&first).is_some() && len(&first) == len(have),
            None => reused && len(&first).is_some(),
        };
        if first != photo && same {
            return Ok(first);
        }
        let dest = {
            let mut taken = reserved();
            let dest = plan_live(cat, &row, &photo, &taken)?;
            hold.add(&mut taken, dest.clone());
            dest
        };
        let tmp = Temp::beside(&dest)?;
        match &source {
            Some(have) => drop(std::fs::copy(have, &tmp.0)?),
            None => download_fresh(t, cat, &mut row, |r| r.live_url.as_ref(), Job::Original, &tmp.0, peers)?,
        }
        settle(tmp, dest, &mut hold, |taken| plan_live(cat, &row, &photo, taken))
    })();
    let (live, live_error) = match video {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(e)),
    };
    Ok(Fetched {
        path: photo,
        live,
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
    // Originals are kept for good; thumbnails and viewer images are a cache.
    let get = |url: &str| match job {
        Job::Original => t.download(url, dest),
        Job::Thumb | Job::Medium => t.download_cache(url, dest),
    };
    match get(&url) {
        Ok(_) => Ok(()),
        Err(e) if e.is_expired_url() => {
            // Another download thread may have refreshed it meanwhile.
            let stored = cat.asset(&row.id)?;
            *row = match stored {
                Some(r) if url_of(&r).is_some_and(|u| *u != url) => r,
                _ => refresh(t, cat, row, peers)?,
            };
            let url = url_of(row).cloned().ok_or_else(|| url_refused(e))?;
            get(&url).map(drop).map_err(url_refused)
        }
        Err(e) => Err(url_refused(e)),
    }
}

/// A 401/421 from a signed content URL is that URL refused, not the
/// session (icloud-session judges the session only on its own hosts), so it
/// must not read as `session_rejected` and a request to sign in again.
fn url_refused(e: Error) -> Error {
    match e {
        Error::Session(icloud_session::Error::Http {
            status: status @ (401 | 421),
            body,
        }) => Error::Other(format!("the signed download URL was refused (HTTP {status}): {body}")),
        e => e,
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
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {
            std::fs::hard_link(from, to)?;
            std::fs::remove_file(from)
        }
        Err(e) => Err(e.into()),
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
    for n in 1..10_000 {
        let (photo, live) = original_names(library, row, n);
        if !is_free(cat, row, taken, &photo)? {
            continue;
        }
        if let Some(l) = &live
            && (*l == photo || !is_free(cat, row, taken, l)?)
        {
            continue;
        }
        return Ok((photo, live));
    }
    Err(Error::Other(format!("no free name for {}", row.filename)))
}

/// The `n`th name for an original under `library` (1 is the file's own
/// name), and for its Live Photo video if it has one.
fn original_names(library: &Path, row: &Row, n: u32) -> (PathBuf, Option<PathBuf>) {
    let (y, m, _) = icloud_session::time::civil_from_days(row.created.div_euclid(86_400));
    let dir = library.join(format!("{y:04}")).join(format!("{m:02}"));
    let filename = safe_filename(&row.filename);
    let (stem, ext) = split_name(&filename);
    let live = row
        .live_url
        .is_some()
        .then(|| numbered(&dir, stem, n, live_ext(row.live_type.as_deref())));
    (numbered(&dir, stem, n, ext), live)
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

/// The longest file name Linux takes, in bytes.
const NAME_MAX: usize = 255;
/// Room a saved name keeps for " (9999)" and a longer Live Photo extension.
const NAME_ROOM: usize = " (9999)".len() + 8;
/// A longer "extension" is part of the stem.
const EXT_MAX: usize = 16;

/// The server's file name made safe to save: no `/`, no control characters
/// or invisible direction marks (which can make "gpj.exe" show as
/// "exe.jpg"), and short enough to number.
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .filter(|&c| {
            !c.is_control() && !matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let s = match cleaned.trim() {
        "" | "." | ".." => return "photo".into(),
        s => s,
    };
    let (stem, ext) = match split_name(s) {
        (stem, ext) if ext.len() <= EXT_MAX => (stem, ext),
        _ => (s, ""),
    };
    let mut end = stem.len().min(NAME_MAX - NAME_ROOM - ext.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", &stem[..end])
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
    let removed = std::cell::Cell::new(0);
    // Whether the file is gone now; one already missing is not counted.
    let remove = |p: &Path| -> bool {
        p.starts_with(&dirs.cache)
            && match std::fs::remove_file(p) {
                Ok(()) => {
                    removed.set(removed.get() + 1);
                    true
                }
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            }
    };
    for (id, thumb, medium) in cat.cached_renditions_of_removed()? {
        if let Some(p) = thumb
            && remove(&p)
        {
            cat.set_path(&id, PathKind::Thumb, None)?;
        }
        if let Some(p) = medium
            && remove(&p)
        {
            cat.set_path(&id, PathKind::Medium, None)?;
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
        }
    }
    Ok(removed.get())
}

#[derive(Debug)]
pub struct Event {
    pub id: String,
    pub job: Job,
    pub result: Result<Fetched>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// What the user is looking at: thumbs, the viewer, a clicked download.
    Now,
    /// "Download all originals".
    Background,
}

type Notify = Box<dyn Fn(Event) + Send + Sync>;

/// Download threads the app and `icloud-photos download` run.
pub const DOWNLOAD_WORKERS: usize = 4;

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
    targets: Mutex<Targets>,
    notify: Notify,
}

/// A fixed pool of download threads, each with its own catalog connection.
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<Inner>,
}

impl Downloader {
    pub fn start(transport: Arc<dyn Transport>, targets: Targets, workers: usize, notify: Notify) -> Downloader {
        let inner = Arc::new(Inner {
            queues: Mutex::new(Queues {
                now: VecDeque::new(),
                background: VecDeque::new(),
                pending: HashSet::new(),
            }),
            wake: Condvar::new(),
            transport,
            targets: Mutex::new(targets),
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
        self.inner.targets.lock().unwrap_or_else(|e| e.into_inner()).library = dir;
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
        let targets = inner.targets.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if cat.is_none() {
            cat = Catalog::open(&targets.dirs.catalog()).ok();
        }
        let result = match &cat {
            Some(cat) => {
                // `refresh` leaves out the ids it has seen and stops at a batch.
                let peers = || {
                    let q = inner.queues.lock().unwrap_or_else(|e| e.into_inner());
                    let queued = q.now.iter().chain(q.background.iter());
                    queued.take(LOOKUP_BATCH).map(|(id, _)| id.clone()).collect()
                };
                fetch_detailed_with(&*inner.transport, cat, &targets, &id, job, &peers)
            }
            None => Err(Error::Other("could not open the catalog".into())),
        };
        inner
            .queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&(id.clone(), job));
        (inner.notify)(Event { id, job, result });
    }
}

#[cfg(test)]
mod tests {
    use super::safe_filename;

    #[test]
    fn safe_filename_keeps_ordinary_names() {
        assert_eq!(safe_filename("IMG_0001.HEIC"), "IMG_0001.HEIC");
        assert_eq!(safe_filename("Été à Paris.jpg"), "Été à Paris.jpg");
        assert_eq!(safe_filename("a/b.jpg"), "a_b.jpg");
        assert_eq!(safe_filename(".."), "photo");
    }

    #[test]
    fn safe_filename_drops_control_and_direction_characters() {
        // "photo\u{202E}gpj.exe" shows as "photoexe.jpg".
        assert_eq!(safe_filename("photo\u{202E}gpj.exe"), "photogpj.exe");
        assert_eq!(safe_filename("a\nb\tc\u{7f}\u{1b}[31m.jpg"), "abc[31m.jpg");
        for c in [
            '\u{200E}', '\u{200F}', '\u{202A}', '\u{202D}', '\u{2066}', '\u{2069}', '\u{85}',
        ] {
            assert_eq!(safe_filename(&format!("x{c}y.jpg")), "xy.jpg");
        }
        assert_eq!(safe_filename("\u{202E}\u{2066}"), "photo");
    }

    #[test]
    fn safe_filename_fits_a_numbered_name_and_video_in_255_bytes() {
        let name = safe_filename(&format!("{}.JPG", "é".repeat(300)));
        assert!(name.ends_with(".JPG"));
        assert!(
            name.len() + " (9999)".len() + ".MOV".len() <= 255,
            "{} bytes",
            name.len()
        );
        assert!(name.len() > 200);
        // A huge "extension" is cut too.
        let name = safe_filename(&format!("a.{}", "x".repeat(400)));
        assert!(
            name.len() + " (9999)".len() + ".MOV".len() <= 255,
            "{} bytes",
            name.len()
        );
    }
}
