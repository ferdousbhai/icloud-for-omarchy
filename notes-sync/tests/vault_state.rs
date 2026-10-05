//! state.json: round trips, validation, and what it looks like on disk.
//! Originally derived from icloud-md's tests.

use std::path::Path;

use icloud_notes_sync::GENERATOR;
use icloud_notes_sync::cloudkit::{SharedDatabaseCursor, ZoneId};
use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::vault::state::{
    Account, AttachmentEntry, CURRENT_LAYOUT_VERSION, CloneState, FolderEntry, NoteEntry, SharerHomeEntry,
    TableAttachmentEntry, TitleMode, TrashedEntry, read_clone_state, write_clone_state,
};
use indexmap::IndexMap;

fn notes(entries: Vec<(&str, NoteEntry)>) -> IndexMap<String, NoteEntry> {
    entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn base(sync_token: &str, entries: Vec<(&str, NoteEntry)>) -> CloneState {
    CloneState {
        sync_token: Some(sync_token.into()),
        notes: notes(entries),
        ..Default::default()
    }
}

fn round_trip(state: &CloneState) -> CloneState {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), state).unwrap();
    read_clone_state(dir.path()).unwrap().unwrap()
}

fn write_raw(dir: &Path, json: &str) {
    std::fs::create_dir_all(dir.join(".icloud-notes")).unwrap();
    std::fs::write(dir.join(".icloud-notes/state.json"), json).unwrap();
}

fn state_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".icloud-notes/state.json")).unwrap()
}

#[test]
fn round_trips_shared_zone_owners_and_per_zone_sync_tokens() {
    let mut shared = NoteEntry::new("Cooking Recipes (REC).md", "b", 2);
    shared.shared_zone_owner = Some("_owner1".into());
    let mut state = base(
        "private-token",
        vec![
            ("REC-PRIVATE", NoteEntry::new("Own Note (REC).md", "a", 1)),
            ("REC-SHARED", shared),
        ],
    );
    state.shared_zone_sync_tokens = Some(
        [("_owner1".to_owned(), "shared-token-1".to_owned())]
            .into_iter()
            .collect(),
    );
    let back = round_trip(&state);
    assert_eq!(back.shared_zone_sync_tokens, state.shared_zone_sync_tokens);
    assert_eq!(back.notes["REC-SHARED"].shared_zone_owner.as_deref(), Some("_owner1"));
    assert_eq!(back.notes["REC-PRIVATE"].shared_zone_owner, None);
    assert_eq!(back.sync_token.as_deref(), Some("private-token"));
}

#[test]
fn round_trips_unpublishable_reason_absent_means_publishable() {
    let reason = "contains embedded content this tool can't parse (com.apple.notes.table)";
    let mut degraded = NoteEntry::new("Table Note (REC).md", "a", 1);
    degraded.unpublishable_reason = Some(reason.into());
    let back = round_trip(&base(
        "token",
        vec![
            ("REC-DEGRADED", degraded),
            ("REC-CLEAN", NoteEntry::new("Plain Note (REC).md", "b", 2)),
        ],
    ));
    assert_eq!(back.notes["REC-DEGRADED"].unpublishable_reason.as_deref(), Some(reason));
    assert_eq!(back.notes["REC-CLEAN"].unpublishable_reason, None);
}

#[test]
fn round_trips_pending_rename() {
    let mut pending = NoteEntry::new("Shopping list.md", "a", 1);
    pending.pending_rename = Some("Groceries.md".into());
    let back = round_trip(&base(
        "token",
        vec![
            ("REC-PENDING", pending),
            ("REC-SETTLED", NoteEntry::new("Plain.md", "b", 2)),
        ],
    ));
    assert_eq!(
        back.notes["REC-PENDING"].pending_rename.as_deref(),
        Some("Groceries.md")
    );
    assert_eq!(back.notes["REC-SETTLED"].pending_rename, None);
}

#[test]
fn round_trips_frontmatter_title() {
    let mut keyed = NoteEntry::new("Untitled.md", "a", 1);
    keyed.frontmatter_title = Some("Restaurants ".into());
    let back = round_trip(&base(
        "token",
        vec![("REC-KEYED", keyed), ("REC-PLAIN", NoteEntry::new("Plain.md", "b", 2))],
    ));
    assert_eq!(
        back.notes["REC-KEYED"].frontmatter_title.as_deref(),
        Some("Restaurants ")
    );
    assert_eq!(back.notes["REC-PLAIN"].frontmatter_title, None);
}

