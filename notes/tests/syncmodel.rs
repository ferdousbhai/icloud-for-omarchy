//! fixtures/syncmodel.json holds calls of SyncModel and NoteTree, each with
//! the result the Qt app's C++ gave, which this port was held to before the
//! C++ went: the merge of unsaved edits, the editor's characters, the
//! sidebar tree and the rest of the pure logic. The results stay as they
//! were, to the byte.

use std::collections::{BTreeMap, HashMap};

use icloud_notes::notetree::{self, Listed, Row};
use icloud_notes::syncmodel;
use serde_json::{Map, Value, json};

/// The file as JSON this crate can parse. The C++ cuts a snippet at 140
/// UTF-16 units, through a character when one straddles the cut, and its
/// JSON then carries half a surrogate pair (`\ud83d`), which is not Unicode
/// text. This crate cuts at the same place and gives U+FFFD for the half.
fn parseable(fixtures: &str) -> String {
    let high = |escape: &[u8]| {
        [&b"\\ud8"[..], b"\\ud9", b"\\uda", b"\\udb"]
            .iter()
            .any(|start| escape.starts_with(start))
    };
    let mut out = String::with_capacity(fixtures.len());
    let mut rest = fixtures;
    while let Some(at) = rest.find('\\') {
        out += &rest[..at];
        rest = &rest[at..];
        if high(rest.as_bytes()) && !rest[6..].starts_with("\\ud") {
            out += "\\ufffd";
            rest = &rest[6..];
        } else {
            // An escaped backslash is not the start of the next escape.
            let escaped = rest[1..].chars().next().map_or(0, char::len_utf8);
            out += &rest[..1 + escaped];
            rest = &rest[1 + escaped..];
        }
    }
    out + rest
}

fn text(args: &[Value], i: usize) -> &str {
    args[i].as_str().unwrap_or_default()
}

fn strings(value: &Value) -> Vec<String> {
    let items = value.as_array().into_iter().flatten();
    items
        .map(|item| item.as_str().unwrap_or_default().to_string())
        .collect()
}

/// A note as the fixtures give one: whatever it carries rides along.
#[derive(Clone)]
struct AnyNote(Map<String, Value>);

impl Listed for AnyNote {
    fn path(&self) -> &str {
        self.0["path"].as_str().unwrap()
    }

    fn folder(&self) -> &str {
        self.0["folder"].as_str().unwrap()
    }
}

/// A row as the C++ hands one to its list: every row carries every key.
fn row_json(row: &Row<AnyNote>) -> Value {
    let mut out = match &row.note {
        Some(note) => note.0.clone(),
        None => json!({"folder": row.folder, "path": "", "file": "", "title": "", "modifiedMs": 0})
            .as_object()
            .unwrap()
            .clone(),
    };
    out.insert("kind".into(), json!(if row.note.is_some() { "note" } else { "folder" }));
    out.insert("depth".into(), json!(row.depth));
    out.insert("count".into(), json!(row.count));
    out.insert("expandable".into(), json!(row.expandable));
    out.insert("expanded".into(), json!(row.expanded));
    Value::Object(out)
}

