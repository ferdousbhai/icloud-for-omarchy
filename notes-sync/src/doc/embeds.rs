//! Embedded objects: attachment references, embed slots, inline embed
//! markers, the unknown-content banner, and the push-side embed plan.
//! Originally derived from icloud-md. Offsets are UTF-16 code units.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::Result;
use super::attach::uti_for_file_name;
use super::proto::topotext;
use super::text::decode_note_string;
use crate::js::{encode_uri_component, from_utf16, is_whitespace16, posix, starts_with_at, utf16};
use crate::md::table::{MarkdownTableBlock, find_markdown_table_blocks};

/// U+FFFC, one per embed in a note's visible text.
pub const OBJECT_REPLACEMENT_CHARACTER: char = '\u{FFFC}';
const ORC: u16 = 0xfffc;

/// UTI marking an `Attachment` record as a table sub-document.
const TABLE_UTI: &str = "com.apple.notes.table";

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
    /// Attachment links that stand for no embed the note has: files to add
    /// as new attachments. Each one's U+FFFC is in the text too.
    pub new_links: Vec<NewAttachmentLink>,
}

/// A link of the kind pull writes for a file attachment,
/// `[name](attachments/file)` or `![name](attachments/file)`, found in a
/// note's text (offsets in UTF-16 units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentLink {
    /// Where the link starts, its `!` included.
    pub start: usize,
    pub end: usize,
    /// Where its `[` is.
    pub bracket: usize,
    /// The link as written.
    pub text: String,
    /// Its target as written (percent-encoded), `attachments/...`.
    pub href: String,
}

/// An [`AttachmentLink`] to a file that is not an attachment of the note yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttachmentLink {
    /// Which of the reconstructed text's U+FFFC placeholders is this
    /// link's, counting from 0.
    pub slot_index: usize,
    pub text: String,
    pub href: String,
}

const ADMONITION_HEADER: &str = "> [!danger] Unparsed content";

/// `UNKNOWN_CONTENT_BANNER`.
pub const UNKNOWN_CONTENT_BANNER: &str = "> [!danger] Unparsed content\n\
> This note contains content this tool can't parse or place precisely. \
It stays read-only here until that's resolved (e.g. by editing the note in Notes directly).\n\n";

