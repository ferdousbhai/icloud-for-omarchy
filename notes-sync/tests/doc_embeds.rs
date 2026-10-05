//! Embedded objects: attachments and content this tool can't parse
//! (pushing edits around them is in `doc_embed_push.rs`). Originally derived from icloud-md's tests.

mod common;

use common::reference;

use icloud_notes_sync::cloudkit::FieldValue;
use icloud_notes_sync::doc::embeds::{
    EmbedMarkerContent, EmbedSlot, UNKNOWN_CONTENT_BANNER, decode_note_embed_slots, format_attachment_markdown,
    format_embed_marker, has_attachment_reference, has_embed_marker, has_unknown_content_marker, is_image_uti,
    is_table_uti, parse_embed_markers, render_placeholders,
};
use icloud_notes_sync::doc::proto::topotext::{self, AttachmentInfo, AttributeRun};
use icloud_notes_sync::doc::proto::{Message, versioned_document};
use icloud_notes_sync::doc::text::compress_note_document;
use icloud_notes_sync::js::{base64_decode, base64_encode, slice16};
use icloud_notes_sync::vault::attachments::{AttachmentAsset, decode_attachment_filename, parse_asset_field};
use serde_json::json;

fn encode_note_body(text: &str, runs: Vec<AttributeRun>) -> Vec<u8> {
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
    compress_note_document(&wrapper.encode().unwrap())
}

fn info_run(length: u32, id: Option<&str>, uti: Option<&str>) -> AttributeRun {
    AttributeRun {
        length: Some(length),
        attachment_info: Some(AttachmentInfo {
            attachment_identifier: id.map(Into::into),
            type_uti: uti.map(Into::into),
            ..Default::default()
        }),
        ..Default::default()
    }
}

const AUDIO_ATTACHMENT_TEXT_DATA: &str = "H4sIAAAAAAAAE+NgEPrMyMEgwCD1hlFI2jkxJ0ehPLMkQ8ErMS8zOVXBNSc7M6+Y6/3+PVICXCwgdUCVYFqDESzCCBSRlALTGkxSYlwcQLn/QMAPVAdnK8lwSXEJJPhc2rlRw0G3ods/UnqB/1chJg5JIGbUkuOQEBLhYPASmH4r88mnYucbq8RWvT250oY/Y8XpVSfYtII5GIWEvAR2M+dLZK9xldxU0u6seOnrhiRrLhVzF0c3F0czN10TR2cTXRNDFwtdS0tTC10DE0tjc2MnCwMLIxMh4eT8XL3EgoKcVL1ck0TdxNKUzHwAzdJGOPgAAAA=";
const IMAGE_ATTACHMENT_TEXT_DATA: &str = "H4sIAAAAAAAAE23STWgTQRQH8GzSNJupNZNNmrabCENRWQIbQkybopd+2IBFDEoR6sWYZGvSxmzY7LZdP2oRoQehogeh4qGCFoWCH6AglHoRpQoitRRPPYgnlfZQetKiL9tn8NBllxl+M++/j2F4m/DDxduoTfzmEswBpaKzE6qukIF8ocLgPcf0qpXA2JCqMaOisLGCnmfpQraoGjm5ulKRK2Ypm2a6ykYVrTBkMj2vME01SjlZ1wplNqZqIxWWVTVNyepFM0JIj6KUWUZVy4RsvF4gIiV11S6gD2uUOEs4kA7RGiU7SoJyKA6UI7QBpU4ULEnS0I4wTnKiKdRZs3qr0gHpQdEaJRdmDdeyeKzLULslbqhzo+X+9QBG0DpxHw/WgHb4v3170Dpq/TdKHuuvdVSnBMxO3dJeS5zQGYcSQtFrQq0kJzVQgpDuRRtFE8EEtDG0VjAf2jhaC5gfzURrBmtCu4gWAAugXUJrAmtGu4zmB2tBu4LmA2tFm0ATwES0q2hesKDoJU6YT3LWcdppCA4gQHi4FH/g8cAFqc3bpjkiEnrgVOPgy+171+0Hj009plvdgp034CMkSGj6+PKrZ1KXPHkjNRicTW0JDv4aB4v7qoV3cm2f1+bNpffvfv1cWHSs1gph7e6o2+g64/N8XR3vebv0fA58Aj6uGvqkeGv8xf1+89PM90cfNn+vQOhkNdQdbuGJ4Odt/ZStrc/NzKYuTD/c2P4YcLvCAX5EEPrp081e282VVOT22fUHoanTzvBJntvNM4fI/kTf0c5oLBGX4/FoVI73RmNyZ6KvW062x2PJaHtHMhmNCQ1lI1MsZCPDZeV8/s38l+X6cGj3yJ3Vvzm/RsfwAwAA";

