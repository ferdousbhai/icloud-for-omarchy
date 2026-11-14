//! Request field sets for note and folder writes. Originally derived from
//! icloud-md.
//!
//! Field order matters (it is the request JSON's key order, matched to
//! captured web-client requests), hence `UpdateFields` (an `IndexMap`), not
//! a `BTreeMap`.

use serde_json::{Value, json};

use crate::js::{base64_encode, from_utf16, is_whitespace16, utf16};

pub use crate::cloudkit::UpdateFieldValue;
use crate::cloudkit::{CloudKitRecord, UpdateFields};

pub const TRASH_FOLDER_RECORD_NAME: &str = "TrashFolder-CloudKit";
pub const DEFAULT_FOLDER_RECORD_NAME: &str = "DefaultFolder-CloudKit";

const TITLE_MAX_LENGTH: usize = 76;
const SNIPPET_MAX_LENGTH: usize = 500;
const EMPTY_SNIPPET_PLACEHOLDER: &str = "No additional text";

const ECHOED_FIELDS: [&str; 10] = [
    "MinimumSupportedNotesVersion",
    "Folders",
    "Deleted",
    "Folder",
    "CreationDate",
    "ReplicaIDToNotesVersionDataEncrypted",
    "FoldersModificationDate",
    "AttachmentViewType",
    "PaperStyleType",
    "ReplicaIDToUserIDEncrypted",
];
/// Echoed by an update (null when the note has none); emptied by a create,
/// a move and a trashing.
const NULL_FIELDS: [&str; 3] = [
    "FirstAttachmentThumbnail",
    "FirstAttachmentUTIEncrypted",
    "TextDataAsset",
];

fn title16(text: &[u16]) -> &[u16] {
    let line_end = text.iter().position(|&u| u == u16::from(b'\n')).unwrap_or(text.len());
    let first_line = &text[..line_end];
    if first_line.len() <= TITLE_MAX_LENGTH {
        return first_line;
    }
    // `lastIndexOf(" ", 76)`
    let word_boundary = (0..=TITLE_MAX_LENGTH).rev().find(|&i| first_line[i] == u16::from(b' '));
    match word_boundary {
        Some(boundary) if boundary > 0 => &first_line[..boundary],
        _ => &first_line[..TITLE_MAX_LENGTH],
    }
}

fn snippet16(text: &[u16]) -> Vec<u16> {
    let after_title = &text[title16(text).len()..];
    let skip = after_title.iter().take_while(|&&u| is_whitespace16(u)).count();
    let after_title = &after_title[skip..];
    let line_end = after_title
        .iter()
        .position(|&u| u == u16::from(b'\n'))
        .unwrap_or(after_title.len());
    let snippet = &after_title[..line_end.min(SNIPPET_MAX_LENGTH)];
    if snippet.is_empty() {
        utf16(EMPTY_SNIPPET_PLACEHOLDER)
    } else {
        snippet.to_vec()
    }
}

/// `deriveNoteTitle`.
pub fn derive_note_title(text: &str) -> String {
    from_utf16(title16(&utf16(text)))
}

/// `deriveNoteSnippet`.
pub fn derive_note_snippet(text: &str) -> String {
    from_utf16(&snippet16(&utf16(text)))
}

/// `Buffer.from(str, "utf-8").toString("base64")` for a UTF-16 slice.
fn utf8_base64(units: &[u16]) -> String {
    base64_encode(from_utf16(units).as_bytes())
}

/// `folderReference(folderRecordName, zoneOwnerRecordName?)`:
/// `{recordName, action: "VALIDATE", zoneID}`.
fn folder_reference(folder_record_name: &str, zone_owner_record_name: Option<&str>) -> Value {
    let zone_id = match zone_owner_record_name {
        Some(owner) => json!({"zoneName": "Notes", "ownerRecordName": owner}),
        None => json!({"zoneName": "Notes"}),
    };
    json!({"recordName": folder_record_name, "action": "VALIDATE", "zoneID": zone_id})
}

fn echo(fields: &mut UpdateFields, current: &CloudKitRecord, name: &str) {
    if let Some(field) = current.fields.get(name) {
        fields.insert(name.into(), UpdateFieldValue::new(field.value.clone()));
    }
}

