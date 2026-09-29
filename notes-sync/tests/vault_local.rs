//! Ports icloud-md `src/notes/localFileState.test.ts`,
//! `noteTimestamps.test.ts` and `src/vaultRoot.test.ts`.

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
use icloud_notes_sync::md::frontmatter::compose_note_file;
use icloud_notes_sync::vault::base::write_base_copy;
use icloud_notes_sync::vault::local::{
    LocalFileState, LocalNote, apply_note_file_times, creation_date_of, display_path_from, find_vault_root,
    local_file_state, modification_date_of, read_local_note,
};
use icloud_notes_sync::vault::state::{NoteEntry, TitleMode};

// --- localFileState.test.ts ---------------------------------------------------

const REC: &str = "REC1";
const BODY: &str = "# Title\nbody line";
const NOTE_ID: &str = "089D915D-C76E-4F44-AB80-2190073281A3";

fn entry() -> NoteEntry {
    NoteEntry::new("Note.md", "1a", 100)
}

fn seed(dir: &Path, content: &str) {
    std::fs::write(dir.join("Note.md"), content).unwrap();
    write_base_copy(dir, REC, BODY).unwrap();
}

fn state_of(dir: &Path, mode: TitleMode) -> LocalFileState {
    local_file_state(dir, &entry(), REC, mode).unwrap()
}

#[test]
#[ignore = "needs A/B/C"]
fn a_file_matching_the_base_copy_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), BODY);
    assert_eq!(state_of(dir.path(), TitleMode::InBody), LocalFileState::Clean);
}

#[test]
#[ignore = "needs A/B/C"]
fn local_only_frontmatter_leaves_the_note_clean() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &format!("---\ntags: [personal]\n---\n{BODY}"));
    assert_eq!(state_of(dir.path(), TitleMode::InBody), LocalFileState::Clean);
}

#[test]
#[ignore = "needs A/B/C"]
fn frontmatter_with_a_blank_line_separator_is_still_clean() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &format!("---\ntags: [personal]\n---\n\n{BODY}"));
    assert_eq!(state_of(dir.path(), TitleMode::InBody), LocalFileState::Clean);
}

#[test]
#[ignore = "needs A/B/C"]
fn editing_the_body_under_frontmatter_is_modified() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "---\ntags: [personal]\n---\n# Title\nan edited body line");
    assert_eq!(state_of(dir.path(), TitleMode::InBody), LocalFileState::Modified);
}

#[test]
#[ignore = "needs A/B/C"]
fn editing_the_body_without_frontmatter_is_modified() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "# Title\nan edited body line");
    assert_eq!(state_of(dir.path(), TitleMode::InBody), LocalFileState::Modified);
}

#[test]
fn a_missing_file_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), REC, BODY).unwrap();
    assert_eq!(state_of(dir.path(), TitleMode::InBody), LocalFileState::Missing);
}

#[test]
#[ignore = "needs A/B/C"]
fn a_filename_as_title_note_whose_body_starts_blank_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let body = "\n**Yield:** 8 servings";
    std::fs::write(dir.path().join("Note.md"), compose_note_file("", body, NOTE_ID, None)).unwrap();
    write_base_copy(dir.path(), REC, body).unwrap();
    assert_eq!(state_of(dir.path(), TitleMode::Filename), LocalFileState::Clean);
}

#[test]
#[ignore = "needs A/B/C"]
fn trimming_the_leading_blank_line_is_a_real_edit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Note.md"),
        compose_note_file("", "**Yield:** 8 servings", NOTE_ID, None),
    )
    .unwrap();
    write_base_copy(dir.path(), REC, "\n**Yield:** 8 servings").unwrap();
    assert_eq!(state_of(dir.path(), TitleMode::Filename), LocalFileState::Modified);
}

#[test]
#[ignore = "needs A/B/C"]
fn read_local_note_hands_back_the_frontmatter_of_a_clean_file() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        &format!("---\napple-note-title: A very long title\n---\n{BODY}"),
    );
    let LocalNote::Present {
        state,
        frontmatter,
        body,
    } = read_local_note(dir.path(), &entry(), REC, TitleMode::InBody).unwrap()
    else {
        panic!("expected a present file");
    };
    assert_eq!(state, LocalFileState::Clean);
    assert_eq!(body, BODY);
    assert!(frontmatter.contains("apple-note-title: A very long title"));
}

