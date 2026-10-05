//! `diff`. Ports icloud-md `src/commands/diff.ts`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;
use super::remote::{Connector, DefaultConnector, Remote, resolve_folder_account};
use crate::cloudkit::{Database, NoteZone, Transport, note_zone};
use crate::diff3::{CommHunk, diff_comm};
use crate::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
use crate::doc::tables::decode_table_markdown;
use crate::doc::text::decode_note_body_text;
use crate::js::base64_decode;
use crate::vault::attachments::decode_table_attachment;
use crate::vault::epoch::{NoteEpoch, find_epoch_by_id};
use crate::vault::history::{
    VersionSnapshot, find_snapshot_by_id, find_version, history_record_names, resolve_tracked_note,
};
use crate::vault::migrate::require_vault;
use crate::vault::state::CloneState;

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
    match result {
        DiffResult::Snapshot { text, .. } => text.clone(),
        DiffResult::Epoch { sections, .. } => sections
            .iter()
            .map(|s| format!("=== {} ===\n{}", s.label, s.text))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

/// `runDiff`: `to_id` `None` = against the current remote copy.
pub fn run_diff(
    target_dir: &Path,
    file: &str,
    from_id: &str,
    to_id: Option<&str>,
    on_status: &mut dyn FnMut(&str),
) -> Result<DiffResult, Error> {
    run_diff_with(&DefaultConnector, target_dir, file, from_id, to_id, on_status)
}

/// `runDiff` over an explicit connector.
pub fn run_diff_with(
    connector: &dyn Connector,
    target_dir: &Path,
    file: &str,
    from_id: &str,
    to_id: Option<&str>,
    on_status: &mut dyn FnMut(&str),
) -> Result<DiffResult, Error> {
    let state = require_vault(target_dir, on_status)?;
    let tracked = resolve_tracked_note(&state, file, target_dir)?;
    let record_names = history_record_names(&state, &tracked.record_name);
    let zone = note_zone(tracked.entry.shared_zone_owner.as_deref());

    let from = match find_snapshot_by_id(target_dir, &record_names, from_id, file) {
        Ok(snapshot) => snapshot,
        Err(Error::UnknownVersionSnapshot { id, file: f }) => {
            let Some(epoch) = find_epoch_by_id(target_dir, &tracked.record_name, from_id)? else {
                return Err(Error::UnknownVersionSnapshot { id, file: f });
            };
            if let Some(to_id) = to_id {
                return Err(Error::VersionContentUnavailable(format!(
                    "epoch-vs-epoch diff (\"{from_id}..{to_id}\") isn't supported yet - diff a specific record's snapshots \
                     instead (run \"icloud-notes history {file} --records\" for their ids), or diff the epoch against the \
                     current remote copy"
                )));
            }
            return render_epoch_diff(connector, target_dir, &state, &tracked.record_name, &zone, &epoch);
        }
        Err(other) => return Err(other),
    };
    let from_text = decode_snapshot_text(&from)?;

    let (to_text, to_label) = match to_id {
        Some(to_id) => {
            let to = find_snapshot_by_id(target_dir, &record_names, to_id, file)?;
            if to.record_name != from.record_name {
                return Err(Error::VersionContentUnavailable(format!(
                    "\"{from_id}\" and \"{to_id}\" belong to different records - can't diff a note's text against a table's structure"
                )));
            }
            (decode_snapshot_text(&to)?, to_id.to_owned())
        }
        None => {
            let remote = resolve_folder_account(connector, target_dir, state.account.as_ref())?;
            (
                fetch_current_text(&remote.db, &zone, &from.record_name, &from.record_type)?,
                "current".to_owned(),
            )
        }
    };
    let rendered = render_diff(&from_text, &to_text, from_id, &to_label);
    Ok(DiffResult::Snapshot {
        from: from_id.to_owned(),
        to: to_label,
        has_differences: rendered.has_differences,
        text: rendered.text,
    })
}

/// `decodeSnapshotText`.
pub fn decode_snapshot_text(snapshot: &VersionSnapshot) -> Result<String, Error> {
    let bytes = base64_decode(&snapshot.value_base64);
    let decoded = if snapshot.field == "TextDataEncrypted" {
        decode_note_body_text(&bytes)
    } else {
        decode_table_markdown(&bytes)
    };
    decoded.map_err(|e| Error::Internal(e.to_string()))
}

fn render_epoch_diff(
    connector: &dyn Connector,
    target_dir: &Path,
    state: &CloneState,
    note_record_name: &str,
    zone: &NoteZone,
    epoch: &NoteEpoch,
) -> Result<DiffResult, Error> {
    let mut remote: Option<Remote> = None;
    let mut sections = Vec::new();
    let mut has_differences = false;
    for (record_name, snapshot_id) in &epoch.snapshots {
        let label = if record_name == note_record_name {
            "note text".to_owned()
        } else {
            format!("table {record_name}")
        };
        let Some(snapshot_id) = snapshot_id else {
            sections.push(DiffEpochSection {
                label,
                skipped: true,
                has_differences: None,
                text: "(no snapshot was ever captured for this record at this epoch - skipped)".into(),
            });
            continue;
        };
        let Some(snapshot) = find_version(target_dir, record_name, snapshot_id)? else {
            sections.push(DiffEpochSection {
                label,
                skipped: true,
                has_differences: None,
                text: format!("(the recorded snapshot \"{snapshot_id}\" no longer exists locally - skipped)"),
            });
            continue;
        };
        if remote.is_none() {
            remote = Some(resolve_folder_account(connector, target_dir, state.account.as_ref())?);
        }
        let db = &remote.as_ref().expect("connected above").db;
        let current = fetch_current_text(db, zone, record_name, &snapshot.record_type)
            .and_then(|to| Ok((decode_snapshot_text(&snapshot)?, to)));
        match current {
            Ok((from_text, to_text)) => {
                let rendered = render_diff(&from_text, &to_text, &epoch.id, "current");
                has_differences |= rendered.has_differences;
                sections.push(DiffEpochSection {
                    label,
                    skipped: false,
                    has_differences: Some(rendered.has_differences),
                    text: rendered.text,
                });
            }
            Err(Error::VersionContentUnavailable(reason)) => sections.push(DiffEpochSection {
                label,
                skipped: true,
                has_differences: None,
                text: format!("(Can't complete this operation: {reason}.)"),
            }),
            Err(other) => return Err(other),
        }
    }
    Ok(DiffResult::Epoch {
        epoch_id: epoch.id.clone(),
        has_differences,
        sections,
    })
}

fn fetch_current_text<T: Transport>(
    db: &Database<T>,
    zone: &NoteZone,
    record_name: &str,
    record_type: &str,
) -> Result<String, Error> {
    let records = db.lookup_records(zone, &[record_name.to_owned()])?;
    let Some(record) = records.first().filter(|r| !r.is_deleted()) else {
        return Err(Error::VersionContentUnavailable(format!(
            "\"{record_name}\" no longer exists remotely"
        )));
    };
    if record_type == "Note" {
        return match classify_note_record(record, &ClassifyOptions::default()) {
            NoteDecodeResult::Ok(decoded) => Ok(decoded.body_text),
            _ => Err(Error::VersionContentUnavailable(
                "the current remote note isn't in a readable state".into(),
            )),
        };
    }
    decode_table_attachment(Some(record))
        .ok_or_else(|| Error::VersionContentUnavailable("the current remote table isn't in a readable state".into()))
}

/// `RenderedDiff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDiff {
    pub text: String,
    pub has_differences: bool,
}

/// `renderDiff`: node-diff3 `diffComm` over lines, `  `/`- `/`+ ` prefixed.
pub fn render_diff(from_text: &str, to_text: &str, from_label: &str, to_label: &str) -> RenderedDiff {
    let a: Vec<&str> = from_text.split('\n').collect();
    let b: Vec<&str> = to_text.split('\n').collect();
    let mut lines = vec![format!("--- {from_label}"), format!("+++ {to_label}")];
    let mut changed = false;
    for hunk in diff_comm(&a, &b) {
        match hunk {
            CommHunk::Common(common) => lines.extend(common.iter().map(|l| format!("  {l}"))),
            CommHunk::Diff { buffer1, buffer2 } => {
                changed = true;
                lines.extend(buffer1.iter().map(|l| format!("- {l}")));
                lines.extend(buffer2.iter().map(|l| format!("+ {l}")));
            }
        }
    }
    if !changed {
        lines.push("(no differences)".into());
    }
    RenderedDiff {
        text: lines.join("\n"),
        has_differences: changed,
    }
}
