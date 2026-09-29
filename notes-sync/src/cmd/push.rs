//! `push` (and the plan `status` shares). Ports icloud-md
//! `src/commands/push.ts`. Owner: workstream D. Refusal strings live in
//! `plan.rs`.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::plan::SerializedPlanEntry;
use super::{Error, SyncNotice};

/// `PushOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushOptions {
    pub dry_run: bool,
}

/// `ExecuteOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteOutcome {
    pub succeeded: bool,
    pub message: String,
}

/// `PushEntryResult`: a serialized plan entry plus, after a real push, what
/// executing it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEntryResult {
    #[serde(flatten)]
    pub entry: SerializedPlanEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExecuteOutcome>,
}

/// `PushResult`. Key order: dryRun, pushed, entries, unchanged, notices
/// (a dry run omits `pushed`; an empty real push has `pushed: 0` after
/// `notices` - see runPush's early return).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed: Option<usize>,
    pub entries: Vec<PushEntryResult>,
    pub unchanged: usize,
    pub notices: Vec<SyncNotice>,
}

/// `runPush`.
pub fn run_push(
    target_dir: &Path,
    on_status: &mut dyn FnMut(&str),
    options: &PushOptions,
) -> Result<PushResult, Error> {
    todo!()
}
