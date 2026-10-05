//! Attachment files: downloading, placing and tracking them. Originally derived from icloud-md's tests.

use std::collections::HashSet;
use std::path::Path;

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
use icloud_notes_sync::doc::embeds::AttachmentReference;
use icloud_notes_sync::vault::attachments::{
    MatchedAttachment, decode_table_attachment, extract_media_record_names, match_attachment_records,
    remove_attachments_for_note, remove_table_attachments_for_note,
};
use icloud_notes_sync::vault::state::{AttachmentEntry, TableAttachmentEntry};
use indexmap::IndexMap;
use serde_json::{Value, json};

fn record(record_name: &str, record_type: &str, fields: &[(&str, Value, &str)]) -> CloudKitRecord {
    CloudKitRecord {
        record_name: record_name.into(),
        record_type: record_type.into(),
        fields: fields
            .iter()
            .map(|(k, v, t)| {
                (
                    k.to_string(),
                    FieldValue {
                        value: v.clone(),
                        type_: t.to_string(),
                    },
                )
            })
            .collect(),
        ..Default::default()
    }
}

fn audio_attachment_record() -> CloudKitRecord {
    record(
        "7DAFDA6F-4AC4-41D8-9958-049373B80824",
        "Attachment",
        &[
            ("UTI", json!("com.apple.m4a-audio"), "STRING"),
            (
                "Media",
                json!({ "recordName": "0B8509A3-A5FC-470B-A777-03BFFFDFB5F9", "action": "VALIDATE" }),
                "REFERENCE",
            ),
            (
                "MergeableDataAsset",
                json!({
                    "fileChecksum": "ARXoH1EL3hYxm2DAq9w4Nspo2ZYb",
                    "size": 225719,
                    "downloadURL": "https://cvws.icloud-content.com/B/audio-mergeable-data",
                }),
                "ASSETID",
            ),
        ],
    )
}

fn audio_media_record() -> CloudKitRecord {
    record(
        "0B8509A3-A5FC-470B-A777-03BFFFDFB5F9",
        "Media",
        &[
            (
                "Asset",
                json!({
                    "fileChecksum": "AUMraefNsgNffHQpfB8oQFo5P51-",
                    "size": 51432744,
                    "downloadURL": "https://cvws.icloud-content.com/B/audio-asset",
                }),
                "ASSETID",
            ),
            (
                "FilenameEncrypted",
                json!("Q2FsbCB3aXRoIEphbmljZSBFbGtpbnMubTRh"),
                "ENCRYPTED_BYTES",
            ),
        ],
    )
}

fn image_attachment_record() -> CloudKitRecord {
    record(
        "7ED80274-4400-4C02-87EA-F542F056FF02",
        "Attachment",
        &[
            ("UTI", json!("public.jpeg"), "STRING"),
            (
                "Media",
                json!({ "recordName": "066C8A2E-796F-403F-AD75-A5267CBD0E18", "action": "VALIDATE" }),
                "REFERENCE",
            ),
            ("MergeableDataAsset", Value::Null, "ASSETID"),
            ("Height", json!(3024), "INT64"),
            ("Width", json!(4032), "INT64"),
        ],
    )
}

fn image_media_record() -> CloudKitRecord {
    record(
        "066C8A2E-796F-403F-AD75-A5267CBD0E18",
        "Media",
        &[
            (
                "Asset",
                json!({
                    "fileChecksum": "ARKf/Vy+irL9d80LorfL6M0D7FG5",
                    "size": 2908682,
                    "downloadURL": "https://cvws.icloud-content.com/B/image-asset",
                }),
                "ASSETID",
            ),
            ("FilenameEncrypted", json!("XzcxMzAwOTMuanBlZw=="), "ENCRYPTED_BYTES"),
        ],
    )
}

