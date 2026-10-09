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
        let before = if start.account == account { snapshot(&start) } else { Snapshot::new() };
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

        let fetched = records
            .iter()
            .map(|r| Reminder::from_record(r).map_err(Error::Other))
            .collect::<Result<Vec<_>>>()?;
        let _lock = self.store.lock().map_err(io)?;
        // Re-read under the lock: a write may have landed meanwhile.
        let mut cache = self.cache()?;
        if cache.account != account {
            cache.reminders.clear();
        }
        cache.account = account;
        let changed = records.len();
        cache.reminders = merge(&before, std::mem::take(&mut cache.reminders), fetched, full);
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
            apply_record(&mut cache, r)?;
        }
        self.store.save_cache(&cache).map_err(io)
    }

    /// A fresh copy of one reminder from iCloud; `None` when it is gone.
    fn fetch(&self, id: &str) -> Result<Option<Reminder>> {
        let ck = CloudKit::connect(self.t)?;
        let records = ck.lookup(&[id])?;
        let live = saved_reminder(id, &records)?;
        self.store_records(&records)?;
        Ok(live)
    }

    /// Folds a successful write's records into the cache. The write
    /// happened, so a failure here is a warning, not an error: reporting
    /// it as failed would invite writing it again.
    fn cache_written(&self, records: &[Record]) -> Option<String> {
        self.store_records(records)
            .err()
            .map(|e| format!("saved in iCloud; local cache not updated: {e}"))
    }

    /// Adds a reminder to `list_id`.
    pub fn add(&self, list_id: &str, title: &str, notes: &str, due: Option<&Due>) -> Result<Saved<Reminder>> {
        let replica = self.store.replica().map_err(io)?;
        let uuid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let ck = CloudKit::connect(self.t)?;
        let op = model::create_op(&uuid, list_id, title, notes, due, &replica, now_ms());
        let records = ck.modify(vec![op])?;
        let id = format!("Reminder/{uuid}");
        let live = saved_reminder(&id, &records)?
            .ok_or_else(|| Error::Other(format!("CloudKit answered the new {id} as deleted")))?;
        Ok(Saved {
            warning: self.cache_written(&records),
            value: live,
        })
    }

    /// Writes `changes` to the reminder `r` (as cached). When it changed on
    /// another device since (CloudKit's CONFLICT), the change is written
    /// once more over the fresh copy: these are field-level intents (this
    /// title, completed), so they win over what they did not touch; those
    /// the fresh copy already has are dropped, and when none is left
    /// nothing is written. Returns the reminder as saved; `None` after a
    /// delete.
    pub fn update(&self, r: &Reminder, changes: &[Change]) -> Result<Saved<Option<Reminder>>> {
        let replica = self.store.replica().map_err(io)?;
        let ck = CloudKit::connect(self.t)?;
        let write = |r: &Reminder, changes: &[Change]| {
            let op = model::update_op(r, changes, &replica, now_ms()).map_err(Error::Other)?;
            ck.modify(vec![op])
        };
        let records = match write(r, changes) {
            Err(e) if e.is_conflict() => {
                let Some(fresh) = self.fetch(&r.id)? else {
                    return if changes.contains(&Change::Deleted) {
                        Ok(Saved { value: None, warning: None })
                    } else {
                        Err(Error::Other(format!("\"{}\" has been deleted", r.title)))
                    };
                };
                let left: Vec<Change> = changes.iter().filter(|c| !has(&fresh, c)).cloned().collect();
                if left.is_empty() {
                    return Ok(Saved { value: Some(fresh), warning: None });
                }
                write(&fresh, &left)?
            }
            other => other?,
        };
        let live = saved_reminder(&r.id, &records)?;
        Ok(Saved {
            warning: self.cache_written(&records),
            value: live,
        })
    }
}

/// What a write saved, and why the local cache may not show it.
#[derive(Debug)]
pub struct Saved<T> {
    pub value: T,
    pub warning: Option<String>,
}

