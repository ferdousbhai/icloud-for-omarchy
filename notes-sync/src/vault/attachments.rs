//! Attachment download and table-attachment rendering. Ports icloud-md
//! `src/notes/attachmentSync.ts` (plus `noteAttachments.ts`'s
//! `decodeAttachmentFilename` and `parseAssetField`, which only this module
//! uses).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde_json::Value;

use super::layout::reference_record_name;
use super::state::{AttachmentEntry, TableAttachmentEntry};
use crate::cloudkit::client::DownloadQueue;
use crate::cloudkit::{CloudKitRecord, Database, FieldValue, NoteZone, Transport};
use crate::cmd::errors::Error;
use crate::doc::decode::is_deleted;
use crate::doc::embeds::{
    AttachmentReference, EmbedMarkerContent, EmbedSlot, decode_note_embed_slots, format_attachment_markdown,
    format_embed_marker, is_table_uti, render_placeholders,
};
use crate::doc::tables::decode_table_markdown;
use crate::js::base64_decode;
use crate::js::posix;
use crate::md::filename::unique_file_name;

const ATTACHMENTS_DIR: &str = "attachments";

/// `AttachmentAsset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentAsset {
    pub download_url: String,
    pub file_checksum: String,
}

/// `parseAssetField`: an `ASSETID` field's download URL and checksum.
pub fn parse_asset_field(field: Option<&FieldValue>) -> Option<AttachmentAsset> {
    let field = field?;
    if field.type_ != "ASSETID" {
        return None;
    }
    let value = field.value.as_object()?;
    Some(AttachmentAsset {
        download_url: value.get("downloadURL")?.as_str()?.to_owned(),
        file_checksum: value.get("fileChecksum")?.as_str()?.to_owned(),
    })
}

fn extension_for_uti(type_uti: &str) -> &'static str {
    match type_uti {
        "public.jpeg" => ".jpeg",
        "public.png" => ".png",
        "public.heic" => ".heic",
        "public.tiff" => ".tiff",
        "public.gif" => ".gif",
        "public.webp" => ".webp",
        "com.apple.m4a-audio" => ".m4a",
        "com.adobe.pdf" => ".pdf",
        _ => "",
    }
}

/// `decodeAttachmentFilename`: the Media record's own file name, else
/// `<recordName><ext for the UTI>`.
pub fn decode_attachment_filename(field: Option<&FieldValue>, record_name: &str, type_uti: &str) -> String {
    if let Some(Value::String(b64)) = field.map(|f| &f.value) {
        let name = String::from_utf8_lossy(&base64_decode(b64)).into_owned();
        if !name.is_empty() {
            return name;
        }
    }
    format!("{record_name}{}", extension_for_uti(type_uti))
}

/// `MatchedAttachment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedAttachment {
    pub record_name: String,
    pub relative_file: String,
    pub link_path: String,
    pub needs_download: bool,
    pub download_url: String,
    pub entry: AttachmentEntry,
}

/// `extractMediaRecordNames`.
pub fn extract_media_record_names(
    refs: &[AttachmentReference],
    attachment_records: &[CloudKitRecord],
) -> Vec<Option<String>> {
    refs.iter()
        .map(|r| {
            let record = attachment_records
                .iter()
                .rev()
                .find(|rec| rec.record_name == r.attachment_identifier)?;
            if record.record_type != "Attachment" {
                return None;
            }
            reference_record_name(record.fields.get("Media").map(|f| &f.value))
        })
        .collect()
}

