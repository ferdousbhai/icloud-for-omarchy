//! Record classification: the shared skip/decode rules `clone`, `pull` and
//! `push` use. Originally derived from icloud-md.

use serde_json::Value;

use super::embeds::{
    AttachmentReference, EmbedSlot, OBJECT_REPLACEMENT_CHARACTER, UNKNOWN_CONTENT_BANNER, embed_slots_of,
};
use super::encode::TRASH_FOLDER_RECORD_NAME;
use super::format::{FormatParagraph, decode_note_format, formats_round_trip_equal, trim_trailing_whitespace};
use super::text::decode_note_string;
use crate::cloudkit::{CloudKitRecord, FieldValue};
use crate::js::{base64_decode, buffer_to_utf8};
use crate::md::parse::parse_note_markdown;
use crate::md::render::render_note_markdown;
use crate::md::title::split_title_paragraph;
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

/// Why a note whose text lives in a `TextDataAsset` is read-only (upstream
/// icloud-md PR #29, verbatim).
pub const TEXT_DATA_ASSET_UNPUBLISHABLE_REASON: &str =
    "is so large that Apple keeps its text in a separate file, which can't be written back yet";

/// `classifyNoteRecord`.
///
/// A note whose text Apple moved into a `TextDataAsset` (upstream PR #29)
/// reads like any other once `inline_asset_bodies` has fetched it, but
/// writing it back would mean uploading a new asset - a path never captured -
/// so it arrives read-only.
pub fn classify_note_record(record: &CloudKitRecord, options: &ClassifyOptions) -> NoteDecodeResult {
    let result = classify_note_body(record, options);
    match result {
        NoteDecodeResult::Ok(mut note)
            if note.publishable && record.fields.get("TextDataAsset").is_some_and(|f| !f.value.is_null()) =>
        {
            note.publishable = false;
            note.unpublishable_reason = Some(TEXT_DATA_ASSET_UNPUBLISHABLE_REASON.into());
            NoteDecodeResult::Ok(note)
        }
        other => other,
    }
}

fn classify_note_body(record: &CloudKitRecord, options: &ClassifyOptions) -> NoteDecodeResult {
    if is_deleted(record) {
        return NoteDecodeResult::Deleted;
    }
    let Some(Value::String(text_value)) = record.fields.get("TextDataEncrypted").map(|f| &f.value) else {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::MissingBody);
    };
    let Ok(string) = decode_note_string(&base64_decode(text_value)) else {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::Undecodable);
    };
    let body_text = string.string.clone().unwrap_or_default();
    let title = decode_title_field(record.fields.get("TitleEncrypted"));
    let title_line = body_text.split('\n').next().unwrap_or("").to_string();

    let Some(embed_slots) = embed_slots_of(&string) else {
        let banner_text = format!("{UNKNOWN_CONTENT_BANNER}{body_text}");
        return NoteDecodeResult::Ok(Box::new(DecodedNote {
            title,
            title_line,
            body_text: banner_text.clone(),
            markdown_text: banner_text,
            format: None,
            title_stripped: false,
            embed_slots: Vec::new(),
            attachments: Vec::new(),
            publishable: false,
            unpublishable_reason: Some(
                "contains unrecognized embedded content this tool couldn't parse or place precisely".into(),
            ),
        }));
    };
    let attachments: Vec<AttachmentReference> = embed_slots
        .iter()
        .filter_map(|slot| match slot {
            EmbedSlot::Attachment(reference) => Some(reference.clone()),
            EmbedSlot::Unknown { .. } => None,
        })
        .collect();
    let base = DecodedNote {
        title,
        title_line,
        body_text: body_text.clone(),
        markdown_text: body_text.clone(),
        format: None,
        title_stripped: false,
        embed_slots,
        attachments,
        publishable: false,
        unpublishable_reason: None,
    };
    let unpublishable = |reason: String| {
        NoteDecodeResult::Ok(Box::new(DecodedNote {
            unpublishable_reason: Some(reason),
            ..base.clone()
        }))
    };

    let paragraphs = match decode_note_format(&body_text, &string.attribute_run) {
        Ok(paragraphs) => paragraphs,
        Err(reason) => return unpublishable(reason),
    };
    let strip_title = options.title_mode == TitleMode::Filename
        && paragraphs
            .first()
            .is_some_and(|title| !title.text.contains(OBJECT_REPLACEMENT_CHARACTER));
    let projected = if strip_title {
        split_title_paragraph(&paragraphs).body
    } else {
        paragraphs.clone()
    };

    if strip_title && projected.is_empty() {
        return NoteDecodeResult::Ok(Box::new(DecodedNote {
            markdown_text: String::new(),
            format: Some(paragraphs),
            title_stripped: true,
            publishable: true,
            ..base
        }));
    }

    let rendered = render_note_markdown(&projected);
    let reparsed = parse_note_markdown(&rendered);
    let projected_text = projected
        .iter()
        .map(|p| trim_trailing_whitespace(p).text)
        .collect::<Vec<_>>()
        .join("\n");
    let survives = match &reparsed {
        Ok(reparsed) => reparsed.text == projected_text && formats_round_trip_equal(&projected, &reparsed.paragraphs),
        Err(_) => false,
    };
    if !survives {
        return unpublishable("the note's formatting doesn't survive this tool's markdown round trip".into());
    }
    NoteDecodeResult::Ok(Box::new(DecodedNote {
        markdown_text: rendered,
        format: Some(paragraphs),
        title_stripped: strip_title,
        publishable: true,
        ..base
    }))
}

/// `isDeleted`: the record-level `deleted` flag, a non-zero `Deleted` field,
/// or a note parented to the Trash folder.
pub fn is_deleted(record: &CloudKitRecord) -> bool {
    if record.deleted == Some(true) {
        return true;
    }
    if let Some(Value::Number(n)) = record.fields.get("Deleted").map(|f| &f.value)
        && n.as_f64() != Some(0.0)
    {
        return true;
    }
    matches!(
        record.fields.get("Folder").map(|f| &f.value),
        Some(Value::Object(folder)) if folder.get("recordName") == Some(&Value::String(TRASH_FOLDER_RECORD_NAME.into()))
    )
}

/// `decodeTitleField`.
fn decode_title_field(field: Option<&FieldValue>) -> String {
    match field.map(|f| &f.value) {
        Some(Value::String(value)) => buffer_to_utf8(&base64_decode(value)),
        _ => String::new(),
    }
}
