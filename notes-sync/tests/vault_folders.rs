//! Ports icloud-md `src/notes/folderReconcile.test.ts` and `folderCreate.test.ts`.

use std::collections::HashSet;
use std::path::Path;

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
use icloud_notes_sync::vault::base::{read_base_copy, write_base_copy};
use icloud_notes_sync::vault::folders::{
    FolderCreatePlan, PlannedFolder, Relocation, plan_folder_creates, reconcile_note_placements, remove_stale_dirs,
};
use icloud_notes_sync::vault::layout::{PreviousLayout, StateDirInfo, build_vault_layout};
use icloud_notes_sync::vault::state::{AttachmentEntry, FolderEntry, NoteEntry, SharerHomeEntry};
use indexmap::IndexMap;
use serde_json::json;

// --- folderReconcile -------------------------------------------------------------

fn folder_record(record_name: &str, title: &str) -> CloudKitRecord {
    let mut fields = IndexMap::new();
    fields.insert(
        "TitleEncrypted".to_owned(),
        FieldValue {
            value: json!(icloud_notes_sync::js::base64_encode(title.as_ref())),
            type_: "ENCRYPTED_BYTES".into(),
        },
    );
    CloudKitRecord {
        record_name: record_name.into(),
        record_type: "Folder".into(),
        fields,
        ..Default::default()
    }
}

fn write_vault_file(root: &Path, file: &str, content: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join(file)).unwrap()
}

fn note(file: &str, tag: &str, date: i64, folder: &str) -> NoteEntry {
    let mut e = NoteEntry::new(file, tag, date);
    e.folder_record_name = Some(folder.into());
    e
}

fn attachment(file: &str, media: &str, checksum: &str, note: &str) -> AttachmentEntry {
    AttachmentEntry {
        file: file.into(),
        media_record_name: media.into(),
        media_file_checksum: checksum.into(),
        note_record_name: note.into(),
    }
}

fn alpha_beta_layout() -> icloud_notes_sync::vault::layout::VaultLayout {
    build_vault_layout(
        &[
            folder_record("DefaultFolder-CloudKit", "Notes"),
            folder_record("A", "Alpha"),
            folder_record("B", "Beta"),
        ],
        &[],
        PreviousLayout::default(),
    )
}

#[test]
fn remote_folder_rename_moves_note_and_empties_old_dir() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let folders: IndexMap<String, FolderEntry> = [
        ("DefaultFolder-CloudKit".to_owned(), FolderEntry::new("Notes", "Notes")),
        ("F".to_owned(), FolderEntry::new("Recipes", "Recipes")),
    ]
    .into_iter()
    .collect();
    let homes: IndexMap<String, SharerHomeEntry> = IndexMap::new();
    let layout = build_vault_layout(
        &[
            folder_record("DefaultFolder-CloudKit", "Notes"),
            folder_record("F", "Cooking"),
        ],
        &[],
        PreviousLayout {
            folders: Some(&folders),
            sharer_homes: Some(&homes),
        },
    );
    assert_eq!(layout.folder_dirs["F"], "Cooking");

    let mut notes: IndexMap<String, NoteEntry> = IndexMap::new();
    notes.insert("REC".into(), note("Recipes/Pie.md", "a", 1, "F"));
    write_vault_file(root, "Recipes/Pie.md", "pie");
    std::fs::create_dir_all(root.join("Cooking")).unwrap();

    let relocations = reconcile_note_placements(root, &layout, &mut notes, &mut IndexMap::new()).unwrap();
    assert_eq!(
        relocations,
        vec![Relocation {
            from: "Recipes/Pie.md".into(),
            to: "Cooking/Pie.md".into(),
        }]
    );
    assert_eq!(notes["REC"].file, "Cooking/Pie.md");
    assert_eq!(read(root, "Cooking/Pie.md"), "pie");

    let current: HashSet<String> = layout.all_dirs.iter().cloned().collect();
    remove_stale_dirs(root, &["Notes".to_owned(), "Recipes".to_owned()], &current);
    assert!(!root.join("Recipes").exists());
}

#[test]
fn remote_note_move_relocates_file_and_attachment() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let layout = alpha_beta_layout();
    let mut notes: IndexMap<String, NoteEntry> = IndexMap::new();
    notes.insert("REC".into(), note("Alpha/Photo note.md", "a", 1, "B"));
    let mut attachments: IndexMap<String, AttachmentEntry> = IndexMap::new();
    attachments.insert(
        "ATT".into(),
        attachment("Alpha/attachments/photo.jpeg", "M", "c", "REC"),
    );
    write_vault_file(root, "Alpha/Photo note.md", "![photo.jpeg](attachments/photo.jpeg)");
    write_vault_file(root, "Alpha/attachments/photo.jpeg", "bytes");

    let relocations = reconcile_note_placements(root, &layout, &mut notes, &mut attachments).unwrap();
    assert_eq!(relocations.len(), 1);
    assert_eq!(notes["REC"].file, "Beta/Photo note.md");
    assert_eq!(attachments["ATT"].file, "Beta/attachments/photo.jpeg");
    assert_eq!(read(root, "Beta/attachments/photo.jpeg"), "bytes");
    assert_eq!(
        read(root, "Beta/Photo note.md"),
        "![photo.jpeg](attachments/photo.jpeg)"
    );
}

