//! Vault migrations: the runner (originally derived from icloud-md's tests),
//! 2 → 3, and the 3 → 4 move from `.icloud-md/` to `.icloud-notes/`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use icloud_notes_sync::GENERATOR;
use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::history::{HistoryOptions, run_history};
use icloud_notes_sync::cmd::vault_info::run_vault_info;
use icloud_notes_sync::vault::base::{read_base_copy, write_base_copy};
use icloud_notes_sync::vault::local::{LocalFileState, local_file_state};
use icloud_notes_sync::vault::migrate::{
    VaultMigration, open_vault, read_vault, run_vault_migrations, tombstone, vault_migrations,
};
use icloud_notes_sync::vault::state::{
    CURRENT_LAYOUT_VERSION, CloneState, NoteEntry, RawStateFile, TitleMode, read_clone_state, state_file_path,
    write_clone_state,
};
use serde_json::{Value, json};

fn write_state_at_version(dir: &Path, state: Value) {
    std::fs::create_dir_all(dir.join(".icloud-md")).unwrap();
    std::fs::write(
        dir.join(".icloud-md/state.json"),
        serde_json::to_string_pretty(&state).unwrap() + "\n",
    )
    .unwrap();
}

/// The live state file, wherever it is.
fn read_state_file(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(state_file_path(dir)).unwrap()).unwrap()
}

fn open(dir: &Path) -> Result<Option<CloneState>, Error> {
    open_vault(dir, &mut |_| {})
}

fn migration<'a>(
    from: u64,
    to: u64,
    describe: &'static str,
    run: impl Fn(&Path, RawStateFile) -> Result<RawStateFile, Error> + 'a,
) -> VaultMigration<'a> {
    VaultMigration {
        from,
        to,
        describe,
        run: Box::new(run),
    }
}

#[test]
fn open_vault_returns_none_for_a_non_clone() {
    let dir = tempfile::tempdir().unwrap();
    assert!(open(dir.path()).unwrap().is_none());
}

#[test]
fn open_vault_reads_a_current_version_vault_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(
        dir.path(),
        &CloneState {
            sync_token: Some("token".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let state = open(dir.path()).unwrap().unwrap();
    assert_eq!(state.layout_version, Some(CURRENT_LAYOUT_VERSION));
    assert_eq!(state.sync_token.as_deref(), Some("token"));
}

#[test]
fn open_vault_refuses_a_layout_version_1_vault() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(dir.path(), json!({"syncToken": "token", "notes": {}}));
    let err = open(dir.path()).unwrap_err();
    assert!(err.to_string().contains("old flat layout"), "{err}");
}

#[test]
fn open_vault_refuses_a_vault_from_a_newer_tool_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let future = json!({"layoutVersion": CURRENT_LAYOUT_VERSION + 1, "syncToken": "token", "notes": {}});
    write_state_at_version(dir.path(), future.clone());
    let err = open(dir.path()).unwrap_err();
    assert!(err.to_string().contains("newer version of icloud-notes"), "{err}");
    assert_eq!(read_state_file(dir.path()), future);
}

#[test]
fn write_clone_state_stamps_the_generator() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &CloneState::default()).unwrap();
    let state = open(dir.path()).unwrap().unwrap();
    assert_eq!(state.generator.as_deref(), Some(GENERATOR));
    assert!(GENERATOR.starts_with("icloud-notes-sync "));
}

#[test]
fn runs_each_migration_in_order_and_commits_after_each() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {}}),
    );
    let seen = RefCell::new(Vec::new());
    let migrations = vec![
        migration(2, 3, "second", |_, mut state| {
            seen.borrow_mut().push(2);
            state.insert("addedByTwo".into(), true.into());
            Ok(state)
        }),
        migration(3, 4, "third", |_, mut state| {
            assert_eq!(state["addedByTwo"], json!(true));
            assert_eq!(state["layoutVersion"], json!(3));
            seen.borrow_mut().push(3);
            state.insert("addedByThree".into(), true.into());
            Ok(state)
        }),
    ];
    run_vault_migrations(dir.path(), &migrations, 4, &mut |_| {}).unwrap();
    assert_eq!(*seen.borrow(), [2, 3]);
    let state = read_state_file(dir.path());
    assert_eq!(state["layoutVersion"], json!(4));
    assert_eq!(state["addedByTwo"], json!(true));
    assert_eq!(state["addedByThree"], json!(true));
}