/// `matchAttachmentRecords`.
#[allow(clippy::too_many_arguments)]
pub fn match_attachment_records(
    refs: &[AttachmentReference],
    media_record_names: &[Option<String>],
    media_records: &[CloudKitRecord],
    note_record_name: &str,
    existing_attachments: &IndexMap<String, AttachmentEntry>,
    used_attachment_file_names: &mut HashSet<String>,
    note_dir: &str,
) -> Vec<Option<MatchedAttachment>> {
    let mut matched = Vec::with_capacity(refs.len());
    for (i, r) in refs.iter().enumerate() {
        let Some(Some(media_record_name)) = media_record_names.get(i) else {
            matched.push(None);
            continue;
        };
        if media_record_name.is_empty() {
            matched.push(None);
            continue;
        }
        let Some(media) = media_records.iter().rev().find(|m| m.record_name == *media_record_name) else {
            matched.push(None);
            continue;
        };
        if media.record_type != "Media" {
            matched.push(None);
            continue;
        }
        let Some(asset) = parse_asset_field(media.fields.get("Asset")) else {
            matched.push(None);
            continue;
        };
        let existing = existing_attachments.get(&r.attachment_identifier);
        let file_name = match existing {
            Some(e) => posix::basename(&e.file).to_owned(),
            None => unique_file_name(
                &decode_attachment_filename(
                    media.fields.get("FilenameEncrypted"),
                    &r.attachment_identifier,
                    &r.type_uti,
                ),
                used_attachment_file_names,
            ),
        };
        used_attachment_file_names.insert(file_name.clone());
        let relative_file = match existing {
            Some(e) => e.file.clone(),
            None => posix::join(&[note_dir, ATTACHMENTS_DIR, &file_name]),
        };
        matched.push(Some(MatchedAttachment {
            record_name: r.attachment_identifier.clone(),
            link_path: posix::relative(note_dir, &relative_file),
            needs_download: existing.is_none_or(|e| e.media_file_checksum != asset.file_checksum),
            download_url: asset.download_url,
            entry: AttachmentEntry {
                file: relative_file.clone(),
                media_record_name: media_record_name.clone(),
                media_file_checksum: asset.file_checksum,
                note_record_name: note_record_name.to_owned(),
            },
            relative_file,
        }));
    }
    matched
}

/// `TableAttachmentSnapshotSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAttachmentSnapshotSource {
    pub record_name: String,
    pub note_record_name: String,
    pub record_change_tag: String,
    pub value_base64: String,
}

/// `AttachmentSyncResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentSyncResult {
    pub body_text: String,
    pub attachments: IndexMap<String, AttachmentEntry>,
    pub stale_attachment_record_names: Vec<String>,
    pub table_attachments: IndexMap<String, TableAttachmentEntry>,
    pub stale_table_attachment_record_names: Vec<String>,
    pub table_attachment_snapshots: Vec<TableAttachmentSnapshotSource>,
}

/// The Attachment and Media records of the notes a pull or clone resolves,
/// looked up once per zone ([`AttachmentRecords::prefetch`]) instead of
/// twice per note. A name is asked for at most once: one the server didn't
/// answer stays unanswered, as a per-note lookup would have left it.
#[derive(Debug, Default)]
pub struct AttachmentRecords {
    zones: HashMap<NoteZone, ZoneAttachmentRecords>,
}

#[derive(Debug, Default)]
struct ZoneAttachmentRecords {
    records: HashMap<String, CloudKitRecord>,
    asked: HashSet<String>,
}

impl AttachmentRecords {
    /// Looks up every Attachment the embeds of `notes` (all in `zone`)
    /// name, then the Media records of the file attachments among them: two
    /// `records/lookup` walks (200 names a request) for the whole zone.
    /// Records that aren't live, decodable notes contribute nothing.
    pub fn prefetch<T: Transport>(
        &mut self,
        db: &Database<T>,
        zone: &NoteZone,
        notes: &[&CloudKitRecord],
    ) -> Result<(), Error> {
        let mut refs: Vec<AttachmentReference> = Vec::new();
        for note in notes {
            if note.record_type != "Note" || is_deleted(note) {
                continue;
            }
            let Some(Value::String(text)) = note.fields.get("TextDataEncrypted").map(|f| &f.value) else {
                continue;
            };
            let Ok(Some(slots)) = decode_note_embed_slots(&base64_decode(text)) else {
                continue;
            };
            refs.extend(slots.into_iter().filter_map(|slot| match slot {
                EmbedSlot::Attachment(r) => Some(r),
                EmbedSlot::Unknown { .. } => None,
            }));
        }
        let names: Vec<String> = refs.iter().map(|r| r.attachment_identifier.clone()).collect();
        self.ensure(db, zone, &names)?;
        let file_refs: Vec<AttachmentReference> = refs.into_iter().filter(|r| !is_table_uti(&r.type_uti)).collect();
        let media_names: Vec<String> = extract_media_record_names(&file_refs, &self.records_named(zone, &names))
            .into_iter()
            .flatten()
            .collect();
        self.ensure(db, zone, &media_names)
    }

