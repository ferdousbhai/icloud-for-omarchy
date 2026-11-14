//! Attachment download and table-attachment rendering. Originally derived
//! from icloud-md.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde_json::Value;

use super::layout::reference_record_name;
use super::state::{AttachmentEntry, TableAttachmentEntry, is_plain_record_name};
use crate::cloudkit::client::DownloadQueue;
use crate::cloudkit::{CloudKitRecord, Database, FieldValue, NoteZone, Transport};
use crate::cmd::errors::Error;
use crate::doc::attach::{MAX_NEW_ATTACHMENT_BYTES, uti_for_file_name};
use crate::doc::decode::is_deleted;
use crate::doc::embeds::{
    AttachmentReference, EmbedMarkerContent, EmbedSlot, decode_note_embed_slots, format_attachment_markdown,
    format_embed_marker, is_table_uti, render_placeholders,
};
use crate::doc::tables::decode_table_markdown;
use crate::js::base64_decode;
use crate::js::decode_uri_component;
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

pub fn extension_for_uti(type_uti: &str) -> &'static str {
    match type_uti {
        "public.jpeg" => ".jpeg",
        "public.png" => ".png",
        "public.heic" => ".heic",
        "public.tiff" => ".tiff",
        "public.gif" | "com.compuserve.gif" => ".gif",
        "public.webp" | "org.webmproject.webp" => ".webp",
        "com.apple.m4a-audio" => ".m4a",
        "com.adobe.pdf" => ".pdf",
        _ => "",
    }
}

/// `decodeAttachmentFilename`: the Media record's own file name, else
/// `<recordName><ext for the UTI>`. Both come from the server, a shared
/// note's from whoever owns it, and the name is joined under the note's
/// `attachments/` as it is, so it is made one plain file name first.
pub fn decode_attachment_filename(field: Option<&FieldValue>, record_name: &str, type_uti: &str) -> String {
    if let Some(Value::String(b64)) = field.map(|f| &f.value) {
        let name = plain_file_name(&String::from_utf8_lossy(&base64_decode(b64)));
        if !name.is_empty() {
            return name;
        }
    }
    let ext = extension_for_uti(type_uti);
    match plain_file_name(&format!("{record_name}{ext}")) {
        name if name.is_empty() => format!("attachment{ext}"),
        name => name,
    }
}

/// The longest attachment file name: a file name holds 255 bytes, and this
/// one may still take [`unique_file_name`]'s ` N` and, while it downloads,
/// a `.<pid>.tmp` suffix.
const MAX_ATTACHMENT_NAME_BYTES: usize = 200;

