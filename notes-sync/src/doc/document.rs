//! The note CRDT document. Ports icloud-md `src/notes/noteDocument.ts`.
//! Owner: workstream B.
#![allow(unused_variables)]

use super::Result;
use super::proto::topotext::{AttributeRun, Substring};

/// `RunCoord`: a topotext `CharID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunCoord {
    pub replica: u32,
    pub clock: u32,
}

/// `TextRun`: one topotext `Substring`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    pub coord: RunCoord,
    /// UTF-16 length.
    pub length: u32,
    pub anchor: RunCoord,
    pub tombstone: bool,
    /// Outgoing child edges: 0-based indexes into the runs array, always
    /// later indexes (topological order). Empty on the end sentinel.
    pub sequence: Vec<u32>,
}

/// `ReplicaEntry`: one `VectorTimestamp.Clock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaEntry {
    /// 16-byte replica UUID.
    pub id: Vec<u8>,
    /// First entry is the replica's text clock; later entries preserved
    /// verbatim. (TS keeps these as `number[]` of `ReplicaClock.clock`, with
    /// `subclock` presence handled in parse/encode - see noteDocument.ts.)
    pub counters: Vec<u32>,
}

/// `NoteDocument`. `attribute_runs` are the generated messages themselves,
/// opaque to editing logic (only `.length` is touched) and carrying unknown
/// fields through.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteDocument {
    /// `versioned_document.Document.serializationVersion`.
    pub root_serialization_version: u32,
    /// `versioned_document.Version.serializationVersion`.
    pub version_serialization_version: u32,
    /// `versioned_document.Version.minimumSupportedVersion`.
    pub minimum_supported_version: u32,
    pub text: String,
    pub runs: Vec<TextRun>,
    pub replicas: Vec<ReplicaEntry>,
    pub attribute_runs: Vec<AttributeRun>,
}

/// `ApplyTextEditOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyTextEditOptions {
    /// 16-byte replica UUID identifying this tool's edits.
    pub replica_id: [u8; 16],
}

/// `Splice`: one hunk of `computeSplices` (UTF-16 offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub delete_length: usize,
    pub insert_text: String,
}

/// `parseNoteDocument`: raw (decompressed) versioned-document bytes → model.
pub fn parse_note_document(raw: &[u8]) -> Result<NoteDocument> {
    todo!()
}

/// `encodeNoteDocument`: model → raw bytes (not compressed).
pub fn encode_note_document(doc: &NoteDocument) -> Vec<u8> {
    todo!()
}

/// `noteDocumentRoundTrips`: `encode(parse(raw)) == raw`, byte for byte.
pub fn note_document_round_trips(raw: &[u8]) -> bool {
    todo!()
}

/// `applyTextEdit`: per-hunk CRDT edit in place; false if the text is
/// unchanged.
pub fn apply_text_edit(doc: &mut NoteDocument, new_text: &str, options: &ApplyTextEditOptions) -> Result<bool> {
    todo!()
}

/// `buildInitialNoteDocument`: a fresh document holding `text`.
pub fn build_initial_note_document(text: &str, replica_id: &[u8; 16]) -> NoteDocument {
    todo!()
}

/// `computeSplice`: the single spanning splice.
pub fn compute_splice(old_text: &str, new_text: &str) -> Splice {
    todo!()
}

/// `computeSplices`: per-hunk splices.
pub fn compute_splices(old_text: &str, new_text: &str) -> Vec<Splice> {
    todo!()
}

/// `validateDocumentInvariants`: `Err` with icloud-md's message when the runs
/// don't describe the text.
pub fn validate_document_invariants(doc: &NoteDocument) -> Result<()> {
    todo!()
}

/// `parseTextRun`.
pub fn parse_text_run(run: &Substring) -> TextRun {
    todo!()
}

/// `encodeTextRun`.
pub fn encode_text_run(run: &TextRun) -> Substring {
    todo!()
}
