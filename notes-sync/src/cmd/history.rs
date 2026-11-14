//! `history`. Originally derived from icloud-md.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Error, NoticeLevel, SyncNotice};
use crate::vault::epoch::{NoteEpoch, list_epochs_and_unreadable};
use crate::vault::history::{
    VersionSnapshot, history_record_names, list_versions_and_unreadable, resolve_tracked_note,
};
use crate::vault::migrate::read_vault;

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
/// `{"mode":"records","records":[..]}`, with `notices` when a history file
/// could not be read and was left out of the listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum HistoryResult {
    Epochs {
        epochs: Vec<HistoryEpochRow>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notices: Vec<SyncNotice>,
    },
    Records {
        records: Vec<HistoryRecordRow>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notices: Vec<SyncNotice>,
    },
}

impl HistoryResult {
    pub fn notices(&self) -> &[SyncNotice] {
        match self {
            HistoryResult::Epochs { notices, .. } | HistoryResult::Records { notices, .. } => notices,
        }
    }
}

/// One warning per history file left out of a listing.
fn unreadable_notices(files: Vec<String>) -> Vec<SyncNotice> {
    files
        .into_iter()
        .map(|file| SyncNotice {
            level: NoticeLevel::Warn,
            message: format!("{file} can't be read, so it is left out of the history"),
        })
        .collect()
}

/// `runHistory`: the epoch timeline (or, with `records`, the flat
/// per-record snapshot list), newest first.
pub fn run_history(target_dir: &Path, file: &str, options: &HistoryOptions) -> Result<HistoryResult, Error> {
    let state = read_vault(target_dir)?;
    let record_name = resolve_tracked_note(&state, file, target_dir)?.record_name;
    if options.records {
        let (records, unreadable) =
            build_record_history(target_dir, &history_record_names(&state, &record_name), &record_name)?;
        return Ok(HistoryResult::Records {
            records,
            notices: unreadable_notices(unreadable),
        });
    }
    let (epochs, unreadable) = list_epochs_and_unreadable(target_dir, &record_name)?;
    let mut rows = Vec::with_capacity(epochs.len());
    let mut previous: Option<&NoteEpoch> = None;
    for epoch in &epochs {
        rows.push(describe_epoch(epoch, previous, &record_name));
        previous = Some(epoch);
    }
    rows.reverse();
    Ok(HistoryResult::Epochs {
        epochs: rows,
        notices: unreadable_notices(unreadable),
    })
}

/// How a record of the note is named in a row: the note itself, or one of
/// its tables.
fn record_label(record_name: &str, note_record_name: &str) -> String {
    if record_name == note_record_name {
        "note".to_owned()
    } else {
        format!("table {record_name}")
    }
}

fn describe_epoch(epoch: &NoteEpoch, previous: Option<&NoteEpoch>, note_record_name: &str) -> HistoryEpochRow {
    let mut changed = Vec::new();
    let mut carried = Vec::new();
    for (record_name, snapshot_id) in &epoch.snapshots {
        let label = record_label(record_name, note_record_name);
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
) -> Result<(Vec<HistoryRecordRow>, Vec<String>), Error> {
    let mut rows: Vec<(VersionSnapshot, String)> = Vec::new();
    let mut unreadable = Vec::new();
    for rn in record_names {
        let label = record_label(rn, note_record_name);
        let (snapshots, skipped) = list_versions_and_unreadable(target_dir, rn)?;
        unreadable.extend(skipped);
        for snapshot in snapshots.into_iter().rev() {
            rows.push((snapshot, label.clone()));
        }
    }
    rows.sort_by(|(a, _), (b, _)| b.timestamp.cmp(&a.timestamp));
    let rows = rows
        .into_iter()
        .map(|(snapshot, label)| HistoryRecordRow {
            id: snapshot.id,
            timestamp: snapshot.timestamp,
            label,
            record_change_tag: snapshot.record_change_tag,
        })
        .collect();
    Ok((rows, unreadable))
}