#[test]
fn reports_each_migration_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {}}),
    );
    let migrations = vec![migration(2, 3, "recording note ids", |_, s| Ok(s))];
    let mut described = Vec::new();
    run_vault_migrations(dir.path(), &migrations, 3, &mut |m| described.push(m.describe)).unwrap();
    assert_eq!(described, ["recording note ids"]);
}

#[test]
fn a_crash_mid_chain_leaves_the_last_committed_version_and_replays_from_there() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {}}),
    );
    let attempts = Cell::new(0);
    let migrations = vec![
        migration(2, 3, "first", |_, mut s| {
            s.insert("first".into(), true.into());
            Ok(s)
        }),
        migration(3, 4, "second", |_, mut s| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                return Err(Error::Internal("interrupted".into()));
            }
            s.insert("second".into(), true.into());
            Ok(s)
        }),
    ];
    let err = run_vault_migrations(dir.path(), &migrations, 4, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("interrupted"));
    let after_crash = read_state_file(dir.path());
    assert_eq!(after_crash["layoutVersion"], json!(3));
    assert_eq!(after_crash["first"], json!(true));
    assert!(after_crash.get("second").is_none());

    run_vault_migrations(dir.path(), &migrations, 4, &mut |_| {}).unwrap();
    let after_retry = read_state_file(dir.path());
    assert_eq!(after_retry["layoutVersion"], json!(4));
    assert_eq!(after_retry["second"], json!(true));
    assert_eq!(attempts.get(), 2);
}

#[test]
fn a_gap_in_the_chain_fails_loudly_instead_of_half_migrating() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {}}),
    );
    let migrations = vec![migration(3, 4, "unreachable", |_, s| Ok(s))];
    let err = run_vault_migrations(dir.path(), &migrations, 4, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("bug in icloud-notes-sync"), "{err}");
    assert_eq!(read_state_file(dir.path())["layoutVersion"], json!(2));
}

// --- the real chain -------------------------------------------------------

#[test]
fn a_version_2_vault_migrates_forward_on_first_contact_in_place() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token",
               "notes": {"REC-1": {"file": "Notes/A.md", "recordChangeTag": "t", "modificationDate": 1}}}),
    );
    let mut described = Vec::new();
    let state = open_vault(dir.path(), &mut |m| described.push(m.to_owned()))
        .unwrap()
        .unwrap();
    assert_eq!(state.layout_version, Some(CURRENT_LAYOUT_VERSION));
    assert_eq!(state.title_mode, Some(TitleMode::InBody));
    assert_eq!(state.notes["REC-1"].file, "Notes/A.md");
    assert_eq!(
        described,
        vault_migrations()
            .iter()
            .map(|m| format!("Updating this vault's layout: {}...", m.describe))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        read_state_file(dir.path())["layoutVersion"],
        json!(CURRENT_LAYOUT_VERSION)
    );
}

#[test]
fn migrating_twice_is_a_no_op_the_second_time() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {}}),
    );
    open(dir.path()).unwrap();
    let mut described = Vec::new();
    open_vault(dir.path(), &mut |m| described.push(m.to_owned())).unwrap();
    assert!(described.is_empty());
}

#[test]
fn a_filename_as_title_vault_keeps_its_mode_through_migration() {
    let dir = tempfile::tempdir().unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {}, "titleMode": "filename"}),
    );
    assert_eq!(open(dir.path()).unwrap().unwrap().title_mode, Some(TitleMode::Filename));
}

