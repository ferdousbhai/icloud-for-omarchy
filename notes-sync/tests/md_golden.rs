//! The Markdown renderer/parser over the golden corpora (tests/golden/): real
//! notes, hand-written edge cases and seeded fuzz inputs, each with the
//! output recorded from this crate (`common::check_golden`; re-record with
//! `ICLOUD_NOTES_SYNC_REGEN=1`).

mod common;

use common::check_golden;

use icloud_notes_sync::doc::format::{FormatParagraph, InlineSpan};
use icloud_notes_sync::doc::format::{formats_round_trip_equal, normalize_spans, trim_trailing_whitespace};
use icloud_notes_sync::md::parse::{count_quote_markers, parse_note_markdown};
use icloud_notes_sync::md::render::{render_note_markdown, spelling_candidates};
use serde::Deserialize;
use serde_json::{Value, json};

fn paragraphs(value: &Value) -> Vec<FormatParagraph> {
    Vec::<FormatParagraph>::deserialize(value).unwrap()
}

#[test]
fn render_golden() {
    check_golden("md_render.json", None, |case| {
        vec![("markdown", json!(render_note_markdown(&paragraphs(&case["paragraphs"]))))]
    });
}

/// Over real notes, the renderer's output on the fuzz corpus (`source:
/// "render"`), hand-written cases and a random line "soup".
#[test]
fn parse_golden() {
    check_golden("md_parse.json", None, |case| {
        let result = match parse_note_markdown(case["markdown"].as_str().unwrap()) {
            Ok(parsed) => json!({ "ok": { "paragraphs": parsed.paragraphs, "text": parsed.text } }),
            Err(refusal) => json!({ "unsupported": refusal.reason }),
        };
        vec![("result", result)]
    });
}

#[test]
fn projection_golden() {
    check_golden("md_projection.json", None, |case| {
        let a = paragraphs(&case["a"]);
        let b = paragraphs(&case["b"]);
        let normalized: Vec<Vec<InlineSpan>> = a.iter().map(normalize_spans).collect();
        let trimmed: Vec<FormatParagraph> = a.iter().map(trim_trailing_whitespace).collect();
        vec![
            ("equal", json!(formats_round_trip_equal(&a, &b))),
            ("normalized", json!(normalized)),
            ("trimmed", json!(trimmed)),
        ]
    });
}

#[test]
fn count_quote_markers_golden() {
    check_golden("md_misc.json", Some("countQuoteMarkers"), |case| {
        vec![("count", json!(count_quote_markers(case["line"].as_str().unwrap())))]
    });
}

#[test]
fn spelling_candidates_golden() {
    check_golden("md_misc.json", Some("spellingCandidates"), |case| {
        let lines = case["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap());
        let candidates: Vec<Value> = spelling_candidates(lines)
            .into_iter()
            .map(|c| json!({ "obsidian": c.obsidian, "autolink": c.autolink }))
            .collect();
        vec![("candidates", json!(candidates))]
    });
}
