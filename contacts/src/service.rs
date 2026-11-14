//! Sync and writes: what the window and the command line both do, through
//! one [`SessionTransport`] and one [`Store`].
//!
//! A sync fetches every contact (one request after `/co/startup`; a few
//! thousand cards are a few hundred kilobytes) and replaces the cache.
//! Writes go straight to iCloud, carrying the cached tokens (fresh ones
//! from `/co/startup` when iCloud refuses those), and the cache takes the
//! card as iCloud saved it. There is no offline queue: a change made without
//! a network fails and says so.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::api::{Client, Error, Result, SessionTransport, Tokens};
use crate::model::{Change, Contact};
use crate::store::{Cache, Store};

pub struct Service<'a> {
    t: &'a SessionTransport,
    store: Store,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub contacts: usize,
    /// Cards added, changed or gone since the last sync.
    pub changed: usize,
}

/// The contact a write saved, and why the local cache may not show it.
#[derive(Debug)]
pub struct Saved {
    pub value: Contact,
    pub warning: Option<String>,
}

fn io(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

impl<'a> Service<'a> {
    pub fn new(t: &'a SessionTransport, store: Store) -> Self {
        Service { t, store }
    }

    /// The cache as last saved (it is replaced whole, so a read needs no
    /// lock).
    pub fn cache(&self) -> Result<Cache> {
        self.store.cache().map_err(io)
    }

    /// Fetches every contact, saves them, and returns the new cache.
    pub fn sync(&self) -> Result<(SyncReport, Cache)> {
        let fetched = Client::connect(self.t)?.fetch()?;
        let _lock = self.store.lock().map_err(io)?;
        let before = self.cache()?;
        let mut cache = Cache {
            tokens: Some(fetched.tokens),
            me: fetched.me,
            contacts: fetched.cards.into_iter().map(Contact).collect(),
            ..Cache::default()
        };
        cache.sort();
        let changed = changes(&before, &cache);
        self.store.save_cache(&mut cache).map_err(io)?;
        let report = SyncReport {
            contacts: cache.contacts.len(),
            changed,
        };
        Ok((report, cache))
    }

    /// Runs one write with the cached tokens; with fresh ones when iCloud
    /// refuses those (or none are cached). The tokens it ends with are
    /// saved, with the card, by [`Service::record`].
    fn with_tokens<T>(&self, client: &Client, write: impl Fn(&Tokens) -> Result<T>) -> Result<T> {
        if let Some(tokens) = self.cache()?.tokens {
            match write(&tokens) {
                Err(e) if e.is_stale_token() => {}
                other => return other,
            }
        }
        let (tokens, _) = client.startup()?;
        write(&tokens)
    }

    /// Folds a write's result (its tokens, and `change`: the card as saved,
    /// or gone) into the cache. The write happened, so a failure here is a
    /// warning, not an error: reporting it as failed would invite writing it
    /// again.
    fn record(&self, tokens: Tokens, change: impl FnOnce(&mut Cache)) -> Option<String> {
        let save = || -> std::io::Result<()> {
            let _lock = self.store.lock()?;
            let mut cache = self.store.cache()?;
            cache.tokens = Some(tokens);
            change(&mut cache);
            self.store.save_cache(&mut cache)
        };
        save()
            .err()
            .map(|e| format!("saved in iCloud; local cache not updated: {e}"))
    }

    /// Puts the card a write answered in the cache.
    fn put(&self, (card, tokens): (Map<String, Value>, Tokens)) -> Saved {
        let value = Contact(card);
        let warning = self.record(tokens, |cache| cache.put(value.clone()));
        Saved { value, warning }
    }

    /// Adds a contact made of `changes` (a new card has nothing to remove);
    /// `is_company` shows it by its company name.
    pub fn add(&self, changes: &[Change], is_company: bool) -> Result<Saved> {
        let mut card = Contact::new(is_company);
        card.apply(changes).map_err(Error::Other)?;
        let client = Client::connect(self.t)?;
        Ok(self.put(self.with_tokens(&client, |t| client.write(t, None, &card.0))?))
    }

    /// Writes `changes` to the contact `c` (as cached). When it changed on
    /// another device since (iCloud's 412), the changes are applied once
    /// more to a fresh copy and written over it: they are field-level
    /// intents (this last name, add this number), so what they did not
    /// touch keeps the other device's version. Those the fresh copy already
    /// has are dropped, and when none is left nothing is written.
    pub fn update(&self, c: &Contact, changes: &[Change]) -> Result<Saved> {
        let client = Client::connect(self.t)?;
        let write = |base: &Contact, changes: &[Change]| -> Result<(Map<String, Value>, Tokens)> {
            let mut card = base.clone();
            card.apply(changes).map_err(Error::Other)?;
            self.with_tokens(&client, |t| client.write(t, Some("PUT"), &card.0))
        };
        let written = match write(c, changes) {
            Err(e) if e.is_conflict() => {
                let fresh = self
                    .fresh(c)?
                    .ok_or_else(|| Error::Other(format!("\"{}\" has been deleted", c.display_name())))?;
                let left: Vec<Change> = changes.iter().filter(|ch| !fresh.has(ch)).cloned().collect();
                if left.is_empty() {
                    return Ok(Saved {
                        value: fresh,
                        warning: None,
                    });
                }
                write(&fresh, &left)?
            }
            other => other?,
        };
        Ok(self.put(written))
    }

    /// Deletes the contact `c`. When it changed on another device since, it
    /// is deleted as it is now; when it is already gone, that is done too.
    /// Answers why the local cache may still show it, if it may.
    pub fn delete(&self, c: &Contact) -> Result<Option<String>> {
        let client = Client::connect(self.t)?;
        let tokens = match self.with_tokens(&client, |t| client.delete(t, c.id(), c.etag())) {
            Err(e) if e.is_conflict() => {
                let Some(fresh) = self.fresh(c)? else {
                    return Ok(None);
                };
                self.with_tokens(&client, |t| client.delete(t, fresh.id(), fresh.etag()))?
            }
            other => other?,
        };
        Ok(self.record(tokens, |cache| cache.remove(c.id())))
    }

    /// The contact `c` as iCloud has it now (a sync: there is no call for
    /// one card); `None` when it has been deleted.
    fn fresh(&self, c: &Contact) -> Result<Option<Contact>> {
        let (_, cache) = self.sync()?;
        Ok(cache.contacts.into_iter().find(|o| o.id() == c.id()))
    }
}

/// Cards added, changed (another etag) or gone between two caches.
fn changes(before: &Cache, after: &Cache) -> usize {
    fn etags(c: &Cache) -> HashMap<&str, &str> {
        c.contacts.iter().map(|c| (c.id(), c.etag())).collect()
    }
    let (was, now) = (etags(before), etags(after));
    let changed_or_new = now.iter().filter(|(id, etag)| was.get(*id) != Some(*etag)).count();
    let gone = was.keys().filter(|id| !now.contains_key(*id)).count();
    changed_or_new + gone
}