    /// Looks up the names in `zone` not asked for yet (first occurrence
    /// order, each once).
    fn ensure<T: Transport>(&mut self, db: &Database<T>, zone: &NoteZone, names: &[String]) -> Result<(), Error> {
        let cache = self.zones.entry(zone.clone()).or_default();
        let mut missing = Vec::new();
        for name in names {
            if cache.asked.insert(name.clone()) {
                missing.push(name.clone());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        // A later duplicate wins, as `.rev().find()` over the answer did.
        for record in db.lookup_records(zone, &missing)? {
            cache.records.insert(record.record_name.clone(), record);
        }
        Ok(())
    }

    /// The records answered for `names` in `zone`, in `names` order.
    fn records_named(&self, zone: &NoteZone, names: &[String]) -> Vec<CloudKitRecord> {
        let Some(cache) = self.zones.get(zone) else {
            return Vec::new();
        };
        names.iter().filter_map(|n| cache.records.get(n)).cloned().collect()
    }
}

/// `decodeTableAttachment`: the table's markdown, or `None` when the record
/// isn't a readable table.
pub fn decode_table_attachment(record: Option<&CloudKitRecord>) -> Option<String> {
    let record = record?;
    if record.record_type != "Attachment" {
        return None;
    }
    let Value::String(b64) = &record.fields.get("MergeableDataEncrypted")?.value else {
        return None;
    };
    decode_table_markdown(&base64_decode(b64)).ok()
}

/// `resolveNoteAttachments`: download file attachments (through
/// `downloads`), render tables inline, markers for anything unresolvable.
/// Records come from `records`; names [`AttachmentRecords::prefetch`] didn't
/// cover are looked up here.
#[allow(clippy::too_many_arguments)]
pub fn resolve_note_attachments<T: Transport>(
    db: &Database<T>,
    records: &mut AttachmentRecords,
    downloads: &mut DownloadQueue<'_, T>,
    zone: &NoteZone,
    target_dir: &Path,
    note_record_name: &str,
    body_text: &str,
    slots: &[EmbedSlot],
    existing_attachments: &IndexMap<String, AttachmentEntry>,
    existing_table_attachments: &IndexMap<String, TableAttachmentEntry>,
    used_attachment_file_names: &mut HashSet<String>,
    note_dir: &str,
) -> Result<AttachmentSyncResult, Error> {
    let previous_for_note: Vec<String> = existing_attachments
        .iter()
        .filter(|(_, e)| e.note_record_name == note_record_name)
        .map(|(k, _)| k.clone())
        .collect();
    let previous_table_for_note: Vec<String> = existing_table_attachments
        .iter()
        .filter(|(_, e)| e.note_record_name == note_record_name)
        .map(|(k, _)| k.clone())
        .collect();

    if slots.is_empty() {
        return Ok(AttachmentSyncResult {
            body_text: body_text.to_owned(),
            stale_attachment_record_names: previous_for_note,
            stale_table_attachment_record_names: previous_table_for_note,
            ..Default::default()
        });
    }

    let identified: Vec<&AttachmentReference> = slots
        .iter()
        .filter_map(|s| match s {
            EmbedSlot::Attachment(r) => Some(r),
            EmbedSlot::Unknown { .. } => None,
        })
        .collect();
    let names: Vec<String> = identified.iter().map(|r| r.attachment_identifier.clone()).collect();
    records.ensure(db, zone, &names)?;
    let attachment_records = records.records_named(zone, &names);
    let by_name = |name: &str| attachment_records.iter().rev().find(|r| r.record_name == name);

    let mut replacements: Vec<Option<String>> = vec![None; slots.len()];
    let mut file_refs: Vec<AttachmentReference> = Vec::new();
    let mut file_ref_indexes: Vec<usize> = Vec::new();
    let mut table_attachments = IndexMap::new();
    let mut table_attachment_snapshots = Vec::new();

    for (i, slot) in slots.iter().enumerate() {
        let r = match slot {
            EmbedSlot::Unknown { type_uti } => {
                replacements[i] = Some(format_embed_marker(&EmbedMarkerContent {
                    type_uti: type_uti.clone(),
                    attachment_identifier: None,
                }));
                continue;
            }
            EmbedSlot::Attachment(r) => r,
        };
        if is_table_uti(&r.type_uti) {
            let record = by_name(&r.attachment_identifier);
            match decode_table_attachment(record) {
                None => replacements[i] = Some(format_embed_marker(&marker_for(r))),
                Some(markdown) => {
                    replacements[i] = Some(markdown);
                    table_attachments.insert(
                        r.attachment_identifier.clone(),
                        TableAttachmentEntry {
                            note_record_name: note_record_name.to_owned(),
                        },
                    );
                    if let Some(Value::String(raw)) = record
                        .and_then(|r| r.fields.get("MergeableDataEncrypted"))
                        .map(|f| &f.value)
                    {
                        table_attachment_snapshots.push(TableAttachmentSnapshotSource {
                            record_name: r.attachment_identifier.clone(),
                            note_record_name: note_record_name.to_owned(),
                            record_change_tag: record.and_then(|r| r.record_change_tag.clone()).unwrap_or_default(),
                            value_base64: raw.clone(),
                        });
                    }
                }
            }
        } else {
            file_ref_indexes.push(i);
            file_refs.push(r.clone());
        }
    }

    let mut attachments = IndexMap::new();
    if !file_refs.is_empty() {
        let media_record_names = extract_media_record_names(&file_refs, &attachment_records);
        let known: Vec<String> = media_record_names.iter().flatten().cloned().collect();
        records.ensure(db, zone, &known)?;
        let media_records = records.records_named(zone, &known);
        let matched = match_attachment_records(
            &file_refs,
            &media_record_names,
            &media_records,
            note_record_name,
            existing_attachments,
            used_attachment_file_names,
            note_dir,
        );
        for (j, r) in file_refs.iter().enumerate() {
            let index = file_ref_indexes[j];
            let Some(attachment) = &matched[j] else {
                replacements[index] = Some(format_embed_marker(&marker_for(r)));
                continue;
            };
            if attachment.needs_download {
                let dest = target_dir.join(&attachment.relative_file);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                downloads.push(attachment.download_url.clone(), dest)?;
            }
            attachments.insert(attachment.record_name.clone(), attachment.entry.clone());
            replacements[index] = Some(format_attachment_markdown(r, &attachment.link_path));
        }
    }

    let stale_attachment_record_names = previous_for_note
        .into_iter()
        .filter(|rn| !attachments.contains_key(rn))
        .collect();
    let stale_table_attachment_record_names = previous_table_for_note
        .into_iter()
        .filter(|rn| !table_attachments.contains_key(rn))
        .collect();

    Ok(AttachmentSyncResult {
        body_text: render_placeholders(body_text, &replacements),
        attachments,
        stale_attachment_record_names,
        table_attachments,
        stale_table_attachment_record_names,
        table_attachment_snapshots,
    })
}

fn marker_for(r: &AttachmentReference) -> EmbedMarkerContent {
    EmbedMarkerContent {
        type_uti: Some(r.type_uti.clone()),
        attachment_identifier: Some(r.attachment_identifier.clone()),
    }
}

/// `removeAttachmentsForNote`: delete the note's attachment files; returns
/// their record names.
pub fn remove_attachments_for_note(
    target_dir: &Path,
    note_record_name: &str,
    attachments: &IndexMap<String, AttachmentEntry>,
) -> Result<Vec<String>, Error> {
    let mut removed = Vec::new();
    for (record_name, entry) in attachments {
        if entry.note_record_name != note_record_name {
            continue;
        }
        safe_unlink(&target_dir.join(&entry.file))?;
        removed.push(record_name.clone());
    }
    Ok(removed)
}

/// `removeTableAttachmentsForNote`.
pub fn remove_table_attachments_for_note(
    note_record_name: &str,
    table_attachments: &IndexMap<String, TableAttachmentEntry>,
) -> Vec<String> {
    table_attachments
        .iter()
        .filter(|(_, e)| e.note_record_name == note_record_name)
        .map(|(k, _)| k.clone())
        .collect()
}

/// `rm`/`unlink` ignoring ENOENT.
pub fn safe_unlink(path: &Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