const TABLE_B64: &str = "eJzt1E9oFFccwPGZyWZ39iWNr5OahofQdgxJjHZdB5tDxUOTqKTEoJtNemghxHW0u2x2ZXdFI3opiBcPihRLyUULObVNm4stNFQwSslhD+k/Dy0UNNAiEimoB7H27SbVbLKhh1JP32GH3/x+7/d5895jd23D+fq2ZRvSUF/etoQtgvrZbDZUX+ptO6BabcNx3Vej/3KpoG06VtTUsUbHGh1rdbR1DOr4gqoXQgRKM+vMUq+nNtumarMtZ6P7WncsPnIg7Xdn00dHMz3JnJ8oJLOZPv9QIZ6NJQ+/X1A/mx+Y35tiwhQnRJ8TXPj2G/1RsjxhaeHl2G6WK6auWKoc2y3VJGw99kRf63Tf0+cOyzZLt/OS3p7cOhuev3dhr3F5z0yxMXq+S1dNR16fbL6/M198PD98LRG5dHXKaReOsKKlbQXU02Mq10K6FnxWW9ZZW6Uz9Kym6lPCtvQhBRxLmhWZVZHVVGSB//tE7vRfnPbnHtxtGZ+9NTbUsLB4Ilc2dY7sOntj/5mBybbQw9S2pX0Kvafwin3W61rdqhMpdYoqnfVrnEhtRRasyEIVma06vMV31On5Gla8I6xrLy57xz+9pa/muhW9eraoXNa7O9bjWG9F/+PZWioS2+IYVWZZw1Ss0amyxsaVa+x67mt07eRBP1NIFsbcpkSu2o/YDeT99CE3mMjFssfybnhwsLenN3PQP+6GE7nF3rxbl/DT6aWk4+VEdjQycuRI2o90x3rikf6B/qOjB/xclYGBQi6ZOdyxftVA6S3L+zPZgp+PLP3NrB7o7V5jAIFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAIBAIBAKBQCAQCAQCgUAgEAgEAoFAVApPnmycaYlt/u6jYzt+/OlU17vvePLED198dfOT7WO/zQ21npuO/uHJ65PN93fmi4/nh68lIpeuTnnSP9V2OvDhGxOfnpt69PFfG+Ke3Dobnr93Ya9xec9MsTF6vsuTEyd794nxz4Y7fy/KP9XufZ68sqlzZNfZG/vPDEy2hR6mtnnyvdZXZFPdo19v/mLEPo8PbvTknf6L0/7cg7st47O3xoYaFt7cIJRYtUrHsi19m38DuMWS1w==";

fn table_attachment_record() -> CloudKitRecord {
    record(
        "92df3572-94a6-48f5-be27-075e91f80a2c",
        "Attachment",
        &[
            ("UTI", json!("com.apple.notes.table"), "STRING"),
            ("MergeableDataEncrypted", json!(TABLE_B64), "ENCRYPTED_BYTES"),
        ],
    )
}

fn entry(file: &str, note: &str) -> AttachmentEntry {
    AttachmentEntry {
        file: file.into(),
        media_record_name: "MEDIA".into(),
        media_file_checksum: "checksum".into(),
        note_record_name: note.into(),
    }
}

fn attachments(entries: &[(&str, AttachmentEntry)]) -> IndexMap<String, AttachmentEntry> {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn aref(id: &str, uti: &str) -> AttachmentReference {
    AttachmentReference {
        attachment_identifier: id.into(),
        type_uti: uti.into(),
    }
}

fn image_ref() -> Vec<AttachmentReference> {
    vec![aref("7ED80274-4400-4C02-87EA-F542F056FF02", "public.jpeg")]
}

const IMAGE_MEDIA: &str = "066C8A2E-796F-403F-AD75-A5267CBD0E18";

fn tracked_image(file: &str, checksum: &str) -> IndexMap<String, AttachmentEntry> {
    attachments(&[(
        "7ED80274-4400-4C02-87EA-F542F056FF02",
        AttachmentEntry {
            file: file.into(),
            media_record_name: IMAGE_MEDIA.into(),
            media_file_checksum: checksum.into(),
            note_record_name: "NOTE2".into(),
        },
    )])
}

fn match_image(
    existing: &IndexMap<String, AttachmentEntry>,
    used: &mut HashSet<String>,
    note_dir: &str,
) -> Vec<Option<MatchedAttachment>> {
    match_attachment_records(
        &image_ref(),
        &[Some(IMAGE_MEDIA.to_owned())],
        &[image_media_record()],
        "NOTE2",
        existing,
        used,
        note_dir,
    )
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn remove_attachments_for_note_deletes_only_that_notes_files() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    std::fs::create_dir_all(dir.join("attachments")).unwrap();
    std::fs::write(dir.join("attachments/keep-mine.jpg"), "a").unwrap();
    std::fs::write(dir.join("attachments/keep-other.jpg"), "b").unwrap();
    let all = attachments(&[
        ("ATT1", entry("attachments/keep-mine.jpg", "NOTE1")),
        ("ATT2", entry("attachments/keep-other.jpg", "NOTE2")),
    ]);
    let removed = remove_attachments_for_note(dir, "NOTE1", &all).unwrap();
    assert_eq!(removed, vec!["ATT1".to_owned()]);
    assert!(!exists(&dir.join("attachments/keep-mine.jpg")));
    assert!(exists(&dir.join("attachments/keep-other.jpg")));
}

#[test]
fn remove_attachments_for_note_tolerates_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let all = attachments(&[("ATT1", entry("attachments/missing.jpg", "NOTE1"))]);
    assert_eq!(
        remove_attachments_for_note(dir.path(), "NOTE1", &all).unwrap(),
        vec!["ATT1".to_owned()]
    );
}

