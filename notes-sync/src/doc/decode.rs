//! Record classification: the shared skip/decode rules `clone`, `pull` and
//! `push` use. Originally derived from icloud-md.

use serde_json::Value;

use super::embeds::{
    AttachmentReference, EmbedSlot, OBJECT_REPLACEMENT_CHARACTER, UNKNOWN_CONTENT_BANNER, embed_slots_of, is_table_uti,
};
use super::encode::TRASH_FOLDER_RECORD_NAME;
use super::format::{FormatParagraph, ParagraphKind, decode_note_format, trim_trailing_whitespace};
use super::text::decode_note_string;
use crate::cloudkit::{CloudKitRecord, FieldValue};
use crate::js::{base64_decode, buffer_to_utf8};
use crate::md::parse::ParsedNoteMarkdown;
use crate::md::render::{projection_survives, render_note_markdown};
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

impl DecodedNote {
    /// `parsed`, what the note's file reads as, with the lines the file
    /// can't hold put back as the note has them. An in-body vault's file
    /// body starts after the frontmatter and every blank line after it
    /// (`split_frontmatter`), so the empty lines a note starts with are in
    /// the note but never in what its file reads as. A filename-as-title
    /// note that is only its title has an empty file, which reads as one
    /// empty line rather than none. (The title of a stripped note is put
    /// back by the caller.)
    pub fn with_unwritten_lines(&self, parsed: ParsedNoteMarkdown) -> ParsedNoteMarkdown {
        let plain_empty =
            |p: &FormatParagraph| p.text.is_empty() && p.kind == ParagraphKind::Body && p.block_quote_level == 0;
        if self.title_stripped {
            let title_only = self.format.as_ref().is_some_and(|f| f.len() == 1);
            return if title_only && parsed.paragraphs.len() == 1 && plain_empty(&parsed.paragraphs[0]) {
                ParsedNoteMarkdown::from_paragraphs(Vec::new())
            } else {
                parsed
            };
        }
        // Each empty line the Markdown starts with is an empty paragraph
        // (one in a quote, a list or a heading has its marker).
        let leading = self
            .markdown_text
            .split('\n')
            .take_while(|line| line.is_empty())
            .count();
        let kept = parsed.paragraphs.iter().take_while(|p| plain_empty(p)).count();
        if leading <= kept {
            return parsed;
        }
        let empty = FormatParagraph {
            kind: ParagraphKind::Body,
            indent: 0,
            block_quote_level: 0,
            done: None,
            start_number: 0,
            text: String::new(),
            spans: Vec::new(),
            start: 0,
        };
        let mut paragraphs = vec![empty; leading - kept];
        paragraphs.extend(parsed.paragraphs);
        ParsedNoteMarkdown::from_paragraphs(paragraphs)
    }
}

/// `NoteDecodeResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteDecodeResult {
    Deleted,
    Unsyncable(UnsyncableReason),
    Ok(Box<DecodedNote>),
}

/// The reason engines up to 0.10.1 gave a note whose text lives in a
/// `TextDataAsset` (upstream icloud-md PR #29, verbatim), which they could
/// read but not write. Such a note is written back now; a vault's state may
/// still carry the reason, which is dropped when the state is read.
pub const TEXT_DATA_ASSET_UNPUBLISHABLE_REASON: &str =
    "is so large that Apple keeps its text in a separate file, which can't be written back yet";

/// The largest `TextDataEncrypted` (base64) decoded: CloudKit caps an inline
/// field near 1 MB, so only an inlined `TextDataAsset` comes close, and a
/// body past this is undecodable rather than inflated.
pub const MAX_TEXT_DATA_BASE64: usize = 16 << 20;

/// `classifyNoteRecord`.
///
/// A note whose text Apple moved into a `TextDataAsset` (upstream PR #29)
/// reads like any other once `Database::inline_asset_bodies` has fetched
/// it, and is written back like any other: `Database::update_records`
/// uploads its new text as an asset.
pub fn classify_note_record(record: &CloudKitRecord, options: &ClassifyOptions) -> NoteDecodeResult {
    if is_deleted(record) {
        return NoteDecodeResult::Deleted;
    }
    let Some(Value::String(text_value)) = record.fields.get("TextDataEncrypted").map(|f| &f.value) else {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::MissingBody);
    };
    if text_value.len() > MAX_TEXT_DATA_BASE64 {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::Undecodable);
    }
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
        paragraphs[1..].to_vec()
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
    let trimmed: Vec<FormatParagraph> = projected.iter().map(trim_trailing_whitespace).collect();
    if !projection_survives(&trimmed, &rendered) {
        return unpublishable("the note's formatting doesn't survive this tool's markdown round trip".into());
    }
    if !tables_read_back(&trimmed, &rendered, &base.embed_slots) {
        return unpublishable(
            "a table shares its line with text, or sits in a list, heading, quote or code block, or the line after it \
             reads as one of its rows: its Markdown table can't be read back"
                .into(),
        );
    }
    NoteDecodeResult::Ok(Box::new(DecodedNote {
        markdown_text: rendered,
        format: Some(paragraphs),
        title_stripped: strip_title,
        publishable: true,
        ..base
    }))
}

/// Whether every table of the note reads back from where its Markdown table
/// is written, in place of its U+FFFC: the placeholder is a plain line of
/// its own (no text before or after it, no list, heading, quote, code or
/// styling around it, any of which keeps the table from being found where
/// it is written, or ends its last row early), and the line after it can't
/// be read as one more of its rows. `paragraphs` are the ones `rendered`
/// writes, their embeds numbered from the first of `slots`.
fn tables_read_back(paragraphs: &[FormatParagraph], rendered: &str, slots: &[EmbedSlot]) -> bool {
    let is_table = |slot: usize| matches!(slots.get(slot), Some(EmbedSlot::Attachment(reference)) if is_table_uti(&reference.type_uti));
    let placeholder = OBJECT_REPLACEMENT_CHARACTER.to_string();
    let mut slot = 0;
    for paragraph in paragraphs {
        for _ in paragraph.text.chars().filter(|&c| c == OBJECT_REPLACEMENT_CHARACTER) {
            if is_table(slot)
                && !(paragraph.kind == ParagraphKind::Body
                    && paragraph.block_quote_level == 0
                    && paragraph.text == placeholder)
            {
                return false;
            }
            slot += 1;
        }
    }
    let reads_as_row = |line: &str| {
        let line = line.trim();
        line.len() >= 2 && line.starts_with('|') && line.ends_with('|')
    };
    let lines: Vec<&str> = rendered.split('\n').collect();
    let mut slot = 0;
    for (index, line) in lines.iter().enumerate() {
        for _ in line.chars().filter(|&c| c == OBJECT_REPLACEMENT_CHARACTER) {
            if is_table(slot) && (*line != placeholder || lines.get(index + 1).is_some_and(|next| reads_as_row(next))) {
                return false;
            }
            slot += 1;
        }
    }
    true
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