fn build_note_relocation_fields(current: &CloudKitRecord, folder_record_name: &str, now_ms: i64) -> UpdateFields {
    let mut fields = UpdateFields::new();
    echo(&mut fields, current, "CreationDate");
    fields.insert("ModificationDate".into(), UpdateFieldValue::new(json!(now_ms)));
    echo(&mut fields, current, "TitleEncrypted");
    fields.insert(
        "Folders".into(),
        UpdateFieldValue::new(json!([folder_reference(folder_record_name, None)])),
    );
    fields.insert("FoldersModificationDate".into(), UpdateFieldValue::new(json!(now_ms)));
    fields.insert(
        "Folder".into(),
        UpdateFieldValue::new(folder_reference(folder_record_name, None)),
    );
    echo(&mut fields, current, "SnippetEncrypted");
    // A note whose text Apple keeps in a `TextDataAsset` has no text but
    // that: emptying the field leaves the record with none, which iCloud
    // refuses ("Note record must have either TextDataEncrypted or
    // TextDataAsset"). Its text is left alone: neither field is sent, the
    // inline one included, since a reader may have filled it from the asset.
    let text_as_asset = has_text_data_asset(current);
    for name in NULL_FIELDS {
        if text_as_asset && name == "TextDataAsset" {
            continue;
        }
        fields.insert(name.into(), UpdateFieldValue::EMPTY);
    }
    if !text_as_asset {
        echo(&mut fields, current, "TextDataEncrypted");
    }
    fields
}

/// Whether the note's text is stored as a `TextDataAsset` (a very large note).
pub fn has_text_data_asset(record: &CloudKitRecord) -> bool {
    record.fields.get("TextDataAsset").is_some_and(|f| !f.value.is_null())
}

/// `buildNoteTrashFields`.
pub fn build_note_trash_fields(current: &CloudKitRecord, now_ms: i64) -> UpdateFields {
    build_note_relocation_fields(current, TRASH_FOLDER_RECORD_NAME, now_ms)
}

/// `buildNoteMoveFields`.
pub fn build_note_move_fields(current: &CloudKitRecord, folder_record_name: &str, now_ms: i64) -> UpdateFields {
    build_note_relocation_fields(current, folder_record_name, now_ms)
}

/// `buildNoteCreateFields`. `folder_record_name` defaults to
/// `DefaultFolder-CloudKit` in TS.
pub fn build_note_create_fields(
    new_text_data_base64: &str,
    new_text: &str,
    now_ms: i64,
    folder_record_name: &str,
    zone_owner_record_name: Option<&str>,
) -> UpdateFields {
    let text = utf16(new_text);
    let folder = folder_reference(folder_record_name, zone_owner_record_name);
    let mut fields = UpdateFields::new();
    fields.insert("CreationDate".into(), UpdateFieldValue::new(json!(now_ms)));
    fields.insert("Folders".into(), UpdateFieldValue::new(json!([folder])));
    fields.insert("Folder".into(), UpdateFieldValue::new(folder));
    fields.insert("ModificationDate".into(), UpdateFieldValue::new(json!(now_ms)));
    fields.insert(
        "TitleEncrypted".into(),
        UpdateFieldValue::new(json!(utf8_base64(title16(&text)))),
    );
    fields.insert(
        "SnippetEncrypted".into(),
        UpdateFieldValue::new(json!(utf8_base64(&snippet16(&text)))),
    );
    for name in NULL_FIELDS {
        fields.insert(name.into(), UpdateFieldValue::EMPTY);
    }
    fields.insert(
        "TextDataEncrypted".into(),
        UpdateFieldValue::new(json!(new_text_data_base64)),
    );
    fields
}

/// `buildNoteUpdateFields`.
pub fn build_note_update_fields(
    current: &CloudKitRecord,
    new_text_data_base64: &str,
    new_text: &str,
    modification_date_ms: i64,
) -> UpdateFields {
    let text = utf16(new_text);
    let mut fields = UpdateFields::new();
    fields.insert(
        "ModificationDate".into(),
        UpdateFieldValue::new(json!(modification_date_ms)),
    );
    fields.insert(
        "TitleEncrypted".into(),
        UpdateFieldValue::new(json!(utf8_base64(title16(&text)))),
    );
    for name in ECHOED_FIELDS {
        echo(&mut fields, current, name);
    }
    fields.insert(
        "SnippetEncrypted".into(),
        UpdateFieldValue::new(json!(utf8_base64(&snippet16(&text)))),
    );
    for name in NULL_FIELDS {
        let value = current.fields.get(name).map(|f| f.value.clone()).unwrap_or(Value::Null);
        fields.insert(name.into(), UpdateFieldValue::new(value));
    }
    fields.insert(
        "TextDataEncrypted".into(),
        UpdateFieldValue::new(json!(new_text_data_base64)),
    );
    fields
}

