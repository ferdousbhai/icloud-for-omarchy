//! Whole-note epochs (`.icloud-md/history/<note>/epochs/`): one per pull/push
//! run that changed a note, indexing which snapshot was current for each of
//! its records. Ports icloud-md `src/notes/noteEpoch.ts`.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::history::{capture_file_name, history_dir, list_versions, read_json_dir};
use super::rt;
use super::state::to_js_json;
use crate::cmd::errors::Error;
use crate::js;

/// `NoteEpoch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteEpoch {
    pub id: String,
    pub timestamp: String,
    pub note_record_name: String,
    /// recordName → snapshot id current at this epoch (`null`: none yet).
    pub snapshots: IndexMap<String, Option<String>>,
}

fn epoch_dir(target_dir: &Path, note_record_name: &str) -> PathBuf {
    history_dir(target_dir).join(note_record_name).join("epochs")
}

/// `recordEpoch`: `record_names` conventionally the note first, then its
/// tables (`history_record_names`).
pub fn record_epoch(target_dir: &Path, note_record_name: &str, record_names: &[String]) -> Result<(), Error> {
    let mut snapshots = IndexMap::new();
    for record_name in record_names {
        let versions = list_versions(target_dir, record_name)?;
        snapshots.insert(record_name.clone(), versions.last().map(|v| v.id.clone()));
    }
    let captured_at = rt::now_ms();
    let id = rt::random_uuid();
    let epoch = NoteEpoch {
        id: id.clone(),
        timestamp: js::iso_string(captured_at),
        note_record_name: note_record_name.to_owned(),
        snapshots,
    };
    let dir = epoch_dir(target_dir, note_record_name);
    std::fs::create_dir_all(&dir)?;
    let existing = list_epochs(target_dir, note_record_name)?;
    let json = serde_json::to_value(&epoch).expect("epoch serializes");
    std::fs::write(
        dir.join(capture_file_name(captured_at, existing.len(), &id)),
        to_js_json(&json),
    )?;
    Ok(())
}

/// `listEpochs`: oldest first.
pub fn list_epochs(target_dir: &Path, note_record_name: &str) -> Result<Vec<NoteEpoch>, Error> {
    read_json_dir(&epoch_dir(target_dir, note_record_name))
}

/// `findEpochById`.
pub fn find_epoch_by_id(target_dir: &Path, note_record_name: &str, id: &str) -> Result<Option<NoteEpoch>, Error> {
    Ok(list_epochs(target_dir, note_record_name)?
        .into_iter()
        .find(|e| e.id == id))
}
