//! File names and titles (`md::filename`, `md::title`) over the golden
//! corpus (tests/golden/md_names.json.gz), outputs recorded from this crate
//! (`common::check_golden`).

mod common;

use common::check_golden;

use std::collections::HashSet;

use icloud_notes_sync::md::filename::*;
use icloud_notes_sync::md::title::*;
use icloud_notes_sync::vault::state::TitleMode;
use serde_json::json;

#[test]
fn names_golden() {
    check_golden("md_names.json", Some("names"), |case| {
        let title = case["title"].as_str().unwrap();
        let file = case["fileName"].as_str().unwrap();
        vec![
            ("noteFileName", json!(note_file_name(title))),
            ("inBody", json!(note_file_name_for(title, TitleMode::InBody))),
            ("filename", json!(note_file_name_for(title, TitleMode::Filename))),
            ("needingInBody", json!(title_needing_frontmatter(title, TitleMode::InBody))),
            ("needingFilename", json!(title_needing_frontmatter(title, TitleMode::Filename))),
            ("carries", json!(file_name_carries_title(file, title))),
            ("encoded", json!(encode_title_stem(title))),
            ("decoded", json!(decode_title_stem(title))),
            ("problem", json!(representability_problem(title))),
            ("carried", json!(carried_title_spelling(title))),
            ("fromFile", json!(title_from_note_file_name(&format!("Notes/{file}")))),
        ]
    });
}

#[test]
fn unique_file_names_golden() {
    check_golden("md_names.json", Some("unique"), |case| {
        let used: HashSet<String> = case["used"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        vec![("result", json!(unique_file_name(case["fileName"].as_str().unwrap(), &used)))]
    });
}