#[test]
#[ignore = "needs A/B/C"]
fn read_local_note_splits_with_the_vault_shape() {
    let dir = tempfile::tempdir().unwrap();
    let body_text = "\n**Yield:** 8 servings";
    std::fs::write(
        dir.path().join("Note.md"),
        compose_note_file("", body_text, NOTE_ID, None),
    )
    .unwrap();
    write_base_copy(dir.path(), REC, body_text).unwrap();
    let LocalNote::Present { state, body, .. } =
        read_local_note(dir.path(), &entry(), REC, TitleMode::Filename).unwrap()
    else {
        panic!("expected a present file");
    };
    assert_eq!(state, LocalFileState::Clean);
    assert_eq!(body, body_text);
}

#[test]
fn read_local_note_reports_a_vanished_file_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    write_base_copy(dir.path(), REC, BODY).unwrap();
    assert_eq!(
        read_local_note(dir.path(), &entry(), REC, TitleMode::InBody).unwrap(),
        LocalNote::Missing
    );
}

// --- noteTimestamps.test.ts ---------------------------------------------------

fn record_with_dates(creation: Option<i64>, modification: Option<i64>) -> CloudKitRecord {
    let mut record = CloudKitRecord {
        record_name: "REC".into(),
        record_type: "Note".into(),
        ..Default::default()
    };
    for (name, value) in [("CreationDate", creation), ("ModificationDate", modification)] {
        if let Some(v) = value {
            record.fields.insert(
                name.into(),
                FieldValue {
                    value: v.into(),
                    type_: "TIMESTAMP".into(),
                },
            );
        }
    }
    record
}

fn ms(t: std::time::SystemTime) -> u128 {
    t.duration_since(UNIX_EPOCH).unwrap().as_millis()
}

#[test]
fn date_readers_read_the_record_fields() {
    let record = record_with_dates(Some(1000), Some(2000));
    assert_eq!(creation_date_of(&record), 1000);
    assert_eq!(modification_date_of(&record), 2000);
}

#[test]
fn date_readers_default_to_zero() {
    let record = record_with_dates(None, None);
    assert_eq!(creation_date_of(&record), 0);
    assert_eq!(modification_date_of(&record), 0);
}

#[test]
fn apply_note_file_times_sets_mtime_and_atime() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    std::fs::write(&file, "hello").unwrap();
    apply_note_file_times(
        &file,
        &record_with_dates(Some(1_700_000_000_000), Some(1_750_000_000_000)),
    )
    .unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    assert_eq!(ms(meta.modified().unwrap()), 1_750_000_000_000);
    assert_eq!(ms(meta.accessed().unwrap()), 1_700_000_000_000);
}

#[test]
fn apply_note_file_times_falls_back_atime_to_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    std::fs::write(&file, "hello").unwrap();
    apply_note_file_times(&file, &record_with_dates(None, Some(1_750_000_000_000))).unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    assert_eq!(ms(meta.modified().unwrap()), 1_750_000_000_000);
    assert_eq!(ms(meta.accessed().unwrap()), 1_750_000_000_000);
}

#[test]
fn apply_note_file_times_is_a_no_op_without_modification_date() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    std::fs::write(&file, "hello").unwrap();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(5));
    apply_note_file_times(&file, &record_with_dates(Some(1_700_000_000_000), None)).unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
}

// --- vaultRoot.test.ts --------------------------------------------------------

fn make_vault(root: &Path) {
    std::fs::create_dir_all(root.join(".icloud-md")).unwrap();
    std::fs::write(root.join(".icloud-md/state.json"), "{}").unwrap();
}

#[test]
fn find_vault_root_finds_the_vault_from_a_nested_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    make_vault(&root);
    let nested = root.join("Recipes/Desserts");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(find_vault_root(&nested).unwrap().as_deref(), Some(root.as_path()));
    assert_eq!(find_vault_root(&root).unwrap().as_deref(), Some(root.as_path()));
}

#[test]
fn find_vault_root_returns_none_outside_any_vault() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(find_vault_root(dir.path()).unwrap(), None);
}

#[test]
fn find_vault_root_picks_the_innermost_vault() {
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path().join("outer");
    let inner = outer.join("inner");
    make_vault(&outer);
    make_vault(&inner);
    assert_eq!(find_vault_root(&inner).unwrap().as_deref(), Some(inner.as_path()));
    assert_eq!(find_vault_root(&outer).unwrap().as_deref(), Some(outer.as_path()));
}

#[test]
fn display_path_renders_a_vault_file_relative_to_the_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    assert_eq!(
        display_path_from(&root, "Recipes/Pie.md", &root.join("Recipes")),
        "Pie.md"
    );
    assert_eq!(
        display_path_from(&root, "Work/Standup.md", &root.join("Recipes")),
        "../Work/Standup.md"
    );
    assert_eq!(display_path_from(&root, "Pie.md", &root), "Pie.md");
}
