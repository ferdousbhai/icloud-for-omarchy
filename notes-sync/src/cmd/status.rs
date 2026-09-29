//! `status`: the push plan, rendered. Ports icloud-md
//! `src/commands/status.ts`. Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::plan::SerializedPlanEntry;
use super::{Error, SyncNotice};

/// `StatusResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub entries: Vec<SerializedPlanEntry>,
    pub unchanged: usize,
    pub notices: Vec<SyncNotice>,
}

/// `runStatus`.
pub fn run_status(target_dir: &Path, on_status: &mut dyn FnMut(&str)) -> Result<StatusResult, Error> {
    todo!()
}
