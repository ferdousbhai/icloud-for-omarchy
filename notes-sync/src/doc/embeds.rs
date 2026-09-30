//! Embedded objects: attachment references, embed slots, inline embed
//! markers, the unknown-content banner, and the push-side embed plan. Ports
//! icloud-md `src/notes/noteAttachments.ts`, `unknownContent.ts` and
//! `embedPushEdit.ts`. Offsets are UTF-16 code units.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::Result;
use super::proto::{Message, topotext};
use super::text::{decompress_note_document, parse_versioned_document};
use crate::js::{from_utf16, utf16};
use crate::md::table::{MarkdownTableBlock, find_markdown_table_blocks};

/// U+FFFC, one per embed in a note's visible text.
pub const OBJECT_REPLACEMENT_CHARACTER: char = '\u{FFFC}';
const ORC: u16 = 0xfffc;

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
/// Serializes like the TS union (`{kind: "attachment", ref}` /
/// `{kind: "unknown", typeUti?}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "EmbedSlotJson", into = "EmbedSlotJson")]
pub enum EmbedSlot {
    Attachment(AttachmentReference),
    /// Placeholder whose `attachmentInfo` run was absent or incomplete.
    Unknown {
        type_uti: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum EmbedSlotJson {
    Attachment {
        #[serde(rename = "ref")]
        reference: AttachmentReference,
    },
    Unknown {
        #[serde(rename = "typeUti", default, skip_serializing_if = "Option::is_none")]
        type_uti: Option<String>,
    },
}

impl From<EmbedSlotJson> for EmbedSlot {
    fn from(j: EmbedSlotJson) -> Self {
        match j {
            EmbedSlotJson::Attachment { reference } => EmbedSlot::Attachment(reference),
            EmbedSlotJson::Unknown { type_uti } => EmbedSlot::Unknown { type_uti },
        }
    }
}

impl From<EmbedSlot> for EmbedSlotJson {
    fn from(slot: EmbedSlot) -> Self {
        match slot {
            EmbedSlot::Attachment(reference) => EmbedSlotJson::Attachment { reference },
            EmbedSlot::Unknown { type_uti } => EmbedSlotJson::Unknown { type_uti },
        }
    }
}

/// `EmbedMarkerContent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbedMarkerContent {
    pub type_uti: Option<String>,
    pub attachment_identifier: Option<String>,
}

/// `ParsedEmbedMarker` (offsets in UTF-16 units). `type_uti` is `"unknown"`
/// when the marker has no `type` attribute, as in TS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEmbedMarker {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub type_uti: String,
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

const ADMONITION_HEADER: &str = "> [!danger] Unparsed content";

/// `UNKNOWN_CONTENT_BANNER`.
pub const UNKNOWN_CONTENT_BANNER: &str = "> [!danger] Unparsed content\n\
> This note contains content this tool can't parse or place precisely. \
It stays read-only here until that's resolved (e.g. by editing the note in Notes directly).\n\n";

fn decode_string(compressed: &[u8]) -> Result<topotext::String> {
    let raw = decompress_note_document(compressed)?;
    let versioned = parse_versioned_document(&raw)?;
    Ok(topotext::String::decode(&versioned.data)?)
}

/// `decodeNoteAttachmentRefs`.
pub fn decode_note_attachment_refs(compressed: &[u8]) -> Result<Vec<AttachmentReference>> {
    let s = decode_string(compressed)?;
    Ok(s.attribute_run
        .iter()
        .filter_map(|run| {
            let info = run.attachment_info.as_ref()?;
            Some(AttachmentReference {
                attachment_identifier: info.attachment_identifier.clone()?,
                type_uti: info.type_uti.clone()?,
            })
        })
        .collect())
}

/// `decodeNoteEmbedSlots`: `Ok(None)` when the embed structure defies the
/// model (the note then gets the banner and stays read-only).
pub fn decode_note_embed_slots(compressed: &[u8]) -> Result<Option<Vec<EmbedSlot>>> {
    let s = decode_string(compressed)?;
    Ok(embed_slots_of(&s))
}

/// The pure part of `decodeNoteEmbedSlots`, on an already-decoded string.
pub fn embed_slots_of(s: &topotext::String) -> Option<Vec<EmbedSlot>> {
    let text = utf16(s.string.as_deref().unwrap_or(""));
    let mut info_by_offset: HashMap<usize, &topotext::AttachmentInfo> = HashMap::new();
    let mut offset = 0usize;
    for run in &s.attribute_run {
        if let Some(info) = &run.attachment_info {
            if run.len() != 1 || text.get(offset) != Some(&ORC) {
                return None;
            }
            info_by_offset.insert(offset, info);
        }
        offset += run.len() as usize;
    }
    if offset > text.len() {
        return None;
    }
    let mut slots = Vec::new();
    for (i, &unit) in text.iter().enumerate() {
        if unit != ORC {
            continue;
        }
        let info = info_by_offset.get(&i);
        match info {
            Some(info) if info.attachment_identifier.is_some() && info.type_uti.is_some() => {
                slots.push(EmbedSlot::Attachment(AttachmentReference {
                    attachment_identifier: info.attachment_identifier.clone().unwrap_or_default(),
                    type_uti: info.type_uti.clone().unwrap_or_default(),
                }))
            }
            _ => slots.push(EmbedSlot::Unknown {
                type_uti: info.and_then(|info| info.type_uti.clone()),
            }),
        }
    }
    Some(slots)
}

const IMAGE_UTIS: [&str; 6] = [
    "public.jpeg",
    "public.png",
    "public.heic",
    "public.tiff",
    "public.gif",
    "public.webp",
];

/// `isImageUti`.
pub fn is_image_uti(type_uti: &str) -> bool {
    IMAGE_UTIS.contains(&type_uti)
}

/// `isTableUti`.
pub fn is_table_uti(type_uti: &str) -> bool {
    type_uti == TABLE_UTI
}

/// `hasAttachmentReference`: `/!?\[[^\]]*\]\(attachments\/[^)]+\)/`.
pub fn has_attachment_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
    let needle = b"(attachments/";
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'[' {
            continue;
        }
        let Some(close) = bytes[i + 1..].iter().position(|&c| c == b']').map(|p| p + i + 1) else {
            return false;
        };
        let after = close + 1;
        if !bytes[after..].starts_with(needle) {
            continue;
        }
        let rest = after + needle.len();
        if let Some(paren) = bytes[rest..].iter().position(|&c| c == b')')
            && paren > 0
        {
            return true;
        }
    }
    false
}

