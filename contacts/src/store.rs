//! What this computer keeps, in `~/.local/share/icloud-contacts/`:
//!
//! - `cache.json`: the contacts as last synced (each card whole, as iCloud
//!   sent it), the "me" card's id and the tokens writes carry. Only a cache:
//!   iCloud holds the contacts, and a sync rebuilds it. No credentials: the
//!   iCloud session is icloud-session's, fetched over D-Bus per request.
//! - `lock`: `flock`ed around every read-modify-write of the cache, so the
//!   window and the command line never interleave.
//!
//! The cache is replaced whole (a temp file, then rename). One that does
//! not parse is an error naming it, never silently reset.

use std::io;
use std::path::PathBuf;

use icloud_app::store;
use serde::{Deserialize, Serialize};

use crate::api::Tokens;
use crate::model::Contact;

/// Bumped when `cache.json` changes shape; an older one is synced afresh.
const CACHE_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub tokens: Option<Tokens>,
    /// The "me" card's contactId.
    pub me: Option<String>,
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

pub use icloud_app::store::Lock;

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// `$XDG_DATA_HOME/icloud-contacts` (`~/.local/share/icloud-contacts`).
    pub fn default_dir() -> Option<PathBuf> {
        store::data_dir("icloud-contacts")
    }

    pub fn lock(&self) -> io::Result<Lock> {
        store::lock(&self.dir, "lock")
    }

    /// The cache: empty when there is none yet, or when an older version
    /// of the app wrote it (the next sync then fetches everything).
    pub fn cache(&self) -> io::Result<Cache> {
        Ok(store::read_json_version(&self.dir.join("cache.json"), CACHE_VERSION)?.unwrap_or_default())
    }

    /// Saves `cache`, stamped with this version's [`CACHE_VERSION`].
    pub fn save_cache(&self, cache: &mut Cache) -> io::Result<()> {
        cache.version = CACHE_VERSION;
        store::write_json(&self.dir.join("cache.json"), cache)
    }
}
