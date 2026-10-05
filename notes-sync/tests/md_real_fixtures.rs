//! The real captured notes and tables (tests/fixtures/real): the Markdown
//! rendered from each decoded format / table grid, against its golden.

use icloud_notes_sync::doc::format::FormatParagraph;
use icloud_notes_sync::doc::format::formats_round_trip_equal;
use icloud_notes_sync::md::parse::parse_note_markdown;
use icloud_notes_sync::md::render::render_note_markdown;
use icloud_notes_sync::md::table::render_markdown_table;
use serde_json::Value;

fn fixtures() -> Vec<Value> {
    let dir = format!("{}/tests/fixtures/real", env!("CARGO_MANIFEST_DIR"));
    let index: Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/index.json")).unwrap()).unwrap();
    index
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let file = format!("{dir}/{}", entry["file"].as_str().unwrap());
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
        })
        .collect()
}

#[test]
fn real_notes_render_like_icloud_md() {
    let mut notes = 0;
    for fixture in fixtures().iter().filter(|f| f["kind"] == "note") {
        let golden = &fixture["golden"];
        let format: Vec<FormatParagraph> = serde_json::from_value(golden["format"].clone()).unwrap();
        let markdown = render_note_markdown(&format);
        assert_eq!(markdown, golden["markdown"].as_str().unwrap(), "{}", fixture["name"]);
        let back = parse_note_markdown(&markdown).unwrap();
        assert!(
            formats_round_trip_equal(&format, &back.paragraphs),
            "{}",
            fixture["name"]
        );
        notes += 1;
    }
    assert_eq!(notes, 4);
}

#[test]
fn real_tables_render_like_icloud_md() {
    let mut tables = 0;
    for fixture in fixtures() {
        let goldens: Vec<&Value> = match fixture["kind"].as_str().unwrap() {
            "table" => vec![&fixture["golden"]],
            "tableRevisions" => fixture["revisions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| &r["golden"])
                .collect(),
            _ => continue,
        };
        for golden in goldens {
            let Some(markdown) = golden["markdown"].as_str() else {
                continue;
            };
            let grid: Vec<Vec<String>> = serde_json::from_value(golden["grid"].clone()).unwrap();
            assert_eq!(render_markdown_table(&grid).unwrap(), markdown, "{}", fixture["name"]);
            tables += 1;
        }
    }
    assert!(tables > 10, "{tables}");
}