/// One call, as the fixtures name it.
fn call(function: &str, a: &[Value]) -> Value {
    match function {
        "conflictBody" => json!(syncmodel::conflict_body(text(a, 0), text(a, 1), text(a, 2))),
        "twoWayConflictBody" => json!(syncmodel::two_way_conflict_body(text(a, 0), text(a, 1), text(a, 2))),
        "restoreEditorChars" => json!(syncmodel::restore_editor_chars(text(a, 0), text(a, 1))),
        "editorForm" => json!(syncmodel::editor_form(text(a, 0))),
        "splitEnvelope" => {
            let (envelope, body) = syncmodel::split_envelope(text(a, 0));
            json!({"envelope": envelope, "body": body})
        }
        "extractNoteId" => json!(syncmodel::extract_note_id(text(a, 0))),
        "isSyncConflictName" => json!(syncmodel::is_sync_conflict_name(text(a, 0))),
        "hasConflictMarkers" => json!(syncmodel::has_conflict_markers(text(a, 0))),
        "parseConflicts" => syncmodel::parse_conflicts(text(a, 0))
            .iter()
            .map(|h| json!({"first": h.first, "last": h.last, "local": h.local, "base": h.base, "remote": h.remote}))
            .collect(),
        "hasUnreadableConflicts" => json!(syncmodel::has_unreadable_conflicts(text(a, 0))),
        "stripConflictMarkers" => json!(syncmodel::strip_conflict_markers(text(a, 0))),
        "resolveConflicts" => json!(syncmodel::resolve_conflicts(text(a, 0), &strings(&a[1]))),
        "linesMissingFrom" => json!(syncmodel::lines_missing_from(&strings(&a[0]), &strings(&a[1]))),
        "hasTable" => json!(syncmodel::has_table(text(a, 0))),
        "sortFolders" => {
            let mut folders = strings(&a[0]);
            syncmodel::sort_folders(&mut folders, text(a, 1));
            json!(folders)
        }
        "retitleInBody" => json!(syncmodel::retitle_in_body(text(a, 0), text(a, 1))),
        "parseStatusJson" => Value::Object(syncmodel::parse_status_json(text(a, 0).as_bytes())),
        "parseHistoryJson" => Value::Object(syncmodel::parse_history_json(text(a, 0).as_bytes())),
        "stripMarkdownLead" => json!(syncmodel::strip_markdown_lead(text(a, 0))),
        "previewNote" => {
            let preview = syncmodel::preview_note(text(a, 0), text(a, 1), text(a, 2));
            json!({"title": preview.title, "snippet": preview.snippet})
        }
        "toggleCheckbox" => json!(syncmodel::toggle_checkbox(text(a, 0), a[1].as_i64().unwrap())),
        "parentOf" => json!(notetree::parent_of(text(a, 0))),
        "noteTreeRows" => {
            let counts: HashMap<String, i64> = a[1]
                .as_object()
                .unwrap()
                .iter()
                .map(|(folder, count)| (folder.clone(), count.as_i64().unwrap()))
                .collect();
            let notes: Vec<AnyNote> = a[2]
                .as_array()
                .unwrap()
                .iter()
                .map(|note| AnyNote(note.as_object().unwrap().clone()))
                .collect();
            let rows = notetree::rows(
                &strings(&a[0]),
                &counts,
                &notes,
                &strings(&a[3]),
                a[4].as_bool().unwrap(),
                &strings(&a[5]),
            );
            rows.iter().map(row_json).collect()
        }
        other => panic!("fixtures call {other}, which this test does not know"),
    }
}

#[test]
fn the_port_answers_every_fixture_as_the_cpp_did() {
    // sort_folders collates by the process's locale: the same one wherever
    // this runs, and the one the fixtures were dumped in.
    // SAFETY: this is the only test of this binary, so nothing else reads or
    // changes the environment while it runs.
    unsafe { std::env::set_var("LC_ALL", "C.UTF-8") };
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/syncmodel.json");
    let cases: Vec<Value> = serde_json::from_str(&parseable(&std::fs::read_to_string(path).unwrap())).unwrap();
    let mut counted: BTreeMap<String, usize> = BTreeMap::new();
    let mut differing = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let function = case["function"].as_str().unwrap();
        *counted.entry(function.to_string()).or_default() += 1;
        let result = call(function, case["args"].as_array().unwrap());
        if result != case["result"] {
            differing.push(format!(
                "case {i}, {function}{}\n   C++: {}\n  Rust: {result}",
                case["args"], case["result"]
            ));
        }
    }
    assert!(
        differing.is_empty(),
        "{} differ:\n{}",
        differing.len(),
        differing.join("\n")
    );
    // Every function the C++ dumper knows is in the file, the merge most of all.
    assert_eq!(counted.len(), 23, "{counted:?}");
    assert!(cases.len() >= 954 && counted["conflictBody"] >= 262, "{counted:?}");
}
