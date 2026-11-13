//! Sync and writes: what the window and the command line both do, through
//! one [`Transport`] and one [`Store`].
//!
//! A sync fetches every contact (one request after `/co/startup`; a few
//! thousand cards are a few hundred kilobytes) and replaces the cache.
//! Writes go straight to iCloud, carrying the cached tokens (fresh ones
//! from `/co/startup` when iCloud refuses those), and the cache takes the
//! card as iCloud saved it. There is no offline queue: a change made without
//! a network fails and says so.

use std::collections::HashMap;

use crate::api::{Client, Error, Result, Tokens, Transport, Written};
use crate::model::{Change, Contact};
use crate::store::{Cache, Store};

pub struct Service<'a> {
    t: &'a dyn Transport,
    store: Store,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub contacts: usize,
    /// Cards added, changed or gone since the last sync.
    pub changed: usize,
}

/// What a write saved, and why the local cache may not show it.
#[derive(Debug)]
pub struct Saved<T> {
    pub value: T,
    pub warning: Option<String>,
}

impl<T> Saved<T> {
    fn map<U>(self, f: impl FnOnce(T) -> U) -> Saved<U> {
        Saved {
            value: f(self.value),
            warning: self.warning,
        }
    }
}

fn io(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

impl<'a> Service<'a> {
    pub fn new(t: &'a dyn Transport, store: Store) -> Self {
        Service { t, store }
    }

    /// The cache as last saved (it is replaced whole, so a read needs no
    /// lock).
    pub fn cache(&self) -> Result<Cache> {
        self.store.cache().map_err(io)
    }

    /// Fetches every contact, saves them, and returns the new cache.
    pub fn sync(&self) -> Result<(SyncReport, Cache)> {
        let account = self.t.account()?;
        let fetched = Client::connect(self.t)?.fetch()?;
        let _lock = self.store.lock().map_err(io)?;
        let before = self.cache()?;
        let mut cache = Cache {
            account: account.clone(),
            tokens: Some(fetched.tokens),
            me: fetched.me,
            synced_ms: Some(icloud_session::time::now_ms()),
            contacts: fetched.cards.into_iter().map(Contact).collect(),
            ..Cache::default()
        };
        cache.sort();
        let changed = if before.account == account {
            changes(&before, &cache)
        } else {
            cache.contacts.len()
        };
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
    fn with_tokens(&self, client: &Client, write: impl Fn(&Tokens) -> Result<Written>) -> Result<Written> {
        if let Some(tokens) = self.cache()?.tokens {
            match write(&tokens) {
                Err(e) if e.is_stale_token() => {}
                other => return other,
            }
        }
        let (tokens, _) = client.startup()?;
        write(&tokens)
    }

    /// Folds a write's result (the card `id` as saved, or gone) into the
    /// cache. The write happened, so a failure here is a warning, not an
    /// error: reporting it as failed would invite writing it again.
    fn record(&self, id: &str, w: Written) -> Saved<Option<Contact>> {
        let card = w.card.map(Contact);
        let save = || -> std::io::Result<()> {
            let _lock = self.store.lock()?;
            let mut cache = self.store.cache()?;
            cache.tokens = Some(w.tokens);
            match &card {
                Some(c) => cache.put(c.clone()),
                None => cache.remove(id),
            }
            self.store.save_cache(&mut cache)
        };
        Saved {
            warning: save()
                .err()
                .map(|e| format!("saved in iCloud; local cache not updated: {e}")),
            value: card,
        }
    }

    /// Adds a contact made of `changes` (a new card has nothing to remove);
    /// `is_company` shows it by its company name.
    pub fn add(&self, changes: &[Change], is_company: bool) -> Result<Saved<Contact>> {
        let mut card = Contact::new(is_company);
        card.apply(changes).map_err(Error::Other)?;
        let client = Client::connect(self.t)?;
        let w = self.with_tokens(&client, |t| client.write(t, None, &card.0))?;
        Ok(self.record(card.id(), w).map(|c| c.expect("a create answers its card")))
    }

    /// Writes `changes` to the contact `c` (as cached). When it changed on
    /// another device since (iCloud's 412), the changes are applied once
    /// more to a fresh copy and written over it: they are field-level
    /// intents (this last name, add this number), so what they did not
    /// touch keeps the other device's version. Those the fresh copy already
    /// has are dropped, and when none is left nothing is written.
    pub fn update(&self, c: &Contact, changes: &[Change]) -> Result<Saved<Contact>> {
        let client = Client::connect(self.t)?;
        let write = |base: &Contact, changes: &[Change]| -> Result<Written> {
            let mut card = base.clone();
            card.apply(changes).map_err(Error::Other)?;
            self.with_tokens(&client, |t| client.write(t, Some("PUT"), &card.0))
        };
        let w = match write(c, changes) {
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
        Ok(self.record(c.id(), w).map(|c| c.expect("an update answers its card")))
    }

    /// Deletes the contact `c`. When it changed on another device since, it
    /// is deleted as it is now; when it is already gone, that is done too.
    pub fn delete(&self, c: &Contact) -> Result<Saved<()>> {
        let client = Client::connect(self.t)?;
        let w = match self.with_tokens(&client, |t| client.delete(t, c.id(), c.etag())) {
            Err(e) if e.is_conflict() => {
                let Some(fresh) = self.fresh(c)? else {
                    return Ok(Saved {
                        value: (),
                        warning: None,
                    });
                };
                self.with_tokens(&client, |t| client.delete(t, fresh.id(), fresh.etag()))?
            }
            other => other?,
        };
        Ok(self.record(c.id(), w).map(drop))
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
