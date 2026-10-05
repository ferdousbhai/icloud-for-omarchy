//! `diff3` (three-way merge, conflict markers) over random three-way edits
//! in the golden corpus (tests/golden/diff3.json.gz), outputs recorded from
//! this crate (`common::check_golden`).

mod common;

use common::check_golden;

use icloud_notes_sync::diff3::*;
use serde_json::{Value, json};

fn array(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect()
}

#[test]
fn merges_golden() {
    check_golden("diff3.json", Some("merges"), |case| {
        let merged = merge_note_versions(
            case["base"].as_str().unwrap(),
            case["local"].as_str().unwrap(),
            case["remote"].as_str().unwrap(),
        );
        let (base_lines, local_lines, remote_lines) = (
            array(&case["lines"]["base"]),
            array(&case["lines"]["local"]),
            array(&case["lines"]["remote"]),
        );
        let options = MergeDiff3Options {
            exclude_false_conflicts: false,
            ..Default::default()
        };
        let (conflict, result) = merge_diff3(&local_lines, &base_lines, &remote_lines, &options);
        let comm: Vec<Value> = diff_comm(&local_lines, &remote_lines)
            .into_iter()
            .map(|hunk| match hunk {
                CommHunk::Common(common) => json!({ "common": common }),
                CommHunk::Diff { buffer1, buffer2 } => json!({ "buffer1": buffer1, "buffer2": buffer2 }),
            })
            .collect();
        let indices: Vec<Value> = diff_indices(&base_lines, &local_lines)
            .into_iter()
            .map(|h| json!({"buffer1": [h.buffer1.0, h.buffer1.1], "buffer2": [h.buffer2.0, h.buffer2.1]}))
            .collect();
        vec![
            ("merged", json!({ "text": merged.text, "hasConflict": merged.has_conflict })),
            ("plain", json!({ "conflict": conflict, "result": result })),
            ("comm", json!(comm)),
            ("indices", json!(indices)),
        ]
    });
}

#[test]
fn conflict_markers_golden() {
    check_golden("diff3.json", Some("markers"), |case| {
        vec![("has", json!(has_conflict_markers(case["text"].as_str().unwrap())))]
    });
}
