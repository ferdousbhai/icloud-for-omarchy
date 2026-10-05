//! Ports icloud-md `src/commands/push.test.ts`.

mod common;

use common::{no_network, read, write_vault_file};

use std::path::Path;

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::plan::{PlanEntry, PlanEntryKind, PlanResolution};
use icloud_notes_sync::cmd::push::{
    BuildPushPlanResult, PushOptions, apply_remote_merge, build_push_plan, merge_remote_change, run_push_with,
};
use icloud_notes_sync::vault::base::{read_base_copy, write_base_copy};
use icloud_notes_sync::vault::local::{LocalFileState, local_file_state};
use icloud_notes_sync::vault::state::{
    AttachmentEntry, CloneState, FolderEntry, NoteEntry, SharerHomeEntry, TableAttachmentEntry, TitleMode,
    write_clone_state,
};
use indexmap::IndexMap;

/// A folder-layout vault: "Notes", "Recipes", and a sharer ("Pat") with one
/// shared folder.
fn state() -> CloneState {
    let mut folders = IndexMap::new();
    folders.insert("DefaultFolder-CloudKit".to_owned(), FolderEntry::new("Notes", "Notes"));
    folders.insert("F-RECIPES".to_owned(), FolderEntry::new("Recipes", "Recipes"));
    let mut shared = FolderEntry::new("Shared Recipes", "Shared Recipes");
    shared.shared_zone_owner = Some("_owner1".into());
    folders.insert("F-SHARED".to_owned(), shared);
    let mut homes = IndexMap::new();
    homes.insert(
        "_owner1".to_owned(),
        SharerHomeEntry {
            name: "Pat".into(),
            dir_name: "Pat".into(),
        },
    );
    let mut notes = IndexMap::new();
    let mut rec1 = NoteEntry::new("Notes/Tracked.md", "1a", 100);
    rec1.folder_record_name = Some("DefaultFolder-CloudKit".into());
    notes.insert("REC1".to_owned(), rec1);
    CloneState {
        sync_token: Some("token".into()),
        folders: Some(folders),
        sharer_homes: Some(homes),
        notes,
        ..Default::default()
    }
}

fn empty_state() -> CloneState {
    CloneState {
        notes: IndexMap::new(),
        ..state()
    }
}

fn empty_state_with_shared_permission(permission: &str) -> CloneState {
    let mut s = empty_state();
    s.folders.as_mut().unwrap()["F-SHARED"].permission = Some(permission.into());
    s
}

fn shared_note(file: &str, folder: Option<&str>) -> NoteEntry {
    let mut e = NoteEntry::new(file, "1a", 100);
    e.folder_record_name = folder.map(Into::into);
    e.shared_zone_owner = Some("_owner1".into());
    e
}

fn with_notes(mut s: CloneState, notes: Vec<(&str, NoteEntry)>) -> CloneState {
    s.notes = notes.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    s
}

fn plan(dir: &Path) -> BuildPushPlanResult {
    match build_push_plan(&no_network(), dir, &mut |_| {}) {
        Ok(p) => p,
        Err(e) => panic!("plan failed: {e}"),
    }
}

fn plan_entries(dir: &Path) -> Vec<PlanEntry> {
    plan(dir).entries.into_iter().map(|e| e.entry).collect()
}

fn plan_err(dir: &Path) -> Error {
    match build_push_plan(&no_network(), dir, &mut |_| {}) {
        Ok(_) => panic!("expected the plan to fail"),
        Err(e) => e,
    }
}

fn assert_unbound(dir: &Path) {
    let err = plan_err(dir);
    assert!(
        matches!(err, Error::UnboundAccount { .. }),
        "expected UnboundAccount, got {err:?}"
    );
}

fn reason(e: &PlanEntry) -> &str {
    e.reason.as_deref().unwrap_or("")
}

#[test]
fn refuses_when_theres_no_cloned_state() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(plan_err(dir.path()), Error::NotClonedDirectory { .. }));
}

#[test]
fn untracked_md_in_known_folder_is_a_create_candidate_reaching_network() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    write_vault_file(dir.path(), "Recipes/New Note.md", "Hello");
    assert_unbound(dir.path());
}

