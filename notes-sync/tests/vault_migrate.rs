//! Ports icloud-md `src/notes/vaultMigrations.test.ts`.

use std::cell::{Cell, RefCell};
use std::path::Path;

use icloud_notes_sync::GENERATOR;
use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::vault::base::write_base_copy;
use icloud_notes_sync::vault::local::{LocalFileState, local_file_state};
use icloud_notes_sync::vault::migrate::{VaultMigration, open_vault, run_vault_migrations, vault_migrations};
use icloud_notes_sync::vault::state::{
    CURRENT_LAYOUT_VERSION, CloneState, NoteEntry, RawStateFile, TitleMode, write_clone_state,
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

fn read_state_file(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(".icloud-md/state.json")).unwrap()).unwrap()
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
    assert!(err.to_string().contains("newer version of icloud-md"), "{err}");
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
    assert!(err.to_string().contains("bug in icloud-md"), "{err}");
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
        [format!(
            "Updating this vault's layout: {}...",
            vault_migrations()[0].describe
        )]
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
    write_state_at_version(dir.path(), state);
    open(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Notes/A.md")).unwrap(),
        after_first
    );
}