#[test]
fn basename_collision_in_target_uniquifies() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let layout = alpha_beta_layout();
    let mut notes: IndexMap<String, NoteEntry> = IndexMap::new();
    notes.insert("REC-STAY".into(), note("Beta/Pie.md", "a", 1, "B"));
    notes.insert("REC-MOVE".into(), note("Alpha/Pie.md", "b", 2, "B"));
    write_vault_file(root, "Beta/Pie.md", "staying");
    write_vault_file(root, "Alpha/Pie.md", "moving");

    reconcile_note_placements(root, &layout, &mut notes, &mut IndexMap::new()).unwrap();
    assert_eq!(notes["REC-STAY"].file, "Beta/Pie.md");
    assert_eq!(notes["REC-MOVE"].file, "Beta/Pie 2.md");
    assert_eq!(read(root, "Beta/Pie 2.md"), "moving");
}

#[test]
fn attachment_collision_rewrites_body_and_base_copy_identically() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let layout = alpha_beta_layout();
    let body = "![photo.jpeg](attachments/photo.jpeg)";
    let mut notes: IndexMap<String, NoteEntry> = IndexMap::new();
    notes.insert("REC-STAY".into(), note("Beta/Staying.md", "a", 1, "B"));
    notes.insert("REC-MOVE".into(), note("Alpha/Moving.md", "b", 2, "B"));
    let mut attachments: IndexMap<String, AttachmentEntry> = IndexMap::new();
    attachments.insert(
        "ATT-STAY".into(),
        attachment("Beta/attachments/photo.jpeg", "M1", "c1", "REC-STAY"),
    );
    attachments.insert(
        "ATT-MOVE".into(),
        attachment("Alpha/attachments/photo.jpeg", "M2", "c2", "REC-MOVE"),
    );
    write_vault_file(root, "Beta/Staying.md", body);
    write_vault_file(root, "Beta/attachments/photo.jpeg", "staying-bytes");
    write_vault_file(root, "Alpha/Moving.md", body);
    write_vault_file(root, "Alpha/attachments/photo.jpeg", "moving-bytes");
    write_base_copy(root, "REC-MOVE", body).unwrap();

    reconcile_note_placements(root, &layout, &mut notes, &mut attachments).unwrap();

    assert_eq!(attachments["ATT-MOVE"].file, "Beta/attachments/photo 2.jpeg");
    let moved = read(root, "Beta/Moving.md");
    assert_eq!(moved, "![photo.jpeg](attachments/photo%202.jpeg)");
    assert_eq!(
        read_base_copy(root, "REC-MOVE").unwrap().as_deref(),
        Some(moved.as_str())
    );
    assert_eq!(read(root, "Beta/attachments/photo.jpeg"), "staying-bytes");
}

#[test]
fn locally_missing_file_left_tracked_at_old_path() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let layout = alpha_beta_layout();
    let mut notes: IndexMap<String, NoteEntry> = IndexMap::new();
    notes.insert("REC".into(), note("Alpha/Gone.md", "a", 1, "B"));
    std::fs::create_dir_all(root.join("Alpha")).unwrap();

    let relocations = reconcile_note_placements(root, &layout, &mut notes, &mut IndexMap::new()).unwrap();
    assert!(relocations.is_empty());
    assert_eq!(notes["REC"].file, "Alpha/Gone.md");
}

#[test]
fn remove_stale_dirs_leaves_dir_with_untracked_files() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    write_vault_file(root, "Old/Keep me.md", "untracked");
    let current: HashSet<String> = ["New".to_owned()].into_iter().collect();
    remove_stale_dirs(root, &["Old".to_owned()], &current);
    assert_eq!(read(root, "Old/Keep me.md"), "untracked");
}

// --- folderCreate ----------------------------------------------------------------

