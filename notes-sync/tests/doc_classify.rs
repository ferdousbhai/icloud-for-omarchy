//! Ports icloud-md `src/notes/decodeNoteRecord.test.ts`.

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
use icloud_notes_sync::doc::decode::{
    ClassifyOptions, DecodedNote, NoteDecodeResult, UnsyncableReason, classify_note_record,
};
use icloud_notes_sync::doc::embeds::{AttachmentReference, EmbedSlot, UNKNOWN_CONTENT_BANNER};
use icloud_notes_sync::doc::format::ParagraphKind;
use icloud_notes_sync::doc::proto::topotext::{self, AttachmentInfo, AttributeRun, ParagraphStyle};
use icloud_notes_sync::doc::proto::{Message, versioned_document};
use icloud_notes_sync::doc::text::compress_note_document;
use icloud_notes_sync::js::{base64_encode, len16, slice16};
use icloud_notes_sync::vault::state::TitleMode;
use serde_json::{Value, json};

const AUDIO_ATTACHMENT_TEXT_DATA: &str = "H4sIAAAAAAAAE+NgEPrMyMEgwCD1hlFI2jkxJ0ehPLMkQ8ErMS8zOVXBNSc7M6+Y6/3+PVICXCwgdUCVYFqDESzCCBSRlALTGkxSYlwcQLn/QMAPVAdnK8lwSXEJJPhc2rlRw0G3ods/UnqB/1chJg5JIGbUkuOQEBLhYPASmH4r88mnYucbq8RWvT250oY/Y8XpVSfYtII5GIWEvAR2M+dLZK9xldxU0u6seOnrhiRrLhVzF0c3F0czN10TR2cTXRNDFwtdS0tTC10DE0tjc2MnCwMLIxMh4eT8XL3EgoKcVL1ck0TdxNKUzHwAzdJGOPgAAAA=";
const IMAGE_ATTACHMENT_TEXT_DATA: &str = "H4sIAAAAAAAAE23STWgTQRQH8GzSNJupNZNNmrabCENRWQIbQkybopd+2IBFDEoR6sWYZGvSxmzY7LZdP2oRoQehogeh4qGCFoWCH6AglHoRpQoitRRPPYgnlfZQetKiL9tn8NBllxl+M++/j2F4m/DDxduoTfzmEswBpaKzE6qukIF8ocLgPcf0qpXA2JCqMaOisLGCnmfpQraoGjm5ulKRK2Ypm2a6ykYVrTBkMj2vME01SjlZ1wplNqZqIxWWVTVNyepFM0JIj6KUWUZVy4RsvF4gIiV11S6gD2uUOEs4kA7RGiU7SoJyKA6UI7QBpU4ULEnS0I4wTnKiKdRZs3qr0gHpQdEaJRdmDdeyeKzLULslbqhzo+X+9QBG0DpxHw/WgHb4v3170Dpq/TdKHuuvdVSnBMxO3dJeS5zQGYcSQtFrQq0kJzVQgpDuRRtFE8EEtDG0VjAf2jhaC5gfzURrBmtCu4gWAAugXUJrAmtGu4zmB2tBu4LmA2tFm0ATwES0q2hesKDoJU6YT3LWcdppCA4gQHi4FH/g8cAFqc3bpjkiEnrgVOPgy+171+0Hj009plvdgp034CMkSGj6+PKrZ1KXPHkjNRicTW0JDv4aB4v7qoV3cm2f1+bNpffvfv1cWHSs1gph7e6o2+g64/N8XR3vebv0fA58Aj6uGvqkeGv8xf1+89PM90cfNn+vQOhkNdQdbuGJ4Odt/ZStrc/NzKYuTD/c2P4YcLvCAX5EEPrp081e282VVOT22fUHoanTzvBJntvNM4fI/kTf0c5oLBGX4/FoVI73RmNyZ6KvW062x2PJaHtHMhmNCQ1lI1MsZCPDZeV8/s38l+X6cGj3yJ3Vvzm/RsfwAwAA";

fn make_record(fields: Vec<(&str, Value, &str)>) -> CloudKitRecord {
    CloudKitRecord {
        record_name: "R1".into(),
        record_type: "Note".into(),
        record_change_tag: Some("1a".into()),
        fields: fields
            .into_iter()
            .map(|(k, value, t)| (k.to_string(), FieldValue { value, type_: t.into() }))
            .collect(),
        ..Default::default()
    }
}

