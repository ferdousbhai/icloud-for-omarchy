//! The folder tree and where notes go in the vault. Originally derived from icloud-md's tests.

use std::collections::HashMap;

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue, Participant};
use icloud_notes_sync::vault::layout::{
    FolderInfo, FolderTree, NotePlacement, PreviousLayout, SharedZoneRecords, build_folder_tree, build_vault_layout,
    decode_folder_record, note_dir_of, place_note, sanitize_folder_dir_name, sharer_display_name,
};
use icloud_notes_sync::vault::state::{FolderEntry, SharerHomeEntry};
use indexmap::IndexMap;
use serde_json::json;

fn field(value: serde_json::Value, type_: &str) -> FieldValue {
    FieldValue {
        value,
        type_: type_.into(),
    }
}

fn folder_record(record_name: &str, title: &str, parent: Option<&str>) -> CloudKitRecord {
    let mut fields = IndexMap::new();
    fields.insert(
        "TitleEncrypted".to_owned(),
        field(
            json!(icloud_notes_sync::js::base64_encode(title.as_ref())),
            "ENCRYPTED_BYTES",
        ),
    );
    CloudKitRecord {
        record_name: record_name.into(),
        record_type: "Folder".into(),
        fields,
        parent_record_name: parent.map(str::to_owned),
        ..Default::default()
    }
}

fn note_record(record_name: &str, folder: Option<&str>) -> CloudKitRecord {
    let mut fields = IndexMap::new();
    if let Some(folder) = folder {
        fields.insert("Folder".to_owned(), field(json!({ "recordName": folder }), "REFERENCE"));
    }
    CloudKitRecord {
        record_name: record_name.into(),
        record_type: "Note".into(),
        fields,
        ..Default::default()
    }
}

fn share_record(given: Option<&str>, family: Option<&str>, email: Option<&str>) -> CloudKitRecord {
    CloudKitRecord {
        record_name: "Share-1".into(),
        record_type: "cloudkit.share".into(),
        participants: Some(vec![Participant {
            type_: Some("OWNER".into()),
            given_name: given.map(str::to_owned),
            family_name: family.map(str::to_owned),
            email_address: email.map(str::to_owned),
            ..Default::default()
        }]),
        ..Default::default()
    }
}

fn perm_share(record_name: &str, permission: &str) -> CloudKitRecord {
    CloudKitRecord {
        record_name: record_name.into(),
        record_type: "cloudkit.share".into(),
        current_user_permission: Some(permission.into()),
        ..Default::default()
    }
}

// --- folderTree ------------------------------------------------------------------

#[test]
fn decode_folder_record_decodes_base64_title() {
    let info = decode_folder_record(&folder_record("A", "Another Folder", None)).unwrap();
    assert_eq!(
        info,
        FolderInfo {
            record_name: "A".into(),
            title: "Another Folder".into(),
            ..Default::default()
        }
    );
}

#[test]
fn decode_folder_record_carries_share_reference() {
    let mut record = folder_record("A", "Shared Recipes", None);
    record.share_record_name = Some("Share-1234".into());
    assert_eq!(
        decode_folder_record(&record).unwrap().share_record_name.as_deref(),
        Some("Share-1234")
    );
}

#[test]
fn decode_folder_record_reads_parent_folder_reference() {
    let mut record = folder_record(
        "1F59CBAE-CDEF-4374-A4B8-6D1C4B8A11D2",
        "Subf",
        Some("EB6DBFC9-FDC1-4CE8-8C7E-7A531331280A"),
    );
    record.fields.insert(
        "ParentFolder".into(),
        field(
            json!({ "recordName": "EB6DBFC9-FDC1-4CE8-8C7E-7A531331280A", "action": "VALIDATE" }),
            "REFERENCE",
        ),
    );
    assert_eq!(
        decode_folder_record(&record).unwrap().parent_record_name.as_deref(),
        Some("EB6DBFC9-FDC1-4CE8-8C7E-7A531331280A")
    );
}

#[test]
fn decode_folder_record_falls_back_to_record_level_parent() {
    let info = decode_folder_record(&folder_record("B", "Nested", Some("A"))).unwrap();
    assert_eq!(info.parent_record_name.as_deref(), Some("A"));
}

#[test]
fn decode_folder_record_ignores_non_folders_and_trash() {
    assert_eq!(decode_folder_record(&note_record("N", None)), None);
    assert_eq!(
        decode_folder_record(&folder_record("TrashFolder-CloudKit", "Recently Deleted", None)),
        None
    );
}