/// `name` as one file name: `/` and control characters (NUL among them)
/// become `_`, `.` and `..` (which `posix::join` would resolve) become
/// empty, and a name over [`MAX_ATTACHMENT_NAME_BYTES`] is cut on a
/// character boundary, keeping a short extension.
fn plain_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c.is_control() { '_' } else { c })
        .collect();
    match cleaned.as_str() {
        "." | ".." => String::new(),
        _ if cleaned.len() > MAX_ATTACHMENT_NAME_BYTES => {
            let ext = Some(posix::extname(&cleaned))
                .filter(|e| e.len() <= 16)
                .unwrap_or_default();
            let mut cut = MAX_ATTACHMENT_NAME_BYTES - ext.len();
            while !cleaned.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}{ext}", &cleaned[..cut])
        }
        _ => cleaned,
    }
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
    /// Names iCloud answered an error for other than `NOT_FOUND`, with its
    /// `serverErrorCode`: nothing is known of these records, which is not
    /// the same as their being gone.
    failed: HashMap<String, String>,
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
            refs.extend(slots.iter().filter_map(attachment_of).cloned());
        }
        self.ensure_refs(db, zone, &refs).map(drop)
    }

    /// The first record a note with the embeds `slots` needs (an Attachment
    /// record, or a file attachment's Media record) that iCloud answered an
    /// error for other than `NOT_FOUND`, with that error's code. Such a note
    /// can't be resolved this run: read as gone, its attachment would
    /// become a marker and the file it has here be deleted.
    pub fn lookup_failure<T: Transport>(
        &mut self,
        db: &Database<T>,
        zone: &NoteZone,
        slots: &[EmbedSlot],
    ) -> Result<Option<(String, String)>, Error> {
        let refs: Vec<AttachmentReference> = slots.iter().filter_map(attachment_of).cloned().collect();
        let names = self.ensure_refs(db, zone, &refs)?;
        let failed = self.zones.get(zone).map(|cache| &cache.failed);
        Ok(names
            .into_iter()
            .find_map(|name| failed?.get(&name).map(|code| (name.clone(), code.clone()))))
    }

    /// Looks up the Attachment records of `refs`, then the Media records of
    /// the file attachments among them; every name that was needed.
    fn ensure_refs<T: Transport>(
        &mut self,
        db: &Database<T>,
        zone: &NoteZone,
        refs: &[AttachmentReference],
    ) -> Result<Vec<String>, Error> {
        let mut names: Vec<String> = refs.iter().map(|r| r.attachment_identifier.clone()).collect();
        self.ensure(db, zone, &names)?;
        let file_refs: Vec<AttachmentReference> = refs.iter().filter(|r| !is_table_uti(&r.type_uti)).cloned().collect();
        let media_names: Vec<String> = extract_media_record_names(&file_refs, &self.records_named(zone, &names))
            .into_iter()
            .flatten()
            .collect();
        self.ensure(db, zone, &media_names)?;
        names.extend(media_names);
        Ok(names)
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
        let (records, failures) = db.lookup_records_and_failures(zone, &missing)?;
        // A later duplicate wins, as `.rev().find()` over the answer did.
        for record in records {
            cache.records.insert(record.record_name.clone(), record);
        }
        cache.failed.extend(failures);
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

/// The attachment an embed slot names, if it names one.
fn attachment_of(slot: &EmbedSlot) -> Option<&AttachmentReference> {
    match slot {
        EmbedSlot::Attachment(r) => Some(r),
        EmbedSlot::Unknown { .. } => None,
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
    let previous_table_for_note = remove_table_attachments_for_note(note_record_name, existing_table_attachments);

    if slots.is_empty() {
        return Ok(AttachmentSyncResult {
            body_text: body_text.to_owned(),
            stale_attachment_record_names: previous_for_note,
            stale_table_attachment_record_names: previous_table_for_note,
            ..Default::default()
        });
    }

    let names: Vec<String> = slots
        .iter()
        .filter_map(attachment_of)
        .map(|r| r.attachment_identifier.clone())
        .collect();
    records.ensure(db, zone, &names)?;
    let attachment_records = records.records_named(zone, &names);
    let by_name = |name: &str| attachment_records.iter().rev().find(|r| r.record_name == name);
    // Record names are unique within a zone only, and a shared zone's owner
    // picks theirs: a name another note's attachment or table is tracked
    // under stays a marker, so its file, entry and history are left alone.
    let tracked_for_another_note = |name: &str| {
        let other = |owner: &str| owner != note_record_name;
        existing_attachments
            .get(name)
            .is_some_and(|e| other(&e.note_record_name))
            || existing_table_attachments
                .get(name)
                .is_some_and(|e| other(&e.note_record_name))
    };

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
        if tracked_for_another_note(&r.attachment_identifier) {
            replacements[i] = Some(format_embed_marker(&marker_for(r)));
            continue;
        }
        if is_table_uti(&r.type_uti) {
            // A table's versions are kept under its record name
            // (`history/<recordName>/`): one that can't name a file stays a
            // marker, like a table that can't be decoded.
            let record = by_name(&r.attachment_identifier).filter(|_| is_plain_record_name(&r.attachment_identifier));
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

/// A file linked from a note that push adds to it as a new attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttachmentFile {
    /// Vault-root-relative, e.g. `Notes/attachments/plan.pdf`.
    pub file: String,
    /// From the note's directory: `attachments/plan.pdf`.
    pub link_path: String,
    pub name: String,
    pub type_uti: &'static str,
}

/// The file a new attachment link (`href`, as written in a note in
/// `note_dir`) names, or why it can't be added: it has to be a regular,
/// non-empty file of at most [`MAX_NEW_ATTACHMENT_BYTES`] directly in the
/// note folder's own `attachments/` directory. A `..`, a deeper path or a
/// symbolic link could point anywhere, so none is followed, and the
/// directory itself has to resolve to a place inside the vault (the app's
/// preview applies the same test).
pub fn resolve_new_attachment_file(
    target_dir: &Path,
    note_dir: &str,
    href: &str,
) -> Result<std::result::Result<NewAttachmentFile, String>, Error> {
    let refuse = |why: String| Ok(Err(why));
    let outside = || {
        format!(
            "the link \"{href}\" doesn't name a file directly in this folder's {ATTACHMENTS_DIR}/ directory - \
             only a file there can be added as an attachment"
        )
    };
    let Some(link_path) = decode_uri_component(href) else {
        return refuse(outside());
    };
    let name = match link_path.strip_prefix("attachments/") {
        Some(name) if name.len() > MAX_ATTACHMENT_NAME_BYTES => {
            return refuse(format!(
                "the linked file's name is longer than {MAX_ATTACHMENT_NAME_BYTES} bytes - give it a shorter one"
            ));
        }
        Some(name) if !name.is_empty() && plain_file_name(name) == name => name.to_owned(),
        _ => return refuse(outside()),
    };
    let directory = target_dir.join(note_dir).join(ATTACHMENTS_DIR);
    let path = directory.join(&name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return refuse(format!(
                "the linked file \"{link_path}\" doesn't exist - put the file in this folder's {ATTACHMENTS_DIR}/ \
                 directory, or remove the link"
            ));
        }
        Err(e) => return Err(e.into()),
    };
    if !metadata.file_type().is_file() {
        return refuse(format!(
            "the linked file \"{link_path}\" is not a regular file (a directory or a symbolic link can't be added \
             as an attachment)"
        ));
    }
    // Where the directory really is: inside the vault, and holding the file.
    let contained = match (
        std::fs::canonicalize(target_dir),
        std::fs::canonicalize(&directory),
        std::fs::canonicalize(&path),
    ) {
        (Ok(vault), Ok(directory), Ok(real)) => directory.starts_with(&vault) && real.parent() == Some(&directory),
        _ => false,
    };
    if !contained {
        return refuse(outside());
    }
    let size = metadata.len();
    if size == 0 {
        return refuse(format!("the linked file \"{link_path}\" is empty"));
    }
    if size > MAX_NEW_ATTACHMENT_BYTES {
        return refuse(format!(
            "the linked file \"{link_path}\" is larger than {} MB, the most this tool adds as an attachment",
            MAX_NEW_ATTACHMENT_BYTES / (1024 * 1024)
        ));
    }
    Ok(Ok(NewAttachmentFile {
        file: posix::join(&[note_dir, ATTACHMENTS_DIR, &name]),
        link_path,
        type_uti: uti_for_file_name(&name),
        name,
    }))
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
    unlink_if_there(path).map(drop)
}

/// Removes the file at `path`; whether there was one.
pub(crate) fn unlink_if_there(path: &Path) -> Result<bool, Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