#[test]
fn decode_note_embed_slots_localizes_the_real_image_attachment() {
    let slots = decode_note_embed_slots(&base64_decode(IMAGE_ATTACHMENT_TEXT_DATA)).unwrap();
    assert_eq!(
        slots,
        Some(vec![EmbedSlot::Attachment(reference(
            "7ED80274-4400-4C02-87EA-F542F056FF02",
            "public.jpeg"
        ))])
    );
}

#[test]
fn decode_note_embed_slots_localizes_the_real_audio_attachment() {
    let slots = decode_note_embed_slots(&base64_decode(AUDIO_ATTACHMENT_TEXT_DATA)).unwrap();
    assert_eq!(
        slots,
        Some(vec![EmbedSlot::Attachment(reference(
            "7DAFDA6F-4AC4-41D8-9958-049373B80824",
            "com.apple.m4a-audio"
        ))])
    );
    assert_eq!(
        serde_json::to_value(&slots).unwrap(),
        json!([{"kind": "attachment", "ref": {"attachmentIdentifier": "7DAFDA6F-4AC4-41D8-9958-049373B80824", "typeUti": "com.apple.m4a-audio"}}])
    );
}

#[test]
fn decode_note_embed_slots_maps_placeholders_by_offset() {
    let body = encode_note_body(
        "T\n\u{fffc}\nmid\n\u{fffc}",
        vec![
            AttributeRun::with_length(2),
            info_run(1, Some("A-1"), Some("public.jpeg")),
            AttributeRun::with_length(5),
        ],
    );
    assert_eq!(
        decode_note_embed_slots(&body).unwrap(),
        Some(vec![
            EmbedSlot::Attachment(reference("A-1", "public.jpeg")),
            EmbedSlot::Unknown { type_uti: None }
        ])
    );
}

#[test]
fn decode_note_embed_slots_carries_a_partial_attachment_infos_uti_into_the_unknown_slot() {
    let body = encode_note_body(
        "x\u{fffc}",
        vec![
            AttributeRun::with_length(1),
            info_run(1, None, Some("com.apple.drawing.2")),
        ],
    );
    assert_eq!(
        decode_note_embed_slots(&body).unwrap(),
        Some(vec![EmbedSlot::Unknown {
            type_uti: Some("com.apple.drawing.2".into())
        }])
    );
}

#[test]
fn decode_note_embed_slots_returns_none_when_an_attachment_info_run_isnt_a_lone_placeholder() {
    let overlong = encode_note_body(
        "x\u{fffc}y",
        vec![
            AttributeRun::with_length(1),
            info_run(2, Some("A"), Some("public.jpeg")),
        ],
    );
    assert_eq!(decode_note_embed_slots(&overlong).unwrap(), None);
    let off = encode_note_body(
        "xy",
        vec![
            AttributeRun::with_length(1),
            info_run(1, Some("A"), Some("public.jpeg")),
        ],
    );
    assert_eq!(decode_note_embed_slots(&off).unwrap(), None);
}

#[test]
fn decode_note_embed_slots_returns_none_when_run_lengths_overshoot_the_text() {
    let body = encode_note_body("hi", vec![AttributeRun::with_length(5)]);
    assert_eq!(decode_note_embed_slots(&body).unwrap(), None);
}

#[test]
fn decode_note_embed_slots_tolerates_an_under_covering_or_absent_run_table() {
    assert_eq!(
        decode_note_embed_slots(&encode_note_body("plain text", vec![])).unwrap(),
        Some(vec![])
    );
    assert_eq!(
        decode_note_embed_slots(&encode_note_body("tail \u{fffc}", vec![AttributeRun::with_length(2)])).unwrap(),
        Some(vec![EmbedSlot::Unknown { type_uti: None }])
    );
}

#[test]
fn is_image_uti_recognizes_known_image_utis_and_rejects_others() {
    assert!(is_image_uti("public.jpeg"));
    assert!(is_image_uti("public.png"));
    assert!(!is_image_uti("com.apple.m4a-audio"));
    assert!(!is_image_uti("com.adobe.pdf"));
}

