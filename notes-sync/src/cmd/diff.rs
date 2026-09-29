//! `diff`. Ports icloud-md `src/commands/diff.ts`. Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;

/// `DiffEpochSection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEpochSection {
    pub label: String,
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_differences: Option<bool>,
    pub text: String,
}

/// `DiffResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffResult {
    #[serde(rename_all = "camelCase")]
    Snapshot {
        from: String,
        to: String,
        has_differences: bool,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Epoch {
        epoch_id: String,
        has_differences: bool,
        sections: Vec<DiffEpochSection>,
    },
}

impl DiffResult {
    pub fn has_differences(&self) -> bool {
        match self {
            DiffResult::Snapshot { has_differences, .. } | DiffResult::Epoch { has_differences, .. } => {
                *has_differences
            }
        }
    }
}

/// `renderDiffResult`.
pub fn render_diff_result(result: &DiffResult) -> String {
    todo!()
}

/// `runDiff`: `to_id` `None` = against the current remote copy.
pub fn run_diff(
    target_dir: &Path,
    file: &str,
    from_id: &str,
    to_id: Option<&str>,
    on_status: &mut dyn FnMut(&str),
) -> Result<DiffResult, Error> {
    todo!()
}
