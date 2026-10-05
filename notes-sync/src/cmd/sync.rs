//! `sync`: push, then pull, in one process. The Notes app
//! (icloud-notes) syncs this way: one run, so one icloud-sessiond
//! `Session()` call, one TLS connection and one vault lock instead of two of
//! each. Each half is exactly what `push` and `pull` do on their own.
//!
//! The pull runs whatever the push did, except when the push found the
//! sign-in gone (every request would be refused the same way) or iCloud out
//! of reach (the pull would only fail again; the next sync retries both).

use std::path::Path;

use super::SyncProgress;
use super::errors::{EXIT_OK, EXIT_SIGN_IN, Error};
use super::pull::{self, PullOptions, PullSummary};
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
    /// The run's exit code: the worse of the two halves, a sign-in required
    /// (2) above anything else.
    pub fn exit_code(&self) -> u8 {
        let push = half_exit_code(&self.push);
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

/// Whether a failed push leaves the pull out.
pub fn push_error_skips_pull(error: &Error) -> bool {
    matches!(error, Error::SignInRequired) || error.is_network()
}

/// `sync`.
pub fn run_sync(target_dir: &Path, progress: &mut dyn SyncProgress, on_status: &mut dyn FnMut(&str)) -> SyncOutcome {
    run_sync_with(&DefaultConnector, target_dir, progress, on_status)
}

/// `sync` over an explicit connector, which it connects once for both
/// halves.
pub fn run_sync_with(
    connector: &dyn Connector,
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
) -> SyncOutcome {
    let shared = SharedConnector::new(connector);
    let push = push::run_push_with(&shared, target_dir, on_status, &PushOptions { dry_run: false });
    let pull = match &push {
        Err(error) if push_error_skips_pull(error) => None,
        _ => Some(pull::run_pull_with(
            &shared,
            target_dir,
            progress,
            on_status,
            &PullOptions::default(),
        )),
    };
    SyncOutcome { push, pull }
}
