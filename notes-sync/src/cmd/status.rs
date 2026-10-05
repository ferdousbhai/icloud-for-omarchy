//! `status`: the push plan, rendered. Ports icloud-md
//! `src/commands/status.ts`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::plan::{SerializedPlanEntry, count_unchanged_notes};
use super::push::build_push_plan;
use super::remote::{Connector, DefaultConnector};
use super::{Error, SyncNotice};
use crate::vault::history::without_recording;

/// `StatusResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub entries: Vec<SerializedPlanEntry>,
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
    let plan = without_recording(|| build_push_plan(connector, target_dir, on_status))?;
    let entries: Vec<SerializedPlanEntry> = plan.entries.iter().map(|e| e.entry.serialize()).collect();
    Ok(StatusResult {
        unchanged: count_unchanged_notes(&entries, plan.state.notes.len()),
        entries,
        notices: plan.notices,
    })
}