#[test]
fn decode_folder_record_tolerates_missing_title() {
    let record = CloudKitRecord {
        record_name: "A".into(),
        record_type: "Folder".into(),
        ..Default::default()
    };
    assert_eq!(decode_folder_record(&record).unwrap().title, "");
}

#[test]
fn sanitize_folder_dir_name_strips_unsafe_and_trailing_dots() {
    assert_eq!(sanitize_folder_dir_name("Recipes: a/b?"), "Recipes ab");
    assert_eq!(sanitize_folder_dir_name("ends with dot."), "ends with dot");
}

#[test]
fn sanitize_folder_dir_name_falls_back_for_unusable_titles() {
    assert_eq!(sanitize_folder_dir_name(""), "Untitled Folder");
    assert_eq!(sanitize_folder_dir_name("///"), "Untitled Folder");
}

fn info(record_name: &str, title: &str, parent: Option<&str>) -> FolderInfo {
    FolderInfo {
        record_name: record_name.into(),
        title: title.into(),
        parent_record_name: parent.map(str::to_owned),
        ..Default::default()
    }
}

fn tree(folders: &[FolderInfo], preferred: Option<&[(&str, &str)]>) -> FolderTree {
    let map: Option<HashMap<String, String>> =
        preferred.map(|p| p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect());
    build_folder_tree(folders, map.as_ref())
}

fn dir_path(tree: &FolderTree, record_name: &str) -> Option<String> {
    tree.get(record_name).map(|n| n.dir_path.clone())
}

#[test]
fn build_folder_tree_nests_children() {
    let result = tree(
        &[
            info("top", "Top", None),
            info("mid", "Mid", Some("top")),
            info("leaf", "Leaf", Some("mid")),
        ],
        None,
    );
    assert_eq!(dir_path(&result, "leaf").as_deref(), Some("Top/Mid/Leaf"));
    assert_eq!(result.roots.len(), 1);
}

#[test]
fn build_folder_tree_uniquifies_case_insensitively() {
    let result = tree(&[info("a", "recipes", None), info("b", "Recipes", None)], None);
    let mut names = vec![dir_path(&result, "a").unwrap(), dir_path(&result, "b").unwrap()];
    names.sort();
    assert_eq!(names, vec!["Recipes 2", "recipes"]);
}

#[test]
fn build_folder_tree_keeps_equal_titles_apart_in_different_groups() {
    let result = tree(
        &[
            info("p1", "Parent One", None),
            info("p2", "Parent Two", None),
            info("c1", "Notes", Some("p1")),
            info("c2", "Notes", Some("p2")),
        ],
        None,
    );
    assert_eq!(dir_path(&result, "c1").as_deref(), Some("Parent One/Notes"));
    assert_eq!(dir_path(&result, "c2").as_deref(), Some("Parent Two/Notes"));
}

#[test]
fn build_folder_tree_reserves_attachments_everywhere() {
    let result = tree(
        &[
            info("a", "attachments", None),
            info("p", "Parent", None),
            info("nested", "Attachments", Some("p")),
        ],
        None,
    );
    assert_eq!(dir_path(&result, "a").as_deref(), Some("attachments 2"));
    assert_eq!(dir_path(&result, "nested").as_deref(), Some("Parent/Attachments 2"));
}

#[test]
fn build_folder_tree_reserves_state_dir_at_top_only() {
    let result = tree(
        &[
            info("a", ".icloud-md", None),
            info("p", "Parent", None),
            info("nested", ".icloud-md", Some("p")),
        ],
        None,
    );
    assert_eq!(dir_path(&result, "a").as_deref(), Some(".icloud-md 2"));
    assert_eq!(dir_path(&result, "nested").as_deref(), Some("Parent/.icloud-md"));
}

#[test]
fn build_folder_tree_honors_preferred_names() {
    let result = tree(
        &[info("new", "Shared", None), info("old", "Shared", None)],
        Some(&[("old", "Shared")]),
    );
    assert_eq!(dir_path(&result, "old").as_deref(), Some("Shared"));
    assert_eq!(dir_path(&result, "new").as_deref(), Some("Shared 2"));
}

