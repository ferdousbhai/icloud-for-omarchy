//! Sync and writes: what the window, the command line and the background
//! timer all do, through one [`Transport`] and one [`Store`].
//!
//! A sync takes every `List` (a full changes/zone for that type: lists are
//! few, and their changes do not show in the reminders' delta stream, per
//! Psavvas/iCloud-Reminders-for-Windows), then the `Reminder` records
//! changed since the stored sync token (all of them without one, or when
//! CloudKit answers `CHANGE_TOKEN_EXPIRED`). Writes go straight to iCloud;
//! the cache is updated from the records records/modify answers with
//! (CloudKit Web Services returns the saved records). There is no offline
//! queue: a change made without a network fails and says so.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::cloudkit::{CloudKit, Error, Record, Result, Transport};
use crate::due::Due;
use crate::model::{self, Change, List, Parsed, Reminder};
use crate::store::{Cache, Store};

/// A background run syncs when the last sync is older than this.
pub const SYNC_EVERY: Duration = Duration::from_secs(4 * 60);

pub struct Service<'a> {
    t: &'a dyn Transport,
    pub store: Store,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub lists: usize,
    pub reminders: usize,
    /// Reminder records that came in (changed or gone).
    pub changed: usize,
    /// The token was refused (or missing) and every reminder was fetched.
    pub full: bool,
}

pub fn now_ms() -> i64 {
    icloud_session::time::now_ms()
}