fn encode_text_field(text: &str, runs: Vec<AttributeRun>) -> (&'static str, Value, &'static str) {
    let s = topotext::String {
        string: Some(text.into()),
        attribute_run: runs,
        ..Default::default()
    };
    let wrapper = versioned_document::Document {
        version: vec![versioned_document::Version {
            minimum_supported_version: Some(0),
            data: Some(s.encode().unwrap()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let compressed = compress_note_document(&wrapper.encode().unwrap());
    (
        "TextDataEncrypted",
        json!(base64_encode(&compressed)),
        "ENCRYPTED_BYTES",
    )
}

fn styled(length: usize, style: u32) -> AttributeRun {
    AttributeRun {
        length: Some(length as u32),
        paragraph_style: Some(ParagraphStyle {
            style: Some(style),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn info(length: u32, id: Option<&str>, uti: &str) -> AttributeRun {
    AttributeRun {
        length: Some(length),
        attachment_info: Some(AttachmentInfo {
            attachment_identifier: id.map(Into::into),
            type_uti: Some(uti.into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn classify(record: &CloudKitRecord, title_mode: TitleMode) -> NoteDecodeResult {
    classify_note_record(record, &ClassifyOptions { title_mode })
}

fn ok(result: NoteDecodeResult) -> DecodedNote {
    match result {
        NoteDecodeResult::Ok(decoded) => *decoded,
        other => panic!("expected ok, got {other:?}"),
    }
}

fn reference(id: &str, uti: &str) -> AttachmentReference {
    AttachmentReference {
        attachment_identifier: id.into(),
        type_uti: uti.into(),
    }
}

// --- classifications that never reach the Markdown gate -------------------

#[test]
fn an_attachment_info_run_not_on_a_lone_placeholder_banner_marks_the_note() {
    let record = make_record(vec![encode_text_field(
        "Hi\n\u{fffc}",
        vec![AttributeRun::with_length(2), info(2, Some("A-1"), "public.jpeg")],
    )]);
    let banner_text = format!("{UNKNOWN_CONTENT_BANNER}Hi\n\u{fffc}");
    assert_eq!(
        classify(&record, TitleMode::InBody),
        NoteDecodeResult::Ok(Box::new(DecodedNote {
            title: String::new(),
            title_line: "Hi".into(),
            body_text: banner_text.clone(),
            markdown_text: banner_text,
            format: None,
            title_stripped: false,
            embed_slots: vec![],
            attachments: vec![],
            publishable: false,
            unpublishable_reason: Some(
                "contains unrecognized embedded content this tool couldn't parse or place precisely".into()
            ),
        }))
    );
}

#[test]
fn a_note_missing_text_data_encrypted_is_unsyncable_with_the_missing_body_reason() {
    assert_eq!(
        classify(&make_record(vec![]), TitleMode::InBody),
        NoteDecodeResult::Unsyncable(UnsyncableReason::MissingBody)
    );
    // A non-string value is a missing body too.
    let record = make_record(vec![("TextDataEncrypted", Value::Null, "ENCRYPTED_BYTES")]);
    assert_eq!(
        classify(&record, TitleMode::InBody),
        NoteDecodeResult::Unsyncable(UnsyncableReason::MissingBody)
    );
    assert_eq!(UnsyncableReason::MissingBody.as_str(), "missing-body");
}

#[test]
fn a_note_whose_body_bytes_are_present_but_unparseable_is_undecodable() {
    let record = make_record(vec![(
        "TextDataEncrypted",
        json!(base64_encode(b"not a gzipped note document")),
        "ENCRYPTED_BYTES",
    )]);
    assert_eq!(
        classify(&record, TitleMode::InBody),
        NoteDecodeResult::Unsyncable(UnsyncableReason::Undecodable)
    );
    assert_eq!(UnsyncableReason::Undecodable.as_str(), "undecodable");
}

#[test]
fn a_note_explicitly_marked_deleted_is_deleted() {
    let record = make_record(vec![("Deleted", json!(1), "INT64")]);
    assert_eq!(classify(&record, TitleMode::InBody), NoteDecodeResult::Deleted);
    // Deleted: 0 is not.
    let live = make_record(vec![("Deleted", json!(0), "INT64")]);
    assert_eq!(
        classify(&live, TitleMode::InBody),
        NoteDecodeResult::Unsyncable(UnsyncableReason::MissingBody)
    );
}

#[test]
fn a_note_in_the_trash_folder_is_treated_as_deleted() {
    let record = make_record(vec![(
        "Folder",
        json!({"recordName": "TrashFolder-CloudKit"}),
        "REFERENCE",
    )]);
    assert_eq!(classify(&record, TitleMode::InBody), NoteDecodeResult::Deleted);
    let mut tombstone = make_record(vec![]);
    tombstone.deleted = Some(true);
    assert_eq!(classify(&tombstone, TitleMode::InBody), NoteDecodeResult::Deleted);
}

// --- cases through the Markdown round-trip gate ------------------------------

#[test]
fn a_plain_text_note_decodes_as_ok_with_no_attachments() {
    let record = make_record(vec![encode_text_field("Grocery list\nEggs\nMilk", vec![])]);
    let result = ok(classify(&record, TitleMode::InBody));
    assert_eq!(result.body_text, "Grocery list\nEggs\nMilk");
    assert_eq!(result.markdown_text, "Grocery list\nEggs\nMilk");
    assert!(result.embed_slots.is_empty());
    assert!(result.attachments.is_empty());
    assert!(result.publishable);
    assert_eq!(
        result.format.unwrap().iter().map(|p| p.kind).collect::<Vec<_>>(),
        vec![ParagraphKind::Body; 3]
    );
}

#[test]
fn a_real_audio_attachment_note_decodes_as_ok_surfacing_the_attachment_reference() {
    let record = make_record(vec![(
        "TextDataEncrypted",
        json!(AUDIO_ATTACHMENT_TEXT_DATA),
        "ENCRYPTED_BYTES",
    )]);
    let result = ok(classify(&record, TitleMode::InBody));
    assert!(result.body_text.contains("Call with Janice Elkins"));
    assert_eq!(
        result.attachments,
        vec![reference("7DAFDA6F-4AC4-41D8-9958-049373B80824", "com.apple.m4a-audio")]
    );
    assert!(result.publishable);
}

#[test]
fn a_real_image_attachment_note_decodes_as_ok_surfacing_the_attachment_reference() {
    let record = make_record(vec![
        (
            "TextDataEncrypted",
            json!(IMAGE_ATTACHMENT_TEXT_DATA),
            "ENCRYPTED_BYTES",
        ),
        ("FirstAttachmentThumbnail", json!({"fileChecksum": "x"}), "ASSETID"),
    ]);
    let result = ok(classify(&record, TitleMode::InBody));
    assert_eq!(
        result.attachments,
        vec![reference("7ED80274-4400-4C02-87EA-F542F056FF02", "public.jpeg")]
    );
    assert!(result.publishable);
}

#[test]
fn a_placeholder_with_no_matching_attachment_run_becomes_an_unknown_slot() {
    let record = make_record(vec![encode_text_field("Some note\n\u{fffc}", vec![])]);
    let result = ok(classify(&record, TitleMode::InBody));
    assert_eq!(result.body_text, "Some note\n\u{fffc}");
    assert_eq!(result.markdown_text, "Some note\n\u{fffc}");
    assert_eq!(result.embed_slots, vec![EmbedSlot::Unknown { type_uti: None }]);
    assert!(result.attachments.is_empty());
    assert!(result.publishable);
}

#[test]
fn a_partially_identified_attachment_run_yields_an_unknown_slot_carrying_its_uti() {
    let record = make_record(vec![encode_text_field(
        "Title\n\u{fffc}",
        vec![AttributeRun::with_length(6), info(1, None, "com.apple.drawing.2")],
    )]);
    let result = ok(classify(&record, TitleMode::InBody));
    assert_eq!(
        result.embed_slots,
        vec![EmbedSlot::Unknown {
            type_uti: Some("com.apple.drawing.2".into())
        }]
    );
    assert!(result.attachments.is_empty());
    assert!(result.publishable);
}

#[test]
fn a_note_with_trailing_spaces_stays_publishable_and_the_rendering_trims_them() {
    let record = make_record(vec![encode_text_field("Title \nFried Egg \nplain", vec![])]);
    let result = ok(classify(&record, TitleMode::InBody));
    assert!(result.publishable, "{:?}", result.unpublishable_reason);
    assert_eq!(result.body_text, "Title \nFried Egg \nplain");
    assert_eq!(result.markdown_text, "Title\nFried Egg\nplain");
}

fn titled(text: &str, runs: Vec<AttributeRun>) -> CloudKitRecord {
    make_record(vec![encode_text_field(text, runs)])
}

#[test]
fn filename_mode_leaves_the_title_paragraph_out_of_the_markdown() {
    let record = titled("My Note\n\nBody text", vec![styled(8, 0), styled(10, 3)]);
    let in_body = ok(classify(&record, TitleMode::InBody));
    let filename = ok(classify(&record, TitleMode::Filename));
    assert_eq!(in_body.markdown_text, "# My Note\n\nBody text");
    assert_eq!(filename.markdown_text, "\nBody text");
    assert!(filename.title_stripped);
    assert!(!in_body.title_stripped);
}

#[test]
fn the_whole_formatting_model_survives_stripping() {
    let record = titled("My Note\nBody text", vec![styled(8, 0), styled(9, 3)]);
    let result = ok(classify(&record, TitleMode::Filename));
    let format = result.format.unwrap();
    assert_eq!(format.len(), 2);
    assert_eq!(format[0].kind, ParagraphKind::Title);
    assert_eq!(format[0].text, "My Note");
}

#[test]
fn title_line_is_the_notes_real_first_line_not_apples_truncated_title() {
    let long = "A first line that runs well past the seventy-six characters Apple truncates its title metadata at";
    let mut record = titled(&format!("{long}\nBody"), vec![styled(len16(long) + 1, 0), styled(4, 3)]);
    record.fields.insert(
        "TitleEncrypted".into(),
        FieldValue {
            value: json!(base64_encode(slice16(long, 0, 76).as_bytes())),
            type_: "ENCRYPTED_BYTES".into(),
        },
    );
    let result = ok(classify(&record, TitleMode::Filename));
    assert_eq!(result.title_line, long);
    assert_ne!(result.title, result.title_line);
}

#[test]
fn a_monospaced_first_paragraph_strips_without_corrupting_the_fence() {
    let record = titled("code one\ncode two", vec![styled(9, 4), styled(8, 4)]);
    let result = ok(classify(&record, TitleMode::Filename));
    assert_eq!(
        result.markdown_text.matches("```").count() % 2,
        0,
        "{}",
        result.markdown_text
    );
}

#[test]
fn a_title_paragraph_holding_an_embed_placeholder_is_never_stripped() {
    let record = titled(
        "\u{fffc}\nBody text",
        vec![info(1, Some("A-1"), "public.jpeg"), styled(10, 3)],
    );
    let result = ok(classify(&record, TitleMode::Filename));
    assert!(!result.title_stripped);
    assert!(result.markdown_text.contains('\u{fffc}'));
}

#[test]
fn a_single_line_note_strips_to_an_empty_body() {
    let record = titled("Just a title", vec![styled(12, 0)]);
    let result = ok(classify(&record, TitleMode::Filename));
    assert_eq!(result.markdown_text, "");
    assert!(result.title_stripped);
    assert!(result.publishable);
}

#[test]
fn body_text_is_the_notes_raw_text_in_both_modes() {
    let record = titled("My Note\n\nBody text", vec![styled(8, 0), styled(10, 3)]);
    let in_body = ok(classify(&record, TitleMode::InBody));
    let filename = ok(classify(&record, TitleMode::Filename));
    assert_eq!(in_body.body_text, "My Note\n\nBody text");
    assert_eq!(filename.body_text, in_body.body_text);
}

/// Every real fixture note classifies exactly as icloud-md does (goldens'
/// markdown from the exporter).
#[test]
fn real_fixture_notes_classify_to_their_golden_markdown() {
    for file in [
        "real_plain_note.json",
        "real_unicode_note.json",
        "real_first_save_note.json",
        "real_formatted_multi_edit_note.json",
    ] {
        let path = format!("{}/tests/fixtures/real/{file}", env!("CARGO_MANIFEST_DIR"));
        let json: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let record = make_record(vec![("TextDataEncrypted", json["base64"].clone(), "ENCRYPTED_BYTES")]);
        let result = ok(classify(&record, TitleMode::InBody));
        assert_eq!(
            result.markdown_text,
            json["golden"]["markdown"].as_str().unwrap(),
            "{file}"
        );
    }
}