#[test]
fn build_folder_tree_reassigns_unavailable_preferred_name() {
    let result = tree(
        &[info("a", "Kept", None), info("b", "Wants Kept", None)],
        Some(&[("a", "Kept"), ("b", "Kept")]),
    );
    assert_eq!(dir_path(&result, "a").as_deref(), Some("Kept"));
    assert_eq!(dir_path(&result, "b").as_deref(), Some("Wants Kept"));
}

#[test]
fn build_folder_tree_promotes_unknown_parent_to_root() {
    let result = tree(&[info("orphan", "Orphan", Some("gone"))], None);
    assert_eq!(dir_path(&result, "orphan").as_deref(), Some("Orphan"));
}

#[test]
fn build_folder_tree_breaks_cycles() {
    let result = tree(
        &[
            info("a", "A", Some("b")),
            info("b", "B", Some("a")),
            info("child", "Child", Some("b")),
        ],
        None,
    );
    assert_eq!(result.by_record_name.len(), 3);
    assert!(dir_path(&result, "child").unwrap().ends_with("Child"));
    for name in ["a", "b", "child"] {
        assert_ne!(dir_path(&result, name).unwrap(), "");
    }
}

#[test]
fn build_folder_tree_is_deterministic() {
    let folders = vec![info("z", "Same", None), info("a", "Same", None)];
    let first = tree(&folders, None);
    let reversed: Vec<FolderInfo> = folders.iter().rev().cloned().collect();
    let second = tree(&reversed, None);
    assert_eq!(dir_path(&first, "a"), dir_path(&second, "a"));
    assert_eq!(dir_path(&first, "a").as_deref(), Some("Same"));
    assert_eq!(dir_path(&first, "z").as_deref(), Some("Same 2"));
}

// --- folderLayout ----------------------------------------------------------------

fn own_records() -> Vec<CloudKitRecord> {
    vec![
        folder_record("DefaultFolder-CloudKit", "Notes", None),
        folder_record("F-RECIPES", "Recipes", None),
        folder_record("F-DESSERTS", "Desserts", Some("F-RECIPES")),
    ]
}

fn zone(owner: &str, records: Vec<CloudKitRecord>) -> SharedZoneRecords {
    SharedZoneRecords {
        owner_record_name: owner.into(),
        records,
    }
}

fn folders(entries: &[(&str, FolderEntry)]) -> IndexMap<String, FolderEntry> {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn shared_folder_entry(name: &str, owner: &str, permission: &str) -> FolderEntry {
    FolderEntry {
        shared_zone_owner: Some(owner.into()),
        permission: Some(permission.into()),
        ..FolderEntry::new(name, name)
    }
}

fn homes(entries: &[(&str, &str)]) -> IndexMap<String, SharerHomeEntry> {
    entries
        .iter()
        .map(|(owner, name)| {
            (
                owner.to_string(),
                SharerHomeEntry {
                    name: name.to_string(),
                    dir_name: name.to_string(),
                },
            )
        })
        .collect()
}

fn prev<'a>(
    folders: &'a IndexMap<String, FolderEntry>,
    homes: &'a IndexMap<String, SharerHomeEntry>,
) -> PreviousLayout<'a> {
    PreviousLayout {
        folders: Some(folders),
        sharer_homes: Some(homes),
    }
}

#[test]
fn build_vault_layout_maps_own_folders_to_nested_dirs() {
    let layout = build_vault_layout(&own_records(), &[], PreviousLayout::default());
    assert_eq!(layout.folder_dirs["DefaultFolder-CloudKit"], "Notes");
    assert_eq!(layout.folder_dirs["F-DESSERTS"], "Recipes/Desserts");
    assert_eq!(
        layout.state_folders["F-DESSERTS"],
        FolderEntry {
            parent_record_name: Some("F-RECIPES".into()),
            ..FolderEntry::new("Desserts", "Desserts")
        }
    );
}

#[test]
fn build_vault_layout_names_sharer_home_after_owner() {
    let z = zone(
        "_owner1",
        vec![
            share_record(Some("Hassan"), Some("Almemari"), None),
            note_record("N1", None),
        ],
    );
    let layout = build_vault_layout(&own_records(), &[z], PreviousLayout::default());
    assert_eq!(layout.sharer_home_dirs["_owner1"], "Hassan Almemari");
    assert_eq!(
        layout.state_sharer_homes["_owner1"],
        SharerHomeEntry {
            name: "Hassan Almemari".into(),
            dir_name: "Hassan Almemari".into(),
        }
    );
}