#[test]
fn reads_a_pre_shared_notes_state_file() {
    let back = round_trip(&base(
        "old-token",
        vec![("REC-1", NoteEntry::new("Note (REC1).md", "tag", 5))],
    ));
    assert_eq!(back.shared_zone_sync_tokens, None);
    assert_eq!(back.notes["REC-1"].shared_zone_owner, None);
    assert_eq!(back.notes["REC-1"].file, "Note (REC1).md");
}

#[test]
fn round_trips_the_shared_database_cursor() {
    let mut state = base("t", vec![]);
    state.shared_database = Some(SharedDatabaseCursor {
        sync_token: "db-token".into(),
        zones: vec![ZoneId {
            zone_name: "Notes".into(),
            owner_record_name: Some("_owner1".into()),
        }],
        listed_at: 42,
    });
    assert_eq!(round_trip(&state).shared_database, state.shared_database);
}

/// Older vaults have no `sharedDatabase`; a malformed one is only a lost
/// cache (the next pull lists the shared zones from scratch), not corruption.
#[test]
fn a_missing_or_malformed_shared_database_cursor_reads_as_none() {
    assert_eq!(round_trip(&base("t", vec![])).shared_database, None);
    for bad in [
        r#""x""#,
        r#"{"syncToken": 1, "zones": [], "listedAt": 0}"#,
        r#"{"syncToken": "x"}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_raw(
            dir.path(),
            &format!(r#"{{"layoutVersion": 3, "syncToken": "t", "notes": {{}}, "sharedDatabase": {bad}}}"#),
        );
        let state = read_clone_state(dir.path()).unwrap().unwrap();
        assert_eq!(state.shared_database, None, "{bad}");
        assert_eq!(state.sync_token.as_deref(), Some("t"));
    }
}

#[test]
fn round_trips_the_bound_account() {
    let mut state = base("token", vec![]);
    state.account = Some(Account {
        apple_id: "me@example.com".into(),
        dsid: "1234".into(),
    });
    assert_eq!(round_trip(&state).account, state.account);
}

#[test]
fn reads_a_pre_account_binding_state_file() {
    assert_eq!(round_trip(&base("old-token", vec![])).account, None);
}

#[test]
fn refuses_a_pre_folder_layout_vault() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(
        dir.path(),
        r#"{"syncToken":"old","notes":{"REC-1":{"file":"Note.md","recordChangeTag":"t","modificationDate":1}}}"#,
    );
    assert!(matches!(
        read_clone_state(dir.path()),
        Err(Error::UnsupportedVaultLayout { .. })
    ));
}

#[test]
fn corrupt_for_a_malformed_account_field() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(
        dir.path(),
        &format!(
            r#"{{"layoutVersion":{CURRENT_LAYOUT_VERSION},"notes":{{}},"account":{{"appleId":"me@example.com"}}}}"#
        ),
    );
    assert!(matches!(read_clone_state(dir.path()), Err(Error::CorruptStateFile(_))));
}

#[test]
fn round_trips_table_attachments() {
    let mut state = base(
        "token",
        vec![("REC-TABLE", NoteEntry::new("Table Note (REC).md", "a", 1))],
    );
    state.table_attachments = Some(
        [(
            "ATT-1".to_owned(),
            TableAttachmentEntry {
                note_record_name: "REC-TABLE".into(),
            },
        )]
        .into_iter()
        .collect(),
    );
    assert_eq!(round_trip(&state).table_attachments, state.table_attachments);
}

#[test]
fn reads_a_pre_table_history_state_file() {
    assert_eq!(round_trip(&base("old-token", vec![])).table_attachments, None);
}

#[test]
fn corrupt_for_a_malformed_table_attachment_entry() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(
        dir.path(),
        &format!(r#"{{"layoutVersion":{CURRENT_LAYOUT_VERSION},"notes":{{}},"tableAttachments":{{"ATT-1":{{}}}}}}"#),
    );
    assert!(matches!(read_clone_state(dir.path()), Err(Error::CorruptStateFile(_))));
}

