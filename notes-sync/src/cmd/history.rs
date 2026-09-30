//! `history`. Ports icloud-md `src/commands/history.ts`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;
use crate::js;
use crate::vault::epoch::{NoteEpoch, list_epochs};
use crate::vault::history::{VersionSnapshot, history_record_names, list_versions, resolve_tracked_note};
use crate::vault::migrate::require_vault;

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

/// `runHistory`: the epoch timeline (or, with `records`, the flat
/// per-record snapshot list), newest first.
pub fn run_history(target_dir: &Path, file: &str, options: &HistoryOptions) -> Result<HistoryResult, Error> {
    let state = require_vault(target_dir, &mut |_| {})?;
    let record_name = resolve_tracked_note(&state, file, target_dir)?.record_name;
    if options.records {
        return Ok(HistoryResult::Records {
            records: build_record_history(target_dir, &history_record_names(&state, &record_name), &record_name)?,
        });
    }
    let epochs = list_epochs(target_dir, &record_name)?;
    let mut rows = Vec::with_capacity(epochs.len());
    let mut previous: Option<&NoteEpoch> = None;
    for epoch in &epochs {
        rows.push(describe_epoch(epoch, previous, &record_name));
        previous = Some(epoch);
    }
    rows.reverse();
    Ok(HistoryResult::Epochs { epochs: rows })
}

fn describe_epoch(epoch: &NoteEpoch, previous: Option<&NoteEpoch>, note_record_name: &str) -> HistoryEpochRow {
    let mut changed = Vec::new();
    let mut carried = Vec::new();
    for (record_name, snapshot_id) in &epoch.snapshots {
        let label = if record_name == note_record_name {
            "note".to_owned()
        } else {
            format!("table {record_name}")
        };
        match previous {
            Some(prev) if prev.snapshots.get(record_name) == Some(snapshot_id) => carried.push(label),
            _ => changed.push(label),
        }
    }
    HistoryEpochRow {
        id: epoch.id.clone(),
        timestamp: epoch.timestamp.clone(),
        changed,
        carried_over: carried,
    }
}

fn build_record_history(
    target_dir: &Path,
    record_names: &[String],
    note_record_name: &str,
) -> Result<Vec<HistoryRecordRow>, Error> {
    let mut rows: Vec<(VersionSnapshot, String)> = Vec::new();
    for rn in record_names {
        let label = if rn == note_record_name {
            "note".to_owned()
        } else {
            format!("table {rn}")
        };
        for snapshot in list_versions(target_dir, rn)?.into_iter().rev() {
            rows.push((snapshot, label.clone()));
        }
    }
    rows.sort_by(|(a, _), (b, _)| js::locale_compare(&b.timestamp, &a.timestamp));
    Ok(rows
        .into_iter()
        .map(|(snapshot, label)| HistoryRecordRow {
            id: snapshot.id,
            timestamp: snapshot.timestamp,
            label,
            record_change_tag: snapshot.record_change_tag,
        })
        .collect())
}
