//! Embedded objects: attachment references, embed slots, inline embed
//! markers, the unknown-content banner, and the push-side embed plan. Ports
//! icloud-md `src/notes/noteAttachments.ts`, `unknownContent.ts` and
//! `embedPushEdit.ts`. Owner: workstream B.
#![allow(unused_variables)]

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::Result;
use crate::md::table::MarkdownTableBlock;

/// U+FFFC, one per embed in a note's visible text.
pub const OBJECT_REPLACEMENT_CHARACTER: char = '\u{FFFC}';

/// UTI marking an `Attachment` record as a table sub-document.
pub const TABLE_UTI: &str = "com.apple.notes.table";

/// `AttachmentReference`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentReference {
    /// Also the CloudKit recordName of the `Attachment` record.
    pub attachment_identifier: String,
    pub type_uti: String,
}

/// `EmbedSlot`: one U+FFFC placeholder's embed, in document order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EmbedSlot {
    Attachment(AttachmentReference),
    /// Placeholder whose `attachmentInfo` run was absent or incomplete.
    Unknown {
        type_uti: Option<String>,
    },
}

/// `EmbedMarkerContent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbedMarkerContent {
    pub type_uti: Option<String>,
    pub attachment_identifier: Option<String>,
}

/// `ParsedEmbedMarker` (offsets in UTF-16 units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEmbedMarker {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub type_uti: Option<String>,
    pub attachment_identifier: Option<String>,
}

/// `MatchedTableBlock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedTableBlock {
    pub reference: AttachmentReference,
    pub block: MarkdownTableBlock,
}

/// `EmbedRepresentationPlan`; `Err(reason)` is `{ok: false, reason}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedRepresentation {
    /// The local text with every marker and table block replaced by U+FFFC.
    pub reconstructed_body_text: String,
    /// Table slots in document order.
    pub tables: Vec<MatchedTableBlock>,
}

/// `UNKNOWN_CONTENT_BANNER` (copy verbatim from unknownContent.ts,
/// including `ADMONITION_HEADER`).
pub fn unknown_content_banner() -> &'static str {
    todo!()
}

/// `decodeNoteAttachmentRefs`.
pub fn decode_note_attachment_refs(compressed: &[u8]) -> Result<Vec<AttachmentReference>> {
    todo!()
}

/// `decodeNoteEmbedSlots`: `Ok(None)` when the embed structure defies the
/// model (the note then gets the banner and stays read-only).
pub fn decode_note_embed_slots(compressed: &[u8]) -> Result<Option<Vec<EmbedSlot>>> {
    todo!()
}

/// `isImageUti`.
pub fn is_image_uti(type_uti: &str) -> bool {
    todo!()
}

/// `isTableUti`.
pub fn is_table_uti(type_uti: &str) -> bool {
    type_uti == TABLE_UTI
}

/// `hasAttachmentReference`: `/!?\[[^\]]*\]\(attachments\/[^)]+\)/`.
pub fn has_attachment_reference(text: &str) -> bool {
    todo!()
}

/// `formatAttachmentMarkdown`.
pub fn format_attachment_markdown(reference: &AttachmentReference, relative_file: &str) -> String {
    todo!()
}

/// `renderPlaceholders`: substitute each U+FFFC in order.
pub fn render_placeholders(body_text: &str, replacements: &[Option<String>]) -> String {
    todo!()
}

/// `hasUnknownContentMarker`.
pub fn has_unknown_content_marker(text: &str) -> bool {
    todo!()
}

/// `formatEmbedMarker`.
pub fn format_embed_marker(content: &EmbedMarkerContent) -> String {
    todo!()
}

/// `parseEmbedMarkers`.
pub fn parse_embed_markers(text: &str) -> Vec<ParsedEmbedMarker> {
    todo!()
}

/// `hasEmbedMarker`: `/<apple-embed\b/`.
pub fn has_embed_marker(text: &str) -> bool {
    todo!()
}

/// `combineUnpublishableReasons`.
pub fn combine_unpublishable_reasons(a: Option<&str>, b: Option<&str>) -> Option<String> {
    todo!()
}

/// `planEmbedRepresentations`.
pub fn plan_embed_representations(
    local_text: &str,
    slots: &[EmbedSlot],
    tracked_file_attachment_ids: &HashSet<String>,
) -> std::result::Result<EmbedRepresentation, String> {
    todo!()
}