#[test]
fn round_trips_the_folder_tree_and_note_membership() {
    let mut pie = NoteEntry::new("Recipes/Pie.md", "a", 1);
    pie.folder_record_name = Some("FOLDER-1".into());
    let mut state = base(
        "token",
        vec![
            ("REC-1", pie),
            ("REC-SHARED", NoteEntry::new("Cooking Recipes.md", "b", 2)),
        ],
    );
    let mut desserts = FolderEntry::new("Desserts", "Desserts");
    desserts.parent_record_name = Some("FOLDER-1".into());
    let mut shared = FolderEntry::new("Shared Recipes", "Shared Recipes");
    shared.shared_zone_owner = Some("_owner1".into());
    shared.permission = Some("READ_WRITE".into());
    state.folders = Some(
        [
            ("DefaultFolder-CloudKit", FolderEntry::new("Notes", "Notes")),
            ("FOLDER-1", FolderEntry::new("Recipes", "Recipes")),
            ("FOLDER-2", desserts),
            ("FOLDER-SHARED", shared),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    );
    let back = round_trip(&state);
    let folders = back.folders.as_ref().unwrap();
    assert_eq!(folders["FOLDER-1"].dir_name, "Recipes");
    assert_eq!(folders["FOLDER-1"].parent_record_name, None);
    assert_eq!(folders["FOLDER-2"].parent_record_name.as_deref(), Some("FOLDER-1"));
    assert_eq!(folders["DefaultFolder-CloudKit"].name, "Notes");
    assert_eq!(folders["FOLDER-SHARED"].permission.as_deref(), Some("READ_WRITE"));
    assert_eq!(folders["FOLDER-1"].permission, None);
    assert_eq!(back.notes["REC-1"].folder_record_name.as_deref(), Some("FOLDER-1"));
    assert_eq!(back.notes["REC-SHARED"].folder_record_name, None);
}

#[test]
fn reads_a_pre_folder_support_state_file() {
    assert_eq!(round_trip(&base("old-token", vec![])).folders, None);
}

#[test]
fn corrupt_for_a_malformed_folder_entry() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(
        dir.path(),
        &format!(
            r#"{{"layoutVersion":{CURRENT_LAYOUT_VERSION},"notes":{{}},"folders":{{"FOLDER-1":{{"name":"Recipes"}}}}}}"#
        ),
    );
    assert!(matches!(read_clone_state(dir.path()), Err(Error::CorruptStateFile(_))));
}

#[test]
fn round_trips_the_trash_registry() {
    let mut state = base("token", vec![]);
    state.trashed = Some(
        [(
            "REC-1".to_owned(),
            TrashedEntry {
                file: "Gone.md".into(),
                trashed_at: 1_784_216_572_571,
            },
        )]
        .into_iter()
        .collect(),
    );
    assert_eq!(round_trip(&state).trashed, state.trashed);
}

#[test]
fn reads_a_pre_trash_registry_state_file() {
    assert_eq!(round_trip(&base("old-token", vec![])).trashed, None);
}

#[test]
fn corrupt_for_a_malformed_trashed_entry() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(
        dir.path(),
        &format!(
            r#"{{"layoutVersion":{CURRENT_LAYOUT_VERSION},"notes":{{}},"trashed":{{"REC-1":{{"file":"Gone.md"}}}}}}"#
        ),
    );
    assert!(matches!(read_clone_state(dir.path()), Err(Error::CorruptStateFile(_))));
}

// --- what state.json looks like on disk --------------------------------------

/// What a clone of the tiny cassette writes.
fn tiny_clone_state() -> CloneState {
    let mut entry = NoteEntry::new("Notes/Test Note.md", "25q", 1_752_564_000_000);
    entry.folder_record_name = Some("DefaultFolder-CloudKit".into());
    CloneState {
        account: Some(Account {
            apple_id: "harness@example.com".into(),
            dsid: "10000000001".into(),
        }),
        title_mode: Some(TitleMode::InBody),
        sync_token: Some("AQAAAAAAAAAB".into()),
        shared_zone_sync_tokens: Some(IndexMap::new()),
        shared_database: Some(SharedDatabaseCursor {
            sync_token: "AQAAAAAAAAAC".into(),
            zones: Vec::new(),
            listed_at: 1_790_000_000_000,
        }),
        notes: notes(vec![("03667d1d-eee8-4e98-82fb-8c5cd02fd9d1", entry)]),
        folders: Some(
            [("DefaultFolder-CloudKit".to_owned(), FolderEntry::new("Notes", "Notes"))]
                .into_iter()
                .collect(),
        ),
        sharer_homes: Some(IndexMap::new()),
        attachments: Some(IndexMap::new()),
        table_attachments: Some(IndexMap::new()),
        ..Default::default()
    }
}

#[test]
fn clone_write_matches_the_recorded_clone_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &tiny_clone_state()).unwrap();
    let expected = std::fs::read_to_string("tests/differential/expected/tiny-clone/vault/.icloud-notes/state.json")
        .unwrap()
        .replace("\"<generator>\"", &format!("\"{GENERATOR}\""));
    assert_eq!(state_text(dir.path()), expected);
}

