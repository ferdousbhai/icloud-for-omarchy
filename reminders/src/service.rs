//! Sync and writes: what the window, the command line and the background
//! timer all do, through one [`Transport`] and one [`Store`].
//!
//! A sync takes every `List` (a full changes/zone for that type: lists are
//! few, and their changes do not show in the reminders' delta stream, per
//! Psavvas/iCloud-Reminders-for-Windows), then the `Reminder` records
//! changed since the stored sync token (all of them without one, or when
//! CloudKit refuses the token). Writes go straight to iCloud; the cache is
//! updated from what CloudKit answers. There is no offline queue: a change
//! made without a network fails and says so.

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

impl<'a> Service<'a> {
    pub fn new(t: &'a dyn Transport, store: Store) -> Self {
        Service { t, store }
    }

    fn io(e: std::io::Error) -> Error {
        Error::Other(format!("cannot write the cache: {e}"))
    }

    /// The cache as last saved (files are replaced whole, so a read needs
    /// no lock).
    pub fn cache(&self) -> Cache {
        self.store.cache()
    }

    /// Fetches what changed and saves it. `full` ignores the sync token.
    pub fn sync(&self, full: bool) -> Result<SyncReport> {
        let account = self.t.account()?;
        let ck = CloudKit::connect(self.t)?;
        let (list_records, _) = ck.all_changes(&["List"], None)?;
        let mut lists: Vec<List> = list_records.iter().filter_map(List::from_record).collect();
        lists.sort_by_key(|l| l.name.to_lowercase());

        let start = self.store.cache();
        let same_account = start.account == account;
        let token = start.sync_token.clone().filter(|_| same_account && !full);
        let (records, new_token, full) = match token {
            Some(t) => match ck.all_changes(&["Reminder"], Some(&t)) {
                Ok((records, token)) => (records, token, false),
                // An expired or unknown token: start over.
                Err(Error::CloudKit { .. }) => {
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

        let _lock = self.store.lock().map_err(Self::io)?;
        // Re-read under the lock: a write may have landed meanwhile.
        let mut cache = self.store.cache();
        if full || cache.account != account {
            cache.reminders.clear();
        }
        cache.account = account;
        let changed = records.len();
        for r in &records {
            apply_record(&mut cache, r);
        }
        cache.lists = lists;
        cache.sync_token = Some(new_token);
        cache.synced_ms = Some(now_ms());
        self.store.save_cache(&cache).map_err(Self::io)?;
        Ok(SyncReport {
            lists: cache.lists.len(),
            reminders: cache.reminders.len(),
            changed,
            full,
        })
    }

    /// Saves what CloudKit answered for one reminder.
    fn store_records(&self, records: &[Record]) -> Result<()> {
        let _lock = self.store.lock().map_err(Self::io)?;
        let mut cache = self.store.cache();
        for r in records {
            apply_record(&mut cache, r);
        }
        self.store.save_cache(&cache).map_err(Self::io)
    }

    /// A fresh copy of one reminder from iCloud.
    fn fetch(&self, id: &str) -> Result<Reminder> {
        let ck = CloudKit::connect(self.t)?;
        let records = ck.lookup(&[id])?;
        let rec = records
            .iter()
            .find(|r| r.name == id)
            .ok_or_else(|| Error::Other(format!("iCloud has no reminder {id}")))?;
        if let Some((code, reason)) = &rec.error {
            return Err(Error::CloudKit {
                code: code.clone(),
                reason: reason.clone(),
            });
        }
        self.store_records(std::slice::from_ref(rec))?;
        match Reminder::from_record(rec) {
            Parsed::Live(r) => Ok(*r),
            _ => Err(Error::Other(format!("{id} has been deleted"))),
        }
    }

    /// Adds a reminder to `list_id`.
    pub fn add(&self, list_id: &str, title: &str, notes: &str, due: Option<&Due>) -> Result<Reminder> {
        let replica = self.store.replica().map_err(Self::io)?;
        let uuid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let ck = CloudKit::connect(self.t)?;
        let op = model::create_op(&uuid, list_id, title, notes, due, &replica, now_ms());
        let records = ck.modify(vec![op])?;
        let id = format!("Reminder/{uuid}");
        self.saved(&id, &records)
    }

    /// The reminder as CloudKit answered a write with it, or looked up when
    /// the answer carries no fields.
    fn saved(&self, id: &str, records: &[Record]) -> Result<Reminder> {
        if let Some(rec) = records.iter().find(|r| r.name == id && !r.fields.is_empty())
            && let Parsed::Live(r) = Reminder::from_record(rec)
        {
            self.store_records(std::slice::from_ref(rec))?;
            return Ok(*r);
        }
        self.fetch(id)
    }

    /// Writes `changes` to the reminder `r` (as cached). When it changed on
    /// another device since (CloudKit's CONFLICT), the change is written
    /// once more over the fresh copy: these are field-level intents (this
    /// title, completed), so they win over what they did not touch.
    pub fn update(&self, r: &Reminder, changes: &[Change]) -> Result<Option<Reminder>> {
        let replica = self.store.replica().map_err(Self::io)?;
        let ck = CloudKit::connect(self.t)?;
        let deleting = changes.contains(&Change::Deleted);
        let write = |r: &Reminder| ck.modify(vec![model::update_op(r, changes, &replica, now_ms())]);
        let records = match write(r) {
            Err(e) if e.is_conflict() => write(&self.fetch(&r.id)?)?,
            other => other?,
        };
        if deleting {
            let _lock = self.store.lock().map_err(Self::io)?;
            let mut cache = self.store.cache();
            cache.reminders.remove(&r.id);
            self.store.save_cache(&cache).map_err(Self::io)?;
            return Ok(None);
        }
        self.saved(&r.id, &records).map(Some)
    }
}

/// Folds one changes/zone or lookup record into the cache.
fn apply_record(cache: &mut Cache, r: &Record) {
    match Reminder::from_record(r) {
        Parsed::Live(rem) => {
            cache.reminders.insert(rem.id.clone(), *rem);
        }
        Parsed::Gone(id) => {
            cache.reminders.remove(&id);
        }
        Parsed::NotAReminder => {}
    }
}