#[test]
fn build_vault_layout_falls_back_to_email_then_owner_id() {
    let email = zone("_owner1", vec![share_record(None, None, Some("pal@example.com"))]);
    let bare = zone("_owner2", vec![note_record("N1", None)]);
    let layout = build_vault_layout(&[], &[email, bare], PreviousLayout::default());
    assert_eq!(layout.sharer_home_dirs["_owner1"], "pal@example.com");
    assert_eq!(layout.sharer_home_dirs["_owner2"], "_owner2");
}

#[test]
fn build_vault_layout_roots_shared_folder_under_home() {
    let z = zone(
        "_owner1",
        vec![
            share_record(Some("Pat"), None, None),
            folder_record("F-SHARED", "Shared Recipes", None),
        ],
    );
    let layout = build_vault_layout(&own_records(), &[z], PreviousLayout::default());
    assert_eq!(layout.folder_dirs["F-SHARED"], "Pat/Shared Recipes");
    assert_eq!(
        layout.state_folders["F-SHARED"].shared_zone_owner.as_deref(),
        Some("_owner1")
    );
}

#[test]
fn build_vault_layout_one_top_level_namespace() {
    let z = zone("_owner1", vec![share_record(Some("Recipes"), None, None)]);
    let layout = build_vault_layout(&own_records(), &[z], PreviousLayout::default());
    assert_eq!(layout.folder_dirs["F-RECIPES"], "Recipes");
    assert_eq!(layout.sharer_home_dirs["_owner1"], "Recipes 2");
}

#[test]
fn build_vault_layout_carries_forward_unsent_folders() {
    let f = folders(&[("F-OLD", FolderEntry::new("Recipes", "Recipes"))]);
    let h = homes(&[]);
    let layout = build_vault_layout(&[folder_record("F-NEW", "Recipes", None)], &[], prev(&f, &h));
    assert_eq!(layout.folder_dirs["F-OLD"], "Recipes");
    assert_eq!(layout.folder_dirs["F-NEW"], "Recipes 2");
}

#[test]
fn build_vault_layout_drops_tombstoned_folder() {
    let tombstone = CloudKitRecord {
        record_name: "F-GONE".into(),
        record_type: "Folder".into(),
        deleted: Some(true),
        ..Default::default()
    };
    let f = folders(&[("F-GONE", FolderEntry::new("Old", "Old"))]);
    let h = homes(&[]);
    let layout = build_vault_layout(&[tombstone], &[], prev(&f, &h));
    assert!(!layout.folder_dirs.contains_key("F-GONE"));
}

#[test]
fn build_vault_layout_stores_share_permission_inherited_by_nested() {
    let mut shared = folder_record("F-SHARED", "Shared Recipes", None);
    shared.share_record_name = Some("Share-F".into());
    let z = zone(
        "_owner1",
        vec![
            share_record(Some("Pat"), None, None),
            perm_share("Share-F", "READ_WRITE"),
            shared,
            folder_record("F-NESTED", "Desserts", Some("F-SHARED")),
        ],
    );
    let layout = build_vault_layout(&[], &[z], PreviousLayout::default());
    assert_eq!(
        layout.state_folders["F-SHARED"].permission.as_deref(),
        Some("READ_WRITE")
    );
    assert_eq!(
        layout.state_folders["F-NESTED"].permission.as_deref(),
        Some("READ_WRITE")
    );
}

#[test]
fn build_vault_layout_carries_permission_forward() {
    let z = zone("_owner1", vec![folder_record("F-SHARED", "Shared Recipes", None)]);
    let f = folders(&[(
        "F-SHARED",
        shared_folder_entry("Shared Recipes", "_owner1", "READ_ONLY"),
    )]);
    let h = homes(&[("_owner1", "Pat")]);
    let layout = build_vault_layout(&[], &[z], prev(&f, &h));
    assert_eq!(
        layout.state_folders["F-SHARED"].permission.as_deref(),
        Some("READ_ONLY")
    );
}

#[test]
fn build_vault_layout_resent_share_permission_wins() {
    let mut shared = folder_record("F-SHARED", "Shared Recipes", None);
    shared.share_record_name = Some("Share-F".into());
    let z = zone("_owner1", vec![perm_share("Share-F", "READ_ONLY"), shared]);
    let f = folders(&[(
        "F-SHARED",
        shared_folder_entry("Shared Recipes", "_owner1", "READ_WRITE"),
    )]);
    let h = homes(&[("_owner1", "Pat")]);
    let layout = build_vault_layout(&[], &[z], prev(&f, &h));
    assert_eq!(
        layout.state_folders["F-SHARED"].permission.as_deref(),
        Some("READ_ONLY")
    );
}