/// `decodeNoteEmbedSlots`: `Ok(None)` when the embed structure defies the
/// model (the note then gets the banner and stays read-only).
pub fn decode_note_embed_slots(compressed: &[u8]) -> Result<Option<Vec<EmbedSlot>>> {
    Ok(embed_slots_of(&decode_note_string(compressed)?))
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

/// Whether `text` holds something written as a link into `attachments/`
/// (`hasAttachmentReference`), readable as an attachment link or not.
pub fn has_attachment_reference(text: &str) -> bool {
    let scan = scan_attachment_links(&utf16(text));
    !scan.links.is_empty() || !scan.unreadable.is_empty()
}

/// Every attachment link in `text`, in order: an unescaped `[`, the link
/// text up to the first unescaped `]` on the same line, then
/// `(attachments/...)` with no space or line break in it. A `!` right
/// before the `[` of a link to an image belongs to the link.
pub fn find_attachment_links(text: &str) -> Vec<AttachmentLink> {
    scan_attachment_links(&utf16(text)).links
}

struct AttachmentLinkScan {
    links: Vec<AttachmentLink>,
    /// Where a `[` opens something that points into `attachments/` and is
    /// not a link as above (its text runs over a line break, or its target
    /// holds a space or never closes).
    unreadable: Vec<usize>,
}

fn scan_attachment_links(units: &[u16]) -> AttachmentLinkScan {
    let is = |at: usize, c: u8| units.get(at) == Some(&u16::from(c));
    let target = utf16("(attachments/");
    let mut scan = AttachmentLinkScan {
        links: Vec::new(),
        unreadable: Vec::new(),
    };
    let mut i = 0usize;
    while i < units.len() {
        if is(i, b'\\') {
            i += 2;
            continue;
        }
        if !is(i, b'[') {
            i += 1;
            continue;
        }
        let mut close = i + 1;
        let mut one_line = true;
        while close < units.len() && !is(close, b']') {
            one_line = one_line && !is(close, b'\n');
            close += if is(close, b'\\') { 2 } else { 1 };
        }
        if !is(close, b']') || !units[close + 1..].starts_with(&target) {
            i += 1;
            continue;
        }
        let href_start = close + 1 + target.len();
        if is(href_start, b')') {
            // `attachments/` itself: no file is named.
            i += 1;
            continue;
        }
        let length = units[href_start..]
            .iter()
            .position(|&u| u == u16::from(b')') || is_whitespace16(u))
            .filter(|&length| is(href_start + length, b')'));
        let (Some(length), true) = (length, one_line) else {
            scan.unreadable.push(i);
            i = href_start;
            continue;
        };
        let end = href_start + length + 1;
        let href = from_utf16(&units[close + 2..end - 1]);
        // Only an image's link is written with a `!`: before any other
        // file's, it is the note's own text.
        let embedded = i > 0 && is(i - 1, b'!') && is_image_uti(uti_for_file_name(&href));
        let start = if embedded { i - 1 } else { i };
        scan.links.push(AttachmentLink {
            start,
            end,
            bracket: i,
            text: from_utf16(&units[start..end]),
            href,
        });
        i = end;
    }
    scan
}

/// The two ways the link pull writes for `relative_file` may be spelled:
/// as [`format_attachment_markdown`] writes it now, and as it did before it
/// escaped the link text.
pub fn attachment_link_spellings(reference: &AttachmentReference, relative_file: &str) -> [String; 2] {
    [
        format_attachment_markdown(reference, relative_file),
        unescaped_attachment_markdown(reference, relative_file),
    ]
}

/// `formatAttachmentMarkdown`. The link text is the file name, from the
/// server: `[`, `]` and `\` are escaped so it can't end the link early or
/// open another, and a line break, which would end it, reads as a space.
pub fn format_attachment_markdown(reference: &AttachmentReference, relative_file: &str) -> String {
    let mut display_name = String::new();
    for c in posix::basename(relative_file).chars() {
        match c {
            '[' | ']' | '\\' => {
                display_name.push('\\');
                display_name.push(c);
            }
            '\n' | '\r' => display_name.push(' '),
            _ => display_name.push(c),
        }
    }
    attachment_link(reference, relative_file, &display_name)
}

/// The link [`format_attachment_markdown`] wrote before it escaped the
/// link text, which push still takes for the attachment.
fn unescaped_attachment_markdown(reference: &AttachmentReference, relative_file: &str) -> String {
    attachment_link(reference, relative_file, posix::basename(relative_file))
}

fn attachment_link(reference: &AttachmentReference, relative_file: &str, display_name: &str) -> String {
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
    const OPEN: &str = "<apple-embed";
    text.match_indices(OPEN).any(|(at, _)| {
        !text
            .as_bytes()
            .get(at + OPEN.len())
            .is_some_and(|&byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

struct LocalRepresentation {
    start: usize,
    end: usize,
    table: Option<MatchedTableBlock>,
}

/// `planEmbedRepresentations`. `tracked_file_attachments` maps each file
/// attachment this note tracks to its link path from the note's directory:
/// the link pull wrote for it stands for the attachment, as a marker does.
/// Any other attachment link (outside a table or marker) is a file to add:
/// it becomes a placeholder too, and is listed in `new_links`.
pub fn plan_embed_representations(
    local_text: &str,
    slots: &[EmbedSlot],
    tracked_file_attachments: &HashMap<String, String>,
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
            if let Some(link_path) = tracked_file_attachments.get(&reference.attachment_identifier) {
                let from = representations.last().map_or(0, |r| r.end);
                let find = |link: Vec<u16>| {
                    (from..=units.len().saturating_sub(link.len()))
                        .find(|&i| units.len() >= link.len() && units[i..i + link.len()] == link[..])
                        .map(|start| (start, link.len()))
                };
                let Some((start, length)) = find(utf16(&format_attachment_markdown(reference, link_path)))
                    .or_else(|| find(utf16(&unescaped_attachment_markdown(reference, link_path))))
                else {
                    return Err(format!(
                        "the link to the attachment \"{}\" was removed, changed or moved - attachments can only be \
                         removed or moved in Notes itself; put the link back as it was",
                        posix::basename(link_path)
                    ));
                };
                representations.push(LocalRepresentation {
                    start,
                    end: start + length,
                    table: None,
                });
                continue;
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

    finish_embed_plan(units, representations)
}

/// The plan of a note that has no embeds yet (a new note's file): its
/// attachment links are the files to add.
pub fn plan_new_attachment_links(local_text: &str) -> std::result::Result<EmbedRepresentation, String> {
    finish_embed_plan(utf16(local_text), Vec::new())
}

/// Adds the attachment links that stand for no existing embed to
/// `representations` (the existing ones, in order) and puts a placeholder
/// in place of each.
fn finish_embed_plan(
    units: Vec<u16>,
    representations: Vec<LocalRepresentation>,
) -> std::result::Result<EmbedRepresentation, String> {
    let existing = representations.len();
    let scan = scan_attachment_links(&units);
    // One inside a table or a marker is that embed's own text.
    let covered = |at: usize| representations.iter().any(|r| at >= r.start && at < r.end);
    if scan.unreadable.iter().any(|&at| !covered(at)) {
        return Err(
            "contains an \"attachments/...\" reference that isn't written the way this tool writes attachment \
             links, like [plan.pdf](attachments/plan.pdf) - rewrite it that way, or remove it"
                .into(),
        );
    }
    let new: Vec<AttachmentLink> = scan.links.into_iter().filter(|link| !covered(link.bracket)).collect();
    // (representation, the new link it is for)
    let mut all: Vec<(LocalRepresentation, Option<AttachmentLink>)> =
        representations.into_iter().map(|r| (r, None)).collect();
    for link in new {
        all.push((
            LocalRepresentation {
                start: link.start,
                end: link.end,
                table: None,
            },
            Some(link),
        ));
    }
    // Stable: the existing ones keep their order among themselves.
    all.sort_by_key(|(r, _)| r.start);
    if all.len() > existing {
        let mut previous_end = 0usize;
        for (representation, _) in &all {
            if representation.start < previous_end {
                return Err(
                    "an attachment link overlaps an embed marker, a table or another attachment's link - \
                     put it on a line of its own"
                        .into(),
                );
            }
            previous_end = representation.end;
        }
    }
    let new_links: Vec<NewAttachmentLink> = all
        .iter()
        .enumerate()
        .filter_map(|(slot_index, (_, link))| {
            link.as_ref().map(|link| NewAttachmentLink {
                slot_index,
                text: link.text.clone(),
                href: link.href.clone(),
            })
        })
        .collect();
    let representations: Vec<LocalRepresentation> = all.into_iter().map(|(r, _)| r).collect();

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
        new_links,
    })
}
