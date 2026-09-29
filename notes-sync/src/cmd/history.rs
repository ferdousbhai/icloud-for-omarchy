//! `history`. Ports icloud-md `src/commands/history.ts`. Owner: workstream D.
#![allow(unused_variables)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;

/// `HistoryOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryOptions {
    pub records: bool,
}

/// `HistoryEpochRow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEpochRow {
    pub id: String,
    pub timestamp: String,
    pub changed: Vec<String>,
    pub carried_over: Vec<String>,
}

/// `HistoryRecordRow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecordRow {
    pub id: String,
    pub timestamp: String,
    pub label: String,
    pub record_change_tag: String,
}

/// `HistoryResult`: `{"mode":"epochs","epochs":[..]}` or
/// `{"mode":"records","records":[..]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum HistoryResult {
    Epochs { epochs: Vec<HistoryEpochRow> },
    Records { records: Vec<HistoryRecordRow> },
}

/// `runHistory`.
pub fn run_history(target_dir: &Path, file: &str, options: &HistoryOptions) -> Result<HistoryResult, Error> {
    todo!()
}