#[test]
fn format_attachment_markdown_embeds_images_and_links_everything_else() {
    assert_eq!(
        format_attachment_markdown(&reference("A", "public.jpeg"), "attachments/photo.jpeg"),
        "![photo.jpeg](attachments/photo.jpeg)"
    );
    assert_eq!(
        format_attachment_markdown(&reference("B", "com.apple.m4a-audio"), "attachments/call.m4a"),
        "[call.m4a](attachments/call.m4a)"
    );
}

#[test]
fn format_attachment_markdown_percent_encodes_path_segments_with_spaces() {
    assert_eq!(
        format_attachment_markdown(
            &reference("A", "com.apple.m4a-audio"),
            "attachments/Call with Janice Elkins.m4a"
        ),
        "[Call with Janice Elkins.m4a](attachments/Call%20with%20Janice%20Elkins.m4a)"
    );
}

fn filename_field(value: serde_json::Value) -> FieldValue {
    FieldValue {
        value,
        type_: "ENCRYPTED_BYTES".into(),
    }
}

#[test]
fn decode_attachment_filename_decodes_a_base64_filename_encrypted_field() {
    let field = filename_field(json!(base64_encode(b"photo.jpg")));
    assert_eq!(
        decode_attachment_filename(Some(&field), "REC1", "public.jpeg"),
        "photo.jpg"
    );
}

#[test]
fn decode_attachment_filename_falls_back_to_a_synthesized_name_when_absent() {
    assert_eq!(decode_attachment_filename(None, "REC1", "public.jpeg"), "REC1.jpeg");
    assert_eq!(
        decode_attachment_filename(None, "REC2", "com.apple.m4a-audio"),
        "REC2.m4a"
    );
    assert_eq!(decode_attachment_filename(None, "REC3", "some.unknown.uti"), "REC3");
}

fn asset(value: serde_json::Value, type_: &str) -> FieldValue {
    FieldValue {
        value,
        type_: type_.into(),
    }
}

#[test]
fn parse_asset_field_extracts_download_url_and_file_checksum() {
    let field = asset(
        json!({"downloadURL": "https://cvws.icloud-content.com/x", "fileChecksum": "abc123", "size": 42}),
        "ASSETID",
    );
    assert_eq!(
        parse_asset_field(Some(&field)),
        Some(AttachmentAsset {
            download_url: "https://cvws.icloud-content.com/x".into(),
            file_checksum: "abc123".into()
        })
    );
}

#[test]
fn parse_asset_field_returns_none_for_non_assetid_or_malformed_fields() {
    assert_eq!(parse_asset_field(None), None);
    assert_eq!(parse_asset_field(Some(&asset(json!("not an object"), "ASSETID"))), None);
    assert_eq!(
        parse_asset_field(Some(&asset(json!({"downloadURL": "x"}), "ASSETID"))),
        None
    );
    assert_eq!(
        parse_asset_field(Some(&asset(json!({"downloadURL": "x", "fileChecksum": "y"}), "STRING"))),
        None
    );
}

#[test]
fn is_table_uti_recognizes_the_table_uti_and_rejects_file_utis() {
    assert!(is_table_uti("com.apple.notes.table"));
    assert!(!is_table_uti("public.jpeg"));
    assert!(!is_table_uti("com.apple.m4a-audio"));
}

#[test]
fn render_placeholders_substitutes_a_mix_by_position() {
    let result = render_placeholders(
        "Title\n\u{fffc}\n\u{fffc}\n",
        &[
            Some("| a | b |\n| --- | --- |".into()),
            Some("[call.m4a](attachments/call.m4a)".into()),
        ],
    );
    assert_eq!(
        result,
        "Title\n| a | b |\n| --- | --- |\n[call.m4a](attachments/call.m4a)\n"
    );
}

#[test]
fn render_placeholders_leaves_a_placeholder_untouched_when_its_replacement_is_undefined() {
    assert_eq!(
        render_placeholders("\u{fffc}\u{fffc}", &[Some("resolved".into()), None]),
        "resolved\u{fffc}"
    );
}

#[test]
fn render_placeholders_is_a_no_op_with_no_replacements() {
    assert_eq!(render_placeholders("Plain text", &[]), "Plain text");
}

#[test]
fn has_attachment_reference_detects_a_hand_typed_attachments_link_or_embed() {
    assert!(has_attachment_reference("See ![photo](attachments/photo.jpg) above"));
    assert!(has_attachment_reference("See [file](attachments/notes.pdf) above"));
    assert!(!has_attachment_reference("Just plain text"));
    assert!(!has_attachment_reference(
        "A [normal link](https://example.com) is fine"
    ));
    // Regex corner cases: `[^)]+` needs at least one character, and a later
    // `[` still gets its chance.
    assert!(!has_attachment_reference("[x](attachments/)"));
    assert!(has_attachment_reference("[a] [b](attachments/c)"));
}

