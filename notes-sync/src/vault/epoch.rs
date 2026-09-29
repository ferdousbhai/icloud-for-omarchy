//! Whole-note epochs (`.icloud-md/epochs/`). Ports icloud-md
//! `src/notes/noteEpoch.ts`. Owner: workstream D.
#![allow(unused_variables)]

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

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