/// A note at the top level of the vault goes in the default folder, as one
/// made outside any folder does in Notes.
#[test]
fn loose_top_level_md_is_a_create_candidate_reaching_network() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(dir.path(), "Loose.md", "Hello");
    assert_unbound(dir.path());
}

#[test]
fn md_in_unknown_directory_is_a_real_change() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(dir.path(), "Brand New Folder/Note.md", "Hello");
    assert_unbound(dir.path());
}

#[test]
fn refuses_md_loose_at_top_of_sharer_home() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(dir.path(), "Pat/Loose.md", "Hello");
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Create);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("loose at the top of a sharer's area"));
}

#[test]
fn new_md_inside_shared_folder_is_a_create_candidate() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(dir.path(), "Pat/Shared Recipes/Mine.md", "Hello");
    assert_unbound(dir.path());
}

#[test]
fn refuses_new_md_in_read_only_shared_folder() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state_with_shared_permission("READ_ONLY")).unwrap();
    write_vault_file(dir.path(), "Pat/Shared Recipes/Mine.md", "Hello");
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("read access"));
}

#[test]
fn refuses_edit_in_read_only_shared_folder() {
    let dir = tempfile::tempdir().unwrap();
    let s = with_notes(
        empty_state_with_shared_permission("READ_ONLY"),
        vec![("SH1", shared_note("Pat/Shared Recipes/Theirs.md", Some("F-SHARED")))],
    );
    write_clone_state(dir.path(), &s).unwrap();
    write_base_copy(dir.path(), "SH1", "Hello").unwrap();
    write_vault_file(dir.path(), "Pat/Shared Recipes/Theirs.md", "Hello edited");
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Update);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("read-only for you"));
}

#[test]
fn edit_in_writable_shared_folder_reaches_network() {
    let dir = tempfile::tempdir().unwrap();
    let s = with_notes(
        empty_state_with_shared_permission("READ_WRITE"),
        vec![("SH1", shared_note("Pat/Shared Recipes/Theirs.md", Some("F-SHARED")))],
    );
    write_clone_state(dir.path(), &s).unwrap();
    write_base_copy(dir.path(), "SH1", "Hello").unwrap();
    write_vault_file(dir.path(), "Pat/Shared Recipes/Theirs.md", "Hello edited");
    assert_unbound(dir.path());
}

#[test]
fn refuses_edit_to_individually_shared_note() {
    let dir = tempfile::tempdir().unwrap();
    let s = with_notes(empty_state(), vec![("LOOSE1", shared_note("Pat/Travel List.md", None))]);
    write_clone_state(dir.path(), &s).unwrap();
    write_base_copy(dir.path(), "LOOSE1", "Hello").unwrap();
    write_vault_file(dir.path(), "Pat/Travel List.md", "Hello edited");
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("individually-shared"));
}

fn keyed_loose_state(frontmatter_title: Option<&str>) -> CloneState {
    let mut e = shared_note("Pat/Untitled.md", None);
    e.frontmatter_title = frontmatter_title.map(Into::into);
    let mut s = with_notes(empty_state(), vec![("LOOSE1", e)]);
    s.title_mode = Some(TitleMode::Filename);
    s
}

#[test]
fn recorded_apple_note_title_reads_as_synced() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &keyed_loose_state(Some("Restaurants "))).unwrap();
    write_base_copy(dir.path(), "LOOSE1", "Hello").unwrap();
    write_vault_file(
        dir.path(),
        "Pat/Untitled.md",
        "---\napple-note-id: LOOSE1\napple-note-title: \"Restaurants \"\n---\n\nHello",
    );
    assert!(plan_entries(dir.path()).is_empty());
}

#[test]
fn refuses_genuine_retitle_via_key_of_individually_shared_note() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &keyed_loose_state(Some("Restaurants "))).unwrap();
    write_base_copy(dir.path(), "LOOSE1", "Hello").unwrap();
    write_vault_file(
        dir.path(),
        "Pat/Untitled.md",
        "---\napple-note-id: LOOSE1\napple-note-title: \"A new name \"\n---\n\nHello",
    );
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("individually-shared"));
}

