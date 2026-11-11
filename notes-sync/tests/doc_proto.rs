//! The protobuf decoder's bound on what a document may build: a stream of
//! empty repeated messages, scalars or unknown fields is tiny on the wire but
//! large in memory, and fails past the budget instead of growing.

use icloud_notes_sync::doc::proto::{MAX_DECODED_BYTES, Message, crdt, topotext};

const BUDGET: usize = 1 << 20;

/// `pair` repeated `n` times.
fn stream(pair: [u8; 2], n: usize) -> Vec<u8> {
    pair.repeat(n)
}

#[test]
fn empty_attribute_runs_past_the_budget_fail() {
    let each = std::mem::size_of::<topotext::AttributeRun>();
    // Field 5 `attributeRun`, length 0.
    let fits = stream([0x2a, 0x00], BUDGET / each);
    assert_eq!(
        topotext::String::decode_within(&fits, BUDGET)
            .unwrap()
            .attribute_run
            .len(),
        BUDGET / each
    );
    let over = stream([0x2a, 0x00], BUDGET / each + 1);
    let err = topotext::String::decode_within(&over, BUDGET).unwrap_err();
    assert!(err.0.contains("MiB"), "{err}");
}

#[test]
fn empty_repeated_strings_past_the_budget_fail() {
    // Field 4 `keyItem`, length 0: an empty String each.
    let over = stream([0x22, 0x00], BUDGET / std::mem::size_of::<String>() + 1);
    assert!(crdt::Document::decode_within(&over, BUDGET).is_err());
}

#[test]
fn unknown_fields_past_the_budget_fail() {
    // Field 15, varint 0: undeclared in topotext.String, kept verbatim.
    let over = stream(
        [0x78, 0x00],
        BUDGET / std::mem::size_of::<icloud_notes_sync::doc::proto::UnknownField>() + 1,
    );
    assert!(topotext::String::decode_within(&over, BUDGET).is_err());
}

#[test]
fn the_default_budget_is_far_above_a_large_note() {
    // A hundred thousand formatted runs: a very large real note.
    let runs = 100_000;
    let bytes = stream([0x2a, 0x00], runs);
    assert!(runs * std::mem::size_of::<topotext::AttributeRun>() < MAX_DECODED_BYTES);
    assert_eq!(topotext::String::decode(&bytes).unwrap().attribute_run.len(), runs);
}