fn io(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

impl<'a> Service<'a> {
    pub fn new(t: &'a dyn Transport, store: Store) -> Self {
        Service { t, store }
    }

    /// The cache as last saved (files are replaced whole, so a read needs
    /// no lock).
    pub fn cache(&self) -> Result<Cache> {
        self.store.cache().map_err(io)
    }

    /// Fetches what changed and saves it. `full` ignores the sync token.
    pub fn sync(&self, full: bool) -> Result<SyncReport> {
        let account = self.t.account()?;
        let ck = CloudKit::connect(self.t)?;
        let (list_records, _) = ck.all_changes(&["List"], None)?;
        let mut lists = Vec::new();
        for r in &list_records {
            if let Some(l) = List::from_record(r).map_err(Error::Other)? {
                lists.push(l);
            }
        }
        lists.sort_by_key(|l| l.name.to_lowercase());

        let start = self.cache()?;
        let token = start.sync_token.filter(|_| start.account == account && !full);
        let (records, new_token, full) = match token {
            Some(t) => match ck.all_changes(&["Reminder"], Some(&t)) {
                Ok((records, token)) => (records, token, false),
                // CloudKit refuses a stale token with CHANGE_TOKEN_EXPIRED, or at
                // zone level with BAD_REQUEST ("Unknown sync continuation type");
                // the Notes engine recovers from the latter the same way
                // (notes-sync/src/cloudkit/client.rs, fetch_zone_changes).
                Err(Error::CloudKit { code, .. }) if code == "CHANGE_TOKEN_EXPIRED" || code == "BAD_REQUEST" => {
                    let (records, token) = ck.all_changes(&["Reminder"], None)?;
                    (records, token, true)
                }
                Err(e) => return Err(e),
            },
            None => {
                let (records, token) = ck.all_changes(&["Reminder"], None)?;
                (records, token, true)
            }
        };

        let _lock = self.store.lock().map_err(io)?;
        // Re-read under the lock: a write may have landed meanwhile.
        let mut cache = self.cache()?;
        // What a write saved while the changes were being fetched is newer
        // than the fetched copy: keep it (see apply_record).
        let previous = if cache.account != account {
            Default::default()
        } else if full {
            std::mem::take(&mut cache.reminders)
        } else {
            cache.reminders.clone()
        };
        if cache.account != account {
            cache.reminders.clear();
        }
        cache.account = account;
        let changed = records.len();
        for r in &records {
            apply_record(&mut cache, r, &previous)?;
        }
        cache.lists = lists;
        cache.sync_token = Some(new_token);
        cache.synced_ms = Some(now_ms());
        self.store.save_cache(&cache).map_err(io)?;
        Ok(SyncReport {
            lists: cache.lists.len(),
            reminders: cache.reminders.len(),
            changed,
            full,
        })
    }

    /// Folds what CloudKit answered to a write into the cache: the server's
    /// copy, as saved.
    fn store_records(&self, records: &[Record]) -> Result<()> {
        let _lock = self.store.lock().map_err(io)?;
        let mut cache = self.cache()?;
        for r in records {
            apply_record(&mut cache, r, &BTreeMap::new())?;
        }
        self.store.save_cache(&cache).map_err(io)
    }

    /// A fresh copy of one reminder from iCloud.
    fn fetch(&self, id: &str) -> Result<Reminder> {
        let ck = CloudKit::connect(self.t)?;
        let records = ck.lookup(&[id])?;
        let live = saved_reminder(id, &records)?;
        self.store_records(&records)?;
        live.ok_or_else(|| Error::Other(format!("{id} has been deleted")))
    }

    /// Adds a reminder to `list_id`.
    pub fn add(&self, list_id: &str, title: &str, notes: &str, due: Option<&Due>) -> Result<Reminder> {
        let replica = self.store.replica().map_err(io)?;
        let uuid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let ck = CloudKit::connect(self.t)?;
        let op = model::create_op(&uuid, list_id, title, notes, due, &replica, now_ms());
        let records = ck.modify(vec![op])?;
        let id = format!("Reminder/{uuid}");
        let live = saved_reminder(&id, &records)?;
        self.store_records(&records)?;
        live.ok_or_else(|| Error::Other(format!("CloudKit answered the new {id} as deleted")))
    }

    /// Writes `changes` to the reminder `r` (as cached). When it changed on
    /// another device since (CloudKit's CONFLICT), the change is written
    /// once more over the fresh copy: these are field-level intents (this
    /// title, completed), so they win over what they did not touch.
    /// Returns the reminder as saved; `None` after a delete.
    pub fn update(&self, r: &Reminder, changes: &[Change]) -> Result<Option<Reminder>> {
        let replica = self.store.replica().map_err(io)?;
        let ck = CloudKit::connect(self.t)?;
        let write = |r: &Reminder| {
            let op = model::update_op(r, changes, &replica, now_ms()).map_err(Error::Other)?;
            ck.modify(vec![op])
        };
        let records = match write(r) {
            Err(e) if e.is_conflict() => write(&self.fetch(&r.id)?)?,
            other => other?,
        };
        let live = saved_reminder(&r.id, &records)?;
        self.store_records(&records)?;
        Ok(live)
    }
}

/// The reminder `id` among the records CloudKit answered with: `Some` when
/// live, `None` when deleted. Missing from the answer is an error.
fn saved_reminder(id: &str, records: &[Record]) -> Result<Option<Reminder>> {
    let rec = records
        .iter()
        .find(|r| r.name == id)
        .ok_or_else(|| Error::Other(format!("CloudKit's answer has no record {id}")))?;
    if let Some((code, reason)) = &rec.error {
        return Err(Error::CloudKit {
            code: code.clone(),
            reason: reason.clone(),
        });
    }
    match Reminder::from_record(rec).map_err(Error::Other)? {
        Parsed::Live(r) => Ok(Some(*r)),
        Parsed::Gone(_) => Ok(None),
        Parsed::NotAReminder => Err(Error::Other(format!("{id} is not a reminder"))),
    }
}

/// Folds one changes/zone, lookup or modify record into the cache.
/// Applies one fetched record. A cached copy modified later than the
/// fetched one (a write that landed during the fetch) is kept; a deletion
/// always applies.
fn apply_record(cache: &mut Cache, r: &Record, previous: &BTreeMap<String, Reminder>) -> Result<()> {
    match Reminder::from_record(r).map_err(Error::Other)? {
        Parsed::Live(rem) => {
            let newer = previous
                .get(&rem.id)
                .filter(|c| matches!((c.modified_ms, rem.modified_ms), (Some(a), Some(b)) if a > b));
            let kept = newer.cloned().unwrap_or(*rem);
            cache.reminders.insert(kept.id.clone(), kept);
        }
        Parsed::Gone(id) => {
            cache.reminders.remove(&id);
        }
        Parsed::NotAReminder => {}
    }
    Ok(())
}
