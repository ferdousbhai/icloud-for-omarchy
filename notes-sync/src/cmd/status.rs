//! `status`: the push plan (what `push --dry-run` computes), rendered. Originally derived from icloud-md.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::plan::PlanEntry;
use super::push::{PushOptions, run_push_with};
use super::remote::{Connector, DefaultConnector};
use super::{Error, SyncNotice};

/// `StatusResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub entries: Vec<PlanEntry>,
    pub unchanged: usize,
    pub notices: Vec<SyncNotice>,
}

/// `runStatus`.
pub fn run_status(target_dir: &Path, on_status: &mut dyn FnMut(&str)) -> Result<StatusResult, Error> {
    run_status_with(&DefaultConnector, target_dir, on_status)
}

/// `runStatus` over an explicit connector.
pub fn run_status_with(
    connector: &dyn Connector,
    target_dir: &Path,
    on_status: &mut dyn FnMut(&str),
) -> Result<StatusResult, Error> {
    let preview = run_push_with(connector, target_dir, on_status, &PushOptions { dry_run: true })?;
    Ok(StatusResult {
        entries: preview.entries.into_iter().map(|e| e.entry).collect(),
        unchanged: preview.unchanged,
        notices: preview.notices,
    })
}