#[test]
fn writes_keys_in_field_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = tiny_clone_state();
    state.replica_id = Some("AQIDBAUGBwgJCgsMDQ4PEA==".into());
    // Set in another order than the fields'.
    state.notes["03667d1d-eee8-4e98-82fb-8c5cd02fd9d1"].frontmatter_title = Some("T".into());
    state.notes["03667d1d-eee8-4e98-82fb-8c5cd02fd9d1"].pending_rename = Some("X.md".into());
    write_clone_state(dir.path(), &state).unwrap();
    let expected = format!(
        r#"{{
  "layoutVersion": {CURRENT_LAYOUT_VERSION},
  "generator": "{GENERATOR}",
  "titleMode": "in-body",
  "account": {{
    "appleId": "harness@example.com",
    "dsid": "10000000001"
  }},
  "syncToken": "AQAAAAAAAAAB",
  "sharedZoneSyncTokens": {{}},
  "sharedDatabase": {{
    "syncToken": "AQAAAAAAAAAC",
    "zones": [],
    "listedAt": 1790000000000
  }},
  "replicaId": "AQIDBAUGBwgJCgsMDQ4PEA==",
  "notes": {{
    "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1": {{
      "file": "Notes/Test Note.md",
      "recordChangeTag": "25q",
      "modificationDate": 1752564000000,
      "folderRecordName": "DefaultFolder-CloudKit",
      "pendingRename": "X.md",
      "frontmatterTitle": "T"
    }}
  }},
  "folders": {{
    "DefaultFolder-CloudKit": {{
      "name": "Notes",
      "dirName": "Notes"
    }}
  }},
  "sharerHomes": {{}},
  "attachments": {{}},
  "tableAttachments": {{}}
}}
"#
    );
    assert_eq!(state_text(dir.path()), expected);
    // Reading it back and writing it again changes nothing.
    let back = read_clone_state(dir.path()).unwrap().unwrap();
    write_clone_state(dir.path(), &back).unwrap();
    assert_eq!(state_text(dir.path()), expected);
}

#[test]
fn reads_keys_in_any_order_and_keeps_unknown_top_level_keys() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(
        dir.path(),
        &format!(
            r#"{{"notes":{{"R":{{"modificationDate":1,"someday":true,"recordChangeTag":"t","file":"a.md"}}}},
                "futureKey":{{"x":[1,2]}},"syncToken":"s","layoutVersion":{CURRENT_LAYOUT_VERSION}}}"#
        ),
    );
    let state = read_clone_state(dir.path()).unwrap().unwrap();
    assert_eq!(state.notes["R"], NoteEntry::new("a.md", "t", 1));
    assert_eq!(state.sync_token.as_deref(), Some("s"));
    write_clone_state(dir.path(), &state).unwrap();
    let value: serde_json::Value = serde_json::from_str(&state_text(dir.path())).unwrap();
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "layoutVersion",
            "generator",
            "titleMode",
            "syncToken",
            "notes",
            "futureKey"
        ]
    );
    assert_eq!(value["futureKey"], serde_json::json!({"x": [1, 2]}));
    // Unknown keys inside an entry are not kept.
    assert!(value["notes"]["R"].get("someday").is_none());
}

#[test]
fn entries_serialize_in_field_order() {
    let keys = |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    let mut note = NoteEntry::new("a.md", "t", 1);
    note.pending_rename = Some("P.md".into());
    note.frontmatter_title = Some("T".into());
    note.shared_zone_owner = Some("_o".into());
    note.folder_record_name = Some("F".into());
    assert_eq!(
        keys(&note.to_json()),
        [
            "file",
            "recordChangeTag",
            "modificationDate",
            "sharedZoneOwner",
            "folderRecordName",
            "pendingRename",
            "frontmatterTitle"
        ]
    );
    let mut folder = FolderEntry::new("Sub", "Sub");
    folder.parent_record_name = Some("P".into());
    assert_eq!(keys(&folder.to_json()), ["name", "parentRecordName", "dirName"]);
}

#[test]
fn nested_maps_serialize_in_field_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = base("t", vec![]);
    state.sharer_homes = Some(
        [(
            "_o".to_owned(),
            SharerHomeEntry {
                name: "Pat".into(),
                dir_name: "Pat".into(),
            },
        )]
        .into_iter()
        .collect(),
    );
    state.attachments = Some(
        [(
            "A".to_owned(),
            AttachmentEntry {
                file: "Notes/attachments/x.jpeg".into(),
                media_record_name: "M".into(),
                media_file_checksum: "C".into(),
                note_record_name: "N".into(),
            },
        )]
        .into_iter()
        .collect(),
    );
    write_clone_state(dir.path(), &state).unwrap();
    let text = state_text(dir.path());
    assert!(text.contains(
        "\"A\": {\n      \"file\": \"Notes/attachments/x.jpeg\",\n      \"mediaRecordName\": \"M\",\n      \"mediaFileChecksum\": \"C\",\n      \"noteRecordName\": \"N\"\n    }"
    ));
    assert!(text.contains("\"_o\": {\n      \"name\": \"Pat\",\n      \"dirName\": \"Pat\"\n    }"));
}
