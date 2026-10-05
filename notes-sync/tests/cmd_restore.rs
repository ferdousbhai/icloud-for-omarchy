//! `restore`. Originally derived from icloud-md's tests.

mod common;

use common::state;

use std::path::Path;

use icloud_notes_sync::cmd::restore::run_restore;
use icloud_notes_sync::vault::base::write_base_copy;
use icloud_notes_sync::vault::state::write_clone_state;

fn setup(dir: &Path) {
    write_clone_state(dir, &state()).unwrap();
    write_base_copy(dir, "REC1", "Original synced text").unwrap();
}

fn note(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("Test Note.md")).unwrap()
}

#[test]
fn overwrites_a_locally_edited_note_with_its_base_copy() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    std::fs::write(dir.path().join("Test Note.md"), "Some local edit that can't be pushed").unwrap();
    run_restore(dir.path(), "Test Note.md").unwrap();
    assert_eq!(note(dir.path()), "Original synced text");
}

#[test]
fn refuses_a_file_that_isnt_tracked() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let err = run_restore(dir.path(), "Nonexistent.md").unwrap_err();
    assert!(err.to_string().contains("isn't a tracked note"));
}

#[test]
fn accepts_a_path_with_directory_components() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    std::fs::write(dir.path().join("Test Note.md"), "edited").unwrap();
    let arg = dir.path().join("Test Note.md");
    run_restore(dir.path(), arg.to_str().unwrap()).unwrap();
    assert_eq!(note(dir.path()), "Original synced text");
}

#[test]
fn refuses_when_theres_no_cloned_state() {
    let dir = tempfile::tempdir().unwrap();
    let err = run_restore(dir.path(), "Test Note.md").unwrap_err();
    assert!(err.to_string().contains("doesn't look like a cloned notes directory"));
}

#[test]
fn preserves_local_only_frontmatter() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let frontmatter = "---\ntags: [recipes, weeknight]\naliases: [Pie]\n---\n\n";
    std::fs::write(dir.path().join("Test Note.md"), format!("{frontmatter}Some local edit")).unwrap();
    run_restore(dir.path(), "Test Note.md").unwrap();
    assert_eq!(note(dir.path()), format!("{frontmatter}Original synced text"));
}

#[test]
fn recreates_a_missing_file_from_the_base_copy() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    run_restore(dir.path(), "Test Note.md").unwrap();
    assert_eq!(note(dir.path()), "Original synced text");
}

#[test]
fn leaves_a_frontmatter_only_file_as_envelope_plus_base_copy() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    std::fs::write(dir.path().join("Test Note.md"), "---\ntags: [orphan]\n---\n").unwrap();
    run_restore(dir.path(), "Test Note.md").unwrap();
    assert_eq!(note(dir.path()), "---\ntags: [orphan]\n---\nOriginal synced text");
}