#[test]
fn remove_attachments_for_note_empty_when_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        remove_attachments_for_note(dir.path(), "NOTE1", &IndexMap::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn remove_table_attachments_for_note_returns_only_that_note() {
    let tables: IndexMap<String, TableAttachmentEntry> = [("ATT-TABLE-1", "NOTE1"), ("ATT-TABLE-2", "NOTE2")]
        .iter()
        .map(|(k, n)| {
            (
                k.to_string(),
                TableAttachmentEntry {
                    note_record_name: n.to_string(),
                },
            )
        })
        .collect();
    assert_eq!(
        remove_table_attachments_for_note("NOTE1", &tables),
        vec!["ATT-TABLE-1".to_owned()]
    );
}

#[test]
fn remove_table_attachments_for_note_empty_when_none() {
    assert!(remove_table_attachments_for_note("NOTE1", &IndexMap::new()).is_empty());
}

#[test]
fn extract_media_record_names_audio() {
    let refs = vec![aref("7DAFDA6F-4AC4-41D8-9958-049373B80824", "com.apple.m4a-audio")];
    assert_eq!(
        extract_media_record_names(&refs, &[audio_attachment_record()]),
        vec![Some("0B8509A3-A5FC-470B-A777-03BFFFDFB5F9".to_owned())]
    );
}

#[test]
fn extract_media_record_names_image() {
    assert_eq!(
        extract_media_record_names(&image_ref(), &[image_attachment_record()]),
        vec![Some(IMAGE_MEDIA.to_owned())]
    );
}

#[test]
fn extract_media_record_names_unresolved_is_none() {
    let refs = vec![aref("MISSING", "public.jpeg")];
    assert_eq!(extract_media_record_names(&refs, &[]), vec![None]);
    let mut not_attachment = image_attachment_record();
    not_attachment.record_name = "MISSING".into();
    not_attachment.record_type = "Note".into();
    assert_eq!(extract_media_record_names(&refs, &[not_attachment]), vec![None]);
}

#[test]
fn match_attachment_records_new_audio() {
    let refs = vec![aref("7DAFDA6F-4AC4-41D8-9958-049373B80824", "com.apple.m4a-audio")];
    let mut used = HashSet::new();
    let matched = match_attachment_records(
        &refs,
        &[Some("0B8509A3-A5FC-470B-A777-03BFFFDFB5F9".to_owned())],
        &[audio_media_record()],
        "NOTE1",
        &IndexMap::new(),
        &mut used,
        "",
    );
    assert_eq!(
        matched,
        vec![Some(MatchedAttachment {
            record_name: "7DAFDA6F-4AC4-41D8-9958-049373B80824".into(),
            relative_file: "attachments/Call with Janice Elkins.m4a".into(),
            link_path: "attachments/Call with Janice Elkins.m4a".into(),
            needs_download: true,
            download_url: "https://cvws.icloud-content.com/B/audio-asset".into(),
            entry: AttachmentEntry {
                file: "attachments/Call with Janice Elkins.m4a".into(),
                media_record_name: "0B8509A3-A5FC-470B-A777-03BFFFDFB5F9".into(),
                media_file_checksum: "AUMraefNsgNffHQpfB8oQFo5P51-".into(),
                note_record_name: "NOTE1".into(),
            },
        })]
    );
    assert!(used.contains("Call with Janice Elkins.m4a"));
}

#[test]
fn match_attachment_records_new_image() {
    let matched = match_image(&IndexMap::new(), &mut HashSet::new(), "");
    let m = matched[0].as_ref().unwrap();
    assert_eq!(m.relative_file, "attachments/_7130093.jpeg");
    assert!(m.needs_download);
    assert_eq!(m.entry.media_file_checksum, "ARKf/Vy+irL9d80LorfL6M0D7FG5");
}

#[test]
fn match_attachment_records_skips_redownload_when_checksum_matches() {
    let existing = tracked_image("attachments/_7130093.jpeg", "ARKf/Vy+irL9d80LorfL6M0D7FG5");
    let matched = match_image(&existing, &mut HashSet::new(), "");
    let m = matched[0].as_ref().unwrap();
    assert!(!m.needs_download);
    assert_eq!(m.relative_file, "attachments/_7130093.jpeg");
}

#[test]
fn match_attachment_records_redownloads_when_checksum_changed() {
    let existing = tracked_image("attachments/_7130093.jpeg", "stale-checksum");
    let matched = match_image(&existing, &mut HashSet::new(), "");
    let m = matched[0].as_ref().unwrap();
    assert!(m.needs_download);
    assert_eq!(m.relative_file, "attachments/_7130093.jpeg");
}

#[test]
fn match_attachment_records_none_without_asset() {
    let refs = vec![aref("A", "public.jpeg")];
    let broken = record("M1", "Media", &[]);
    assert_eq!(
        match_attachment_records(
            &refs,
            &[Some("M1".to_owned())],
            &[broken],
            "NOTE1",
            &IndexMap::new(),
            &mut HashSet::new(),
            ""
        ),
        vec![None]
    );
}

#[test]
fn match_attachment_records_nested_note() {
    let matched = match_image(&IndexMap::new(), &mut HashSet::new(), "Recipes/Desserts");
    let m = matched[0].as_ref().unwrap();
    assert_eq!(m.relative_file, "Recipes/Desserts/attachments/_7130093.jpeg");
    assert_eq!(m.link_path, "attachments/_7130093.jpeg");
    assert_eq!(m.entry.file, "Recipes/Desserts/attachments/_7130093.jpeg");
}

#[test]
fn match_attachment_records_keeps_tracked_path() {
    let existing = tracked_image("Recipes/attachments/_7130093.jpeg", "ARKf/Vy+irL9d80LorfL6M0D7FG5");
    let matched = match_image(&existing, &mut HashSet::new(), "Recipes");
    let m = matched[0].as_ref().unwrap();
    assert_eq!(m.relative_file, "Recipes/attachments/_7130093.jpeg");
    assert_eq!(m.link_path, "attachments/_7130093.jpeg");
    assert!(!m.needs_download);
}

#[test]
fn match_attachment_records_disambiguates_collision() {
    let mut used: HashSet<String> = ["_7130093.jpeg".to_owned()].into_iter().collect();
    let matched = match_image(&IndexMap::new(), &mut used, "");
    assert_eq!(
        matched[0].as_ref().unwrap().relative_file,
        "attachments/_7130093 2.jpeg"
    );
}

#[test]
fn decode_table_attachment_renders_real_table() {
    assert_eq!(
        decode_table_attachment(Some(&table_attachment_record())).as_deref(),
        Some("| A0 | B0 |\n| - | - |\n| | |")
    );
}

#[test]
fn decode_table_attachment_none_when_missing() {
    assert_eq!(decode_table_attachment(None), None);
}

#[test]
fn decode_table_attachment_none_for_non_attachment() {
    let mut r = table_attachment_record();
    r.record_type = "Note".into();
    assert_eq!(decode_table_attachment(Some(&r)), None);
}

#[test]
fn decode_table_attachment_none_without_mergeable_data() {
    assert_eq!(decode_table_attachment(Some(&record("R1", "Attachment", &[]))), None);
}

#[test]
fn decode_table_attachment_none_for_malformed_data() {
    let broken = record(
        "R1",
        "Attachment",
        &[(
            "MergeableDataEncrypted",
            json!(icloud_notes_sync::js::base64_encode(b"not a table")),
            "ENCRYPTED_BYTES",
        )],
    );
    assert_eq!(decode_table_attachment(Some(&broken)), None);
}
