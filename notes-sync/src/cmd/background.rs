//! `background`: what the Notes app runs every minute, from its systemd timer
//! while the window is closed and from the window while it is open. It
//! decides whether to sync, from the vault alone and the last sync's record,
//! and then syncs (`sync`) or does nothing.
//!
//! - It syncs when the last sync is `every` old (15 minutes by default; the
//!   window asks for a minute while it is in front), or there was none:
//!   the pull cadence, and what retries a failure.
//! - It syncs sooner when the vault has local changes to push
//!   ([`push::local_changes`], no network) made after the last sync
//!   started. A change that sync left behind (iCloud refused it, or it
//!   failed) is not sent again every minute: it waits for the next due
//!   sync, or a new edit.
//! - Never while one of those changes is younger than [`SETTLE_MS`]: the
//!   file is likely still being written (an editor's autosave, an agent
//!   mid-edit), and each push is a version in iCloud. The next run sends it.
//!
//! Each sync (`sync` or this) is recorded, when it started and whether it
//! worked, in `background.json` beside the vault's state. A sync that is
//! due but finds nobody signed in to icloud-session (or icloud-session out
//! of reach) skips, and is recorded as a failed one, so it is asked again
//! only when the next sync is due.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::push::LocalChanges;
use crate::vault::state::state_dir;

/// A change this young (ms) is left for the next run.
pub const SETTLE_MS: i64 = 10_000;
/// The default `every`: the pull cadence while nothing asks for more.
pub const DEFAULT_EVERY_SECS: u64 = 15 * 60;
/// The record's file name, in the vault's state directory.
pub const RECORD_FILE_NAME: &str = "background.json";

/// The last sync: when it started (ms since the epoch) and whether it
/// worked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRun {
    pub last_run_ms: i64,
    pub last_ok: bool,
}

/// What a run decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The last sync is `every` old, or there was none: sync.
    Due,
    /// Local changes made since the last sync started: sync.
    Changes,
    /// A local change is younger than [`SETTLE_MS`]: nothing now.
    Settling,
    /// Nothing to do.
    Idle,
}

impl Step {
    pub fn syncs(self) -> bool {
        matches!(self, Step::Due | Step::Changes)
    }
}

/// The decision at `now_ms`, from the last sync and the vault's local
/// changes (see the module docs).
pub fn decide(now_ms: i64, last: Option<LastRun>, changes: &LocalChanges, every_ms: i64) -> Step {
    // A time ahead of the clock (skew, a copied file) is no edit in progress.
    if changes.pending
        && let Some(changed) = changes.newest_change_ms
        && (0..SETTLE_MS).contains(&(now_ms - changed))
    {
        return Step::Settling;
    }
    let Some(last) = last else {
        return Step::Due;
    };
    // A last run in the future is a clock set back: due, not put off for good.
    if now_ms - last.last_run_ms >= every_ms || now_ms < last.last_run_ms {
        return Step::Due;
    }
    // No time at all is a cut-off push to settle: sent unless the last
    // sync failed.
    let new_since = changes
        .newest_change_ms
        .map_or(last.last_ok, |changed| changed > last.last_run_ms);
    if changes.pending && new_since {
        Step::Changes
    } else {
        Step::Idle
    }
}

fn record_path(target_dir: &Path) -> PathBuf {
    state_dir(target_dir).join(RECORD_FILE_NAME)
}

