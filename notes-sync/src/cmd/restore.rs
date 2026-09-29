//! `restore`. Ports icloud-md `src/commands/restore.ts`. Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;

/// `RestoreResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResult {
    pub file: String,
}

/// `runRestore`.
pub fn run_restore(target_dir: &Path, file: &str) -> Result<RestoreResult, Error> {
    todo!()
}
