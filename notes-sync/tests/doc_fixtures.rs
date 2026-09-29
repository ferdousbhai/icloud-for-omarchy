//! Ports icloud-md `realFixtures.test.ts`, plus the byte-exactness gates:
//! every real payload decodes and re-encodes byte for byte at every layer,
//! and every golden the Node exporter recorded is reproduced.

mod common;

use icloud_notes_sync::doc::document::{note_document_round_trips, parse_note_document};
use icloud_notes_sync::doc::embeds::decode_note_embed_slots;
use icloud_notes_sync::doc::format::{FormatParagraph, decode_note_format};
use icloud_notes_sync::doc::proto::{Message, crdt, topotext, versioned_document};
use icloud_notes_sync::doc::tables::{grid_from_table_document, parse_table_document, table_document_round_trips};
use icloud_notes_sync::doc::text::{decode_note_body_text, decompress_note_document, parse_versioned_document};

#[test]
fn every_payload_round_trips_byte_for_byte_at_every_protobuf_layer() {
    let payloads = common::all_payloads();
    assert!(payloads.len() >= 51);
    for (label, kind, compressed, _) in payloads {
        let raw = decompress_note_document(&compressed).unwrap();
        let wrapper = versioned_document::Document::decode(&raw).unwrap();
        assert_eq!(wrapper.encode().unwrap(), raw, "{label}: wrapper");
        let data = parse_versioned_document(&raw).unwrap().data;
        let inner = if kind == "note" {
            topotext::String::decode(&data).unwrap().encode().unwrap()
        } else {
            crdt::Document::decode(&data).unwrap().encode().unwrap()
        };
        assert_eq!(inner, data, "{label}: inner document");
    }
}

#[test]
fn note_fixtures_match_their_goldens() {
    for (label, kind, compressed, golden) in common::all_payloads() {
        if kind != "note" {
            continue;
        }
        let raw = decompress_note_document(&compressed).unwrap();
        let text = decode_note_body_text(&compressed).unwrap();
        assert_eq!(text, golden["text"].as_str().unwrap(), "{label}");
        assert_eq!(
            note_document_round_trips(&raw),
            golden["roundTrips"].as_bool().unwrap(),
            "{label}"
        );

        let doc = parse_note_document(&raw).unwrap();
        let lengths: Vec<u64> = doc.attribute_runs.iter().map(|r| u64::from(r.len())).collect();
        let want: Vec<u64> = serde_json::from_value(golden["attributeRunLengths"].clone()).unwrap();
        assert_eq!(lengths, want, "{label}");
        assert_eq!(
            doc.runs.len() as u64,
            golden["document"]["runs"].as_u64().unwrap(),
            "{label}"
        );
        assert_eq!(
            doc.replicas.len() as u64,
            golden["document"]["replicas"].as_u64().unwrap(),
            "{label}"
        );
        assert_eq!(
            u64::from(doc.minimum_supported_version),
            golden["document"]["minimumSupportedVersion"].as_u64().unwrap()
        );

        let slots = decode_note_embed_slots(&compressed).unwrap().unwrap();
        assert_eq!(serde_json::to_value(&slots).unwrap(), golden["embedSlots"], "{label}");

        let format = decode_note_format(&doc.text, &doc.attribute_runs).unwrap();
        let want: Vec<FormatParagraph> = serde_json::from_value(golden["format"].clone()).unwrap();
        assert_eq!(format, want, "{label}");
        assert_eq!(
            serde_json::to_value(&format).unwrap(),
            golden["format"],
            "{label}: JSON shape"
        );
    }
}

#[test]
fn table_fixtures_match_their_goldens() {
    let mut tables = 0;
    for (label, kind, compressed, golden) in common::all_payloads() {
        if kind == "note" {
            continue;
        }
        tables += 1;
        assert_eq!(
            table_document_round_trips(&compressed),
            golden["roundTrips"].as_bool().unwrap(),
            "{label}"
        );
        let doc = parse_table_document(&compressed).unwrap();
        assert_eq!(
            grid_from_table_document(&doc).unwrap(),
            common::grid(&golden["grid"]),
            "{label}"
        );
    }
    assert!(tables >= 45);
}

// --- realFixtures.test.ts ---------------------------------------------------------

#[test]
fn real_captured_notes_decode_to_the_expected_text_and_round_trip() {
    for file in [
        "real_plain_note.json",
        "real_unicode_note.json",
        "real_first_save_note.json",
        "real_formatted_multi_edit_note.json",
    ] {
        let compressed = common::payload(file);
        let raw = decompress_note_document(&compressed).unwrap();
        assert!(note_document_round_trips(&raw), "{file}");
    }
}

#[test]
fn table_revisions_decode_to_their_recorded_grids() {
    for file in [
        "table_evolution_revisions.json",
        "table_long_lived_snapshots.json",
        "table_restyle_revisions.json",
        "table_write_path_revisions.json",
    ] {
        let json = common::fixture(file);
        for revision in json["revisions"].as_array().unwrap() {
            if revision.get("grid").is_none() {
                continue;
            }
            let compressed = icloud_notes_sync::doc::js::base64_decode(revision["base64"].as_str().unwrap());
            let doc = parse_table_document(&compressed).unwrap();
            assert_eq!(grid_from_table_document(&doc).unwrap(), common::grid(&revision["grid"]));
        }
    }
}
