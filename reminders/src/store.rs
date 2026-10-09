//! What this computer keeps, in `~/.local/share/icloud-reminders/`:
//!
//! - `cache.json`: the lists and reminders as last synced, the zone's sync
//!   token and the account (dsid) they belong to. Only a cache: iCloud
//!   holds the reminders, and a sync rebuilds it. No credentials: the
//!   iCloud session is icloud-session's, fetched over D-Bus per request.
//! - `notified.json`: which reminders have notified, for which due time.
//! - `replica`: this computer's ID in `ResolutionTokenMap`.
//! - `lock`: `flock`ed around every read-modify-write of those, so the
//!   window, the command line and the background timer never interleave.
//!
//! Files are replaced whole (a temp file, then rename). One that does not
//! parse is an error naming it, never silently reset.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::model::{List, Reminder};

/// Bumped when `cache.json` changes shape; an older one is synced afresh.
const CACHE_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    /// The dsid the cache belongs to.
    pub account: String,
    /// changes/zone's token for Reminder records.
    pub sync_token: Option<String>,
    /// When the last sync finished (Unix ms).
    pub synced_ms: Option<i64>,
    pub lists: Vec<List>,
    /// By id (`Reminder/<UUID>`).
    pub reminders: BTreeMap<String, Reminder>,
}

impl Cache {
    pub fn list(&self, id: &str) -> Option<&List> {
        self.lists.iter().find(|l| l.id == id)
    }

    /// Where an add that names no list goes: the account's only list, if
    /// it has just one. The records hold no "default list": pyicloud's
    /// `Account` record has only `Name`, and neither it, go-icloud's wire
    /// contracts nor any other client reads one (Tooker/icloud-reminders-cli
    /// makes `--list` required). So with several lists the caller must name
    /// one; a list's name is never taken as a sign that it is the default.
    pub fn only_list(&self) -> Option<&List> {
        match self.lists.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// The reminders `keep` takes: due ones by due time, then undated ones
    /// in their list's own order.
    pub fn sorted(&self, local: &TimeZone, keep: impl Fn(&Reminder) -> bool) -> Vec<&Reminder> {
        let position = |r: &Reminder| {
            self.list(&r.list_id)
                .and_then(|l| l.order.iter().position(|id| id == r.uuid()))
                .unwrap_or(usize::MAX)
        };
        let mut rows: Vec<&Reminder> = self.reminders.values().filter(|r| keep(r)).collect();
        rows.sort_by_key(|r| {
            (
                r.due.as_ref().map_or(i64::MAX, |d| d.instant(local).as_millisecond()),
                position(r),
                r.created_ms,
            )
        });
        rows
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Notified {
    /// False until a background run has seen a synced cache, which marks
    /// what is already due as notified instead of showing every overdue
    /// reminder at once.
    pub started: bool,
    /// The dsid `started` and `fired` are for; another account starts over.
    pub account: String,
    /// Reminder id to the due instant (Unix ms) it notified for.
    pub fired: BTreeMap<String, i64>,
    /// When a background run last tried to sync (Unix ms), succeeded or
    /// not: a signed-out or offline machine is not retried every minute.
    pub sync_attempt_ms: Option<i64>,
}

pub struct Store {
    dir: PathBuf,
}

/// Held while it lives; dropping it unlocks.
pub struct Lock(#[allow(dead_code)] File);

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// `$XDG_DATA_HOME/icloud-reminders` (`~/.local/share/icloud-reminders`;
    /// a relative `XDG_DATA_HOME` is ignored, as the XDG spec says).
    pub fn default_dir() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(data.join("icloud-reminders"))
    }

    pub fn lock(&self) -> io::Result<Lock> {
        fs::create_dir_all(&self.dir)?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("lock"))?;
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(Lock(f))
    }

    /// The cache: empty when there is none yet, or when an older version
    /// of the app wrote it (the next sync then fetches everything).
    pub fn cache(&self) -> io::Result<Cache> {
        let path = self.dir.join("cache.json");
        let Some(v) = read_json::<serde_json::Value>(&path)? else {
            return Ok(Cache::default());
        };
        if v.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(CACHE_VERSION)) {
            return Ok(Cache::default());
        }
        serde_json::from_value(v).map_err(|e| damaged(&path, e))
    }

    pub fn save_cache(&self, cache: &Cache) -> io::Result<()> {
        let mut cache = cache.clone();
        cache.version = CACHE_VERSION;
        write_json(&self.dir.join("cache.json"), &cache)
    }

    /// What has notified; empty before the first background run.
    pub fn notified(&self) -> io::Result<Notified> {
        Ok(read_json(&self.dir.join("notified.json"))?.unwrap_or_default())
    }

    pub fn save_notified(&self, n: &Notified) -> io::Result<()> {
        write_json(&self.dir.join("notified.json"), n)
    }

    /// This computer's replica ID, made the first time it is needed.
    pub fn replica(&self) -> io::Result<String> {
        let path = self.dir.join("replica");
        match fs::read_to_string(&path) {
            Ok(s) => match uuid::Uuid::parse_str(s.trim()) {
                Ok(_) => Ok(s.trim().to_owned()),
                Err(e) => Err(damaged(&path, e)),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let id = uuid::Uuid::new_v4().to_string().to_uppercase();
                write_atomic(&path, id.as_bytes())?;
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }
}

fn damaged(path: &Path, e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{} is damaged ({e}); delete it to start over", path.display()))
}

/// `None` when the file does not exist.
fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| damaged(path, e))
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> io::Result<()> {
    write_atomic(path, &serde_json::to_vec(v).expect("serializable"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("a file in the data directory");
    fs::create_dir_all(dir)?;
    let (tmp, mut f) = tempfile_in(dir)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// A new file beside the target (the std library has no tempfile).
fn tempfile_in(dir: &Path) -> io::Result<(PathBuf, File)> {
    let path = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    let f = OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok((path, f))
}