#[test]
fn the_version_2_migration_stamps_every_tracked_note_file_with_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let id_a = "089D915D-C76E-4F44-AB80-2190073281A3";
    let id_b = "001b9e8a-c474-4311-af32-abe70026b346";
    std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
    std::fs::write(dir.path().join("Notes/A.md"), "# A\n\nBody A").unwrap();
    std::fs::write(dir.path().join("Notes/B.md"), "---\ntags: [keep]\n---\n\n# B\n\nBody B").unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token", "notes": {
            id_a: {"file": "Notes/A.md", "recordChangeTag": "t", "modificationDate": 1},
            id_b: {"file": "Notes/B.md", "recordChangeTag": "t", "modificationDate": 1}}}),
    );
    open(dir.path()).unwrap();
    let a = std::fs::read_to_string(dir.path().join("Notes/A.md")).unwrap();
    assert!(a.contains(&format!("apple-note-id: {id_a}")));
    assert!(a.contains("# A"));
    let b = std::fs::read_to_string(dir.path().join("Notes/B.md")).unwrap();
    assert!(b.contains(&format!("apple-note-id: {id_b}")));
    assert!(b.contains("tags:"));
}

#[test]
fn stamping_ids_does_not_make_a_clean_file_read_as_modified() {
    let dir = tempfile::tempdir().unwrap();
    let id = "089D915D-C76E-4F44-AB80-2190073281A3";
    std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
    std::fs::write(dir.path().join("Notes/A.md"), "# A\n\nBody A").unwrap();
    write_base_copy(dir.path(), id, "# A\n\nBody A").unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token",
               "notes": {id: {"file": "Notes/A.md", "recordChangeTag": "t", "modificationDate": 1}}}),
    );
    open(dir.path()).unwrap();
    assert_eq!(
        local_file_state(dir.path(), &NoteEntry::new("Notes/A.md", "t", 1), id, TitleMode::InBody).unwrap(),
        LocalFileState::Clean
    );
}

#[test]
fn the_migration_skips_a_tracked_file_that_is_not_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let id = "089D915D-C76E-4F44-AB80-2190073281A3";
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token",
               "notes": {id: {"file": "Notes/Gone.md", "recordChangeTag": "t", "modificationDate": 1}}}),
    );
    let state = open(dir.path()).unwrap().unwrap();
    assert_eq!(state.layout_version, Some(CURRENT_LAYOUT_VERSION));
    assert!(!dir.path().join("Notes/Gone.md").exists());
}

#[test]
fn re_running_the_migration_rewrites_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let id = "089D915D-C76E-4F44-AB80-2190073281A3";
    std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
    std::fs::write(dir.path().join("Notes/A.md"), "# A").unwrap();
    write_state_at_version(
        dir.path(),
        json!({"layoutVersion": 2, "syncToken": "token",
               "notes": {id: {"file": "Notes/A.md", "recordChangeTag": "t", "modificationDate": 1}}}),
    );
    open(dir.path()).unwrap();
    let after_first = std::fs::read_to_string(dir.path().join("Notes/A.md")).unwrap();
    run_vault_migrations(
        dir.path(),
        &vault_migrations(),
        u64::from(CURRENT_LAYOUT_VERSION),
        &mut |_| {},
    )
    .unwrap();
    let mut state = read_state_file(dir.path());
    state["layoutVersion"] = json!(2);
    std::fs::remove_file(dir.path().join(".icloud-notes/state.json")).unwrap();
    write_state_at_version(dir.path(), state);
    open(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Notes/A.md")).unwrap(),
        after_first
    );
}

// --- 3 → 4: .icloud-md/ → .icloud-notes/ ------------------------------------

const ID: &str = "089D915D-C76E-4F44-AB80-2190073281A3";
const NOTE: &str = "---\napple-note-id: 089D915D-C76E-4F44-AB80-2190073281A3\n---\n\n# A\n\nBody A";