/// `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// POSIX `path.basename`.
fn basename(p: &str) -> &str {
    let trimmed = p.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `formatAttachmentMarkdown`.
pub fn format_attachment_markdown(reference: &AttachmentReference, relative_file: &str) -> String {
    let display_name = basename(relative_file);
    let href = relative_file
        .split('/')
        .map(encode_uri_component)
        .collect::<Vec<_>>()
        .join("/");
    if is_image_uti(&reference.type_uti) {
        format!("![{display_name}]({href})")
    } else {
        format!("[{display_name}]({href})")
    }
}

/// `renderPlaceholders`: substitute each U+FFFC in order.
pub fn render_placeholders(body_text: &str, replacements: &[Option<String>]) -> String {
    if replacements.is_empty() {
        return body_text.to_string();
    }
    let mut index = 0usize;
    let mut out = String::with_capacity(body_text.len());
    for c in body_text.chars() {
        if c == OBJECT_REPLACEMENT_CHARACTER {
            match replacements.get(index).and_then(|r| r.as_deref()) {
                Some(replacement) => out.push_str(replacement),
                None => out.push(OBJECT_REPLACEMENT_CHARACTER),
            }
            index += 1;
        } else {
            out.push(c);
        }
    }
    out
}

/// `renderAttachmentPlaceholders`.
pub fn render_attachment_placeholders(
    body_text: &str,
    refs: &[AttachmentReference],
    relative_files: &[String],
) -> std::result::Result<String, String> {
    if refs.len() != relative_files.len() {
        return Err(format!(
            "attachment ref count ({}) doesn't match resolved file count ({})",
            refs.len(),
            relative_files.len()
        ));
    }
    let replacements: Vec<Option<String>> = refs
        .iter()
        .zip(relative_files)
        .map(|(r, f)| Some(format_attachment_markdown(r, f)))
        .collect();
    Ok(render_placeholders(body_text, &replacements))
}

/// `hasUnknownContentMarker`.
pub fn has_unknown_content_marker(text: &str) -> bool {
    text.contains(ADMONITION_HEADER)
}

fn embed_label(type_: &str) -> Option<&'static str> {
    Some(match type_ {
        "com.apple.notes.gallery" => "gallery",
        "com.apple.drawing.2" | "com.apple.drawing" => "drawing",
        "com.apple.paper" => "paper document",
        "com.apple.notes.table" => "table",
        "public.url" => "link preview",
        _ => return None,
    })
}

