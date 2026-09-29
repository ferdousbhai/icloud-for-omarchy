//! Ports icloud-md `encodeNoteRecord.test.ts` and `encodeFolderRecord.test.ts`,
//! plus exact request-JSON goldens (key order included) captured from
//! icloud-md itself.

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue, UpdateFields};
use icloud_notes_sync::doc::encode::{
    build_folder_create_fields, build_note_create_fields, build_note_move_fields, build_note_purge_fields,
    build_note_trash_fields, build_note_update_fields, derive_note_snippet, derive_note_title,
};
use icloud_notes_sync::doc::js::{base64_decode, utf16_len};
use serde_json::{Value, json};

const UPDATE: &str = r#"{"ModificationDate":{"value":222},"TitleEncrypted":{"value":"VGl0bGUgbGluZSDwn5iA"},"MinimumSupportedNotesVersion":{"value":0},"Folders":{"value":[{"recordName":"DefaultFolder-CloudKit"}]},"Deleted":{"value":0},"Folder":{"value":{"recordName":"DefaultFolder-CloudKit"}},"CreationDate":{"value":100},"PaperStyleType":{"value":1},"SnippetEncrypted":{"value":"Qm9keSBsaW5l"},"FirstAttachmentThumbnail":{"value":null},"FirstAttachmentUTIEncrypted":{"value":null},"TextDataAsset":{"value":null},"TextDataEncrypted":{"value":"TkVXX0RPQw=="}}"#;
const TRASH: &str = r#"{"CreationDate":{"value":100},"ModificationDate":{"value":999},"TitleEncrypted":{"value":"b2xk"},"Folders":{"value":[{"recordName":"TrashFolder-CloudKit","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}]},"FoldersModificationDate":{"value":999},"Folder":{"value":{"recordName":"TrashFolder-CloudKit","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}},"SnippetEncrypted":{"value":"c25pcHBldA=="},"FirstAttachmentThumbnail":{},"FirstAttachmentUTIEncrypted":{},"TextDataAsset":{},"TextDataEncrypted":{"value":"aWdub3JlZA=="}}"#;
const PURGE: &str = r#"{"CreationDate":{"value":100},"ModificationDate":{"value":999},"TitleEncrypted":{"value":"b2xk"},"Folders":{"value":[{"recordName":"TrashFolder-CloudKit","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}]},"FoldersModificationDate":{"value":999},"Folder":{"value":{"recordName":"TrashFolder-CloudKit","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}},"SnippetEncrypted":{"value":"c25pcHBldA=="},"Deleted":{"value":1},"FirstAttachmentThumbnail":{},"FirstAttachmentUTIEncrypted":{},"TextDataAsset":{},"TextDataEncrypted":{"value":"aWdub3JlZA=="}}"#;
const MOVE: &str = r#"{"CreationDate":{"value":100},"ModificationDate":{"value":999},"TitleEncrypted":{"value":"b2xk"},"Folders":{"value":[{"recordName":"F1","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}]},"FoldersModificationDate":{"value":999},"Folder":{"value":{"recordName":"F1","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}},"SnippetEncrypted":{"value":"c25pcHBldA=="},"FirstAttachmentThumbnail":{},"FirstAttachmentUTIEncrypted":{},"TextDataAsset":{},"TextDataEncrypted":{"value":"aWdub3JlZA=="}}"#;
const CREATE_SHARED: &str = r#"{"CreationDate":{"value":555},"Folders":{"value":[{"recordName":"F-SHARED","action":"VALIDATE","zoneID":{"zoneName":"Notes","ownerRecordName":"_owner1"}}]},"Folder":{"value":{"recordName":"F-SHARED","action":"VALIDATE","zoneID":{"zoneName":"Notes","ownerRecordName":"_owner1"}}},"ModificationDate":{"value":555},"TitleEncrypted":{"value":"U2Nod2FydHogd3JvdGUgdXAgaGlzIGV4cGVyaW1lbnQgc3VwZXJ2aXNpbmcgQ2xhdWRlIHRocm91Z2ggYSByZWFs"},"SnippetEncrypted":{"value":"dGhlb3JldGljYWwgcGh5c2ljcyBjYWxjdWxhdGlvbiwgcHJvZHVjaW5nIGEgcGFwZXIg8J+YgPCfmIA="},"FirstAttachmentThumbnail":{},"FirstAttachmentUTIEncrypted":{},"TextDataAsset":{},"TextDataEncrypted":{"value":"RE9D"}}"#;
const CREATE_SPLIT_SURROGATE: &str = r#"{"CreationDate":{"value":555},"Folders":{"value":[{"recordName":"DefaultFolder-CloudKit","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}]},"Folder":{"value":{"recordName":"DefaultFolder-CloudKit","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}},"ModificationDate":{"value":555},"TitleEncrypted":{"value":"eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh477+9"},"SnippetEncrypted":{"value":"77+9dGFpbA=="},"FirstAttachmentThumbnail":{},"FirstAttachmentUTIEncrypted":{},"TextDataAsset":{},"TextDataEncrypted":{"value":"RE9D"}}"#;
const FOLDER: &str = r#"{"fields":{"TitleEncrypted":{"value":"UmljZXR0ZSDigJMgRG9sY2kg8J+NsA=="},"ParentFolder":{"value":{"recordName":"parent-record","action":"VALIDATE","zoneID":{"zoneName":"Notes"}}}},"parentRecordName":"parent-record"}"#;

