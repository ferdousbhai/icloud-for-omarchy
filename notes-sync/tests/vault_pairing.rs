//! Pairing files with notes by `apple-note-id`, and pending renames.
//! Originally derived from icloud-md's tests.

use std::path::Path;

use icloud_notes_sync::vault::pairing::{
    AmbiguousIdClaim, BlockedRename, IdMovePair, NoteIdResolution, PerformedRename, UntrackedFile,
    pending_rename_target, resolve_note_ids, settle_pending_renames,
};
use icloud_notes_sync::vault::state::{NoteEntry, TitleMode};
use indexmap::IndexMap;

// --- noteIdPairing ------------------------------------------------------------

const A: &str = "089D915D-C76E-4F44-AB80-2190073281A3";
const B: &str = "001b9e8a-c474-4311-af32-abe70026b346";

fn uf(file: &str, id: Option<&str>) -> UntrackedFile {
    UntrackedFile {
        file: file.into(),
        note_id: id.map(str::to_owned),
    }
}

fn resolve(untracked: &[UntrackedFile], tracked: &[&str], present: &[&str]) -> NoteIdResolution {
    let present: Vec<String> = present.iter().map(|s| s.to_string()).collect();
    resolve_note_ids(untracked, tracked.iter().copied(), &|rn| {
        present.iter().any(|p| p == rn)
    })
}