fn index(entries: &[(&str, StateDirInfo)]) -> IndexMap<String, StateDirInfo> {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn notes_folder() -> StateDirInfo {
    StateDirInfo::Folder {
        folder_record_name: "DefaultFolder-CloudKit".into(),
        shared_zone_owner: None,
        permission: None,
    }
}

fn plan(wanted: &[&str], dir_index: &IndexMap<String, StateDirInfo>) -> FolderCreatePlan {
    let mut n = 0;
    let mut next = move || {
        n += 1;
        format!("new-{n}")
    };
    plan_folder_creates(wanted.iter().copied(), dir_index, &mut next)
}

fn planned(record_name: &str, title: &str, dir_path: &str, parent: Option<&str>) -> PlannedFolder {
    PlannedFolder {
        record_name: record_name.into(),
        title: title.into(),
        dir_path: dir_path.into(),
        parent_record_name: parent.map(str::to_owned),
    }
}

fn dir_paths(plan: &FolderCreatePlan) -> Vec<&str> {
    plan.folders.iter().map(|f| f.dir_path.as_str()).collect()
}

#[test]
fn plans_nothing_when_every_dir_exists() {
    let p = plan(&["Notes"], &index(&[("Notes", notes_folder())]));
    assert!(p.folders.is_empty());
    assert!(p.refusals.is_empty());
    assert_eq!(p.dir_to_record_name["Notes"], "DefaultFolder-CloudKit");
}

#[test]
fn plans_one_top_level_folder() {
    let p = plan(&["Recipes"], &index(&[("Notes", notes_folder())]));
    assert_eq!(p.folders, vec![planned("new-1", "Recipes", "Recipes", None)]);
    assert_eq!(p.dir_to_record_name["Recipes"], "new-1");
}

#[test]
fn plans_nested_folder_parent_first() {
    let p = plan(&["Recipes/Desserts/Cakes"], &index(&[]));
    assert_eq!(
        p.folders,
        vec![
            planned("new-1", "Recipes", "Recipes", None),
            planned("new-2", "Desserts", "Recipes/Desserts", Some("new-1")),
            planned("new-3", "Cakes", "Recipes/Desserts/Cakes", Some("new-2")),
        ]
    );
}

#[test]
fn nests_new_folder_under_existing() {
    let p = plan(&["Notes/Archive"], &index(&[("Notes", notes_folder())]));
    assert_eq!(
        p.folders,
        vec![planned(
            "new-1",
            "Archive",
            "Notes/Archive",
            Some("DefaultFolder-CloudKit")
        )]
    );
}

#[test]
fn plans_shared_parent_once() {
    let p = plan(&["Recipes/Desserts", "Recipes/Mains"], &index(&[]));
    assert_eq!(dir_paths(&p), vec!["Recipes", "Recipes/Desserts", "Recipes/Mains"]);
    let parent = p.folders[0].record_name.clone();
    assert_eq!(p.folders[1].parent_record_name.as_deref(), Some(parent.as_str()));
    assert_eq!(p.folders[2].parent_record_name.as_deref(), Some(parent.as_str()));
}

#[test]
fn orders_parents_before_children() {
    let p = plan(&["A/B/C", "A", "A/B"], &index(&[]));
    assert_eq!(dir_paths(&p), vec!["A", "A/B", "A/B/C"]);
}

#[test]
fn refuses_hidden_directory() {
    let p = plan(&[".obsidian/plugins"], &index(&[]));
    assert!(p.folders.is_empty());
    assert_eq!(p.refusals.len(), 1);
    assert!(p.refusals[0].reason.message().contains("hidden directory"));
}

#[test]
fn refuses_reserved_attachments_at_any_depth() {
    let p = plan(&["Notes/attachments"], &index(&[("Notes", notes_folder())]));
    assert!(p.folders.is_empty());
    assert!(p.refusals[0].reason.message().contains("downloaded attachments"));
}

#[test]
fn refuses_folder_inside_sharer_area() {
    let p = plan(
        &["Someone Else/New Folder"],
        &index(&[(
            "Someone Else",
            StateDirInfo::SharerHome {
                shared_zone_owner: "_owner".into(),
            },
        )]),
    );
    assert!(p.folders.is_empty());
    assert!(p.refusals[0].reason.message().contains("another user's shared area"));
}

#[test]
fn refuses_folder_inside_shared_folder() {
    let p = plan(
        &["Someone Else/Shared Recipes/Sub"],
        &index(&[
            (
                "Someone Else",
                StateDirInfo::SharerHome {
                    shared_zone_owner: "_owner".into(),
                },
            ),
            (
                "Someone Else/Shared Recipes",
                StateDirInfo::Folder {
                    folder_record_name: "shared-1".into(),
                    shared_zone_owner: Some("_owner".into()),
                    permission: None,
                },
            ),
        ]),
    );
    assert!(p.folders.is_empty());
    assert!(p.refusals[0].reason.message().contains("shared folder"));
}

#[test]
fn deep_refusal_leaves_no_half_planned_ancestors() {
    let p = plan(&["Recipes/attachments"], &index(&[]));
    assert!(p.folders.is_empty());
    assert!(!p.dir_to_record_name.contains_key("Recipes"));
    assert_eq!(p.refusals.len(), 1);
}

#[test]
fn ancestor_wanted_by_sibling_survives_refused_branch() {
    let p = plan(&["Recipes/attachments", "Recipes/Mains"], &index(&[]));
    assert_eq!(dir_paths(&p), vec!["Recipes", "Recipes/Mains"]);
    assert_eq!(p.refusals.len(), 1);
    assert_eq!(p.refusals[0].dir_path, "Recipes/attachments");
}

#[test]
fn ignores_vault_root() {
    let p = plan(&["", ""], &index(&[]));
    assert!(p.folders.is_empty());
    assert!(p.refusals.is_empty());
}
