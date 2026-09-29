//! Per-record version snapshots (`.icloud-md/history/<recordName>/*.json`)
//! and tracked-file resolution. Ports icloud-md `src/notes/versionHistory.ts`
//! and `trackedFile.ts`. Owner: workstream D.
#![allow(unused_variables)]

use serde::{Deserialize, Serialize};

/// `VersionSnapshot`, as written to disk (key order as TS writes it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionSnapshot {
    pub id: String,
    pub timestamp: String,
    pub record_name: String,
    /// "Note" | "Attachment".
    pub record_type: String,
    /// "TextDataEncrypted" | "MergeableDataEncrypted".
    pub field: String,
    pub record_change_tag: String,
    pub value_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_record_name: Option<String>,
}
