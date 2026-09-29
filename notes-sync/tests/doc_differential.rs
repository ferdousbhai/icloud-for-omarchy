//! Differential tests against icloud-md itself (`tests/doc_node/oracle.mts`,
//! run through its own tsx): the protobuf layer against protobuf-es on
//! hostile inputs, and the CRDT edit pipeline (applyTextEdit +
//! reconcileNoteFormat, the table engine) on the same inputs and the same
//! random draws. Skipped when the icloud-md clone isn't available.

mod common;

use icloud_notes_sync::doc::document::{
    ApplyTextEditOptions, apply_text_edit, build_initial_note_document, encode_note_document, parse_note_document,
};
use icloud_notes_sync::doc::js::{base64_decode, base64_encode};
use icloud_notes_sync::doc::proto::{Message, ProtoResult, crdt, topotext, versioned_document};
use icloud_notes_sync::doc::reconcile::reconcile_note_format_with;
use icloud_notes_sync::doc::text::{compress_note_document, decompress_note_document, parse_versioned_document};
use serde_json::{Value, json};

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut state = seed;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

// --- the protobuf layer vs protobuf-es -----------------------------------------

fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v > 0x7f {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn tag(no: u32, wt: u8, out: &mut Vec<u8>) {
    varint(u64::from(no << 3 | u32::from(wt)), out);
}

fn ld(no: u32, bytes: &[u8], out: &mut Vec<u8>) {
    tag(no, 2, out);
    varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

/// Hand-built hostile messages: unknown fields in several positions and wire
/// types (repeated, interleaved with known fields), non-canonical varints,
/// invalid UTF-8, a BOM, missing required fields, packed and wrong-wire-type
/// scalars, and unknown groups.
fn synthetic_cases() -> Vec<(&'static str, Vec<u8>)> {
    let mut cases = Vec::new();

    // topotext.String: unknown fields around and between known ones.
    let mut s = Vec::new();
    tag(1, 0, &mut s);
    varint(7, &mut s);
    ld(2, b"hello", &mut s);
    tag(99, 5, &mut s);
    s.extend_from_slice(&[1, 2, 3, 4]);
    ld(6, b"embedded", &mut s);
    tag(1, 0, &mut s);
    s.extend_from_slice(&[0x85, 0x80, 0x00]); // overlong varint 5
    tag(98, 1, &mut s);
    s.extend_from_slice(&[9; 8]);
    let mut run = Vec::new();
    tag(1, 0, &mut run);
    varint(5, &mut run);
    tag(4, 0, &mut run); // undeclared AttributeRun field 4
    varint(3, &mut run);
    let mut ps = Vec::new();
    tag(1, 0, &mut ps);
    varint(100, &mut ps);
    tag(10, 0, &mut ps); // undeclared ParagraphStyle field
    varint(1, &mut ps);
    ld(2, &ps, &mut run);
    tag(16, 0, &mut run); // after the last declared field
    varint(2, &mut run);
    tag(4, 0, &mut run); // field 4 again, after field 16
    varint(4, &mut run);
    ld(5, &run, &mut s);
    tag(1, 0, &mut s);
    varint(8, &mut s);
    cases.push(("string", s));

    // Invalid UTF-8, a lone surrogate encoding, and a BOM.
    for (bytes, _) in [
        (&b"bad \xff\xfe utf8"[..], 0),
        (b"\xed\xa0\x80 surrogate", 0),
        (b"\xef\xbb\xbfbom", 0),
        (b"\xef\xbb\xbf\xef\xbb\xbftwo", 0),
        (b"trunc \xe2\x82", 0),
    ] {
        let mut s = Vec::new();
        ld(2, bytes, &mut s);
        cases.push(("string", s));
    }

    // Missing required fields: String.string, Substring.charID, AttributeRun.length, Color.alpha.
    let mut s = Vec::new();
    let mut sub = Vec::new();
    tag(2, 0, &mut sub);
    varint(3, &mut sub);
    ld(3, &sub, &mut s);
    cases.push(("string", s));
    let mut s = Vec::new();
    ld(2, b"x", &mut s);
    let mut run = Vec::new();
    tag(5, 0, &mut run);
    varint(1, &mut run);
    ld(5, &run, &mut s);
    cases.push(("string", s));
    let mut run = Vec::new();
    tag(1, 0, &mut run);
    varint(1, &mut run);
    let mut color = Vec::new();
    for no in 1..=3 {
        tag(no, 5, &mut color);
        color.extend_from_slice(&1.0f32.to_le_bytes());
    }
    ld(10, &color, &mut run);
    cases.push(("attributeRun", run));

    // Packed Substring.child, a signalling-NaN float, negative int32 indent.
    let mut s = Vec::new();
    ld(2, b"ab", &mut s);
    let mut sub = Vec::new();
    let mut id = Vec::new();
    tag(1, 0, &mut id);
    varint(1, &mut id);
    tag(2, 0, &mut id);
    varint(0, &mut id);
    ld(1, &id, &mut sub);
    tag(2, 0, &mut sub);
    varint(2, &mut sub);
    ld(3, &id, &mut sub);
    ld(5, &[1, 2, 3], &mut sub);
    ld(3, &sub, &mut s);
    cases.push(("string", s));
    let mut run = Vec::new();
    tag(1, 0, &mut run);
    varint(1, &mut run);
    let mut color = Vec::new();
    for no in 1..=4 {
        tag(no, 5, &mut color);
        color.extend_from_slice(&0x7fa0_0001u32.to_le_bytes());
    }
    ld(10, &color, &mut run);
    let mut ps = Vec::new();
    tag(4, 0, &mut ps);
    varint(u64::MAX - 1, &mut ps); // int32 -2, ten bytes
    ld(2, &ps, &mut run);
    cases.push(("attributeRun", run));

    // A known scalar with the wrong wire type, and an unknown group.
    let mut run = Vec::new();
    tag(1, 2, &mut run);
    varint(1, &mut run);
    run.push(5);
    tag(20, 3, &mut run);
    tag(1, 0, &mut run);
    varint(9, &mut run);
    tag(20, 4, &mut run);
    cases.push(("attributeRun", run));

    // A uint32 carried in a 10-byte varint, and a repeated message field
    // appearing twice around unknowns (CRDT).
    let mut doc = Vec::new();
    tag(4, 2, &mut doc);
    varint(3, &mut doc);
    doc.extend_from_slice(b"key");
    tag(50, 0, &mut doc);
    varint(1, &mut doc);
    let mut obj = Vec::new();
    let mut objid = Vec::new();
    tag(6, 0, &mut objid);
    varint(u64::MAX, &mut objid);
    tag(1, 0, &mut objid);
    varint(3, &mut objid); // sint64
    let mut reg = Vec::new();
    ld(2, &objid, &mut reg);
    ld(1, &reg, &mut obj);
    ld(3, &obj, &mut doc);
    tag(4, 2, &mut doc);
    varint(1, &mut doc);
    doc.push(b'z');
    ld(3, &obj, &mut doc);
    cases.push(("crdt", doc));
    cases
}

fn codec_round_trip(schema: &str, bytes: &[u8]) -> Value {
    fn run<M: Message>(bytes: &[u8]) -> Value {
        match M::decode(bytes) {
            Err(e) => json!({"decoded": false, "error": e.to_string()}),
            Ok(m) => {
                let encoded: ProtoResult<Vec<u8>> = m.encode();
                match encoded {
                    Ok(b) => json!({"decoded": true, "b64": base64_encode(&b)}),
                    Err(e) => json!({"decoded": true, "encodeError": e.to_string()}),
                }
            }
        }
    }
    match schema {
        "string" => run::<topotext::String>(bytes),
        "crdt" => run::<crdt::Document>(bytes),
        "attributeRun" => run::<topotext::AttributeRun>(bytes),
        "versioned" => run::<versioned_document::Document>(bytes),
        _ => unreachable!(),
    }
}

/// Every real payload, then byte flips, truncations and insertions of them.
fn mutated_cases() -> Vec<(&'static str, Vec<u8>)> {
    let mut next = xorshift(0x5eed_1234_abcd_0001);
    let mut cases = Vec::new();
    for (_, kind, compressed, _) in common::all_payloads() {
        let raw = decompress_note_document(&compressed).unwrap();
        let data = parse_versioned_document(&raw).unwrap().data;
        let schema = if kind == "note" { "string" } else { "crdt" };
        cases.push(("versioned", raw.clone()));
        cases.push((schema, data.clone()));
        for _ in 0..12 {
            let mut mutated = data.clone();
            match next() % 4 {
                0 => {
                    let at = (next() as usize) % mutated.len();
                    mutated[at] ^= 1 << (next() % 8);
                }
                1 => mutated.truncate((next() as usize) % mutated.len()),
                2 => {
                    let at = (next() as usize) % mutated.len();
                    mutated[at] = next() as u8;
                }
                _ => {
                    let at = (next() as usize) % mutated.len();
                    mutated.insert(at, next() as u8);
                }
            }
            cases.push((schema, mutated));
        }
    }
    cases
}

#[test]
fn protobuf_codec_matches_protobuf_es_on_real_and_hostile_inputs() {
    let mut cases = synthetic_cases();
    cases.extend(mutated_cases());
    let requests: Vec<Value> = cases
        .iter()
        .map(|(schema, bytes)| json!({"op": "protoRoundTrip", "schema": schema, "b64": base64_encode(bytes)}))
        .collect();
    let Some(expected) = common::oracle(&Value::Array(requests)) else {
        return;
    };
    let mut mismatches = Vec::new();
    let mut decoded_by_both = 0;
    for (i, ((schema, bytes), want)) in cases.iter().zip(expected.as_array().unwrap()).enumerate() {
        let got = codec_round_trip(schema, bytes);
        // Decode error messages are compared only for presence: V8's
        // DataView bounds errors word "premature EOF" differently.
        let comparable = |v: &Value| {
            if v["decoded"] == false {
                json!({"decoded": false})
            } else {
                v.clone()
            }
        };
        if comparable(&got) != comparable(want) {
            mismatches.push(format!("#{i} {schema}: rust {got} vs node {want}"));
        }
        if got["decoded"] == true {
            decoded_by_both += 1;
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert!(decoded_by_both > 300, "only {decoded_by_both} cases decoded");
}

// --- the edit pipeline vs icloud-md ----------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Deterministic edits of a note's text: insertions, deletions and
/// replacements at random UTF-16-safe points, some multi-hunk, some unicode.
fn random_edit(text: &str, next: &mut impl FnMut() -> u64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pieces = ["x", " edited ", "\n", "é", "\u{1f600}", "new line\n", "", "\t", "**b**"];
    let mut out = chars.clone();
    for _ in 0..(1 + next() % 3) {
        let at = if out.is_empty() {
            0
        } else {
            (next() as usize) % (out.len() + 1)
        };
        match next() % 3 {
            0 => {
                let piece = pieces[(next() as usize) % pieces.len()];
                for (k, c) in piece.chars().enumerate() {
                    out.insert(at + k, c);
                }
            }
            1 if !out.is_empty() => {
                let end = (at + 1 + (next() as usize) % 6).min(out.len());
                let start = at.min(end.saturating_sub(1));
                out.drain(start..end);
            }
            _ => {
                if at < out.len() {
                    out[at] = 'Z';
                }
            }
        }
    }
    let result: String = out.into_iter().collect();
    // Embeds can't be created or deleted by a push; keep the placeholder
    // count identical so the edit stays in the supported space.
    if result.matches('\u{fffc}').count() != text.matches('\u{fffc}').count() {
        return format!("{text} appended");
    }
    result
}

#[test]
fn apply_text_edit_matches_icloud_md_byte_for_byte_on_real_notes() {
    let replica = [0xc3u8; 16];
    let mut next = xorshift(0xfeed_0000_1111_2222);
    let mut requests = Vec::new();
    let mut plans = Vec::new();
    for file in [
        "real_plain_note.json",
        "real_unicode_note.json",
        "real_first_save_note.json",
        "real_formatted_multi_edit_note.json",
    ] {
        let raw = decompress_note_document(&common::payload(file)).unwrap();
        for variant in 0..6 {
            let mut text = parse_note_document(&raw).unwrap().text;
            let mut steps = Vec::new();
            for _ in 0..(1 + variant % 3) {
                text = random_edit(&text, &mut next);
                steps.push(text.clone());
            }
            // Existing replicas too, not only a fresh one.
            let replica_id = if variant % 2 == 0 {
                replica.to_vec()
            } else {
                parse_note_document(&raw).unwrap().replicas[0].id.clone()
            };
            requests.push(json!({
                "op": "edit",
                "raw": base64_encode(&raw),
                "replicaId": hex(&replica_id),
                "steps": steps.iter().map(|t| json!({"text": t})).collect::<Vec<_>>(),
            }));
            plans.push((raw.clone(), replica_id, steps));
        }
    }
    let Some(expected) = common::oracle(&Value::Array(requests)) else {
        return;
    };
    for ((raw, replica_id, steps), want) in plans.iter().zip(expected.as_array().unwrap()) {
        let mut doc = parse_note_document(raw).unwrap();
        let replica: [u8; 16] = replica_id.as_slice().try_into().unwrap();
        for (step, want) in steps.iter().zip(want["steps"].as_array().unwrap()) {
            match apply_text_edit(&mut doc, step, &ApplyTextEditOptions { replica_id: replica }) {
                Ok(changed) => {
                    assert_eq!(Some(changed), want["changed"].as_bool(), "step {step:?}: {want}");
                    let raw = encode_note_document(&doc).unwrap();
                    assert_eq!(
                        base64_encode(&raw),
                        want["raw"].as_str().unwrap(),
                        "document bytes after {step:?}"
                    );
                    assert_eq!(
                        base64_encode(&compress_note_document(&raw)),
                        want["compressed"].as_str().unwrap(),
                        "compressed bytes after {step:?}"
                    );
                }
                Err(e) => {
                    assert_eq!(Some(e.to_string().as_str()), want["error"].as_str(), "step {step:?}");
                    break;
                }
            }
        }
    }
}

/// The push create path and edit+reconcile path on markdown desired states:
/// buildInitialNoteDocument / applyTextEdit then reconcileNoteFormat, with
/// the same todo uuids.
#[test]
fn text_edit_plus_reconcile_matches_icloud_md_byte_for_byte() {
    let scripts: Vec<Vec<&str>> = vec![
        vec![
            "# Title\n\n- [ ] first todo\n- [x] second",
            "# Title\n\n- [x] first todo\n- [x] second\n- [ ] third",
        ],
        vec![
            "plain line",
            "## plain line",
            "## plain **line** here",
            "1. one\n2. two\n\n5. five",
        ],
        vec![
            "Groceries\n- milk\n- eggs",
            "Groceries\n- milk\n  - skim\n- eggs",
            "> quoted\n> **bold** _it_ ~~gone~~ <u>under</u> [link](https://example.com)",
        ],
        vec![
            "A\n\n```\ncode block\n```\nafter",
            "A\n\n```\ncode block changed\n```\nafter",
        ],
        vec!["- [ ] todo two", "- [ ] todo two\n- [ ] step2 verify line"],
    ];
    let mut markdowns: Vec<&str> = scripts.iter().flatten().copied().collect();
    markdowns.push("Rewritten\n\n- [ ] entirely");
    let parse_requests: Vec<Value> = markdowns
        .iter()
        .map(|m| json!({"op": "parseMarkdown", "markdown": m}))
        .collect();
    let Some(parsed) = common::oracle(&Value::Array(parse_requests)) else {
        return;
    };
    let parsed: std::collections::HashMap<&str, Value> = markdowns
        .iter()
        .copied()
        .zip(parsed.as_array().unwrap().iter().cloned())
        .collect();

    let replica = [0x42u8; 16];
    let uuids: Vec<String> = (1..=64).map(|n| format!("{n:08x}-0000-4000-8000-{n:012x}")).collect();
    let mut requests = Vec::new();
    // Created from scratch, and edited on top of a real note.
    let real = decompress_note_document(&common::payload("real_formatted_multi_edit_note.json")).unwrap();
    for script in &scripts {
        requests.push(json!({"op": "edit", "replicaId": hex(&replica), "uuids": uuids,
            "steps": script.iter().map(|m| json!({"markdown": m})).collect::<Vec<_>>()}));
    }
    requests.push(
        json!({"op": "edit", "raw": base64_encode(&real), "replicaId": hex(&replica), "uuids": uuids,
        "steps": [{"markdown": "Rewritten\n\n- [ ] entirely"}]}),
    );
    let Some(expected) = common::oracle(&Value::Array(requests.clone())) else {
        return;
    };

    for (request, want) in requests.iter().zip(expected.as_array().unwrap()) {
        let mut queue = uuids.clone().into_iter();
        let mut mint = || {
            let s = queue.next().expect("uuid queue");
            let digits: String = s.chars().filter(|c| *c != '-').collect();
            std::array::from_fn(|i| u8::from_str_radix(&digits[2 * i..2 * i + 2], 16).unwrap())
        };
        let mut doc = request
            .get("raw")
            .map(|r| parse_note_document(&base64_decode(r.as_str().unwrap())).unwrap());
        for (step, want) in request["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(want["steps"].as_array().unwrap())
        {
            let markdown = step["markdown"].as_str().unwrap();
            let p = &parsed[markdown];
            let text = p["text"].as_str().unwrap();
            let paragraphs: Vec<icloud_notes_sync::doc::format::FormatParagraph> =
                serde_json::from_value(p["paragraphs"].clone()).unwrap();
            let current = match doc.as_mut() {
                None => {
                    doc = Some(build_initial_note_document(text, &replica).unwrap());
                    doc.as_mut().unwrap()
                }
                Some(d) => {
                    let changed = apply_text_edit(d, text, &ApplyTextEditOptions { replica_id: replica }).unwrap();
                    assert_eq!(Some(changed), want["changed"].as_bool(), "{markdown:?}");
                    d
                }
            };
            let reconciled = reconcile_note_format_with(current, &paragraphs, &replica, &mut mint).unwrap();
            let want_reconciled = &want["reconciled"];
            match reconciled {
                Ok(changed) => assert_eq!(
                    json!({"ok": true, "changed": changed}),
                    *want_reconciled,
                    "{markdown:?}"
                ),
                Err(reason) => assert_eq!(json!({"ok": false, "reason": reason}), *want_reconciled, "{markdown:?}"),
            }
            let raw = encode_note_document(current).unwrap();
            assert_eq!(
                base64_encode(&raw),
                want["raw"].as_str().unwrap(),
                "document bytes after {markdown:?}"
            );
            assert_eq!(
                base64_encode(&compress_note_document(&raw)),
                want["compressed"].as_str().unwrap()
            );
        }
    }
}

// --- the table engine vs icloud-md ------------------------------------------------

#[test]
fn table_edits_match_icloud_md_byte_for_byte() {
    use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
    use icloud_notes_sync::doc::table_edit::prepare_table_attachment_update_with;
    use icloud_notes_sync::doc::tables::{grid_from_table_document, parse_table_document};

    let replica_new = [0xa7u8; 16];
    let replica_low = [0x02u8; 16];
    let mut cases: Vec<(String, Vec<Vec<String>>, [u8; 16])> = Vec::new();
    let sources: Vec<String> = [
        "table_first_revision.json",
        "table_final_revision.json",
        "table_long_lived_rev_2ax.json",
        "table_unsorted_tt_regression.json",
        "table_rev_baseline.json",
    ]
    .iter()
    .map(|f| common::payload_base64(f))
    .chain(
        common::fixture("table_restyle_revisions.json")["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["base64"].as_str().unwrap().to_string()),
    )
    .collect();
    for b64 in &sources {
        let grid = grid_from_table_document(&parse_table_document(&base64_decode(b64)).unwrap()).unwrap();
        let (rows, cols) = (grid.len(), grid[0].len());
        let mut variants: Vec<Vec<Vec<String>>> = Vec::new();
        let mut edited = grid.clone();
        edited[0][0] = format!("{}-edited é", edited[0][0]);
        edited[rows - 1][cols - 1] = String::new();
        variants.push(edited);
        let mut with_row = grid.clone();
        with_row.insert(rows / 2, (0..cols).map(|c| format!("new-{c}")).collect());
        variants.push(with_row);
        if rows > 1 {
            let mut without_row = grid.clone();
            without_row.remove(0);
            variants.push(without_row);
        }
        variants.push(
            grid.iter()
                .map(|r| {
                    let mut r = r.clone();
                    r.insert(1.min(cols), "col \u{1f600}".into());
                    r
                })
                .collect(),
        );
        if cols > 1 {
            variants.push(grid.iter().map(|r| r[1..].to_vec()).collect());
        }
        variants.push(grid.iter().rev().cloned().collect());
        for variant in variants {
            cases.push((b64.clone(), variant.clone(), replica_new));
            cases.push((b64.clone(), variant, replica_low));
        }
    }
    let randoms: Vec<String> = (1..=32u8).map(|n| hex(&[n; 16])).collect();
    let requests: Vec<Value> = cases
        .iter()
        .map(|(b64, grid, replica)| json!({"op": "tableEdit", "b64": b64, "grid": grid, "replicaId": hex(replica), "randoms": randoms}))
        .collect();
    let Some(expected) = common::oracle(&Value::Array(requests)) else {
        return;
    };
    for ((b64, grid, replica), want) in cases.iter().zip(expected.as_array().unwrap()) {
        let record = CloudKitRecord {
            record_name: "T".into(),
            record_type: "Attachment".into(),
            fields: [(
                "MergeableDataEncrypted".to_string(),
                FieldValue {
                    value: json!(b64),
                    type_: "ENCRYPTED_BYTES".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut queue = (1..=32u8).map(|n| [n; 16]);
        let mut random = || queue.next().expect("random queue");
        let got = match prepare_table_attachment_update_with(&record, grid, replica, &mut random) {
            Ok(update) => {
                json!({"ok": true, "changed": update.changed, "mergeableDataBase64": update.mergeable_data_base64})
            }
            Err(reason) => json!({"ok": false, "reason": reason}),
        };
        assert_eq!(got, want["result"], "grid {grid:?} replica {:02x}", replica[0]);
    }
}

// --- classifyNoteRecord vs icloud-md (needs workstream C's Markdown) ------------

#[test]
#[ignore = "needs md::render / md::parse / md::title (workstream C)"]
fn classify_note_record_matches_icloud_md() {
    use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
    use icloud_notes_sync::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
    use icloud_notes_sync::vault::state::TitleMode;

    let mut bodies: Vec<String> = [
        "real_plain_note.json",
        "real_unicode_note.json",
        "real_first_save_note.json",
        "real_formatted_multi_edit_note.json",
    ]
    .iter()
    .map(|f| common::payload_base64(f))
    .collect();
    let mut next = xorshift(0xc1a5_5100_0000_0001);
    for file in ["real_plain_note.json", "real_formatted_multi_edit_note.json"] {
        let raw = decompress_note_document(&common::payload(file)).unwrap();
        for _ in 0..8 {
            let mut doc = parse_note_document(&raw).unwrap();
            let text = random_edit(&doc.text, &mut next);
            apply_text_edit(&mut doc, &text, &ApplyTextEditOptions { replica_id: [9; 16] }).unwrap();
            bodies.push(base64_encode(&compress_note_document(
                &encode_note_document(&doc).unwrap(),
            )));
        }
    }
    let mut requests = Vec::new();
    let mut cases = Vec::new();
    for body in &bodies {
        for mode in [TitleMode::InBody, TitleMode::Filename] {
            let record = json!({"recordName": "R", "recordType": "Note", "fields": {
                "TextDataEncrypted": {"value": body, "type": "ENCRYPTED_BYTES"},
                "TitleEncrypted": {"value": base64_encode("Title".as_bytes()), "type": "ENCRYPTED_BYTES"}}});
            requests.push(json!({"op": "classify", "record": record, "titleMode": mode.as_str()}));
            cases.push((record, mode));
        }
    }
    let Some(expected) = common::oracle(&Value::Array(requests)) else {
        return;
    };
    for ((record, mode), want) in cases.iter().zip(expected.as_array().unwrap()) {
        let record: CloudKitRecord = CloudKitRecord {
            record_name: "R".into(),
            record_type: "Note".into(),
            fields: record["fields"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        FieldValue {
                            value: v["value"].clone(),
                            type_: v["type"].as_str().unwrap().into(),
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        let NoteDecodeResult::Ok(got) = classify_note_record(&record, &ClassifyOptions { title_mode: *mode }) else {
            panic!("expected ok");
        };
        assert_eq!(want["status"], "ok");
        assert_eq!(got.body_text, want["bodyText"].as_str().unwrap());
        assert_eq!(got.markdown_text, want["markdownText"].as_str().unwrap(), "{mode:?}");
        assert_eq!(got.publishable, want["publishable"].as_bool().unwrap());
        assert_eq!(
            got.unpublishable_reason.as_deref(),
            want["unpublishableReason"].as_str()
        );
        assert_eq!(got.title_stripped, want["titleStripped"].as_bool().unwrap_or(false));
        assert_eq!(
            serde_json::to_value(&got.format).unwrap(),
            want.get("format").cloned().unwrap_or(Value::Null)
        );
        assert_eq!(serde_json::to_value(&got.embed_slots).unwrap(), want["embedSlots"]);
        assert_eq!(got.title, want["title"].as_str().unwrap());
        assert_eq!(got.title_line, want["titleLine"].as_str().unwrap());
    }
}

// --- the round-trip gates on mutated documents -----------------------------------

#[test]
fn round_trip_gates_match_icloud_md_on_mutated_documents() {
    use icloud_notes_sync::doc::document::note_document_round_trips;
    use icloud_notes_sync::doc::tables::table_document_round_trips;

    let mut next = xorshift(0x0bad_cafe_0000_0042);
    let mut cases: Vec<(bool, Vec<u8>)> = Vec::new();
    for (_, kind, compressed, _) in common::all_payloads() {
        let raw = decompress_note_document(&compressed).unwrap();
        let is_note = kind == "note";
        cases.push((is_note, raw.clone()));
        for _ in 0..10 {
            let mut mutated = raw.clone();
            let at = (next() as usize) % mutated.len();
            match next() % 3 {
                0 => mutated[at] ^= 1 << (next() % 8),
                1 => mutated[at] = mutated[at].wrapping_add(1),
                _ => mutated.truncate(at.max(1)),
            }
            cases.push((is_note, mutated));
        }
    }
    let requests: Vec<Value> = cases
        .iter()
        .map(|(is_note, raw)| {
            if *is_note {
                json!({"op": "noteRoundTrips", "raw": base64_encode(raw)})
            } else {
                json!({"op": "tableRoundTrips", "compressed": base64_encode(&compress_note_document(raw))})
            }
        })
        .collect();
    let Some(expected) = common::oracle(&Value::Array(requests)) else {
        return;
    };
    let mut trues = 0;
    for (i, ((is_note, raw), want)) in cases.iter().zip(expected.as_array().unwrap()).enumerate() {
        let got = if *is_note {
            note_document_round_trips(raw)
        } else {
            table_document_round_trips(&compress_note_document(raw))
        };
        assert_eq!(Some(got), want.as_bool(), "case #{i} (note: {is_note})");
        trues += usize::from(got);
    }
    assert!(trues > 60, "only {trues} documents round-tripped");
}
