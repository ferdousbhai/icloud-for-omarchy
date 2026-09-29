//! Request field sets for note and folder writes. Ports icloud-md
//! `src/notes/encodeNoteRecord.ts` and `encodeFolderRecord.ts`.
//! Owner: workstream B.
//!
//! Field order matters (it is the request JSON's key order, matched to
//! captured web-client requests), hence `UpdateFields` (an `IndexMap`), not
//! the plan's `BTreeMap`.
#![allow(unused_variables)]

use serde_json::Value;

pub use crate::cloudkit::UpdateFieldValue;
use crate::cloudkit::{CloudKitRecord, UpdateFields};

pub const TRASH_FOLDER_RECORD_NAME: &str = "TrashFolder-CloudKit";
pub const DEFAULT_FOLDER_RECORD_NAME: &str = "DefaultFolder-CloudKit";

/// `deriveNoteTitle`.
pub fn derive_note_title(text: &str) -> String {
    todo!()
}

/// `deriveNoteSnippet`.
pub fn derive_note_snippet(text: &str) -> String {
    todo!()
}

/// `folderReference(folderRecordName, zoneOwnerRecordName?)`:
/// `{recordName, action: "VALIDATE", zoneID}`.
pub fn folder_reference(folder_record_name: &str, zone_owner_record_name: Option<&str>) -> Value {
    todo!()
}

/// `buildNoteTrashFields`.
pub fn build_note_trash_fields(current: &CloudKitRecord, now_ms: i64) -> UpdateFields {
    todo!()
}

/// `buildNoteMoveFields`.
pub fn build_note_move_fields(current: &CloudKitRecord, folder_record_name: &str, now_ms: i64) -> UpdateFields {
    todo!()
}

/// `buildNotePurgeFields`.
pub fn build_note_purge_fields(current: &CloudKitRecord, now_ms: i64) -> UpdateFields {
    todo!()
}

/// `buildNoteCreateFields`. `folder_record_name` defaults to
/// `DefaultFolder-CloudKit` in TS.
pub fn build_note_create_fields(
    new_text_data_base64: &str,
    new_text: &str,
    now_ms: i64,
    folder_record_name: &str,
    zone_owner_record_name: Option<&str>,
) -> UpdateFields {
    todo!()
}

/// `buildNoteUpdateFields`.
pub fn build_note_update_fields(
    current: &CloudKitRecord,
    new_text_data_base64: &str,
    new_text: &str,
    modification_date_ms: i64,
) -> UpdateFields {
    todo!()
}

/// `FolderCreateFields`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderCreateFields {
    pub fields: UpdateFields,
    pub parent_record_name: Option<String>,
}

/// `buildFolderCreateFields`.
pub fn build_folder_create_fields(title: &str, parent_record_name: Option<&str>) -> FolderCreateFields {
    todo!()
}