fn mv(rn: &str, file: &str) -> IdMovePair {
    IdMovePair {
        record_name: rn.into(),
        file: file.into(),
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn id_matching_tracked_note_whose_file_is_gone_is_a_move() {
    let r = resolve(&[uf("Recipes/Renamed.md", Some(A))], &[A], &[]);
    assert_eq!(r.moves, vec![mv(A, "Recipes/Renamed.md")]);
    assert!(r.creates.is_empty());
}

#[test]
fn rename_move_and_edit_at_once_still_one_move() {
    let r = resolve(&[uf("Work/Totally Different.md", Some(A))], &[A], &[]);
    assert_eq!(r.moves, vec![mv(A, "Work/Totally Different.md")]);
}

#[test]
fn file_with_no_id_is_a_create() {
    let r = resolve(&[uf("Recipes/Brand New.md", None)], &[A], &[]);
    assert_eq!(r.creates, strs(&["Recipes/Brand New.md"]));
    assert!(r.moves.is_empty());
}

#[test]
fn copy_of_note_whose_original_is_in_place_is_new_note() {
    let r = resolve(&[uf("Recipes/Pie copy.md", Some(A))], &[A], &[A]);
    assert_eq!(r.creates, strs(&["Recipes/Pie copy.md"]));
    assert!(r.moves.is_empty());
    assert!(r.ambiguous.is_empty());
}

#[test]
fn several_copies_with_original_in_place_all_new_notes() {
    let r = resolve(
        &[uf("Recipes/Pie copy.md", Some(A)), uf("Recipes/Pie copy 2.md", Some(A))],
        &[A],
        &[A],
    );
    assert_eq!(r.creates, strs(&["Recipes/Pie copy.md", "Recipes/Pie copy 2.md"]));
    assert!(r.ambiguous.is_empty());
}

#[test]
fn duplicate_claims_with_no_incumbent_are_refused() {
    let r = resolve(
        &[uf("Recipes/Pie.md", Some(A)), uf("Recipes/Pie copy.md", Some(A))],
        &[A],
        &[],
    );
    assert_eq!(
        r.ambiguous,
        vec![AmbiguousIdClaim {
            record_name: A.into(),
            files: strs(&["Recipes/Pie.md", "Recipes/Pie copy.md"]),
        }]
    );
    assert!(r.moves.is_empty());
    assert!(
        r.creates.is_empty(),
        "a refused claimant must not also plan as a create"
    );
}

#[test]
fn id_of_untracked_note_is_a_stale_create() {
    let r = resolve(&[uf("Recipes/From Elsewhere.md", Some(B))], &[A], &[]);
    assert_eq!(r.creates, strs(&["Recipes/From Elsewhere.md"]));
    assert_eq!(r.stale_ids, strs(&["Recipes/From Elsewhere.md"]));
}

#[test]
fn distinct_ids_resolve_independently() {
    let r = resolve(
        &[
            uf("Recipes/Moved A.md", Some(A)),
            uf("Recipes/Moved B.md", Some(B)),
            uf("Recipes/New.md", None),
        ],
        &[A, B],
        &[],
    );
    assert_eq!(r.moves, vec![mv(A, "Recipes/Moved A.md"), mv(B, "Recipes/Moved B.md")]);
    assert_eq!(r.creates, strs(&["Recipes/New.md"]));
}

#[test]
fn creates_keep_input_order() {
    let r = resolve(&[uf("b.md", None), uf("a.md", None), uf("c.md", Some(A))], &[A], &[]);
    assert_eq!(r.creates, strs(&["b.md", "a.md"]));
}

#[test]
fn no_untracked_files_resolves_to_nothing() {
    let r = resolve(&[], &[A], &[A]);
    assert_eq!(r, NoteIdResolution::default());
}

// --- pendingRename ------------------------------------------------------------

const NOTE_ID: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_ID: &str = "99999999-8888-7777-6666-555555555555";

fn vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
    dir
}

fn write_note(dir: &Path, file: &str, id: &str, body: &str) {
    std::fs::write(dir.join(file), format!("---\napple-note-id: {id}\n---\n\n{body}\n")).unwrap();
}

fn entry(file: &str, pending: Option<&str>) -> NoteEntry {
    let mut e = NoteEntry::new(file, "tag", 0);
    e.pending_rename = pending.map(str::to_owned);
    e
}

fn notes_with(file: &str, pending: Option<&str>) -> IndexMap<String, NoteEntry> {
    let mut notes = IndexMap::new();
    notes.insert(NOTE_ID.to_owned(), entry(file, pending));
    notes
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn pending_rename_target_resolves_against_current_directory() {
    let mut e = NoteEntry::new("Notes/Old.md", "", 0);
    e.pending_rename = Some("New.md".into());
    assert_eq!(pending_rename_target(&e).as_deref(), Some("Notes/New.md"));
    e.file = "Archive/Old.md".into();
    assert_eq!(pending_rename_target(&e).as_deref(), Some("Archive/New.md"));
}

#[test]
fn pending_rename_target_is_nothing_when_file_has_the_name() {
    assert_eq!(pending_rename_target(&entry("Notes/New.md", Some("New.md"))), None);
    assert_eq!(pending_rename_target(&entry("Notes/New.md", None)), None);
}

#[test]
fn performed_rename_is_adopted() {
    let dir = vault();
    write_note(dir.path(), "Notes/Groceries.md", NOTE_ID, "Milk");
    let mut notes = notes_with("Notes/Shopping list.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, false, TitleMode::InBody).unwrap();
    assert!(settled.changed);
    assert_eq!(notes[NOTE_ID].file, "Notes/Groceries.md");
    assert_eq!(notes[NOTE_ID].pending_rename, None);
    assert!(settled.performed.is_empty());
}

#[test]
fn right_name_wrong_id_is_never_adopted() {
    let dir = vault();
    write_note(dir.path(), "Notes/Groceries.md", OTHER_ID, "Not ours");
    let mut notes = notes_with("Notes/Shopping list.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, false, TitleMode::InBody).unwrap();
    assert!(settled.changed);
    assert_eq!(notes[NOTE_ID].file, "Notes/Shopping list.md");
    assert_eq!(notes[NOTE_ID].pending_rename, None);
}

#[test]
fn outstanding_rename_left_alone_without_perform() {
    let dir = vault();
    write_note(dir.path(), "Notes/Shopping list.md", NOTE_ID, "Milk");
    let mut notes = notes_with("Notes/Shopping list.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, false, TitleMode::InBody).unwrap();
    assert!(!settled.changed);
    assert_eq!(notes[NOTE_ID].pending_rename.as_deref(), Some("Groceries.md"));
    assert_eq!(listing(&dir.path().join("Notes")), vec!["Shopping list.md"]);
}

#[test]
fn performing_finishes_a_rename() {
    let dir = vault();
    write_note(dir.path(), "Notes/Shopping list.md", NOTE_ID, "Milk");
    let mut notes = notes_with("Notes/Shopping list.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, true, TitleMode::InBody).unwrap();
    assert_eq!(
        settled.performed,
        vec![PerformedRename {
            from: "Notes/Shopping list.md".into(),
            to: "Notes/Groceries.md".into(),
        }]
    );
    assert_eq!(notes[NOTE_ID].file, "Notes/Groceries.md");
    assert_eq!(notes[NOTE_ID].pending_rename, None);
    assert_eq!(listing(&dir.path().join("Notes")), vec!["Groceries.md"]);
    assert!(
        std::fs::read_to_string(dir.path().join("Notes/Groceries.md"))
            .unwrap()
            .contains("Milk")
    );
}

#[test]
fn performing_refuses_to_overwrite_target() {
    let dir = vault();
    write_note(dir.path(), "Notes/Shopping list.md", NOTE_ID, "Milk");
    std::fs::write(dir.path().join("Notes/Groceries.md"), "MINE, UNTRACKED").unwrap();
    let mut notes = notes_with("Notes/Shopping list.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, true, TitleMode::InBody).unwrap();
    assert_eq!(
        settled.blocked,
        vec![BlockedRename {
            file: "Notes/Shopping list.md".into(),
            to: "Notes/Groceries.md".into(),
        }]
    );
    assert!(settled.performed.is_empty());
    assert_eq!(notes[NOTE_ID].pending_rename.as_deref(), Some("Groceries.md"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Notes/Groceries.md")).unwrap(),
        "MINE, UNTRACKED"
    );
}

#[test]
fn pending_rename_to_current_name_is_cleared() {
    let dir = vault();
    write_note(dir.path(), "Notes/Groceries.md", NOTE_ID, "Milk");
    let mut notes = notes_with("Notes/Groceries.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, true, TitleMode::InBody).unwrap();
    assert!(settled.changed);
    assert_eq!(notes[NOTE_ID].pending_rename, None);
    assert!(settled.performed.is_empty());
}

#[test]
fn missing_file_drops_pending_rename() {
    let dir = vault();
    let mut notes = notes_with("Notes/Shopping list.md", Some("Groceries.md"));
    let settled = settle_pending_renames(dir.path(), &mut notes, true, TitleMode::InBody).unwrap();
    assert!(settled.changed);
    assert_eq!(notes[NOTE_ID].file, "Notes/Shopping list.md");
    assert_eq!(notes[NOTE_ID].pending_rename, None);
}

#[test]
fn notes_with_nothing_pending_are_untouched() {
    let dir = vault();
    let mut notes = notes_with("Notes/Groceries.md", None);
    let before = notes[NOTE_ID].clone();
    let settled = settle_pending_renames(dir.path(), &mut notes, true, TitleMode::InBody).unwrap();
    assert!(!settled.changed);
    assert_eq!(notes[NOTE_ID], before);
}