#[test]
fn defers_title_only_refusal_to_live_record_when_state_predates_frontmatter_title() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &keyed_loose_state(None)).unwrap();
    write_base_copy(dir.path(), "LOOSE1", "Hello").unwrap();
    write_vault_file(
        dir.path(),
        "Pat/Untitled.md",
        "---\napple-note-id: LOOSE1\napple-note-title: \"Restaurants \"\n---\n\nHello",
    );
    assert_unbound(dir.path());
}

#[test]
fn refuses_locally_deleted_shared_note_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let s = with_notes(
        empty_state(),
        vec![("SH1", shared_note("Pat/Shared Recipes/Theirs.md", Some("F-SHARED")))],
    );
    write_clone_state(dir.path(), &s).unwrap();
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Delete);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("deleting notes shared by someone else isn't supported"));
    assert!(reason(&entries[0]).contains("restore"));
}

#[test]
fn pairs_renamed_shared_note_into_refused_move() {
    let dir = tempfile::tempdir().unwrap();
    let s = with_notes(
        empty_state(),
        vec![("SH1", shared_note("Pat/Shared Recipes/Theirs.md", Some("F-SHARED")))],
    );
    write_clone_state(dir.path(), &s).unwrap();
    write_base_copy(dir.path(), "SH1", "Hello").unwrap();
    write_vault_file(dir.path(), "Pat/Shared Recipes/Renamed.md", "Hello");
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Move);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert_eq!(
        entries[0].previous_file.as_deref(),
        Some("Pat/Shared Recipes/Theirs.md")
    );
    assert!(reason(&entries[0]).contains("renaming or moving notes shared by someone else"));
}

#[test]
fn refuses_empty_untracked_file_locally() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(dir.path(), "Notes/Empty.md", "");
    let entries: Vec<_> = plan_entries(dir.path()).iter().map(PlanEntry::serialize).collect();
    let json = serde_json::to_value(&entries).unwrap();
    assert_eq!(
        json,
        serde_json::json!([{"kind": "create", "file": "Notes/Empty.md", "resolution": "refused", "reason": "the file is empty - nothing to create"}])
    );
}

#[test]
fn refuses_untracked_file_with_conflict_markers() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(
        dir.path(),
        "Notes/Conflicted.md",
        "a\n<<<<<<< local\nb\n=======\nc\n>>>>>>> remote\n",
    );
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Create);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("conflict markers"));
}

#[test]
fn refuses_untracked_file_referencing_attachments() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(
        dir.path(),
        "Notes/HasAttachment.md",
        "Look:\n\n![pic](attachments/pic.jpg)\n",
    );
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("attachments"));
}

#[test]
fn ignores_a_file_already_tracked() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Synced text");
    write_clone_state(dir.path(), &state()).unwrap();
    assert!(plan_entries(dir.path()).is_empty());
}

#[test]
fn ignores_md_files_inside_attachments_directories() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Synced text");
    write_vault_file(dir.path(), "Notes/attachments/Nested.md", "Hello");
    write_clone_state(dir.path(), &state()).unwrap();
    assert!(plan_entries(dir.path()).is_empty());
}

#[test]
fn missing_tracked_file_needs_a_live_check() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn pairs_missing_file_with_identical_untracked_one_as_move() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Recipes/Tracked.md", "Synced text");
    write_clone_state(dir.path(), &state()).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn refuses_local_move_into_sharer_area_as_a_move() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Pat/Tracked.md", "Synced text");
    write_clone_state(dir.path(), &state()).unwrap();
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Move);
    assert_eq!(entries[0].previous_file.as_deref(), Some("Notes/Tracked.md"));
    assert_eq!(entries[0].file, "Pat/Tracked.md");
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("sharer's area"));
}

#[test]
fn pairs_moved_and_edited_note_by_unique_basename() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Pat/Tracked.md", "Edited after moving");
    write_clone_state(dir.path(), &state()).unwrap();
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Move);
    assert_eq!(entries[0].previous_file.as_deref(), Some("Notes/Tracked.md"));
}

fn attachment(file: &str) -> AttachmentEntry {
    AttachmentEntry {
        file: file.into(),
        media_record_name: "MEDIA1".into(),
        media_file_checksum: "abc".into(),
        note_record_name: "REC1".into(),
    }
}