#[test]
fn build_vault_layout_keeps_home_name_when_share_not_resent() {
    let z = zone("_owner1", vec![note_record("N1", None)]);
    let f = folders(&[]);
    let h = homes(&[("_owner1", "Hassan Almemari")]);
    let layout = build_vault_layout(&[], &[z], prev(&f, &h));
    assert_eq!(layout.sharer_home_dirs["_owner1"], "Hassan Almemari");
}

#[test]
fn place_note_puts_own_note_in_folder() {
    let layout = build_vault_layout(&own_records(), &[], PreviousLayout::default());
    assert_eq!(
        place_note(&layout, &note_record("N1", Some("F-DESSERTS")), None),
        NotePlacement {
            dir: "Recipes/Desserts".into(),
            folder_record_name: Some("F-DESSERTS".into()),
        }
    );
}

#[test]
fn place_note_falls_back_to_default_folder() {
    let layout = build_vault_layout(&own_records(), &[], PreviousLayout::default());
    assert_eq!(
        place_note(&layout, &note_record("N1", Some("F-UNKNOWN")), None),
        NotePlacement {
            dir: "Notes".into(),
            folder_record_name: None,
        }
    );
}

#[test]
fn place_note_puts_shared_folder_note_inside_folder() {
    let z = zone(
        "_owner1",
        vec![
            share_record(Some("Pat"), None, None),
            folder_record("F-SHARED", "Shared Recipes", None),
        ],
    );
    let layout = build_vault_layout(&own_records(), &[z], PreviousLayout::default());
    assert_eq!(
        place_note(&layout, &note_record("N1", Some("F-SHARED")), Some("_owner1")),
        NotePlacement {
            dir: "Pat/Shared Recipes".into(),
            folder_record_name: Some("F-SHARED".into()),
        }
    );
}

#[test]
fn place_note_drops_individually_shared_note_in_home() {
    let z = zone(
        "_owner1",
        vec![
            share_record(Some("Pat"), None, None),
            note_record("N1", Some("DefaultFolder-CloudKit")),
        ],
    );
    let layout = build_vault_layout(&own_records(), &[z], PreviousLayout::default());
    assert_eq!(
        place_note(
            &layout,
            &note_record("N1", Some("DefaultFolder-CloudKit")),
            Some("_owner1")
        ),
        NotePlacement {
            dir: "Pat".into(),
            folder_record_name: None,
        }
    );
}

#[test]
fn note_dir_of_derives_directory() {
    assert_eq!(note_dir_of("Recipes/Pie.md"), "Recipes");
    assert_eq!(note_dir_of("Pie.md"), "");
}

#[test]
fn sharer_display_name_reads_owner_only() {
    let record = CloudKitRecord {
        record_name: "Share-1".into(),
        record_type: "cloudkit.share".into(),
        participants: Some(vec![
            Participant {
                type_: Some("ADMINISTRATOR".into()),
                given_name: Some("Adam".into()),
                family_name: Some("Coddington".into()),
                ..Default::default()
            },
            Participant {
                type_: Some("OWNER".into()),
                given_name: Some("Hassan".into()),
                family_name: Some("Almemari".into()),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };
    assert_eq!(
        sharer_display_name(&zone("_owner1", vec![record])).as_deref(),
        Some("Hassan Almemari")
    );
}

#[test]
fn build_vault_layout_rederives_dir_on_title_change() {
    let f = folders(&[("F", FolderEntry::new("Recipes", "Recipes"))]);
    let h = homes(&[]);
    let layout = build_vault_layout(&[folder_record("F", "Cooking", None)], &[], prev(&f, &h));
    assert_eq!(layout.folder_dirs["F"], "Cooking");
    assert_eq!(layout.state_folders["F"].dir_name, "Cooking");
}

#[test]
fn build_vault_layout_keeps_dir_when_only_siblings_changed() {
    let f = folders(&[("F", FolderEntry::new("Recipes", "Recipes"))]);
    let h = homes(&[]);
    let layout = build_vault_layout(&[folder_record("NEW", "Aardvark", None)], &[], prev(&f, &h));
    assert_eq!(layout.folder_dirs["F"], "Recipes");
}
