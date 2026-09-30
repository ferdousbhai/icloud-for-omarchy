//! Byte-for-byte comparison of the Markdown renderer/parser against
//! icloud-md 0.6.2's own output (tests/golden/gen.mts).

mod common;

use common::golden;

use icloud_notes_sync::doc::format::{FormatParagraph, InlineSpan};
use icloud_notes_sync::doc::format::{formats_round_trip_equal, normalize_spans, trim_trailing_whitespace};
use icloud_notes_sync::md::parse::{count_quote_markers, parse_note_markdown};
use icloud_notes_sync::md::render::{RawSpelling, render_note_markdown, spelling_candidates};
use serde::Deserialize;
use serde_json::Value;

fn paragraphs(value: &Value) -> Vec<FormatParagraph> {
    Vec::<FormatParagraph>::deserialize(value).unwrap()
}

fn report(what: &str, total: usize, failures: &[String]) {
    report_allowing(what, total, failures, 0);
}

/// `allowed`: known markdown-rs/micromark tokenizer divergences on
/// pathological input (see `parse_matches_icloud_md`); the count may only
/// go down.
fn report_allowing(what: &str, total: usize, failures: &[String], allowed: usize) {
    eprintln!("{what}: {}/{} match", total - failures.len(), total);
    for failure in failures.iter().take(8) {
        eprintln!("--- {failure}");
    }
    assert!(
        failures.len() <= allowed,
        "{what}: {} of {total} differ from icloud-md",
        failures.len()
    );
}

#[test]
fn render_matches_icloud_md() {
    let cases = golden("md_render.json");
    let cases = cases.as_array().unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let input = paragraphs(&case["paragraphs"]);
        let want = case["markdown"].as_str().unwrap();
        let got = render_note_markdown(&input);
        if got != want {
            failures.push(format!("input {}\n want {want:?}\n  got {got:?}", case["paragraphs"]));
        }
    }
    report("render", cases.len(), &failures);
}

/// Parse results by corpus. Real notes, the renderer's own output over
/// the fuzz corpus and the hand-written cases must match exactly, save for
/// three rendered fuzz notes whose GFM strikethrough interleaves with
/// strong emphasis across runs (markdown-rs resolves `~~` together with
/// `*`, micromark after it). The random "soup" corpus additionally hits
/// tokenizer divergences no fixup in `md::mdast_fix` covers: markdown-rs
/// panics on some fence-in-list nestings, tab virtual-space columns in
/// fenced code, lazy HTML-flow lines in list items, indented code followed
/// by multi-line empty items. Their counts are pinned here.
#[test]
fn parse_matches_icloud_md() {
    let cases = golden("md_parse.json");
    let cases = cases.as_array().unwrap();
    let mut failures: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    let mut totals: std::collections::BTreeMap<&str, usize> = Default::default();
    for case in cases {
        let source = case["source"].as_str().unwrap();
        *totals.entry(source).or_default() += 1;
        let markdown = case["markdown"].as_str().unwrap();
        let got = parse_note_markdown(markdown);
        let result = &case["result"];
        let ok = match (&got, result.get("ok"), result.get("unsupported")) {
            (Ok(parsed), Some(want), _) => {
                parsed.text == want["text"].as_str().unwrap() && parsed.paragraphs == paragraphs(&want["paragraphs"])
            }
            (Err(refusal), _, Some(reason)) => refusal.reason == reason.as_str().unwrap(),
            _ => false,
        };
        if !ok {
            failures
                .entry(source)
                .or_default()
                .push(format!("markdown {markdown:?}\n want {result}\n  got {got:?}"));
        }
    }
    for (source, total) in totals {
        let allowed = match source {
            "render" => 3,
            "soup" => 26,
            _ => 0,
        };
        report_allowing(
            &format!("parse ({source})"),
            total,
            failures.get(source).map_or(&[][..], Vec::as_slice),
            allowed,
        );
    }
}

#[test]
fn projection_matches_icloud_md() {
    let cases = golden("md_projection.json");
    let cases = cases.as_array().unwrap();
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let a = paragraphs(&case["a"]);
        let b = paragraphs(&case["b"]);
        if formats_round_trip_equal(&a, &b) != case["equal"].as_bool().unwrap() {
            failures.push(format!("#{i} equal"));
        }
        let normalized: Vec<Vec<InlineSpan>> = a.iter().map(normalize_spans).collect();
        if normalized != Vec::<Vec<InlineSpan>>::deserialize(&case["normalized"]).unwrap() {
            failures.push(format!("#{i} normalized"));
        }
        let trimmed: Vec<FormatParagraph> = a.iter().map(trim_trailing_whitespace).collect();
        if trimmed != paragraphs(&case["trimmed"]) {
            failures.push(format!("#{i} trimmed"));
        }
    }
    report("projection", cases.len(), &failures);
}

#[test]
fn misc_helpers_match_icloud_md() {
    let misc = golden("md_misc.json");
    for case in misc["countQuoteMarkers"].as_array().unwrap() {
        assert_eq!(
            count_quote_markers(case["line"].as_str().unwrap()),
            case["count"].as_u64().unwrap() as usize,
            "{case}"
        );
    }
    for case in misc["spellingCandidates"].as_array().unwrap() {
        let lines: Vec<&str> = case["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        let want: Vec<RawSpelling> = case["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| RawSpelling {
                obsidian: c["obsidian"].as_bool().unwrap(),
                autolink: c["autolink"].as_bool().unwrap(),
            })
            .collect();
        assert_eq!(spelling_candidates(lines.iter().copied()), want, "{case}");
    }
}