#[test]
fn refuses_moving_a_note_with_tracked_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    s.attachments = Some(
        [("ATT1".to_owned(), attachment("Notes/attachments/pic.jpg"))]
            .into_iter()
            .collect(),
    );
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Recipes/Tracked.md", "Synced text");
    write_clone_state(dir.path(), &s).unwrap();
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Move);
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("has attachments"));
}

#[test]
fn deleting_a_note_with_a_tracked_attachment_reaches_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    s.attachments = Some(
        [("ATT1".to_owned(), attachment("Notes/attachments/keep.jpg"))]
            .into_iter()
            .collect(),
    );
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_clone_state(dir.path(), &s).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn deleting_a_note_with_a_tracked_table_attachment_reaches_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    s.table_attachments = Some(
        [(
            "ATT-TABLE-1".to_owned(),
            TableAttachmentEntry {
                note_record_name: "REC1".into(),
            },
        )]
        .into_iter()
        .collect(),
    );
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_clone_state(dir.path(), &s).unwrap();
    assert_unbound(dir.path());
}

// --- remote-change merges ---------------------------------------------------------

#[test]
fn remote_merge_keeps_merged_file_modified() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let entry = s.notes["REC1"].clone();
    write_base_copy(dir.path(), "REC1", "line one\n\nline two\n").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "line one edited locally\n\nline two\n");
    let merged = merge_remote_change(dir.path(), "REC1", "", "line one edited locally\n\nline two\n", "line one\n\nline two edited remotely\n", "2b").unwrap();
    apply_remote_merge(dir.path(), &mut s, "REC1", &entry, &merged).unwrap();
    assert!(!merged.has_conflict);
    assert_eq!(
        read(dir.path(), "Notes/Tracked.md"),
        "line one edited locally\n\nline two edited remotely\n"
    );
    assert_eq!(
        read_base_copy(dir.path(), "REC1").unwrap().as_deref(),
        Some("line one\n\nline two edited remotely\n")
    );
    assert_eq!(
        local_file_state(dir.path(), &s.notes["REC1"], "REC1", TitleMode::InBody).unwrap(),
        LocalFileState::Modified
    );
    assert_eq!(s.notes["REC1"].record_change_tag, "2b");
}

#[test]
fn remote_merge_with_tag_only_bump_leaves_edit_uploadable() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let entry = s.notes["REC1"].clone();
    write_base_copy(dir.path(), "REC1", "shared text\n").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "shared text plus my edit\n");
    let merged = merge_remote_change(dir.path(), "REC1", "", "shared text plus my edit\n", "shared text\n", "2b").unwrap();
    apply_remote_merge(dir.path(), &mut s, "REC1", &entry, &merged).unwrap();
    assert_eq!(read(dir.path(), "Notes/Tracked.md"), "shared text plus my edit\n");
    assert_eq!(
        read_base_copy(dir.path(), "REC1").unwrap().as_deref(),
        Some("shared text\n")
    );
    assert_eq!(
        local_file_state(dir.path(), &s.notes["REC1"], "REC1", TitleMode::InBody).unwrap(),
        LocalFileState::Modified
    );
    assert_eq!(s.notes["REC1"].record_change_tag, "2b");
}

#[test]
fn remote_merge_preserves_frontmatter() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let entry = s.notes["REC1"].clone();
    write_base_copy(dir.path(), "REC1", "body\n").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "---\nkeep: me\n---\n\nbody edited\n");
    let merged = merge_remote_change(dir.path(), "REC1", "---\nkeep: me\n---\n\n", "body edited\n", "body\n", "2b").unwrap();
    apply_remote_merge(dir.path(), &mut s, "REC1", &entry, &merged).unwrap();
    let written = read(dir.path(), "Notes/Tracked.md");
    assert!(written.starts_with("---\nkeep: me\n---\n"));
    assert!(written.ends_with("body edited\n"));
}

