//! node-diff3 3.2.1 and `mergeConflict.ts` against the real thing, over
//! random three-way edits (tests/golden/gen.mts).

mod common;

use common::golden;

use icloud_notes_sync::diff3::*;
use serde_json::{Value, json};

fn array(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect()
}

#[test]
fn merges_match_node_diff3() {
    let golden = golden("diff3.json");
    let cases = golden["merges"].as_array().unwrap();
    for case in cases {
        let (base, local, remote) = (
            case["base"].as_str().unwrap(),
            case["local"].as_str().unwrap(),
            case["remote"].as_str().unwrap(),
        );
        let merged = merge_note_versions(base, local, remote);
        assert_eq!(merged.text, case["merged"]["text"].as_str().unwrap(), "{case}");
        assert_eq!(
            merged.has_conflict,
            case["merged"]["hasConflict"].as_bool().unwrap(),
            "{case}"
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
        assert_eq!(json!({"conflict": conflict, "result": result}), case["plain"], "{case}");

        let comm: Vec<Value> = diff_comm(&local_lines, &remote_lines)
            .into_iter()
            .map(|hunk| match hunk {
                CommHunk::Common(common) => json!({ "common": common }),
                CommHunk::Diff { buffer1, buffer2 } => json!({ "buffer1": buffer1, "buffer2": buffer2 }),
            })
            .collect();
        assert_eq!(Value::Array(comm), case["comm"], "{case}");

        let indices: Vec<Value> = diff_indices(&base_lines, &local_lines)
            .into_iter()
            .map(|h| json!({"buffer1": [h.buffer1.0, h.buffer1.1], "buffer2": [h.buffer2.0, h.buffer2.1]}))
            .collect();
        assert_eq!(Value::Array(indices), case["indices"], "{case}");
    }
    eprintln!("diff3: {} merges match", cases.len());
}

#[test]
fn conflict_markers_match_icloud_md() {
    let golden = golden("diff3.json");
    for case in golden["markers"].as_array().unwrap() {
        assert_eq!(
            has_conflict_markers(case["text"].as_str().unwrap()),
            case["has"].as_bool().unwrap(),
            "{case}"
        );
    }
}
