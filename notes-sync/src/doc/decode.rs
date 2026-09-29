//! Record classification: the shared skip/decode rules `clone`, `pull` and
//! `push` use. Ports icloud-md `src/notes/decodeNoteRecord.ts`.
//! Owner: workstream B.
#![allow(unused_variables)]

use super::embeds::{AttachmentReference, EmbedSlot};
use super::format::FormatParagraph;
use crate::cloudkit::CloudKitRecord;
use crate::vault::state::TitleMode;

/// `ClassifyNoteOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassifyOptions {
    /// `Filename`: `markdown_text` excludes the title paragraph and the
    /// round-trip gate runs on that stripped projection.
    pub title_mode: TitleMode,
}

/// Why a record can't be synced (`{status: "unsyncable", reason}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsyncableReason {
    /// Body bytes present but don't parse - a durable fact about the record.
    Undecodable,
    /// No `TextDataEncrypted` - a fact about the delivery; never untrack over it.
    MissingBody,
}

impl UnsyncableReason {
    /// The TS string (`"undecodable"` / `"missing-body"`), which appears in
    /// refusal reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            UnsyncableReason::Undecodable => "undecodable",
            UnsyncableReason::MissingBody => "missing-body",
        }
    }
}

/// `OkNoteDecodeResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedNote {
    /// Apple's truncated display title (`TitleEncrypted`).
    pub title: String,
    /// The note's actual first line - what a filename-as-title vault names
    /// the file after.
    pub title_line: String,
    /// Plain visible text (what the CRDT stores; push splices this).
    pub body_text: String,
    /// Markdown with U+FFFC placeholders still in place; `body_text`
    /// verbatim when not publishable.
    pub markdown_text: String,
    /// The whole note's format model (title included); `None` exactly when
    /// the plain-text fallback applies.
    pub format: Option<Vec<FormatParagraph>>,
    /// `markdown_text` omits the title paragraph (filename-as-title vault and
    /// the title holds no embed). TS `titleStripped?: boolean`.
    pub title_stripped: bool,
    pub embed_slots: Vec<EmbedSlot>,
    pub attachments: Vec<AttachmentReference>,
    pub publishable: bool,
    pub unpublishable_reason: Option<String>,
}

/// `NoteDecodeResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteDecodeResult {
    Deleted,
    Unsyncable(UnsyncableReason),
    Ok(Box<DecodedNote>),
}

/// `classifyNoteRecord`.
pub fn classify_note_record(record: &CloudKitRecord, options: &ClassifyOptions) -> NoteDecodeResult {
    todo!()
}

/// `isDeleted` (Deleted field, or trashed/purged shapes - see TS).
pub fn is_deleted(record: &CloudKitRecord) -> bool {
    todo!()
}
