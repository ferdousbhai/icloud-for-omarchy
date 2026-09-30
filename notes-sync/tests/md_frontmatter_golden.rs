//! Frontmatter (`frontmatter.ts`, `noteIdFrontmatter.ts`) against icloud-md
//! 0.6.2's own output. The `yaml` package re-serializes the whole block on
//! any edit; `md::yaml` reproduces that on the YAML subset it models, and
//! everything in that subset must match byte for byte. YAML outside it
//! (comments, block scalars, anchors, ...) falls back to a line edit whose
//! output may differ: those cases are counted, and must still agree on what
//! the keys read back as.

mod common;

use common::golden;

use icloud_notes_sync::md::frontmatter::*;
use icloud_notes_sync::md::yaml::{Parsed, parse_document};
use serde_json::Value;

#[test]
fn split_matches_icloud_md() {
    let golden = golden("md_frontmatter.json");
    for case in golden["split"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let options = SplitOptions {
            filename_as_title: case["filenameAsTitle"].as_bool().unwrap(),
        };
        let got = split_frontmatter(text, options);
        assert_eq!(
            got.frontmatter,
            case["split"]["frontmatter"].as_str().unwrap(),
            "{case}"
        );
        assert_eq!(got.body, case["split"]["body"].as_str().unwrap(), "{case}");
    }
}

/// The YAML between the fences, when the envelope has one.
fn yaml_body(frontmatter: &str) -> Option<String> {
    let lines: Vec<&str> = frontmatter.split('\n').collect();
    if lines[0] != "---" {
        return None;
    }
    let close = lines.iter().skip(1).position(|l| *l == "---")? + 1;
    Some(lines[1..close].join("\n"))
}

#[test]
fn edits_match_icloud_md() {
    let golden = golden("md_frontmatter.json");
    let cases = golden["ops"].as_array().unwrap();
    let mut failures = Vec::new();
    let mut fallback = 0;
    let mut fallback_differ = 0;
    for case in cases {
        let fm = case["frontmatter"].as_str().unwrap();
        let id = case["id"].as_str().unwrap();
        let title = case["title"].as_str().unwrap();
        let compose_title = case.get("composeTitle").and_then(Value::as_str);
        let opt = |v: &Value| v.as_str().map(str::to_string);
        let reads_ok =
            read_note_id(fm) == opt(&case["readNoteId"]) && read_note_title(fm) == opt(&case["readNoteTitle"]);
        let got = [
            ("setNoteId", set_note_id(fm, id)),
            (
                "setNoteIdValid",
                set_note_id(fm, "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1"),
            ),
            ("clearNoteId", clear_note_id(fm)),
            ("setNoteTitle", set_note_title(fm, title)),
            ("clearNoteTitle", clear_note_title(fm)),
            (
                "composeNoteFile",
                compose_note_file(fm, "body\n", "03667D1D-EEE8-4E98-82FB-8C5CD02FD9D1", compose_title),
            ),
        ];
        let differ: Vec<String> = got
            .iter()
            .filter(|(op, value)| case[*op].as_str().unwrap() != value)
            .map(|(op, value)| format!("{op}: want {:?} got {value:?}", case[*op].as_str().unwrap()))
            .collect();
        let modelled = yaml_body(fm).is_none_or(|body| !matches!(parse_document(&body), Parsed::Unsupported));
        if !reads_ok {
            failures.push(format!(
                "frontmatter {fm:?}: reads differ: {:?} {:?}",
                read_note_id(fm),
                read_note_title(fm)
            ));
        } else if !modelled {
            fallback += 1;
            if !differ.is_empty() {
                fallback_differ += 1;
            }
            // The fallback edit must still read back like icloud-md's.
            for (op, value) in &got {
                let want = case[*op].as_str().unwrap();
                if read_note_id(value) != read_note_id(want) || read_note_title(value) != read_note_title(want) {
                    failures.push(format!(
                        "frontmatter {fm:?}: {op} reads back differently: {value:?} vs {want:?}"
                    ));
                }
            }
        } else if !differ.is_empty() {
            failures.push(format!(
                "frontmatter {fm:?} (title {title:?}):\n  {}",
                differ.join("\n  ")
            ));
        }
    }
    eprintln!(
        "frontmatter: {} cases, {} outside the modelled YAML subset ({} of those edit differently), {} failures",
        cases.len(),
        fallback,
        fallback_differ,
        failures.len()
    );
    for failure in failures.iter().take(10) {
        eprintln!("--- {failure}");
    }
    assert!(
        failures.is_empty(),
        "{} frontmatter cases differ from icloud-md",
        failures.len()
    );
}
