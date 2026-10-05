//! The real captured fixtures, plus the byte-exactness gates:
//! every real payload decodes and re-encodes byte for byte at every layer,
//! and every recorded golden is reproduced (`fixture_goldens_are_current`
//! re-records them with `ICLOUD_NOTES_SYNC_REGEN=1`).

mod common;

use icloud_notes_sync::doc::document::{note_document_round_trips, parse_note_document};
use icloud_notes_sync::doc::embeds::decode_note_embed_slots;
use icloud_notes_sync::doc::format::{FormatParagraph, decode_note_format};
use icloud_notes_sync::doc::proto::{Message, crdt, topotext, versioned_document};
use icloud_notes_sync::doc::tables::{
    decode_table_markdown, grid_from_table_document, parse_table_document, table_document_round_trips,
};
use icloud_notes_sync::doc::text::{decode_note_body_text, decompress_note_document, parse_versioned_document};
use icloud_notes_sync::js::base64_decode;
use icloud_notes_sync::md::render::render_note_markdown;
use serde_json::{Value, json};

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

fn attempt<T: serde::Serialize, E: std::fmt::Display>(result: Result<T, E>) -> Value {
    match result {
        Ok(value) => json!(value),
        Err(error) => json!({ "error": error.to_string() }),
    }
}

/// What this crate makes of a note payload (tests/fixtures/README.md).
fn note_golden(compressed: &[u8]) -> Value {
    let raw = decompress_note_document(compressed).unwrap();
    let doc = parse_note_document(&raw).unwrap();
    let format = decode_note_format(&doc.text, &doc.attribute_runs);
    json!({
        "text": decode_note_body_text(compressed).unwrap(),
        "attributeRunLengths": doc.attribute_runs.iter().map(|r| r.len()).collect::<Vec<_>>(),
        "roundTrips": note_document_round_trips(&raw),
        "document": {
            "runs": doc.runs.len(),
            "replicas": doc.replicas.len(),
            "minimumSupportedVersion": doc.minimum_supported_version,
        },
        "embedSlots": decode_note_embed_slots(compressed).unwrap(),
        "format": match &format {
            Ok(paragraphs) => json!(paragraphs),
            Err(reason) => json!({ "unsupported": reason }),
        },
        "markdown": format.as_ref().ok().map(|p| render_note_markdown(p)),
    })
}

/// What this crate makes of a table payload.
fn table_golden(compressed: &[u8]) -> Value {
    json!({
        "roundTrips": table_document_round_trips(compressed),
        "grid": attempt(parse_table_document(compressed).and_then(|doc| grid_from_table_document(&doc))),
        "markdown": attempt(decode_table_markdown(compressed)),
    })
}

/// Every fixture's `golden` (and each revision's) is what this crate
/// computes from its payload now; `ICLOUD_NOTES_SYNC_REGEN=1` rewrites the
/// ones that differ (review the diff).
#[test]
fn fixture_goldens_are_current() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real");
    let mut stale = Vec::new();
    for file in common::fixture_files() {
        let mut json = common::fixture(&file);
        let note = json["kind"] == "note";
        let golden = |b64: &Value| {
            let compressed = base64_decode(b64.as_str().unwrap());
            if note { note_golden(&compressed) } else { table_golden(&compressed) }
        };
        let mut changed = false;
        if json.get("base64").is_some() {
            let now = golden(&json["base64"]);
            changed |= json["golden"] != now;
            json["golden"] = now;
        }
        for revision in json.get_mut("revisions").and_then(Value::as_array_mut).into_iter().flatten() {
            let now = golden(&revision["base64"]);
            changed |= revision["golden"] != now;
            revision["golden"] = now;
        }
        if changed {
            stale.push(file.clone());
            if common::regen() {
                let text = serde_json::to_string_pretty(&json).unwrap() + "\n";
                std::fs::write(format!("{dir}/{file}"), text).unwrap();
            }
        }
    }
    if common::regen() {
        eprintln!("re-recorded fixture goldens: {stale:?}");
    } else {
        assert!(
            stale.is_empty(),
            "goldens differ from what the crate computes: {stale:?} (if intended: ICLOUD_NOTES_SYNC_REGEN=1, then review the diff)"
        );
    }
}

// --- real fixtures ----------------------------------------------------------------

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
            let compressed = icloud_notes_sync::js::base64_decode(revision["base64"].as_str().unwrap());
            let doc = parse_table_document(&compressed).unwrap();
            assert_eq!(grid_from_table_document(&doc).unwrap(), common::grid(&revision["grid"]));
        }
    }
}