#[test]
fn remote_merge_conflict_writes_markers_and_keeps_base() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    let entry = s.notes["REC1"].clone();
    write_base_copy(dir.path(), "REC1", "shared line\n").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "shared line edited locally\n");
    let merged = merge_remote_change(dir.path(), "REC1", "", "shared line edited locally\n", "shared line edited remotely\n", "2b").unwrap();
    apply_remote_merge(dir.path(), &mut s, "REC1", &entry, &merged).unwrap();
    assert!(merged.has_conflict);
    let written = read(dir.path(), "Notes/Tracked.md");
    assert!(written.contains("<<<<<<< local"));
    assert!(written.contains(">>>>>>> remote"));
    assert_eq!(
        read_base_copy(dir.path(), "REC1").unwrap().as_deref(),
        Some("shared line\n")
    );
    assert_eq!(s.notes["REC1"].record_change_tag, "2b");
}

#[test]
fn never_re_merges_a_file_that_still_carries_conflict_markers() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "shared line\n").unwrap();
    write_vault_file(
        dir.path(),
        "Notes/Tracked.md",
        "<<<<<<< local\nshared line edited locally\n=======\nshared line edited remotely\n>>>>>>> remote\n",
    );
    write_clone_state(dir.path(), &state()).unwrap();
    let entries = plan(dir.path()).entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry.resolution, PlanResolution::Conflict);
    assert!(reason(&entries[0].entry).contains("still contains diff3 conflict markers"));
    assert!(entries[0].action.is_none());
}

#[test]
fn run_push_returns_no_entries_and_zero_pushed_when_plan_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Synced text");
    write_clone_state(dir.path(), &state()).unwrap();
    let result = run_push_with(&no_network(), dir.path(), &mut |_| {}, &PushOptions::default()).unwrap();
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"dryRun":false,"entries":[],"unchanged":1,"notices":[],"pushed":0}"#
    );
}

// --- id-in-frontmatter pairing ------------------------------------------------

const NOTE_ID: &str = "089D915D-C76E-4F44-AB80-2190073281A3";
const OTHER_NOTE_ID: &str = "001b9e8a-c474-4311-af32-abe70026b346";

fn id_state() -> CloneState {
    let base = state();
    let rec1 = base.notes["REC1"].clone();
    with_notes(base, vec![(NOTE_ID, rec1)])
}

fn with_id(id: &str, body: &str) -> String {
    format!("---\napple-note-id: {id}\n---\n\n{body}")
}

#[test]
fn id_pairs_a_note_renamed_moved_and_edited_at_once() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(
        dir.path(),
        "Pat/Totally Different.md",
        &with_id(NOTE_ID, "Edited after renaming"),
    );
    write_clone_state(dir.path(), &id_state()).unwrap();
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, PlanEntryKind::Move);
    assert_eq!(entries[0].previous_file.as_deref(), Some("Notes/Tracked.md"));
    assert_eq!(entries[0].file, "Pat/Totally Different.md");
}

#[test]
fn envelope_stripped_falls_back_to_delete_plus_create() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Pat/Totally Different.md", "Edited after renaming");
    write_clone_state(dir.path(), &id_state()).unwrap();
    assert_unbound(dir.path());
}

/// A copy of a tracked note's file is a new note, as a duplicated note is
/// in Notes: the create gives it its own id.
#[test]
fn copy_with_original_in_place_is_a_create() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Synced text");
    write_vault_file(dir.path(), "Recipes/Tracked copy.md", &with_id(NOTE_ID, "Synced text"));
    write_clone_state(dir.path(), &id_state()).unwrap();
    assert_unbound(dir.path());
}

/// A byte-identical twin of the tracked file is a copy like any other.
#[test]
fn byte_identical_twin_is_a_create() {
    let dir = tempfile::tempdir().unwrap();
    let file = with_id(NOTE_ID, "Synced text");
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", &file);
    write_vault_file(dir.path(), "Notes/Tracked 2.md", &file);
    write_clone_state(dir.path(), &id_state()).unwrap();
    assert_unbound(dir.path());
}

/// Without the id line the same copy is an ordinary new note.
#[test]
fn copy_without_the_id_line_is_still_a_create() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", &with_id(NOTE_ID, "Synced text"));
    write_vault_file(dir.path(), "Notes/Tracked 2.md", "Synced text");
    write_clone_state(dir.path(), &id_state()).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn duplicate_id_claims_without_original_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Recipes/Pie.md", &with_id(NOTE_ID, "Synced text"));
    write_vault_file(dir.path(), "Recipes/Pie copy.md", &with_id(NOTE_ID, "Synced text"));
    write_clone_state(dir.path(), &id_state()).unwrap();
    let entries = plan_entries(dir.path());
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry.resolution, PlanResolution::Refused);
        assert!(reason(entry).contains("apple-note-id"));
        assert!(reason(entry).contains("line from every copy but one"));
    }
    assert!(!entries.iter().any(|e| e.kind == PlanEntryKind::Move));
    assert!(!entries.iter().any(|e| e.kind == PlanEntryKind::Delete));
}

