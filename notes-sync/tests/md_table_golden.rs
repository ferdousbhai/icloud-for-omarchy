//! Tables (`markdownTable.ts`) against icloud-md 0.6.2's own output.

mod common;

use common::{golden, grid};

use icloud_notes_sync::md::table::{find_markdown_table_blocks, parse_markdown_table, render_markdown_table};
use serde_json::{Value, json};

fn check(what: &str, total: usize, failures: Vec<String>) {
    eprintln!("{what}: {}/{total} match", total - failures.len());
    for failure in failures.iter().take(6) {
        eprintln!("--- {failure}");
    }
    assert!(failures.is_empty(), "{what}: {} of {total} differ", failures.len());
}

#[test]
fn render_matches_icloud_md() {
    let golden = golden("md_table.json");
    let cases = golden["render"].as_array().unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let got = render_markdown_table(&grid(&case["grid"]));
        let want = &case["rendered"];
        let same = match (&got, want.get("ok")) {
            (Ok(md), Some(w)) => md == w.as_str().unwrap(),
            (Err(e), None) => Some(&json!(e)) == want.get("error"),
            _ => false,
        };
        if !same {
            failures.push(format!("grid {}\n want {want}\n  got {got:?}", case["grid"]));
        }
    }
    check("table render", cases.len(), failures);
}

#[test]
fn parse_matches_icloud_md() {
    let golden = golden("md_table.json");
    let cases = golden["parse"].as_array().unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let markdown = case["markdown"].as_str().unwrap();
        let got = parse_markdown_table(markdown);
        let want = &case["result"];
        let same = match (&got, want.get("ok")) {
            (Ok(g), Some(w)) => *g == grid(w),
            (Err(e), None) => Some(&json!(e)) == want.get("error"),
            _ => false,
        };
        if !same {
            failures.push(format!("markdown {markdown:?}\n want {want}\n  got {got:?}"));
        }
    }
    check("table parse", cases.len(), failures);
}

#[test]
fn find_blocks_matches_icloud_md() {
    let golden = golden("md_table.json");
    let cases = golden["find"].as_array().unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let text = case["text"].as_str().unwrap();
        let got: Vec<Value> = find_markdown_table_blocks(text)
            .into_iter()
            .map(|b| json!({"startLine": b.start_line, "endLine": b.end_line, "grid": b.grid}))
            .collect();
        let want = &case["blocks"]["ok"];
        if Value::Array(got.clone()) != *want {
            failures.push(format!("text {text:?}\n want {want}\n  got {got:?}"));
        }
    }
    check("table find", cases.len(), failures);
}