const UNKNOWN_TYPE: &str = "unknown";

/// `formatEmbedMarker`.
pub fn format_embed_marker(content: &EmbedMarkerContent) -> String {
    let type_ = content.type_uti.as_deref().unwrap_or(UNKNOWN_TYPE);
    let label = match embed_label(type_) {
        Some(label) => label,
        None if content.type_uti.is_none() => "unidentified embed",
        None => type_,
    };
    let id_attribute = content
        .attachment_identifier
        .as_ref()
        .map(|id| format!(" id=\"{id}\""))
        .unwrap_or_default();
    format!("<apple-embed type=\"{type_}\"{id_attribute}>{label}</apple-embed>")
}

fn is_word_unit(unit: u16) -> bool {
    unit < 0x80 && (unit as u8).is_ascii_alphanumeric() || unit == u16::from(b'_')
}

fn starts_with_at(haystack: &[u16], at: usize, needle: &str) -> bool {
    let needle = utf16(needle);
    haystack.len() >= at + needle.len() && haystack[at..at + needle.len()] == needle[..]
}

/// `/\b<name>="([^"]*)"/.exec(attributes)?.[1]`.
fn attribute_value(attributes: &[u16], name: &str) -> Option<String> {
    let prefix = format!("{name}=\"");
    let prefix_len = utf16(&prefix).len();
    for i in 0..attributes.len() {
        if i > 0 && is_word_unit(attributes[i - 1]) {
            continue;
        }
        if !starts_with_at(attributes, i, &prefix) {
            continue;
        }
        let value_start = i + prefix_len;
        if let Some(len) = attributes[value_start..].iter().position(|&u| u == u16::from(b'"')) {
            return Some(from_utf16(&attributes[value_start..value_start + len]));
        }
    }
    None
}

/// `parseEmbedMarkers`: every
/// `/<apple-embed\b([^>]*)>([^<]*)<\/apple-embed>/g` match, in order.
pub fn parse_embed_markers(text: &str) -> Vec<ParsedEmbedMarker> {
    let units = utf16(text);
    let open = utf16("<apple-embed");
    let close = utf16("</apple-embed>");
    let mut markers = Vec::new();
    let mut i = 0usize;
    while i < units.len() {
        let try_match = || -> Option<(usize, usize, usize)> {
            if !starts_with_at(&units, i, "<apple-embed") {
                return None;
            }
            let after_name = i + open.len();
            if units.get(after_name).is_some_and(|&u| is_word_unit(u)) {
                return None;
            }
            let gt = after_name + units[after_name..].iter().position(|&u| u == u16::from(b'>'))?;
            let lt = gt + 1 + units[gt + 1..].iter().position(|&u| u == u16::from(b'<'))?;
            if units[lt..].starts_with(&close) {
                Some((after_name, gt, lt + close.len()))
            } else {
                None
            }
        };
        match try_match() {
            Some((attributes_start, gt, end)) => {
                let attributes = &units[attributes_start..gt];
                markers.push(ParsedEmbedMarker {
                    start: i,
                    end,
                    text: from_utf16(&units[i..end]),
                    type_uti: attribute_value(attributes, "type").unwrap_or_else(|| UNKNOWN_TYPE.to_string()),
                    attachment_identifier: attribute_value(attributes, "id"),
                });
                i = end;
            }
            None => i += 1,
        }
    }
    markers
}

