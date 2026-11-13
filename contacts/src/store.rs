//! What this computer keeps, in `~/.local/share/icloud-contacts/`:
//!
//! - `cache.json`: the contacts as last synced (each card whole, as iCloud
//!   sent it), the "me" card's id, the tokens writes carry, and the account
//!   (dsid) they belong to. Only a cache: iCloud holds the contacts, and a
//!   sync rebuilds it. No credentials: the iCloud session is
//!   icloud-session's, fetched over D-Bus per request.
//! - `lock`: `flock`ed around every read-modify-write of the cache, so the
//!   window and the command line never interleave.
//!
//! The cache is replaced whole (a temp file, then rename). One that does
//! not parse is an error naming it, never silently reset.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::Tokens;
use crate::model::Contact;

/// Bumped when `cache.json` changes shape; an older one is synced afresh.
const CACHE_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    /// The dsid the cache belongs to.
    pub account: String,
    pub tokens: Option<Tokens>,
    /// The "me" card's contactId.
    pub me: Option<String>,
    /// When the last sync finished (Unix ms).
    pub synced_ms: Option<i64>,
    /// Sorted as Contacts sorts them (last name, first name).
    pub contacts: Vec<Contact>,
}

impl Cache {
    pub fn get(&self, id: &str) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.id() == id)
    }

    pub fn is_me(&self, c: &Contact) -> bool {
        self.me.as_deref() == Some(c.id())
    }

    /// Puts the card in (replacing the one with its id) and keeps the order.
    pub fn put(&mut self, c: Contact) {
        self.contacts.retain(|o| o.id() != c.id());
        self.contacts.push(c);
        self.sort();
    }

    pub fn remove(&mut self, id: &str) {
        self.contacts.retain(|c| c.id() != id);
    }

    pub fn sort(&mut self) {
        self.contacts.sort_by_cached_key(Contact::sort_key);
    }
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

    /// `$XDG_DATA_HOME/icloud-contacts` (`~/.local/share/icloud-contacts`;
    /// a relative `XDG_DATA_HOME` is ignored, as the XDG spec says).
    pub fn default_dir() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(data.join("icloud-contacts"))
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
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Cache::default()),
            Err(e) => return Err(e),
        };
        #[derive(Deserialize)]
        struct Header {
            version: Option<u32>,
        }
        let header: Header = serde_json::from_slice(&bytes).map_err(|e| damaged(&path, e))?;
        if header.version != Some(CACHE_VERSION) {
            return Ok(Cache::default());
        }
        serde_json::from_slice(&bytes).map_err(|e| damaged(&path, e))
    }

    /// Saves `cache`, stamped with this version's [`CACHE_VERSION`].
    pub fn save_cache(&self, cache: &mut Cache) -> io::Result<()> {
        cache.version = CACHE_VERSION;
        write_atomic(
            &self.dir.join("cache.json"),
            &serde_json::to_vec(cache).expect("serializable"),
        )
    }
}

fn damaged(path: &Path, e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{} is damaged ({e}); delete it to start over", path.display()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("a file in the data directory");
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}