/// Whether `r` already is as `change` would make it.
fn has(r: &Reminder, change: &Change) -> bool {
    match change {
        Change::Title(t) => r.title == *t,
        Change::Notes(n) => r.notes == *n,
        Change::Completed(c) => r.completed == *c,
        Change::Due(d) => r.due == *d,
        Change::Deleted => false,
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

/// Folds a lookup or modify record into the cache: the server's copy, as
/// saved.
fn apply_record(cache: &mut Cache, r: &Record) -> Result<()> {
    match Reminder::from_record(r).map_err(Error::Other)? {
        Parsed::Live(rem) => {
            cache.reminders.insert(rem.id.clone(), *rem);
        }
        Parsed::Gone(id) => {
            cache.reminders.remove(&id);
        }
        Parsed::NotAReminder => {}
    }
    Ok(())
}

/// Each cached reminder's change tag when a sync began. A write that lands
/// while the sync fetches changes it: a new tag, a new id, or a gone one.
type Snapshot = BTreeMap<String, Option<String>>;

fn snapshot(cache: &Cache) -> Snapshot {
    cache.reminders.iter().map(|(id, r)| (id.clone(), r.change_tag.clone())).collect()
}

/// A sync's fetched reminders merged into the cache as it is now (`current`,
/// read under the lock). A reminder a write touched during the fetch keeps
/// its cached state, present or deleted, unless the fetched copy was
/// modified later; everything else takes the fetched state. A `full` fetch
/// is the whole set, so untouched reminders it lacks are gone.
fn merge(before: &Snapshot, current: BTreeMap<String, Reminder>, fetched: Vec<Parsed>, full: bool) -> BTreeMap<String, Reminder> {
    let touched = |id: &str| before.get(id) != current.get(id).map(|r| &r.change_tag);
    let mut out: BTreeMap<String, Reminder> = if full {
        current.iter().filter(|(id, _)| touched(id)).map(|(id, r)| (id.clone(), r.clone())).collect()
    } else {
        current.clone()
    };
    for p in fetched {
        match p {
            Parsed::Live(rem) => {
                if touched(&rem.id) {
                    let later = |c: &Reminder| matches!((rem.modified_ms, c.modified_ms), (Some(a), Some(b)) if a > b);
                    if !current.get(&rem.id).is_some_and(later) {
                        continue;
                    }
                }
                out.insert(rem.id.clone(), *rem);
            }
            Parsed::Gone(id) => {
                out.remove(&id);
            }
            Parsed::NotAReminder => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rem(id: &str, tag: &str, modified_ms: i64) -> Reminder {
        Reminder {
            id: id.into(),
            list_id: "List/A".into(),
            title: id.into(),
            notes: String::new(),
            completed: false,
            completed_ms: None,
            due: None,
            priority: 0,
            flagged: false,
            parent_id: None,
            alarms: 0,
            created_ms: None,
            modified_ms: Some(modified_ms),
            change_tag: Some(tag.into()),
            tokens: None,
        }
    }

    fn map(rs: Vec<Reminder>) -> BTreeMap<String, Reminder> {
        rs.into_iter().map(|r| (r.id.clone(), r)).collect()
    }

    fn live(r: &Reminder) -> Parsed {
        Parsed::Live(Box::new(r.clone()))
    }

    #[test]
    fn a_delete_during_the_fetch_stays_deleted() {
        let a = rem("Reminder/a", "t1", 10);
        let before = snapshot(&Cache { reminders: map(vec![a.clone()]), ..Cache::default() });
        for full in [false, true] {
            let out = merge(&before, BTreeMap::new(), vec![live(&a)], full);
            assert!(out.is_empty(), "full={full}");
        }
    }

    #[test]
    fn an_add_during_a_full_fetch_is_kept_and_untouched_absentees_go() {
        let (a, b, c) = (rem("Reminder/a", "t1", 10), rem("Reminder/b", "t1", 20), rem("Reminder/c", "t1", 5));
        // c was cached and unchanged; the server no longer has it.
        let before = snapshot(&Cache { reminders: map(vec![c.clone()]), ..Cache::default() });
        let out = merge(&before, map(vec![b.clone(), c]), vec![live(&a)], true);
        assert_eq!(out.keys().collect::<Vec<_>>(), ["Reminder/a", "Reminder/b"]);
    }

    #[test]
    fn an_edit_during_the_fetch_wins_unless_the_fetched_copy_is_later() {
        let old = rem("Reminder/a", "t1", 10);
        let mine = rem("Reminder/a", "t2", 20);
        let before = snapshot(&Cache { reminders: map(vec![old.clone()]), ..Cache::default() });
        let out = merge(&before, map(vec![mine.clone()]), vec![live(&old)], false);
        assert_eq!(out["Reminder/a"].change_tag.as_deref(), Some("t2"));
        let theirs = rem("Reminder/a", "t3", 30);
        let out = merge(&before, map(vec![mine]), vec![live(&theirs)], false);
        assert_eq!(out["Reminder/a"].change_tag.as_deref(), Some("t3"));
    }
}
