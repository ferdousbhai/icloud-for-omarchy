//! What this computer keeps, in `~/.local/share/icloud-reminders/`:
//!
//! - `cache.json`: the lists and reminders as last synced, the zone's sync
//!   token and the account (dsid) they belong to. Only a cache: iCloud
//!   holds the reminders, and a sync rebuilds it. No credentials: the
//!   iCloud session is icloud-session's, fetched over D-Bus per request.
//! - `notified.json`: which reminders have notified, for which due time.
//! - `replica`: this computer's ID in `ResolutionTokenMap`.
//! - `folder.json`: the Markdown folder ([`crate::folder`]): each list's
//!   file as last written. `folder.lock` is held over a folder run, which
//!   takes `lock` itself for each write.
//! - `lock`: `flock`ed around every read-modify-write of those, so the
//!   window, the command line and the background timer never interleave.
//!
//! Files are replaced whole (a temp file, then rename). One that does not
//! parse is an error naming it, never silently reset.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use icloud_app::store;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::folder;
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

pub use icloud_app::store::Lock;

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// `$XDG_DATA_HOME/icloud-reminders` (`~/.local/share/icloud-reminders`).
    pub fn default_dir() -> Option<PathBuf> {
        store::data_dir("icloud-reminders")
    }

    pub fn lock(&self) -> io::Result<Lock> {
        store::lock(&self.dir, "lock")
    }

    /// The cache: empty when there is none yet, or when an older version
    /// of the app wrote it (the next sync then fetches everything).
    pub fn cache(&self) -> io::Result<Cache> {
        Ok(store::read_json_version(&self.dir.join("cache.json"), CACHE_VERSION)?.unwrap_or_default())
    }

    pub fn save_cache(&self, cache: &Cache) -> io::Result<()> {
        let mut cache = cache.clone();
        cache.version = CACHE_VERSION;
        store::write_json(&self.dir.join("cache.json"), &cache)
    }

    /// What has notified; empty before the first background run.
    pub fn notified(&self) -> io::Result<Notified> {
        Ok(store::read_json(&self.dir.join("notified.json"))?.unwrap_or_default())
    }

    pub fn save_notified(&self, n: &Notified) -> io::Result<()> {
        store::write_json(&self.dir.join("notified.json"), n)
    }

    /// The Markdown folder's state; `None` before its first run.
    pub fn folder(&self) -> io::Result<Option<folder::State>> {
        store::read_json(&self.dir.join("folder.json"))
    }

    pub fn save_folder(&self, state: &folder::State) -> io::Result<()> {
        store::write_json(&self.dir.join("folder.json"), state)
    }

    /// Held over a whole folder run, so two never work on the same files.
    pub fn folder_lock(&self) -> io::Result<Lock> {
        store::lock(&self.dir, "folder.lock")
    }

    /// This computer's replica ID, made the first time it is needed.
    pub fn replica(&self) -> io::Result<String> {
        let path = self.dir.join("replica");
        match fs::read_to_string(&path) {
            Ok(s) => match uuid::Uuid::parse_str(s.trim()) {
                Ok(_) => Ok(s.trim().to_owned()),
                Err(e) => Err(store::damaged(&path, e)),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let id = uuid::Uuid::new_v4().to_string().to_uppercase();
                store::write_atomic(&path, id.as_bytes())?;
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }
}
