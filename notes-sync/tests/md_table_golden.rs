//! Markdown tables over the golden corpus (tests/golden/md_table.json.gz),
//! outputs recorded from this crate (`common::check_golden`).

mod common;

use common::{check_golden, grid};

use icloud_notes_sync::md::table::{find_markdown_table_blocks, parse_markdown_table, render_markdown_table};
use serde_json::{Value, json};

fn result<T: serde::Serialize>(got: Result<T, String>) -> Value {
    match got {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error }),
    }
}

#[test]
fn render_golden() {
    check_golden("md_table.json", Some("render"), |case| {
        vec![("rendered", result(render_markdown_table(&grid(&case["grid"]))))]
    });
}

#[test]
fn parse_golden() {
    check_golden("md_table.json", Some("parse"), |case| {
        vec![(
            "result",
            result(parse_markdown_table(case["markdown"].as_str().unwrap())),
        )]
    });
}

#[test]
fn find_blocks_golden() {
    check_golden("md_table.json", Some("find"), |case| {
        let blocks: Vec<Value> = find_markdown_table_blocks(case["text"].as_str().unwrap())
            .into_iter()
            .map(|b| json!({"startLine": b.start_line, "endLine": b.end_line, "grid": b.grid}))
            .collect();
        vec![("blocks", json!({ "ok": blocks }))]
    });
}
