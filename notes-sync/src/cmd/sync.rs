//! `sync`: push, then pull, in one process. The Notes app
//! (icloud-notes) syncs this way: one run, so one icloud-sessiond
//! `Session()` call, one TLS connection and one vault lock instead of two of
//! each. Each half is exactly what `push` and `pull` do on their own.
//!
//! The pull runs whatever the push did, except when the push found the
//! sign-in gone (every request would be refused the same way) or iCloud out
//! of reach (the pull would only fail again; the next sync retries both).
//! When the push refused a move because the note changed in iCloud since
//! the last pull, the push runs again after the pull has merged that change,
//! and its entries take the refused move's place in the push half.

use std::path::Path;

use super::SyncProgress;
use super::errors::{EXIT_ERROR, EXIT_OK, EXIT_SIGN_IN, Error};
use super::pull::{self, PullSummary};
use super::push::{self, PushOptions, PushResult};
use super::remote::{Connector, DefaultConnector, SharedConnector};

/// What a sync did: the push's result or error, and the pull's (`None` when
/// it did not run; see the module docs).
#[derive(Debug)]
pub struct SyncOutcome {
    pub push: Result<PushResult, Error>,
    pub pull: Option<Result<PullSummary, Error>>,
}

impl SyncOutcome {
    /// The run's exit code: 3 (sign-in required, or the vault not set up
    /// for this account) if either half exited 3, since nothing else can
    /// work until that is fixed; otherwise the higher of the two halves'
    /// codes, so 5 (unavailable) > 4 (not found) > 2 (usage) > 1 (error) >
    /// 0: a specific reason beats the generic error. A push that had a
    /// change refused counts as 1.
    pub fn exit_code(&self) -> u8 {
        let push = match &self.push {
            // A push that ran but had a change refused is an error too.
            Ok(result) if !result.failures().is_empty() => EXIT_ERROR,
            half => half_exit_code(half),
        };
        let pull = self.pull.as_ref().map_or(EXIT_OK, half_exit_code);
        if push == EXIT_SIGN_IN || pull == EXIT_SIGN_IN {
            EXIT_SIGN_IN
        } else {
            push.max(pull)
        }
    }
}

/// One half's exit code, as `push` or `pull` alone would exit.
pub fn half_exit_code<T>(half: &Result<T, Error>) -> u8 {
    half.as_ref().err().map_or(EXIT_OK, Error::exit_code)
}

/// Whether a failed push leaves the pull out: the pull would fail the same
/// way (the sign-in gone or refused, iCloud out of reach).
pub fn push_error_skips_pull(error: &Error) -> bool {
    matches!(
        error,
        Error::SignInRequired | Error::CloudKit(crate::cloudkit::CkError::SessionRejected { .. })
    ) || error.is_network()
}

/// `sync`.
pub fn run_sync(
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &PushOptions,
) -> SyncOutcome {
    run_sync_with(&DefaultConnector, target_dir, progress, on_status, options)
}

/// `sync` over an explicit connector, which it connects once for both
/// halves. `options` are the push half's (never a dry run).
pub fn run_sync_with(
    connector: &dyn Connector,
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &PushOptions,
) -> SyncOutcome {
    let shared = SharedConnector::new(connector);
    let options = PushOptions {
        dry_run: false,
        ..*options
    };
    let mut push = push::run_push_with(&shared, target_dir, on_status, &options);
    let pull = match &push {
        Err(error) if push_error_skips_pull(error) => None,
        _ => Some(pull::run_pull_with(&shared, target_dir, progress, on_status)),
    };
    // A move the push refused because the note changed in iCloud since the
    // last pull: the pull has merged that change into the moved file and
    // brought the note's change tag up to date, so the move goes now rather
    // than a sync later. A plain push keeps the refusal.
    if let (Ok(first), Some(Ok(_))) = (&mut push, &pull)
        && first.entries.iter().any(|e| is_move_changed_remotely(&e.entry))
    {
        match push::run_push_with(&shared, target_dir, on_status, &options) {
            Ok(second) => {
                first.entries.retain(|e| !is_move_changed_remotely(&e.entry));
                first.entries.extend(second.entries);
                first.pushed = Some(first.pushed.unwrap_or(0) + second.pushed.unwrap_or(0));
                first.unchanged = second.unchanged;
                for notice in second.notices {
                    if !first.notices.contains(&notice) {
                        first.notices.push(notice);
                    }
                }
            }
            Err(error) => push = Err(error),
        }
    }
    SyncOutcome { push, pull }
}

/// A move refused as changed remotely since the last pull.
fn is_move_changed_remotely(entry: &super::plan::PlanEntry) -> bool {
    entry.kind == super::plan::PlanEntryKind::Move
        && entry.reason.as_deref() == Some(super::plan::Refusal::MoveChangedRemotely.reason().as_str())
}
