//! What this computer keeps, in `~/.local/share/icloud-reminders/`:
//!
//! - `cache.json`: the lists and reminders as last synced, the zone's sync
//!   token and the account they belong to. Only a cache: iCloud holds the
//!   reminders, and a sync rebuilds it.
//! - `notified.json`: which reminders have notified, for which due time.
//! - `replica`: this computer's ID in `ResolutionTokenMap`.
//! - `lock`: `flock`ed around every read-modify-write of those, so the
//!   window, the command line and the background timer never interleave.
//!
//! Files are replaced whole (a temp file, then rename).

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
    #[serde(default)]
    pub version: u32,
    /// The dsid the cache belongs to.
    #[serde(default)]
    pub account: String,
    /// changes/zone's token for Reminder records.
    #[serde(default)]
    pub sync_token: Option<String>,
    /// When the last sync finished (Unix ms).
    #[serde(default)]
    pub synced_ms: Option<i64>,
    #[serde(default)]
    pub lists: Vec<List>,
    /// By id (`Reminder/<UUID>`).
    #[serde(default)]
    pub reminders: BTreeMap<String, Reminder>,
}

impl Cache {
    pub fn list(&self, id: &str) -> Option<&List> {
        self.lists.iter().find(|l| l.id == id)
    }

    /// Where Apple's default list would be: "Reminders" if there is one,
    /// else the first list.
    pub fn default_list(&self) -> Option<&List> {
        self.lists
            .iter()
            .find(|l| l.name == "Reminders")
            .or_else(|| self.lists.first())
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
                r.created_ms.unwrap_or(0),
            )
        });
        rows
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Notified {
    /// False until the first background run, which marks what is already
    /// due as notified instead of showing every overdue reminder at once.
    #[serde(default)]
    pub started: bool,
    /// Reminder id to the due instant (Unix ms) it notified for.
    #[serde(default)]
    pub fired: BTreeMap<String, i64>,
    /// When a background run last tried to sync (Unix ms), succeeded or
    /// not: a signed-out or offline machine is not retried every minute.
    #[serde(default)]
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

    /// `$XDG_DATA_HOME/icloud-reminders` (`~/.local/share/icloud-reminders`).
    pub fn default_dir() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(data.join("icloud-reminders"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
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

    /// The cache, or an empty one (none yet, unreadable, an older shape).
    pub fn cache(&self) -> Cache {
        read_json::<Cache>(&self.dir.join("cache.json"))
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or_default()
    }

    pub fn save_cache(&self, cache: &Cache) -> io::Result<()> {
        let mut cache = cache.clone();
        cache.version = CACHE_VERSION;
        write_json(&self.dir.join("cache.json"), &cache)
    }

    pub fn notified(&self) -> Notified {
        read_json(&self.dir.join("notified.json")).unwrap_or_default()
    }

    pub fn save_notified(&self, n: &Notified) -> io::Result<()> {
        write_json(&self.dir.join("notified.json"), n)
    }

    /// This computer's replica ID, made once.
    pub fn replica(&self) -> io::Result<String> {
        let path = self.dir.join("replica");
        if let Ok(s) = fs::read_to_string(&path)
            && uuid::Uuid::parse_str(s.trim()).is_ok()
        {
            return Ok(s.trim().to_owned());
        }
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        write_atomic(&path, id.as_bytes())?;
        Ok(id)
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> io::Result<()> {
    write_atomic(path, &serde_json::to_vec(v).expect("serializable"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("a file in the data directory");
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile_in(dir)?;
    tmp.1.write_all(bytes)?;
    tmp.1.sync_all()?;
    fs::rename(&tmp.0, path)
}

/// A new file beside the target (the std library has no tempfile).
fn tempfile_in(dir: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..16 {
        let path = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("could not create a temporary file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_damage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("data"));
        assert!(store.cache().lists.is_empty());
        let mut cache = Cache {
            account: "123".into(),
            sync_token: Some("t".into()),
            ..Cache::default()
        };
        cache.lists.push(List {
            id: "List/A".into(),
            name: "A".into(),
            color: None,
            order: vec![],
        });
        {
            let _lock = store.lock().unwrap();
            store.save_cache(&cache).unwrap();
        }
        let back = store.cache();
        assert_eq!(back.account, "123");
        assert_eq!(back.lists, cache.lists);
        fs::write(store.dir().join("cache.json"), "{not json").unwrap();
        assert!(store.cache().lists.is_empty());
        // No temp files left behind.
        let names: Vec<_> = fs::read_dir(store.dir()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(names.iter().all(|n| !n.to_string_lossy().starts_with(".tmp")), "{names:?}");
    }

    #[test]
    fn the_replica_id_is_made_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let a = store.replica().unwrap();
        assert_eq!(a, store.replica().unwrap());
        assert_eq!(a, a.to_uppercase());
    }
}
