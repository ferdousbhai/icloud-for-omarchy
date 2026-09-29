//! Table writes: grid diff → CRDT operations. Ports icloud-md
//! `src/notes/tableEdit.ts`, `tableCellEdit.ts` and `tablePushEdit.ts`.
//! Owner: workstream B.
#![allow(unused_variables)]

use crate::cloudkit::CloudKitRecord;

/// `TableAttachmentUpdateResult` when `ok`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAttachmentUpdate {
    pub changed: bool,
    pub mergeable_data_base64: String,
}

/// `prepareTableAttachmentUpdate`; `Err(reason)` is `{ok: false, reason}`.
pub fn prepare_table_attachment_update(
    record: &CloudKitRecord,
    desired_grid: &[Vec<String>],
    replica_id: &[u8; 16],
) -> Result<TableAttachmentUpdate, String> {
    todo!()
}
