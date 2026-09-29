//! Formatting reconciliation: rewrite a document's attribute runs to carry a
//! desired format model. Ports icloud-md `src/notes/formatReconcile.ts`.
//! Owner: workstream B.
#![allow(unused_variables)]

use super::document::NoteDocument;
use super::format::FormatParagraph;

/// `ReconcileResult`: `Ok(changed)`, or `Err(reason)` (`{ok: false, reason}`).
pub type ReconcileResult = Result<bool, String>;

/// `reconcileNoteFormat`.
pub fn reconcile_note_format(
    doc: &mut NoteDocument,
    desired: &[FormatParagraph],
    replica_id: &[u8; 16],
) -> ReconcileResult {
    todo!()
}