/// `hasEmbedMarker`: `/<apple-embed\b/`.
pub fn has_embed_marker(text: &str) -> bool {
    let units = utf16(text);
    let open_len = utf16("<apple-embed").len();
    (0..units.len()).any(|i| {
        starts_with_at(&units, i, "<apple-embed") && !units.get(i + open_len).is_some_and(|&u| is_word_unit(u))
    })
}

/// `combineUnpublishableReasons`.
pub fn combine_unpublishable_reasons(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let joined = [a, b].into_iter().flatten().collect::<Vec<_>>().join("; ");
    if joined.is_empty() { None } else { Some(joined) }
}

struct LocalRepresentation {
    start: usize,
    end: usize,
    table: Option<MatchedTableBlock>,
}

/// `planEmbedRepresentations`.
pub fn plan_embed_representations(
    local_text: &str,
    slots: &[EmbedSlot],
    tracked_file_attachment_ids: &HashSet<String>,
) -> std::result::Result<EmbedRepresentation, String> {
    let units = utf16(local_text);
    let markers = parse_embed_markers(local_text);
    let marker_ids: HashSet<&str> = markers
        .iter()
        .filter_map(|m| m.attachment_identifier.as_deref())
        .collect();
    let table_blocks = find_markdown_table_blocks(local_text);
    let mut line_start_offsets = vec![0usize];
    for (i, &u) in units.iter().enumerate() {
        if u == u16::from(b'\n') {
            line_start_offsets.push(i + 1);
        }
    }

    let mut representations: Vec<LocalRepresentation> = Vec::new();
    let mut marker_cursor = 0usize;
    let mut table_cursor = 0usize;

    for slot in slots {
        let marker_content = match slot {
            EmbedSlot::Unknown { type_uti } => Some(EmbedMarkerContent {
                type_uti: type_uti.clone(),
                attachment_identifier: None,
            }),
            EmbedSlot::Attachment(reference) if marker_ids.contains(reference.attachment_identifier.as_str()) => {
                Some(EmbedMarkerContent {
                    type_uti: Some(reference.type_uti.clone()),
                    attachment_identifier: Some(reference.attachment_identifier.clone()),
                })
            }
            EmbedSlot::Attachment(_) => None,
        };

        let Some(marker_content) = marker_content else {
            let EmbedSlot::Attachment(reference) = slot else {
                continue;
            };
            if is_table_uti(&reference.type_uti) {
                let Some(block) = table_blocks.get(table_cursor) else {
                    return Err(format!(
                        "can't tell which table(s) changed (found {} table-shaped block(s) locally, expected more)",
                        table_blocks.len()
                    ));
                };
                table_cursor += 1;
                let start = line_start_offsets.get(block.start_line).copied().unwrap_or(units.len());
                let end = if block.end_line < line_start_offsets.len() {
                    line_start_offsets
                        .get(block.end_line)
                        .copied()
                        .unwrap_or(units.len() + 1)
                        - 1
                } else {
                    units.len()
                };
                representations.push(LocalRepresentation {
                    start,
                    end,
                    table: Some(MatchedTableBlock {
                        reference: reference.clone(),
                        block: block.clone(),
                    }),
                });
                continue;
            }
            if tracked_file_attachment_ids.contains(&reference.attachment_identifier) {
                return Err(
                    "this note has a file attachment - it can't be edited through this tool and stays read-only".into(),
                );
            }
            return Err(format!(
                "the embed marker for \"{}\" ({}) is missing - markers must be left exactly as this tool wrote them",
                reference.type_uti, reference.attachment_identifier
            ));
        };

        let expected = format_embed_marker(&marker_content);
        let Some(marker) = markers.get(marker_cursor) else {
            return Err(format!(
                "an embed marker is missing (expected {expected}) - markers must be left exactly as this tool wrote them"
            ));
        };
        marker_cursor += 1;
        if marker.text != expected {
            return Err(format!(
                "an embed marker was edited or is out of order (found {}, expected {expected}) - markers must be left exactly as this tool wrote them",
                marker.text
            ));
        }
        representations.push(LocalRepresentation {
            start: marker.start,
            end: marker.end,
            table: None,
        });
    }

    if let Some(extra) = markers.get(marker_cursor) {
        return Err(format!(
            "found an embed marker with nothing behind it ({}) - this tool can't create embeds; remove it",
            extra.text
        ));
    }
    if table_cursor < table_blocks.len() {
        return Err(format!(
            "can't tell which table(s) changed (found {} table-shaped block(s) locally, expected {table_cursor})",
            table_blocks.len()
        ));
    }

    let mut previous_end: i64 = -1;
    for representation in &representations {
        if (representation.start as i64) < previous_end {
            return Err(
                "embed markers and tables appear out of document order - markers must stay where this tool wrote them"
                    .into(),
            );
        }
        previous_end = representation.end as i64;
    }

    let mut reconstructed = units;
    for representation in representations.iter().rev() {
        // `slice(0, start) + ORC + slice(end)`, clamped like JS.
        let start = representation.start.min(reconstructed.len());
        let end = representation.end.min(reconstructed.len());
        let mut next = reconstructed[..start].to_vec();
        next.push(ORC);
        next.extend_from_slice(&reconstructed[end..]);
        reconstructed = next;
    }

    Ok(EmbedRepresentation {
        reconstructed_body_text: from_utf16(&reconstructed),
        tables: representations.into_iter().filter_map(|r| r.table).collect(),
    })
}

