//! `pull`. Ports icloud-md `src/commands/pull.ts` and `src/cli/pullReport.ts`.
//! Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Error, SyncNotice, SyncProgress};

/// `PullOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullOptions {
    pub defer_renames: bool,
}

/// `PullChangeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullChangeKind {
    Add,
    Update,
    Merge,
    Remove,
    Move,
    Untrack,
}

/// `PullChangeRemark`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullChangeRemark {
    /// "conflict" | "unsyncable" | "note".
    pub tone: String,
    pub message: String,
}

/// `PullChange`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullChange {
    pub kind: PullChangeKind,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remarks: Option<Vec<PullChangeRemark>>,
}

/// `PullSummary`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullSummary {
    pub added: usize,
    pub updated: usize,
    pub merged: usize,
    pub removed: usize,
    pub attachments_downloaded: usize,
    pub unpublishable: usize,
    pub skipped_new_unsyncable: usize,
    pub dropped_unsyncable: usize,
    pub unshared_untracked: usize,
    pub changes: Vec<PullChange>,
    pub conflicts: Vec<String>,
    pub notices: Vec<SyncNotice>,
}

/// `runPull`.
pub fn run_pull(
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &PullOptions,
) -> Result<PullSummary, Error> {
    todo!()
}

/// `renderPullReport`: the human changelist.
pub fn render_pull_report(summary: &PullSummary, format_path: &dyn Fn(&str) -> String) -> Vec<String> {
    todo!()
}
