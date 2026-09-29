//! Ports icloud-md `src/notes/cloneState.test.ts`, plus byte-exact checks of
//! state.json's per-write-path key order.

use std::path::Path;

use icloud_notes_sync::GENERATOR;
use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::vault::state::{
    Account, AttachmentEntry, CLONE_WRITE_ORDER, CURRENT_LAYOUT_VERSION, CloneState, FOLDER_CREATE_ORDER, FolderEntry,
    NOTE_ADD_ORDER, NOTE_CREATE_ORDER, NoteEntry, PULL_WRITE_ORDER, SharerHomeEntry, TableAttachmentEntry, TitleMode,
    TrashedEntry, read_clone_state, write_clone_state,
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
    std::fs::create_dir_all(dir.join(".icloud-md")).unwrap();
    std::fs::write(dir.join(".icloud-md/state.json"), json).unwrap();
}

fn state_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".icloud-md/state.json")).unwrap()
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

// --- byte-exact key order per write path -------------------------------------

/// What `runClone` hands `writeCloneState` for the tiny cassette.
fn tiny_clone_state() -> CloneState {
    let mut entry = NoteEntry::new("Notes/Test Note.md", "25q", 1_752_564_000_000).with_order(NOTE_ADD_ORDER);
    entry.folder_record_name = Some("DefaultFolder-CloudKit".into());
    CloneState {
        account: Some(Account {
            apple_id: "harness@example.com".into(),
            dsid: "10000000001".into(),
        }),
        title_mode: Some(TitleMode::InBody),
        sync_token: Some("AQAAAAAAAAAB".into()),
        shared_zone_sync_tokens: Some(IndexMap::new()),
        notes: notes(vec![("03667d1d-eee8-4e98-82fb-8c5cd02fd9d1", entry)]),
        folders: Some(
            [("DefaultFolder-CloudKit".to_owned(), FolderEntry::new("Notes", "Notes"))]
                .into_iter()
                .collect(),
        ),
        sharer_homes: Some(IndexMap::new()),
        attachments: Some(IndexMap::new()),
        table_attachments: Some(IndexMap::new()),
        key_order: Some(CLONE_WRITE_ORDER.to_vec()),
        ..Default::default()
    }
}

#[test]
fn clone_write_matches_icloud_md_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &tiny_clone_state()).unwrap();
    let expected = std::fs::read_to_string("tests/differential/expected/tiny-clone/vault/.icloud-md/state.json")
        .unwrap()
        .replace("\"icloud-md 0.6.2\"", &format!("\"{GENERATOR}\""));
    assert_eq!(state_text(dir.path()), expected);
}

#[test]
fn read_modify_write_keeps_read_order() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &tiny_clone_state()).unwrap();
    let mut state = read_clone_state(dir.path()).unwrap().unwrap();
    state.replica_id = Some("AQIDBAUGBwgJCgsMDQ4PEA==".into());
    state.notes["03667d1d-eee8-4e98-82fb-8c5cd02fd9d1"].pending_rename = Some("X.md".into());
    state.notes["03667d1d-eee8-4e98-82fb-8c5cd02fd9d1"].frontmatter_title = Some("T".into());
    write_clone_state(dir.path(), &state).unwrap();
    let expected = format!(
        r#"{{
  "layoutVersion": 3,
  "generator": "{GENERATOR}",
  "titleMode": "in-body",
  "account": {{
    "appleId": "harness@example.com",
    "dsid": "10000000001"
  }},
  "syncToken": "AQAAAAAAAAAB",
  "sharedZoneSyncTokens": {{}},
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
}

#[test]
fn a_read_state_without_generator_keeps_its_slot() {
    // readCloneState rebuilds every key, `generator: undefined` included, so
    // the stamped generator lands second, not last.
    let dir = tempfile::tempdir().unwrap();
    write_raw(dir.path(), r#"{"layoutVersion":3,"notes":{}}"#);
    let state = read_clone_state(dir.path()).unwrap().unwrap();
    write_clone_state(dir.path(), &state).unwrap();
    assert_eq!(
        state_text(dir.path()),
        format!(
            "{{\n  \"layoutVersion\": 3,\n  \"generator\": \"{GENERATOR}\",\n  \"titleMode\": \"in-body\",\n  \"notes\": {{}}\n}}\n"
        )
    );
}

#[test]
fn pull_write_order_appends_layout_version_and_generator() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = tiny_clone_state();
    state.key_order = Some(PULL_WRITE_ORDER.to_vec());
    state.replica_id = Some("R".into());
    state.trashed = Some(IndexMap::new());
    write_clone_state(dir.path(), &state).unwrap();
    let text = state_text(dir.path());
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "account",
            "syncToken",
            "sharedZoneSyncTokens",
            "replicaId",
            "titleMode",
            "notes",
            "folders",
            "sharerHomes",
            "attachments",
            "tableAttachments",
            "trashed",
            "layoutVersion",
            "generator"
        ]
    );
    assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
}

#[test]
fn entry_orders_per_construction_site() {
    let mut create = NoteEntry::new("Notes/New.md", "t", 1).with_order(NOTE_CREATE_ORDER);
    create.folder_record_name = Some("F".into());
    create.shared_zone_owner = Some("_o".into());
    let keys = |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&create.to_json()),
        [
            "file",
            "recordChangeTag",
            "modificationDate",
            "folderRecordName",
            "sharedZoneOwner"
        ]
    );

    let mut add = NoteEntry::new("a.md", "t", 1).with_order(NOTE_ADD_ORDER);
    add.frontmatter_title = Some("T".into());
    add.folder_record_name = Some("F".into());
    // A key assigned later that the construction site didn't have appends.
    add.pending_rename = Some("P.md".into());
    assert_eq!(
        keys(&add.to_json()),
        [
            "file",
            "recordChangeTag",
            "modificationDate",
            "folderRecordName",
            "frontmatterTitle",
            "pendingRename"
        ]
    );

    let mut folder = FolderEntry::new("Sub", "Sub");
    folder.parent_record_name = Some("P".into());
    assert_eq!(keys(&folder.to_json()), ["name", "parentRecordName", "dirName"]);
    folder.key_order = Some(FOLDER_CREATE_ORDER.to_vec());
    assert_eq!(keys(&folder.to_json()), ["name", "dirName", "parentRecordName"]);
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