/// `decodeAttachmentFilename`: a Media record's `FilenameEncrypted`, or
/// `recordName` + an extension guessed from the UTI.
pub fn decode_attachment_filename(
    field: Option<&crate::cloudkit::FieldValue>,
    record_name: &str,
    type_uti: &str,
) -> String {
    if let Some(serde_json::Value::String(value)) = field.map(|f| &f.value) {
        let name = crate::js::buffer_to_utf8(&crate::js::base64_decode(value));
        if !name.is_empty() {
            return name;
        }
    }
    let extension = match type_uti {
        "public.jpeg" => ".jpeg",
        "public.png" => ".png",
        "public.heic" => ".heic",
        "public.tiff" => ".tiff",
        "public.gif" => ".gif",
        "public.webp" => ".webp",
        "com.apple.m4a-audio" => ".m4a",
        "com.adobe.pdf" => ".pdf",
        _ => "",
    };
    format!("{record_name}{extension}")
}

/// `AttachmentAsset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentAsset {
    pub download_url: String,
    pub file_checksum: String,
}

/// `parseAssetField`: the download URL + checksum of an `ASSETID` field.
pub fn parse_asset_field(field: Option<&crate::cloudkit::FieldValue>) -> Option<AttachmentAsset> {
    let field = field?;
    if field.type_ != "ASSETID" {
        return None;
    }
    let value = field.value.as_object()?;
    Some(AttachmentAsset {
        download_url: value.get("downloadURL")?.as_str()?.to_string(),
        file_checksum: value.get("fileChecksum")?.as_str()?.to_string(),
    })
}
