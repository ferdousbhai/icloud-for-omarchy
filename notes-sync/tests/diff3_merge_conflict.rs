//! Port of icloud-md's `mergeConflict.test.ts`.

use icloud_notes_sync::diff3::{has_conflict_markers, merge_note_versions};

#[test]
fn identical_versions_merge_cleanly() {
    let base = "title\n\nline one\nline two\n";
    let outcome = merge_note_versions(base, base, base);
    assert!(!outcome.has_conflict);
    assert_eq!(outcome.text, base);
}

#[test]
fn non_overlapping_edits_merge_cleanly() {
    let outcome = merge_note_versions(
        "title\n\nline one\nline two\n",
        "title\n\nline one\nline two\nline three\n",
        "new title\n\nline one\nline two\n",
    );
    assert!(!outcome.has_conflict);
    assert!(outcome.text.contains("new title"));
    assert!(outcome.text.contains("line three"));
    assert!(!outcome.text.contains("<<<<<<<"));
}

#[test]
fn overlapping_edits_produce_diff3_markers() {
    let outcome = merge_note_versions(
        "title\n\nsame line\n",
        "title\n\nlocal version of the line\n",
        "title\n\nremote version of the line\n",
    );
    assert!(outcome.has_conflict);
    for needle in [
        "<<<<<<< local",
        "||||||| base",
        "=======",
        ">>>>>>> remote",
        "local version of the line",
        "remote version of the line",
    ] {
        assert!(outcome.text.contains(needle), "{needle}");
    }
    assert_eq!(
        outcome.text,
        "title\n\n<<<<<<< local\nlocal version of the line\n||||||| base\nsame line\n=======\nremote version of the line\n>>>>>>> remote\n"
    );
    assert!(has_conflict_markers(&outcome.text));
}

#[test]
fn identical_independent_edits_are_not_a_conflict() {
    let both = "title\n\nsame line, edited the same way\n";
    let outcome = merge_note_versions("title\n\nsame line\n", both, both);
    assert!(!outcome.has_conflict);
    assert_eq!(outcome.text, both);
}

#[test]
fn remote_deletion_against_a_local_edit_conflicts() {
    let outcome = merge_note_versions("title\n\nkeep me\n", "title\n\nkeep me, but edited\n", "");
    assert!(outcome.has_conflict);
    assert!(outcome.text.contains("keep me, but edited"));
}
