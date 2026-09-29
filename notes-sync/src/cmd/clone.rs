//! `clone`. Ports icloud-md `src/commands/clone.ts`. Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Error, SyncNotice, SyncProgress};

/// `CloneOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneOptions {
    pub filename_as_title: bool,
    /// Checked against the icloud-session account's dsid (or Apple ID).
    pub account: Option<String>,
    /// Accepted for compatibility; a no-op (icloud-session never opens a
    /// window from here).
    pub non_interactive: bool,
}

/// `CloneSummary`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneSummary {
    pub written: usize,
    pub written_shared: usize,
    pub written_unpublishable: usize,
    pub attachments_downloaded: usize,
    pub skipped_deleted: usize,
    pub skipped_undecodable: usize,
    pub notices: Vec<SyncNotice>,
}

/// `runClone`.
pub fn run_clone(
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &CloneOptions,
) -> Result<CloneSummary, Error> {
    todo!()
}