fn make_record(fields: &[(&str, Value, &str)]) -> CloudKitRecord {
    CloudKitRecord {
        record_name: "R1".into(),
        record_type: "Note".into(),
        record_change_tag: Some("1a".into()),
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

fn text_of(fields: &UpdateFields, name: &str) -> String {
    String::from_utf8(base64_decode(fields[name].value.as_ref().unwrap().as_str().unwrap())).unwrap()
}

fn json_of(fields: &UpdateFields) -> String {
    serde_json::to_string(fields).unwrap()
}

#[test]
fn title_is_the_first_line_for_short_notes() {
    assert_eq!(derive_note_title("Grocery list\nEggs\nMilk"), "Grocery list");
    assert_eq!(derive_note_title("Single line, no newline"), "Single line, no newline");
}

#[test]
fn long_first_lines_are_cut_at_a_word_boundary() {
    let first = "Schwartz wrote up his experiment supervising Claude through a real theoretical physics calculation, and this line keeps going well past any plausible title limit for quite a while longer";
    assert_eq!(
        derive_note_title(&format!("{first}\nBody")),
        "Schwartz wrote up his experiment supervising Claude through a real"
    );
}

#[test]
fn a_long_first_word_is_hard_cut_rather_than_dropped() {
    assert_eq!(utf16_len(&derive_note_title(&"x".repeat(100))), 76);
}

#[test]
fn snippet_continues_where_a_truncated_title_left_off() {
    let first = "Schwartz wrote up his experiment supervising Claude through a real theoretical physics calculation, producing a paper";
    assert_eq!(
        derive_note_snippet(&format!("{first}\nSecond line")),
        "theoretical physics calculation, producing a paper"
    );
}

#[test]
fn snippet_is_the_first_non_empty_line_after_a_short_title() {
    assert_eq!(derive_note_snippet("Grocery list\nEggs\nMilk"), "Eggs");
    assert_eq!(derive_note_snippet("Grocery list\n\n\nEggs"), "Eggs");
}

#[test]
fn snippet_falls_back_to_the_captured_placeholder_when_there_is_no_body() {
    assert_eq!(derive_note_snippet("Just a title"), "No additional text");
    assert_eq!(derive_note_snippet("Just a title\n\n"), "No additional text");
}

#[test]
fn build_note_update_fields_overrides_content_fields_and_echoes_the_rest() {
    let record = make_record(&[
        ("TitleEncrypted", json!("b2xk"), "ENCRYPTED_BYTES"),
        ("ModificationDate", json!(111), "TIMESTAMP"),
        ("CreationDate", json!(100), "TIMESTAMP"),
        ("Deleted", json!(0), "INT64"),
        ("Folder", json!({"recordName": "DefaultFolder-CloudKit"}), "REFERENCE"),
        ("MinimumSupportedNotesVersion", json!(0), "INT64"),
        ("TextDataEncrypted", json!("aWdub3JlZA=="), "ENCRYPTED_BYTES"),
    ]);
    let fields = build_note_update_fields(&record, "TkVXX0RPQw==", "Title line\nBody line", 222);
    assert_eq!(fields["TextDataEncrypted"].value, Some(json!("TkVXX0RPQw==")));
    assert_eq!(fields["ModificationDate"].value, Some(json!(222)));
    assert_eq!(text_of(&fields, "TitleEncrypted"), "Title line");
    assert_eq!(text_of(&fields, "SnippetEncrypted"), "Body line");
    assert_eq!(fields["CreationDate"].value, Some(json!(100)));
    assert_eq!(
        fields["Folder"].value,
        Some(json!({"recordName": "DefaultFolder-CloudKit"}))
    );
    for name in [
        "TextDataAsset",
        "FirstAttachmentThumbnail",
        "FirstAttachmentUTIEncrypted",
    ] {
        assert_eq!(serde_json::to_string(&fields[name]).unwrap(), r#"{"value":null}"#);
    }
    assert!(!fields.contains_key("PaperStyleType"));
    assert!(!json_of(&fields).contains("\"type\""));
}

#[test]
fn build_note_trash_fields_repoints_the_folder_references_at_trash() {
    let record = make_record(&[
        ("TitleEncrypted", json!("dGl0bGU="), "ENCRYPTED_BYTES"),
        ("SnippetEncrypted", json!("c25pcHBldA=="), "ENCRYPTED_BYTES"),
        ("CreationDate", json!(100), "TIMESTAMP"),
        ("ModificationDate", json!(111), "TIMESTAMP"),
        ("Folder", json!({"recordName": "DefaultFolder-CloudKit"}), "REFERENCE"),
        (
            "Folders",
            json!([{"recordName": "DefaultFolder-CloudKit"}]),
            "REFERENCE_LIST",
        ),
        ("TextDataEncrypted", json!("RE9D"), "ENCRYPTED_BYTES"),
    ]);
    let fields = build_note_trash_fields(&record, 999);
    let trash = json!({"recordName": "TrashFolder-CloudKit", "action": "VALIDATE", "zoneID": {"zoneName": "Notes"}});
    assert_eq!(fields["Folder"].value, Some(trash.clone()));
    assert_eq!(fields["Folders"].value, Some(json!([trash])));
    assert_eq!(fields["ModificationDate"].value, Some(json!(999)));
    assert_eq!(fields["FoldersModificationDate"].value, Some(json!(999)));
    assert_eq!(fields["TitleEncrypted"].value, Some(json!("dGl0bGU=")));
    assert_eq!(fields["SnippetEncrypted"].value, Some(json!("c25pcHBldA==")));
    assert_eq!(fields["TextDataEncrypted"].value, Some(json!("RE9D")));
    assert_eq!(fields["CreationDate"].value, Some(json!(100)));
    assert!(!fields.contains_key("Deleted"));
    for name in [
        "FirstAttachmentThumbnail",
        "FirstAttachmentUTIEncrypted",
        "TextDataAsset",
    ] {
        assert_eq!(serde_json::to_string(&fields[name]).unwrap(), "{}");
    }
}

#[test]
fn build_note_purge_fields_additionally_sets_deleted() {
    let record = make_record(&[
        ("CreationDate", json!(100), "TIMESTAMP"),
        ("TextDataEncrypted", json!("RE9D"), "ENCRYPTED_BYTES"),
    ]);
    let fields = build_note_purge_fields(&record, 999);
    assert_eq!(serde_json::to_string(&fields["Deleted"]).unwrap(), r#"{"value":1}"#);
    assert!(fields.contains_key("Folder"));
    assert_eq!(fields["TextDataEncrypted"].value, Some(json!("RE9D")));
}

#[test]
fn deletion_field_builders_tolerate_a_broken_record() {
    let fields = build_note_purge_fields(&make_record(&[]), 999);
    for name in [
        "TitleEncrypted",
        "SnippetEncrypted",
        "TextDataEncrypted",
        "CreationDate",
    ] {
        assert!(!fields.contains_key(name), "{name}");
    }
    assert_eq!(fields["Deleted"].value, Some(json!(1)));
    assert_eq!(fields["ModificationDate"].value, Some(json!(999)));
}

#[test]
fn build_note_create_fields_matches_the_captured_first_save_request_shape() {
    let fields = build_note_create_fields("RE9D", "Title line\nBody line", 555, "DefaultFolder-CloudKit", None);
    assert_eq!(fields["CreationDate"].value, Some(json!(555)));
    assert_eq!(fields["ModificationDate"].value, Some(json!(555)));
    let default =
        json!({"recordName": "DefaultFolder-CloudKit", "action": "VALIDATE", "zoneID": {"zoneName": "Notes"}});
    assert_eq!(fields["Folder"].value, Some(default.clone()));
    assert_eq!(fields["Folders"].value, Some(json!([default])));
    assert_eq!(text_of(&fields, "TitleEncrypted"), "Title line");
    assert_eq!(text_of(&fields, "SnippetEncrypted"), "Body line");
    assert_eq!(fields["TextDataEncrypted"].value, Some(json!("RE9D")));
    for name in [
        "FirstAttachmentThumbnail",
        "FirstAttachmentUTIEncrypted",
        "TextDataAsset",
    ] {
        assert_eq!(serde_json::to_string(&fields[name]).unwrap(), "{}");
    }
    assert!(!fields.contains_key("ReplicaIDToNotesVersionDataEncrypted"));
    assert!(!fields.contains_key("FoldersModificationDate"));
}

#[test]
fn build_note_create_fields_qualifies_shared_folder_references_with_the_zone_owner() {
    let fields = build_note_create_fields("RE9D", "Hi", 555, "F-SHARED", Some("_owner1"));
    let shared = json!({"recordName": "F-SHARED", "action": "VALIDATE", "zoneID": {"zoneName": "Notes", "ownerRecordName": "_owner1"}});
    assert_eq!(fields["Folder"].value, Some(shared.clone()));
    assert_eq!(fields["Folders"].value, Some(json!([shared])));
}

#[test]
fn a_top_level_folder_carries_exactly_one_field() {
    let created = build_folder_create_fields("Recipes", None);
    assert_eq!(created.fields.keys().collect::<Vec<_>>(), vec!["TitleEncrypted"]);
    assert_eq!(created.parent_record_name, None);
    assert_eq!(text_of(&created.fields, "TitleEncrypted"), "Recipes");
}

#[test]
fn a_folder_titles_non_ascii_characters_survive_the_base64_round_trip() {
    let title = "Ricette – Dolci 🍰";
    assert_eq!(
        text_of(&build_folder_create_fields(title, None).fields, "TitleEncrypted"),
        title
    );
}

#[test]
fn a_nested_folder_adds_parent_folder_and_the_matching_record_level_parent() {
    let created = build_folder_create_fields("Desserts", Some("parent-record"));
    let mut keys: Vec<_> = created.fields.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, vec!["ParentFolder", "TitleEncrypted"]);
    assert_eq!(created.parent_record_name.as_deref(), Some("parent-record"));
    assert_eq!(
        created.fields["ParentFolder"].value,
        Some(json!({"recordName": "parent-record", "action": "VALIDATE", "zoneID": {"zoneName": "Notes"}}))
    );
}

// --- byte-exact request JSON, captured from icloud-md --------------------------

fn full_record() -> CloudKitRecord {
    make_record(&[
        ("TitleEncrypted", json!("b2xk"), "ENCRYPTED_BYTES"),
        ("ModificationDate", json!(111), "TIMESTAMP"),
        ("CreationDate", json!(100), "TIMESTAMP"),
        ("Deleted", json!(0), "INT64"),
        ("Folder", json!({"recordName": "DefaultFolder-CloudKit"}), "REFERENCE"),
        ("MinimumSupportedNotesVersion", json!(0), "INT64"),
        ("TextDataEncrypted", json!("aWdub3JlZA=="), "ENCRYPTED_BYTES"),
        ("SnippetEncrypted", json!("c25pcHBldA=="), "ENCRYPTED_BYTES"),
        ("TextDataAsset", Value::Null, "ASSETID"),
        ("PaperStyleType", json!(1), "INT64"),
        (
            "Folders",
            json!([{"recordName": "DefaultFolder-CloudKit"}]),
            "REFERENCE_LIST",
        ),
    ])
}

#[test]
fn field_sets_serialize_exactly_like_icloud_md() {
    let record = full_record();
    assert_eq!(
        json_of(&build_note_update_fields(
            &record,
            "TkVXX0RPQw==",
            "Title line \u{1f600}\n\u{a0} Body line",
            222
        )),
        UPDATE
    );
    assert_eq!(json_of(&build_note_trash_fields(&record, 999)), TRASH);
    assert_eq!(json_of(&build_note_purge_fields(&record, 999)), PURGE);
    assert_eq!(json_of(&build_note_move_fields(&record, "F1", 999)), MOVE);
    let long = "Schwartz wrote up his experiment supervising Claude through a real theoretical physics calculation, producing a paper \u{1f600}\u{1f600}";
    assert_eq!(
        json_of(&build_note_create_fields(
            "RE9D",
            long,
            555,
            "F-SHARED",
            Some("_owner1")
        )),
        CREATE_SHARED
    );
    // A title cut through a surrogate pair: both halves become U+FFFD bytes,
    // as Buffer.from(str, "utf-8") writes them.
    let split = format!("{}\u{1f600}tail", "x".repeat(75));
    assert_eq!(
        json_of(&build_note_create_fields(
            "RE9D",
            &split,
            555,
            "DefaultFolder-CloudKit",
            None
        )),
        CREATE_SPLIT_SURROGATE
    );
    let folder = build_folder_create_fields("Ricette – Dolci 🍰", Some("parent-record"));
    let as_json = json!({"fields": folder.fields, "parentRecordName": folder.parent_record_name});
    assert_eq!(serde_json::to_string(&as_json).unwrap(), FOLDER);
}