/// A reference to a record of the user's own Notes zone, as the web client
/// writes one: `{recordName, action: "VALIDATE", zoneID}`.
fn own_zone_reference(record_name: &str) -> Value {
    folder_reference(record_name, None)
}

/// The fields of the `Media` record that holds a new attachment's file:
/// `asset` is what `Database::upload_asset` answered for its `Asset` field.
pub fn build_media_create_fields(asset: Value, file_name: &str) -> UpdateFields {
    let mut fields = UpdateFields::new();
    fields.insert("Asset".into(), UpdateFieldValue::new(asset));
    fields.insert(
        "FilenameEncrypted".into(),
        UpdateFieldValue::new(json!(base64_encode(file_name.as_bytes()))),
    );
    fields.insert("MinimumSupportedNotesVersion".into(), UpdateFieldValue::new(json!(0)));
    fields.insert("Deleted".into(), UpdateFieldValue::new(json!(0)));
    fields
}

/// What a new `Attachment` record says about its file.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAttachmentFields<'a> {
    pub type_uti: &'a str,
    /// The file's name: a file attachment's title.
    pub file_name: &'a str,
    pub media_record_name: &'a str,
    pub note_record_name: &'a str,
    pub file_size: u64,
    pub now_ms: i64,
    /// An image's pixel size (width, height), when its header gives it.
    pub dimensions: Option<(u32, u32)>,
}

/// The fields of a new file attachment's `Attachment` record, as one made
/// in Notes carries them (measured 2026-11-11), without what Apple's
/// devices add later (previews, summaries). The cropping quad is the whole
/// image; the doubles are sent as doubles.
pub fn build_attachment_create_fields(attachment: &NewAttachmentFields) -> UpdateFields {
    let mut fields = UpdateFields::new();
    let mut set = |name: &str, value: Value| {
        fields.insert(name.into(), UpdateFieldValue::new(value));
    };
    set("UTI", json!(attachment.type_uti));
    set("UTIEncrypted", json!(base64_encode(attachment.type_uti.as_bytes())));
    set("TitleEncrypted", json!(base64_encode(attachment.file_name.as_bytes())));
    set("Media", own_zone_reference(attachment.media_record_name));
    set("Note", own_zone_reference(attachment.note_record_name));
    set("FileSize", json!(attachment.file_size));
    set("CreationDate", json!(attachment.now_ms));
    set("LastModificationDate", json!(attachment.now_ms));
    set("MinimumSupportedNotesVersion", json!(0));
    set("Deleted", json!(0));
    set("Orientation", json!(0));
    set("ImageFilterType", json!(0));
    for (corner, x, y) in [
        ("TopLeft", 0.0, 1.0),
        ("TopRight", 1.0, 1.0),
        ("BottomRight", 1.0, 0.0),
        ("BottomLeft", 0.0, 0.0),
    ] {
        set(&format!("CroppingQuad{corner}X"), json!(x));
        set(&format!("CroppingQuad{corner}Y"), json!(y));
    }
    if let Some((width, height)) = attachment.dimensions {
        set("Width", json!(f64::from(width)));
        set("Height", json!(f64::from(height)));
    }
    fields
}

/// `FolderCreateFields`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderCreateFields {
    pub fields: UpdateFields,
    pub parent_record_name: Option<String>,
}

/// `buildFolderCreateFields`.
pub fn build_folder_create_fields(title: &str, parent_record_name: Option<&str>) -> FolderCreateFields {
    let mut fields = UpdateFields::new();
    fields.insert(
        "TitleEncrypted".into(),
        UpdateFieldValue::new(json!(base64_encode(title.as_bytes()))),
    );
    let Some(parent) = parent_record_name else {
        return FolderCreateFields {
            fields,
            parent_record_name: None,
        };
    };
    fields.insert(
        "ParentFolder".into(),
        UpdateFieldValue::new(folder_reference(parent, None)),
    );
    FolderCreateFields {
        fields,
        parent_record_name: Some(parent.into()),
    }
}