// --- content this tool cannot parse --------------------------------------------

#[test]
fn unknown_content_banner_is_a_danger_admonition() {
    assert!(UNKNOWN_CONTENT_BANNER.starts_with("> [!danger] Unparsed content\n"));
    assert_eq!(
        UNKNOWN_CONTENT_BANNER,
        "> [!danger] Unparsed content\n> This note contains content this tool can't parse or place precisely. It stays read-only here until that's resolved (e.g. by editing the note in Notes directly).\n\n"
    );
}

#[test]
fn has_unknown_content_marker_detects_the_banner() {
    assert!(has_unknown_content_marker(&format!(
        "{UNKNOWN_CONTENT_BANNER}Some note text"
    )));
}

fn marker(type_uti: Option<&str>, id: Option<&str>) -> String {
    format_embed_marker(&EmbedMarkerContent {
        type_uti: type_uti.map(Into::into),
        attachment_identifier: id.map(Into::into),
    })
}

#[test]
fn has_unknown_content_marker_is_false_for_ordinary_text_and_embed_markers() {
    assert!(!has_unknown_content_marker(
        "Just a normal note.\n\n> A regular quote, not an admonition."
    ));
    assert!(!has_unknown_content_marker(&marker(
        Some("com.apple.notes.gallery"),
        Some("A")
    )));
}

#[test]
fn format_embed_marker_carries_identity_in_attributes_and_a_short_label() {
    assert_eq!(
        marker(Some("com.apple.notes.gallery"), Some("ABC-123")),
        r#"<apple-embed type="com.apple.notes.gallery" id="ABC-123">gallery</apple-embed>"#
    );
}

#[test]
fn format_embed_marker_without_an_identifier_omits_the_id_attribute() {
    assert_eq!(
        marker(Some("com.apple.drawing.2"), None),
        r#"<apple-embed type="com.apple.drawing.2">drawing</apple-embed>"#
    );
}

#[test]
fn format_embed_marker_with_no_identity_at_all_is_the_unknown_marker() {
    assert_eq!(
        marker(None, None),
        r#"<apple-embed type="unknown">unidentified embed</apple-embed>"#
    );
}

#[test]
fn format_embed_marker_falls_back_to_the_raw_uti_as_label() {
    assert_eq!(
        marker(Some("com.example.new-thing"), Some("X")),
        r#"<apple-embed type="com.example.new-thing" id="X">com.example.new-thing</apple-embed>"#
    );
}

#[test]
fn parse_embed_markers_finds_markers_in_document_order_with_offsets_and_identity() {
    let first = marker(Some("com.apple.notes.gallery"), Some("A-1"));
    let second = marker(None, None);
    let text = format!("Intro \u{1f600}\n{first}\nMiddle\n{second}\nOutro");
    let markers = parse_embed_markers(&text);
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0].text, first);
    assert_eq!(markers[0].type_uti, "com.apple.notes.gallery");
    assert_eq!(markers[0].attachment_identifier.as_deref(), Some("A-1"));
    assert_eq!(slice16(&text, markers[0].start, markers[0].end), first);
    assert_eq!(markers[0].start, 9, "UTF-16 offsets");
    assert_eq!(markers[1].text, second);
    assert_eq!(markers[1].type_uti, "unknown");
    assert_eq!(markers[1].attachment_identifier, None);
}

#[test]
fn parse_embed_markers_surfaces_a_mangled_marker_rather_than_skipping_it() {
    let markers = parse_embed_markers(r#"<apple-embed type="com.apple.paper" id="B-2">edited label!</apple-embed>"#);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].attachment_identifier.as_deref(), Some("B-2"));
}

#[test]
fn parse_embed_markers_finds_nothing_in_ordinary_html_ish_text() {
    assert!(parse_embed_markers("Some <b>bold</b> text and a <u>tag</u>").is_empty());
    assert!(parse_embed_markers("<apple-embedded type=\"x\">y</apple-embed>").is_empty());
}

#[test]
fn has_embed_marker_detects_even_a_truncated_marker_opening() {
    assert!(has_embed_marker("pasted <apple-embed type=... junk"));
    assert!(!has_embed_marker("plain text"));
    assert!(!has_embed_marker("<apple-embedded"));
}
