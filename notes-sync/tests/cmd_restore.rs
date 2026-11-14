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

/// A base copy written before #21 kept the blank lines opening the body,
/// which the restored file's envelope takes: restore leaves it clean.
#[test]
fn a_restored_file_reads_back_clean_when_its_base_copy_opens_with_blank_lines() {
    use icloud_notes_sync::vault::local::{LocalFileState, local_file_state};
    use icloud_notes_sync::vault::state::TitleMode;
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    write_base_copy(dir.path(), "REC1", "\n\nOriginal synced text").unwrap();
    std::fs::write(
        dir.path().join("Test Note.md"),
        "---\ntags: [a]\n---\n\nSome local edit",
    )
    .unwrap();
    run_restore(dir.path(), "Test Note.md").unwrap();
    let entry = &state().notes["REC1"];
    let local = local_file_state(dir.path(), entry, "REC1", TitleMode::InBody).unwrap();
    assert_eq!(local, LocalFileState::Clean);
}

/// A restore that can't write the file whole (here a file size limit, as on
/// a full disk) leaves the file as it was, not truncated.
#[test]
fn a_restore_that_cannot_be_written_whole_leaves_the_file_as_it_was() {
    let edit = "Some local edit that can't be pushed\n".repeat(20);
    if let Some(dir) = std::env::var_os(common::LIMITED_VAULT_ENV) {
        assert!(
            run_restore(Path::new(&dir), "Test Note.md").is_err(),
            "the write was meant to fail"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_clone_state(dir.path(), &state()).unwrap();
    write_base_copy(dir.path(), "REC1", &"Original synced text\n".repeat(40)).unwrap();
    std::fs::write(dir.path().join("Test Note.md"), &edit).unwrap();
    common::run_with_file_size_limit(
        "a_restore_that_cannot_be_written_whole_leaves_the_file_as_it_was",
        dir.path(),
    );
    assert_eq!(note(dir.path()), edit);
}

#[cfg(unix)]
#[test]
fn a_restored_file_keeps_its_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    let path = dir.path().join("Test Note.md");
    std::fs::write(&path, "edited").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    run_restore(dir.path(), "Test Note.md").unwrap();
    assert_eq!(note(dir.path()), "Original synced text");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}

/// A missing file restore recreates is stamped like a pulled one: its
/// apple-note-id, and in a filename-as-title vault the apple-note-title its
/// file name can't carry (a leading dot would hide the file), so the next
/// push doesn't retitle the note to "Untitled".
#[test]
fn a_recreated_file_keeps_its_note_id_and_the_title_its_name_cannot_carry() {
    use icloud_notes_sync::cmd::push::build_push_plan;
    use icloud_notes_sync::md::frontmatter::{read_note_id, read_note_title, split_frontmatter};
    use icloud_notes_sync::vault::local::split_options;
    use icloud_notes_sync::vault::state::{CloneState, NoteEntry, TitleMode};
    let dir = tempfile::tempdir().unwrap();
    let record = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
    let title = ".plan";
    let mut entry = NoteEntry::new("Untitled.md", "1a", 100);
    entry.frontmatter_title = Some(title.into());
    let state = CloneState {
        sync_token: Some("token".into()),
        title_mode: Some(TitleMode::Filename),
        notes: [(record.to_owned(), entry)].into_iter().collect(),
        ..Default::default()
    };
    write_clone_state(dir.path(), &state).unwrap();
    write_base_copy(dir.path(), record, "Original synced text").unwrap();

    run_restore(dir.path(), "Untitled.md").unwrap();

    let text = std::fs::read_to_string(dir.path().join("Untitled.md")).unwrap();
    let envelope = split_frontmatter(&text, split_options(TitleMode::Filename));
    assert_eq!(read_note_id(&envelope.frontmatter).as_deref(), Some(record), "{text}");
    assert_eq!(read_note_title(&envelope.frontmatter).as_deref(), Some(title), "{text}");
    let plan = build_push_plan(&common::no_network(), dir.path(), &mut |_| {}).unwrap();
    let entries: Vec<_> = plan.entries.iter().map(|e| (&e.entry.file, e.entry.kind)).collect();
    assert!(entries.is_empty(), "{entries:?}");
}

/// A note whose folder directory was deleted with it comes back where it
/// was, directory and all (it used to fail with "No such file or
/// directory").
#[test]
fn restores_a_note_whose_directory_is_gone_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = state();
    s.notes["REC1"].file = "Recipes/Pie.md".into();
    write_clone_state(dir.path(), &s).unwrap();
    write_base_copy(dir.path(), "REC1", "Original synced text").unwrap();
    run_restore(dir.path(), "Recipes/Pie.md").unwrap();
    let text = std::fs::read_to_string(dir.path().join("Recipes/Pie.md")).unwrap();
    assert!(text.ends_with("Original synced text"), "{text}");
}