/// A layout 3 vault as icloud-md and earlier builds left it: one note, its
/// base copy, history (a snapshot and an epoch), a conflict backup the app
/// made, and a top-level key this build doesn't know.
fn v3_vault(dir: &Path) -> Value {
    let file = |rel: &str, text: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    file("Notes/A.md", NOTE);
    file(&format!(".icloud-md/base/{ID}.md"), "# A\n\nBody A");
    file(&format!(".icloud-md/history/{ID}/0001-abcd.json"), "{\"id\":\"s1\"}\n");
    file(
        &format!(".icloud-md/history/{ID}/epochs/0001-ef01.json"),
        &format!("{{\"id\":\"e1\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"noteRecordName\":\"{ID}\",\"snapshots\":{{}}}}\n"),
    );
    file(".icloud-md/conflict-backups/Notes/A (conflict backup 2026-01-01 000000).md", "old");
    let state = json!({"layoutVersion": 3, "generator": "icloud-md 0.6.2", "titleMode": "in-body",
        "syncToken": "token", "someoneElsesKey": [1],
        "notes": {ID: {"file": "Notes/A.md", "recordChangeTag": "t", "modificationDate": 1}}});
    write_state_at_version(dir, state.clone());
    state
}

/// Every file under `dir`, relative, with its bytes.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn backups(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".icloud-md.bak-"))
        .collect();
    out.sort();
    out
}

fn assert_migrated(dir: &Path, legacy_before: &BTreeMap<String, Vec<u8>>) {
    let current = dir.join(".icloud-notes");
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(current.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["layoutVersion"], json!(4));
    assert_eq!(state["syncToken"], json!("token"));
    assert_eq!(state["someoneElsesKey"], json!([1]));
    assert_eq!(state["notes"][ID]["file"], json!("Notes/A.md"));
    // Everything but state.json moved, byte for byte.
    let mut moved = tree(&current);
    moved.remove("state.json");
    let mut expected = legacy_before.clone();
    expected.remove("state.json");
    assert_eq!(moved, expected);
    assert_eq!(
        tree(&dir.join(".icloud-md")),
        BTreeMap::from([(
            "state.json".to_owned(),
            (serde_json::to_string_pretty(&Value::Object(tombstone())).unwrap() + "\n").into_bytes()
        )])
    );
    assert_eq!(std::fs::read_to_string(dir.join("Notes/A.md")).unwrap(), NOTE);
    assert_eq!(read_base_copy(dir, ID).unwrap().as_deref(), Some("# A\n\nBody A"));
}

#[test]
fn a_layout_3_vault_moves_to_icloud_notes_under_open_vault() {
    let dir = tempfile::tempdir().unwrap();
    v3_vault(dir.path());
    let legacy_before = tree(&dir.path().join(".icloud-md"));
    let mut described = Vec::new();
    let state = open_vault(dir.path(), &mut |m| described.push(m.to_owned()))
        .unwrap()
        .unwrap();
    assert_eq!(
        described,
        ["Updating this vault's layout: moving its state from .icloud-md/ to .icloud-notes/..."]
    );
    assert_eq!(state.layout_version, Some(4));
    assert_migrated(dir.path(), &legacy_before);
    // The backup is a copy of .icloud-md/ as it was.
    let backups = backups(dir.path());
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(tree(&backups[0]), legacy_before);

    // A second open changes nothing and makes no second backup.
    let after = tree(dir.path());
    open(dir.path()).unwrap();
    assert_eq!(tree(dir.path()), after);
}

#[test]
fn read_only_commands_read_a_layout_3_vault_in_place() {
    let dir = tempfile::tempdir().unwrap();
    v3_vault(dir.path());
    let before = tree(dir.path());

    let state = read_vault(dir.path()).unwrap();
    assert_eq!(state.layout_version, Some(3));
    assert_eq!(state.notes[ID].file, "Notes/A.md");
    assert_eq!(read_base_copy(dir.path(), ID).unwrap().as_deref(), Some("# A\n\nBody A"));
    run_history(dir.path(), "Notes/A.md", &HistoryOptions { records: false }).unwrap();
    let info = serde_json::to_value(run_vault_info(dir.path()).unwrap()).unwrap();
    let vault = std::path::absolute(dir.path()).unwrap();
    assert_eq!(info["stateDir"], json!(vault.join(".icloud-md").to_str().unwrap()));
    assert_eq!(info["notes"][0]["baseFile"], json!(format!(".icloud-md/base/{ID}.md")));

    assert_eq!(tree(dir.path()), before, "nothing was migrated");
    assert!(!dir.path().join(".icloud-notes").exists());
}