#[test]
fn id_from_another_vault_plans_as_new_note_with_notice() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Synced text");
    write_vault_file(
        dir.path(),
        "Pat/From Elsewhere.md",
        &with_id(OTHER_NOTE_ID, "Someone else's note"),
    );
    write_clone_state(dir.path(), &id_state()).unwrap();
    let result = plan(dir.path());
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].entry.kind, PlanEntryKind::Create);
    assert_eq!(result.notices.len(), 1);
    assert!(
        result.notices[0]
            .message
            .contains("doesn't track - pushing it as a new note")
    );
}

#[test]
fn tracked_file_with_stripped_frontmatter_is_still_that_note() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Edited, and the id is gone");
    write_clone_state(dir.path(), &id_state()).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn malformed_id_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), NOTE_ID, "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "Synced text");
    write_vault_file(
        dir.path(),
        "Pat/Broken.md",
        "---\napple-note-id: not-a-uuid\n---\n\nA new note",
    );
    write_clone_state(dir.path(), &id_state()).unwrap();
    let result = plan(dir.path());
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].entry.kind, PlanEntryKind::Create);
    assert!(result.notices.is_empty());
}

// --- filename-as-title vaults -------------------------------------------------

fn title_mode_state() -> CloneState {
    CloneState {
        title_mode: Some(TitleMode::Filename),
        ..state()
    }
}

#[test]
fn empty_file_is_a_title_only_note_in_filename_vault() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &with_notes(title_mode_state(), vec![])).unwrap();
    write_vault_file(dir.path(), "Recipes/Sourdough.md", "");
    assert_unbound(dir.path());
}

#[test]
fn empty_file_in_in_body_vault_is_nothing_to_create() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &empty_state()).unwrap();
    write_vault_file(dir.path(), "Recipes/Sourdough.md", "");
    let entries = plan_entries(dir.path());
    assert_eq!(entries[0].resolution, PlanResolution::Refused);
    assert!(reason(&entries[0]).contains("the file is empty"));
}

#[test]
fn emptying_body_in_filename_vault_is_an_ordinary_edit() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Tracked.md", "");
    write_clone_state(dir.path(), &title_mode_state()).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn renaming_a_note_with_attachments_in_place_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = title_mode_state();
    s.attachments = Some(
        [("ATT1".to_owned(), attachment("Notes/attachments/pic.jpg"))]
            .into_iter()
            .collect(),
    );
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(dir.path(), "Notes/Renamed.md", "Synced text");
    write_clone_state(dir.path(), &s).unwrap();
    assert_unbound(dir.path());
}

// --- apple-note-title as a retitle request ----------------------------------------

#[test]
fn clean_files_apple_note_title_goes_to_the_network() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(
        dir.path(),
        "Notes/Tracked.md",
        "---\napple-note-title: \"A new title\"\n---\nSynced text",
    );
    write_clone_state(dir.path(), &title_mode_state()).unwrap();
    assert_unbound(dir.path());
}

#[test]
fn in_body_vault_ignores_apple_note_title() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(
        dir.path(),
        "Notes/Tracked.md",
        "---\napple-note-title: \"A new title\"\n---\nSynced text",
    );
    write_clone_state(dir.path(), &state()).unwrap();
    assert!(plan_entries(dir.path()).is_empty());
}

#[test]
fn still_ignores_ordinary_frontmatter_on_a_clean_file() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), "REC1", "Synced text").unwrap();
    write_vault_file(
        dir.path(),
        "Notes/Tracked.md",
        "---\ntags: [personal]\n---\nSynced text",
    );
    write_clone_state(dir.path(), &title_mode_state()).unwrap();
    assert!(plan_entries(dir.path()).is_empty());
}
