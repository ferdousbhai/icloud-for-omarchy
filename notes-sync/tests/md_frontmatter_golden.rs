//! Frontmatter (`md::frontmatter`) over the golden corpus
//! (tests/golden/md_frontmatter.json.gz): splitting, and reading and editing
//! the note id/title keys over hand-written and generated YAML, outputs
//! recorded from this crate (`common::check_golden`).

mod common;

use common::check_golden;

use icloud_notes_sync::md::frontmatter::*;
use serde_json::{Value, json};

#[test]
fn split_golden() {
    check_golden("md_frontmatter.json", Some("split"), |case| {
        let options = SplitOptions {
            filename_as_title: case["filenameAsTitle"].as_bool().unwrap(),
        };
        let got = split_frontmatter(case["text"].as_str().unwrap(), options);
        vec![("split", json!({ "frontmatter": got.frontmatter, "body": got.body }))]
    });
}

#[test]
fn edits_golden() {
    check_golden("md_frontmatter.json", Some("ops"), |case| {
        let fm = case["frontmatter"].as_str().unwrap();
        let id = case["id"].as_str().unwrap();
        let title = case["title"].as_str().unwrap();
        let compose_title = case.get("composeTitle").and_then(Value::as_str);
        vec![
            ("readNoteId", json!(read_note_id(fm))),
            ("readNoteTitle", json!(read_note_title(fm))),
            ("setNoteId", json!(set_note_id(fm, id))),
            (
                "setNoteIdValid",
                json!(set_note_id(fm, "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1")),
            ),
            ("clearNoteId", json!(clear_note_id(fm))),
            ("setNoteTitle", json!(set_note_title(fm, title))),
            ("clearNoteTitle", json!(clear_note_title(fm))),
            (
                "composeNoteFile",
                json!(compose_note_file(
                    fm,
                    "body\n",
                    "03667D1D-EEE8-4E98-82FB-8C5CD02FD9D1",
                    compose_title
                )),
            ),
        ]
    });
}
