//! File names and titles (`filename.ts`, `titleFilename.ts`,
//! `noteTitleParagraph.ts`) against icloud-md 0.6.2's own output.

use std::collections::HashSet;

use icloud_notes_sync::md::filename::*;
use icloud_notes_sync::md::title::*;
use icloud_notes_sync::vault::state::TitleMode;
use serde_json::Value;

fn load() -> Value {
    let path = format!("{}/tests/golden/md_names.json.gz", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_reader(flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())).unwrap()
}

#[test]
fn names_match_icloud_md() {
    let golden = load();
    let cases = golden["names"].as_array().unwrap();
    for case in cases {
        let title = case["title"].as_str().unwrap();
        let file = case["fileName"].as_str().unwrap();
        let s = |key: &str| case[key].as_str().map(str::to_string);
        assert_eq!(Some(note_file_name(title)), s("noteFileName"), "noteFileName {case}");
        assert_eq!(
            Some(note_file_name_for(title, TitleMode::InBody)),
            s("inBody"),
            "in-body {case}"
        );
        assert_eq!(
            Some(note_file_name_for(title, TitleMode::Filename)),
            s("filename"),
            "filename {case}"
        );
        assert_eq!(
            title_needing_frontmatter(title, TitleMode::InBody),
            s("needingInBody"),
            "{case}"
        );
        assert_eq!(
            title_needing_frontmatter(title, TitleMode::Filename),
            s("needingFilename"),
            "{case}"
        );
        assert_eq!(
            file_name_carries_title(file, title),
            case["carries"].as_bool().unwrap(),
            "carries {case}"
        );
        assert_eq!(Some(encode_title_stem(title)), s("encoded"), "{case}");
        assert_eq!(Some(decode_title_stem(title)), s("decoded"), "{case}");
        assert_eq!(representability_problem(title), s("problem"), "{case}");
        assert_eq!(Some(carried_title_spelling(title)), s("carried"), "{case}");
        assert_eq!(
            Some(title_from_note_file_name(&format!("Notes/{file}"))),
            s("fromFile"),
            "{case}"
        );
    }
    eprintln!("names: {} cases match", cases.len());
}

#[test]
fn unique_file_names_match_icloud_md() {
    let golden = load();
    for case in golden["unique"].as_array().unwrap() {
        let used: HashSet<String> = case["used"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            unique_file_name(case["fileName"].as_str().unwrap(), &used),
            case["result"].as_str().unwrap(),
            "{case}"
        );
    }
}