/// The last sync's record; `None` when there is none (or it is unreadable,
/// which makes a sync due).
pub fn last_run(target_dir: &Path) -> Option<LastRun> {
    let bytes = std::fs::read(record_path(target_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Records a sync that started at `started_ms`. Best effort: without a
/// record the next run finds a sync due.
pub fn record_run(target_dir: &Path, started_ms: i64, ok: bool) {
    let record = LastRun {
        last_run_ms: started_ms,
        last_ok: ok,
    };
    if let Ok(bytes) = serde_json::to_vec(&record) {
        let _ = crate::vault::local::write_atomic(&record_path(target_dir), &bytes, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const MIN: i64 = 60 * 1000;
    const EVERY: i64 = 15 * MIN;

    fn changes(pending: bool, changed_ago: Option<i64>) -> LocalChanges {
        LocalChanges {
            pending,
            files: if pending { vec!["Notes/A.md".into()] } else { Vec::new() },
            newest_change_ms: changed_ago.map(|ago| NOW - ago),
        }
    }

    fn last(ago: i64, ok: bool) -> Option<LastRun> {
        Some(LastRun {
            last_run_ms: NOW - ago,
            last_ok: ok,
        })
    }

    fn step(last: Option<LastRun>, changes: &LocalChanges) -> Step {
        decide(NOW, last, changes, EVERY)
    }

    #[test]
    fn never_run_is_due() {
        assert_eq!(step(None, &changes(false, None)), Step::Due);
    }

    #[test]
    fn a_last_sync_every_old_is_due_failed_or_not() {
        assert_eq!(step(last(EVERY, true), &changes(false, None)), Step::Due);
        assert_eq!(step(last(20 * MIN, false), &changes(false, None)), Step::Due);
        assert_eq!(step(last(EVERY - 1, true), &changes(false, None)), Step::Idle);
    }

    #[test]
    fn every_is_the_callers() {
        // The window in front asks for a minute.
        assert_eq!(decide(NOW, last(MIN, true), &changes(false, None), MIN), Step::Due);
        assert_eq!(decide(NOW, last(MIN - 1, true), &changes(false, None), MIN), Step::Idle);
    }

    #[test]
    fn a_change_since_the_last_sync_is_sent_at_once() {
        assert_eq!(step(last(2 * MIN, true), &changes(true, Some(30_000))), Step::Changes);
    }

    #[test]
    fn a_change_the_last_sync_left_waits_for_the_next_due_one() {
        // Refused by iCloud (the sync failed), or left by a sync that worked.
        assert_eq!(step(last(2 * MIN, false), &changes(true, Some(3 * MIN))), Step::Idle);
        assert_eq!(step(last(2 * MIN, true), &changes(true, Some(3 * MIN))), Step::Idle);
    }

    #[test]
    fn after_a_failed_sync_a_new_edit_is_sent() {
        assert_eq!(step(last(2 * MIN, false), &changes(true, Some(MIN))), Step::Changes);
    }

    #[test]
    fn a_change_younger_than_ten_seconds_waits_even_when_due() {
        assert_eq!(step(last(2 * MIN, true), &changes(true, Some(9_000))), Step::Settling);
        assert_eq!(step(None, &changes(true, Some(9_000))), Step::Settling);
        assert_eq!(step(last(20 * MIN, true), &changes(true, Some(0))), Step::Settling);
        assert_eq!(
            step(last(2 * MIN, true), &changes(true, Some(SETTLE_MS))),
            Step::Changes
        );
    }

    #[test]
    fn a_cut_off_push_with_no_time_is_sent_unless_the_last_sync_failed() {
        assert_eq!(step(last(2 * MIN, true), &changes(true, None)), Step::Changes);
        assert_eq!(step(last(2 * MIN, false), &changes(true, None)), Step::Idle);
    }

    #[test]
    fn times_ahead_of_the_clock_neither_settle_nor_put_off_for_good() {
        assert_eq!(step(last(2 * MIN, true), &changes(true, Some(-5 * MIN))), Step::Changes);
        assert_eq!(step(last(-5 * MIN, true), &changes(false, None)), Step::Due);
    }

    #[test]
    fn the_record_round_trips_beside_the_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".icloud-notes")).unwrap();
        assert_eq!(last_run(dir.path()), None);
        record_run(dir.path(), NOW, false);
        assert_eq!(last_run(dir.path()), last(0, false));
        assert!(dir.path().join(".icloud-notes/background.json").exists());
    }
}