#[test]
fn an_interrupted_move_finishes_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    v3_vault(dir.path());
    let legacy_before = tree(&dir.path().join(".icloud-md"));
    // Stopped after the backup, the new directory and the first rename.
    let backup = dir.path().join(".icloud-md.bak-20260101T000000Z");
    std::fs::create_dir(&backup).unwrap();
    std::fs::create_dir(dir.path().join(".icloud-notes")).unwrap();
    std::fs::rename(
        dir.path().join(".icloud-md/base"),
        dir.path().join(".icloud-notes/base"),
    )
    .unwrap();

    // Meanwhile a read-only command still finds everything.
    assert_eq!(read_vault(dir.path()).unwrap().layout_version, Some(3));
    assert_eq!(read_base_copy(dir.path(), ID).unwrap().as_deref(), Some("# A\n\nBody A"));
    run_history(dir.path(), "Notes/A.md", &HistoryOptions { records: true }).unwrap();

    open(dir.path()).unwrap().unwrap();
    assert_migrated(dir.path(), &legacy_before);
    assert_eq!(backups(dir.path()), [backup], "no second backup");
}

#[test]
fn an_unfinished_backup_is_redone() {
    let dir = tempfile::tempdir().unwrap();
    v3_vault(dir.path());
    let legacy_before = tree(&dir.path().join(".icloud-md"));
    let partial = dir.path().join(".icloud-md.bak-20260101T000000Z.partial");
    std::fs::create_dir_all(partial.join("base")).unwrap();
    open(dir.path()).unwrap().unwrap();
    assert_migrated(dir.path(), &legacy_before);
    assert!(!partial.exists());
    let backups = backups(dir.path());
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(tree(&backups[0]), legacy_before);
}

#[test]
fn a_run_stopped_after_the_commit_leaves_the_tombstone_to_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let state = v3_vault(dir.path());
    let legacy_before = tree(&dir.path().join(".icloud-md"));
    open(dir.path()).unwrap().unwrap();
    // As if the tombstone had never been written.
    write_state_at_version(dir.path(), state);
    assert_eq!(read_vault(dir.path()).unwrap().layout_version, Some(4));
    open(dir.path()).unwrap().unwrap();
    assert_migrated(dir.path(), &legacy_before);
}

#[test]
fn conflict_backups_already_in_icloud_notes_are_merged_not_clobbered() {
    let dir = tempfile::tempdir().unwrap();
    v3_vault(dir.path());
    let theirs = dir.path().join(".icloud-notes/conflict-backups/Notes/B.md");
    std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    std::fs::write(&theirs, "b").unwrap();
    open(dir.path()).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "b");
    assert!(
        dir.path()
            .join(".icloud-notes/conflict-backups/Notes/A (conflict backup 2026-01-01 000000).md")
            .exists()
    );
    assert!(!dir.path().join(".icloud-md/conflict-backups").exists());
}

#[test]
fn an_older_build_refuses_the_moved_vault_as_newer() {
    let dir = tempfile::tempdir().unwrap();
    v3_vault(dir.path());
    open(dir.path()).unwrap().unwrap();
    // What a layout 3 build sees: only .icloud-md/state.json.
    std::fs::rename(dir.path().join(".icloud-notes"), dir.path().join("elsewhere")).unwrap();
    let err = run_vault_migrations(dir.path(), &[], 3, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::VaultFromNewerTool { vault_version: 4, .. }), "{err}");
    // This build, with .icloud-notes/ gone, says what happened.
    let err = read_clone_state(dir.path()).unwrap_err();
    assert!(err.to_string().contains("moved to .icloud-notes/"), "{err}");
}

#[test]
fn a_fresh_clone_writes_layout_4_into_icloud_notes() {
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &CloneState::default()).unwrap();
    assert!(dir.path().join(".icloud-notes/state.json").exists());
    assert!(!dir.path().join(".icloud-md").exists());
    assert_eq!(read_state_file(dir.path())["layoutVersion"], json!(4));
}
