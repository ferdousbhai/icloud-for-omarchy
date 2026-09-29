//! Ports icloud-md `src/commands/pull.test.ts`, `pullResync.test.ts`,
//! `pullTitleMode.test.ts` and `src/cli/pullReport.test.ts`. The report is
//! plain text (no colours), so pullReport's two chalk colour-forcing tests
//! are not ported.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use icloud_notes_sync::cmd::pull::{
    MergeStatus, PullChange, PullChangeKind as K, PullChangeRemark, PullSummary, merge_remote_change_into_local_file,
    reconcile_notes_after_resync_in, rename_for_remote_title, render_pull_report,
};
use icloud_notes_sync::vault::base::{read_base_copy, write_base_copy};
use icloud_notes_sync::vault::local::{LocalFileState, local_file_state};
use icloud_notes_sync::vault::state::{AttachmentEntry, NoteEntry, TitleMode};
use indexmap::IndexMap;

fn write_vault_file(dir: &Path, file: &str, content: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap()
}

fn entry_for(file: &str) -> NoteEntry {
    NoteEntry::new(file, "1a", 100)
}

// --- pull.test.ts: mergeRemoteChangeIntoLocalFile --------------------------------

fn merge(dir: &Path, record: &str, file: &str, remote: &str) -> MergeStatus {
    merge_remote_change_into_local_file(dir, record, file, remote, None, TitleMode::InBody).unwrap()
}

#[test]
#[ignore = "needs A/B/C"]
fn clean_merge_keeps_local_half_uploadable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_base_copy(dir, "REC1", "line one\n\nline two\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", "line one edited locally\n\nline two\n");
    assert_eq!(
        merge(dir, "REC1", "Notes/Note.md", "line one\n\nline two edited remotely\n"),
        MergeStatus::Merged
    );
    assert_eq!(
        read(dir, "Notes/Note.md"),
        "line one edited locally\n\nline two edited remotely\n"
    );
    assert_eq!(
        read_base_copy(dir, "REC1").unwrap().unwrap(),
        "line one\n\nline two edited remotely\n"
    );
    assert_eq!(
        local_file_state(dir, &entry_for("Notes/Note.md"), "REC1", TitleMode::InBody).unwrap(),
        LocalFileState::Modified
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn tag_only_remote_change_leaves_local_edit_uploadable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_base_copy(dir, "REC1", "shared text\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", "shared text plus my edit\n");
    assert_eq!(
        merge(dir, "REC1", "Notes/Note.md", "shared text\n"),
        MergeStatus::Merged
    );
    assert_eq!(read(dir, "Notes/Note.md"), "shared text plus my edit\n");
    assert_eq!(read_base_copy(dir, "REC1").unwrap().unwrap(), "shared text\n");
    assert_eq!(
        local_file_state(dir, &entry_for("Notes/Note.md"), "REC1", TitleMode::InBody).unwrap(),
        LocalFileState::Modified
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn merge_preserves_local_only_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_base_copy(dir, "REC1", "body\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", "---\nkeep: me\n---\n\nbody edited\n");
    merge(dir, "REC1", "Notes/Note.md", "body\n");
    let written = read(dir, "Notes/Note.md");
    assert!(written.starts_with("---\nkeep: me\n---\n"), "{written}");
    assert!(written.ends_with("body edited\n"), "{written}");
}

#[test]
#[ignore = "needs A/B/C"]
fn genuine_conflict_writes_markers_and_keeps_base() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_base_copy(dir, "REC1", "shared line\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", "shared line edited locally\n");
    assert_eq!(
        merge(dir, "REC1", "Notes/Note.md", "shared line edited remotely\n"),
        MergeStatus::Conflict
    );
    let written = read(dir, "Notes/Note.md");
    assert!(written.contains("<<<<<<< local"));
    assert!(written.contains(">>>>>>> remote"));
    assert_eq!(read_base_copy(dir, "REC1").unwrap().unwrap(), "shared line\n");
}

#[test]
#[ignore = "needs A/B/C"]
fn refuses_to_merge_a_file_with_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let conflicted = "<<<<<<< local\nshared line edited locally\n||||||| base\nshared line\n=======\nshared line edited remotely\n>>>>>>> remote\n";
    write_base_copy(dir, "REC1", "shared line\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", conflicted);
    assert_eq!(
        merge(dir, "REC1", "Notes/Note.md", "shared line edited remotely again\n"),
        MergeStatus::UnresolvedMarkers
    );
    assert_eq!(read(dir, "Notes/Note.md"), conflicted);
    assert_eq!(read_base_copy(dir, "REC1").unwrap().unwrap(), "shared line\n");
}

const MERGE_NOTE_ID: &str = "089D915D-C76E-4F44-AB80-2190073281A3";

#[test]
#[ignore = "needs A/B/C"]
fn merge_stamps_id_into_a_file_that_lost_its_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_base_copy(dir, MERGE_NOTE_ID, "line one\n\nline two\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", "line one edited locally\n\nline two\n");
    assert_eq!(
        merge(
            dir,
            MERGE_NOTE_ID,
            "Notes/Note.md",
            "line one\n\nline two edited remotely\n"
        ),
        MergeStatus::Merged
    );
    let written = read(dir, "Notes/Note.md");
    assert!(written.contains(&format!("apple-note-id: {MERGE_NOTE_ID}")));
    assert!(written.contains("line one edited locally"));
    assert_eq!(
        read_base_copy(dir, MERGE_NOTE_ID).unwrap().unwrap(),
        "line one\n\nline two edited remotely\n"
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn merge_keeps_an_existing_id_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let envelope = format!("---\napple-note-id: {MERGE_NOTE_ID}\n---\n\n");
    write_base_copy(dir, MERGE_NOTE_ID, "line one\n").unwrap();
    write_vault_file(dir, "Notes/Note.md", &format!("{envelope}line one edited locally\n"));
    merge(dir, MERGE_NOTE_ID, "Notes/Note.md", "line one\nline two\n");
    let written = read(dir, "Notes/Note.md");
    assert!(written.starts_with(&envelope));
    assert_eq!(written.matches("apple-note-id").count(), 1);
}

#[test]
#[ignore = "needs A/B/C"]
fn merge_preserves_user_keys_alongside_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_base_copy(dir, MERGE_NOTE_ID, "line one\n").unwrap();
    write_vault_file(
        dir,
        "Notes/Note.md",
        "---\ntags: [recipes]\n---\n\nline one edited locally\n",
    );
    merge(dir, MERGE_NOTE_ID, "Notes/Note.md", "line one\nline two\n");
    let written = read(dir, "Notes/Note.md");
    assert!(written.contains("tags:"));
    assert!(written.contains(&format!("apple-note-id: {MERGE_NOTE_ID}")));
}

// --- pullResync.test.ts ------------------------------------------------------------

fn shared_entry(file: &str, owner: Option<&str>) -> NoteEntry {
    let mut e = entry_for(file);
    e.shared_zone_owner = owner.map(str::to_owned);
    e
}

fn seen(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn resync(
    dir: &Path,
    owner: Option<&str>,
    seen: &HashSet<String>,
    notes: &mut IndexMap<String, NoteEntry>,
    attachments: &mut IndexMap<String, AttachmentEntry>,
    summary: &mut PullSummary,
) -> usize {
    reconcile_notes_after_resync_in(
        dir,
        owner,
        seen,
        notes,
        attachments,
        &mut IndexMap::new(),
        summary,
        TitleMode::InBody,
    )
    .unwrap()
}

#[test]
#[ignore = "needs A/B/C"]
fn clean_absent_note_is_removed_like_a_tombstone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Gone.md", "body\n");
    write_base_copy(dir, "REC-GONE", "body\n").unwrap();
    write_vault_file(dir, "Notes/Kept.md", "kept body\n");
    write_base_copy(dir, "REC-KEPT", "kept body\n").unwrap();
    let mut notes = IndexMap::from([
        ("REC-GONE".to_string(), entry_for("Notes/Gone.md")),
        ("REC-KEPT".to_string(), entry_for("Notes/Kept.md")),
    ]);
    let mut summary = PullSummary::default();
    let removed = resync(
        dir,
        None,
        &seen(&["REC-KEPT"]),
        &mut notes,
        &mut IndexMap::new(),
        &mut summary,
    );
    assert_eq!(removed, 1);
    assert!(!notes.contains_key("REC-GONE"));
    assert!(notes.contains_key("REC-KEPT"));
    assert!(!dir.join("Notes/Gone.md").exists());
    assert!(dir.join("Notes/Kept.md").exists());
    assert_eq!(summary.removed, 1);
    assert_eq!(summary.changes, vec![PullChange::new(K::Remove, "Notes/Gone.md")]);
}

#[test]
#[ignore = "needs A/B/C"]
fn absent_note_with_local_edits_becomes_a_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Edited.md", "body plus my local edit\n");
    write_base_copy(dir, "REC-EDITED", "body\n").unwrap();
    let mut notes = IndexMap::from([("REC-EDITED".to_string(), entry_for("Notes/Edited.md"))]);
    let mut summary = PullSummary::default();
    let removed = resync(dir, None, &seen(&[]), &mut notes, &mut IndexMap::new(), &mut summary);
    assert_eq!(removed, 1);
    assert!(notes.contains_key("REC-EDITED"));
    assert!(read(dir, "Notes/Edited.md").contains("<<<<<<< local"));
    assert_eq!(summary.conflicts.len(), 1);
    assert!(summary.conflicts[0].contains("deleted remotely"));
}

#[test]
#[ignore = "needs A/B/C"]
fn reconciliation_is_scoped_to_one_zone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for (file, rec, body) in [
        ("Notes/Private.md", "REC-PRIVATE", "private body\n"),
        ("Notes/SharedA.md", "REC-SHARED-A", "shared body a\n"),
        ("Notes/SharedB.md", "REC-SHARED-B", "shared body b\n"),
    ] {
        write_vault_file(dir, file, body);
        write_base_copy(dir, rec, body).unwrap();
    }
    let mut notes = IndexMap::from([
        ("REC-PRIVATE".to_string(), shared_entry("Notes/Private.md", None)),
        (
            "REC-SHARED-A".to_string(),
            shared_entry("Notes/SharedA.md", Some("_ownerA")),
        ),
        (
            "REC-SHARED-B".to_string(),
            shared_entry("Notes/SharedB.md", Some("_ownerB")),
        ),
    ]);
    let mut summary = PullSummary::default();
    let removed = resync(
        dir,
        Some("_ownerA"),
        &seen(&[]),
        &mut notes,
        &mut IndexMap::new(),
        &mut summary,
    );
    assert_eq!(removed, 1);
    assert!(!notes.contains_key("REC-SHARED-A"));
    assert!(notes.contains_key("REC-PRIVATE"));
    assert!(notes.contains_key("REC-SHARED-B"));
    assert!(dir.join("Notes/Private.md").exists());
    assert!(dir.join("Notes/SharedB.md").exists());
}

#[test]
fn present_under_another_record_type_is_not_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Locked.md", "locked body\n");
    write_base_copy(dir, "REC-LOCKED", "locked body\n").unwrap();
    let mut notes = IndexMap::from([("REC-LOCKED".to_string(), entry_for("Notes/Locked.md"))]);
    let mut summary = PullSummary::default();
    let removed = resync(
        dir,
        None,
        &seen(&["REC-LOCKED"]),
        &mut notes,
        &mut IndexMap::new(),
        &mut summary,
    );
    assert_eq!(removed, 0);
    assert!(notes.contains_key("REC-LOCKED"));
    assert!(dir.join("Notes/Locked.md").exists());
    assert!(summary.changes.is_empty());
}

#[test]
#[ignore = "needs A/B/C"]
fn reconciled_deletion_drops_attachment_tracking_and_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/WithAttachment.md", "body\n");
    write_base_copy(dir, "REC-NOTE", "body\n").unwrap();
    write_vault_file(dir, "Notes/attachments/pic.png", "png-bytes");
    let mut notes = IndexMap::from([("REC-NOTE".to_string(), entry_for("Notes/WithAttachment.md"))]);
    let mut attachments = IndexMap::from([(
        "REC-ATTACHMENT".to_string(),
        AttachmentEntry {
            file: "Notes/attachments/pic.png".into(),
            media_record_name: "REC-MEDIA".into(),
            media_file_checksum: "checksum".into(),
            note_record_name: "REC-NOTE".into(),
        },
    )]);
    let mut summary = PullSummary::default();
    let removed = resync(dir, None, &seen(&[]), &mut notes, &mut attachments, &mut summary);
    assert_eq!(removed, 1);
    assert!(!attachments.contains_key("REC-ATTACHMENT"));
    assert!(!dir.join("Notes/attachments/pic.png").exists());
}

// --- pullTitleMode.test.ts ----------------------------------------------------------

fn vault() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("Notes")).unwrap();
    tmp
}

fn used(files: &[&str]) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for file in files {
        let (dir, name) = file.rsplit_once('/').unwrap();
        map.entry(dir.to_owned()).or_default().insert(name.to_owned());
    }
    map
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn rename(
    dir: &Path,
    file: &str,
    title: &str,
    mode: TitleMode,
    used: &mut HashMap<String, HashSet<String>>,
    on_disk: bool,
    defer: bool,
) -> (String, Option<String>) {
    rename_for_remote_title(dir, file, title, mode, used, on_disk, defer).unwrap()
}

#[test]
#[ignore = "needs A/B/C"]
fn remote_retitle_renames_the_file() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Shopping list.md", "Milk");
    let (file, _) = rename(
        dir,
        "Notes/Shopping list.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Shopping list.md"]),
        true,
        false,
    );
    assert_eq!(file, "Notes/Groceries.md");
    assert_eq!(names_in(&dir.join("Notes")), vec!["Groceries.md"]);
    assert_eq!(read(dir, &file), "Milk");
}

#[test]
#[ignore = "needs A/B/C"]
fn file_already_carrying_title_is_left_alone() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Groceries.md", "Milk");
    let (file, _) = rename(
        dir,
        "Notes/Groceries.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Groceries.md"]),
        true,
        false,
    );
    assert_eq!(file, "Notes/Groceries.md");
}

#[test]
#[ignore = "needs A/B/C"]
fn uniquified_name_is_not_walked_up() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Groceries.md", "Other note");
    write_vault_file(dir, "Notes/Groceries 2.md", "Milk");
    let (file, _) = rename(
        dir,
        "Notes/Groceries 2.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Groceries.md", "Notes/Groceries 2.md"]),
        true,
        false,
    );
    assert_eq!(file, "Notes/Groceries 2.md");
}

#[test]
#[ignore = "needs A/B/C"]
fn rename_never_lands_on_an_untracked_file() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Shopping list.md", "Milk");
    write_vault_file(dir, "Notes/Groceries.md", "MINE, UNTRACKED");
    let (file, _) = rename(
        dir,
        "Notes/Shopping list.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Shopping list.md"]),
        true,
        false,
    );
    assert_eq!(file, "Notes/Groceries 2.md");
    assert_eq!(read(dir, "Notes/Groceries.md"), "MINE, UNTRACKED");
    assert_eq!(read(dir, &file), "Milk");
}

#[test]
#[ignore = "needs A/B/C"]
fn homoglyph_spelling_is_used() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Old.md", "Body");
    let (file, _) = rename(
        dir,
        "Notes/Old.md",
        "Pat/Alex: notes",
        TitleMode::Filename,
        &mut used(&["Notes/Old.md"]),
        true,
        false,
    );
    assert_eq!(file, "Notes/Pat⁄Alex꞉ notes.md");
    assert_eq!(read(dir, &file), "Body");
}

#[test]
#[ignore = "needs A/B/C"]
fn missing_file_placed_at_new_name_without_rename() {
    let tmp = vault();
    let dir = tmp.path();
    let (file, _) = rename(
        dir,
        "Notes/Shopping list.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Shopping list.md"]),
        false,
        false,
    );
    assert_eq!(file, "Notes/Groceries.md");
    assert!(names_in(&dir.join("Notes")).is_empty());
}

#[test]
fn in_body_vault_never_renames() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Shopping list.md", "Groceries\n\nMilk");
    let (file, _) = rename(
        dir,
        "Notes/Shopping list.md",
        "Groceries",
        TitleMode::InBody,
        &mut used(&["Notes/Shopping list.md"]),
        true,
        false,
    );
    assert_eq!(file, "Notes/Shopping list.md");
    assert_eq!(names_in(&dir.join("Notes")), vec!["Shopping list.md"]);
}

#[test]
#[ignore = "needs A/B/C"]
fn defer_records_the_rename_instead() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/Shopping list.md", "Milk");
    let result = rename(
        dir,
        "Notes/Shopping list.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Shopping list.md"]),
        true,
        true,
    );
    assert_eq!(
        result,
        ("Notes/Shopping list.md".to_string(), Some("Groceries.md".to_string()))
    );
    assert_eq!(names_in(&dir.join("Notes")), vec!["Shopping list.md"]);
}

#[test]
#[ignore = "needs A/B/C"]
fn deferred_rename_holds_both_names() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/A.md", "first");
    write_vault_file(dir, "Notes/B.md", "second");
    let mut u = used(&["Notes/A.md", "Notes/B.md"]);
    rename(dir, "Notes/A.md", "C", TitleMode::Filename, &mut u, true, true);
    let wants_a = rename(dir, "Notes/B.md", "A", TitleMode::Filename, &mut u, true, true);
    let wants_c = rename(dir, "Notes/B.md", "C", TitleMode::Filename, &mut u, true, true);
    assert_eq!(wants_a.1.as_deref(), Some("A 2.md"));
    assert_eq!(wants_c.1.as_deref(), Some("C 2.md"));
}

#[test]
#[ignore = "needs A/B/C"]
fn missing_file_is_never_deferred() {
    let tmp = vault();
    let dir = tmp.path();
    let result = rename(
        dir,
        "Notes/Shopping list.md",
        "Groceries",
        TitleMode::Filename,
        &mut used(&["Notes/Shopping list.md"]),
        false,
        true,
    );
    assert_eq!(result, ("Notes/Groceries.md".to_string(), None));
}

#[test]
#[ignore = "needs A/B/C"]
fn freed_name_is_available_to_the_next_note() {
    let tmp = vault();
    let dir = tmp.path();
    write_vault_file(dir, "Notes/A.md", "first");
    write_vault_file(dir, "Notes/B.md", "second");
    let mut u = used(&["Notes/A.md", "Notes/B.md"]);
    let (first, _) = rename(dir, "Notes/A.md", "C", TitleMode::Filename, &mut u, true, false);
    let (second, _) = rename(dir, "Notes/B.md", "A", TitleMode::Filename, &mut u, true, false);
    assert_eq!(first, "Notes/C.md");
    assert_eq!(second, "Notes/A.md");
    assert_eq!(read(dir, "Notes/A.md"), "second");
    assert_eq!(read(dir, "Notes/C.md"), "first");
}

// --- pullReport.test.ts ---------------------------------------------------------------

fn id(f: &str) -> String {
    f.to_owned()
}

fn change(kind: K, file: &str) -> PullChange {
    PullChange::new(kind, file)
}

fn remark(tone: &str, message: &str) -> PullChangeRemark {
    PullChangeRemark {
        tone: tone.into(),
        message: message.into(),
    }
}

fn with_remarks(mut c: PullChange, remarks: Vec<PullChangeRemark>) -> PullChange {
    c.remarks = Some(remarks);
    c
}

#[test]
fn report_says_already_up_to_date() {
    assert_eq!(
        render_pull_report(&PullSummary::default(), &id),
        vec!["Already up to date."]
    );
}

#[test]
fn report_wraps_changelist_with_heading_and_tally() {
    let mut moved = change(K::Move, "Recipes/Pie.md");
    moved.previous_file = Some("Pie.md".into());
    let summary = PullSummary {
        added: 1,
        updated: 1,
        removed: 1,
        changes: vec![
            change(K::Add, "New.md"),
            change(K::Update, "Edited.md"),
            change(K::Remove, "Gone.md"),
            moved,
        ],
        ..Default::default()
    };
    assert_eq!(
        render_pull_report(&summary, &id),
        vec![
            "Changes pulled from iCloud:",
            "",
            "        new file:  New.md",
            "        modified:  Edited.md",
            "        deleted:   Gone.md",
            "        moved:     Pie.md -> Recipes/Pie.md",
            "",
            "1 added, 1 updated, 0 auto-merged, 1 deleted, 1 moved.",
        ]
    );
}

#[test]
fn report_indents_remarks_under_subject() {
    let summary = PullSummary {
        merged: 1,
        conflicts: vec!["Torn.md: merged with conflict markers - resolve manually".into()],
        changes: vec![
            change(K::Merge, "Clean.md"),
            with_remarks(
                change(K::Merge, "Torn.md"),
                vec![remark("conflict", "merged with conflict markers - resolve manually")],
            ),
        ],
        ..Default::default()
    };
    assert_eq!(
        render_pull_report(&summary, &id),
        vec![
            "Changes pulled from iCloud:",
            "",
            "        merged:    Clean.md",
            "        merged:    Torn.md",
            "                   ! merged with conflict markers - resolve manually",
            "",
            "0 added, 0 updated, 1 auto-merged, 0 deleted. (1 conflict(s))",
        ]
    );
}

#[test]
fn report_tallies_untracked_and_read_only() {
    let summary = PullSummary {
        added: 1,
        unpublishable: 1,
        dropped_unsyncable: 1,
        attachments_downloaded: 2,
        changes: vec![
            with_remarks(
                change(K::Add, "Scanned.md"),
                vec![remark(
                    "unsyncable",
                    "read-only: contains content this tool couldn't fully parse",
                )],
            ),
            with_remarks(
                change(K::Untrack, "Broken.md"),
                vec![remark(
                    "unsyncable",
                    "no longer syncable remotely (missing text data) - local copy left in place",
                )],
            ),
        ],
        ..Default::default()
    };
    assert_eq!(
        render_pull_report(&summary, &id),
        vec![
            "Changes pulled from iCloud:",
            "",
            "        new file:  Scanned.md",
            "                   ! read-only: contains content this tool couldn't fully parse",
            "        untracked: Broken.md",
            "                   ! no longer syncable remotely (missing text data) - local copy left in place",
            "",
            "1 added, 0 updated, 0 auto-merged, 0 deleted, 1 untracked, 2 attachment(s) downloaded. (1 read-only)",
        ]
    );
}

#[test]
fn report_formats_both_halves_of_a_move() {
    let mut moved = change(K::Move, "Recipes/Pie.md");
    moved.previous_file = Some("Pie.md".into());
    let summary = PullSummary {
        changes: vec![moved],
        ..Default::default()
    };
    let lines = render_pull_report(&summary, &|f| format!("../{f}"));
    assert!(
        lines[2].ends_with("moved:     ../Pie.md -> ../Recipes/Pie.md"),
        "{lines:?}"
    );
}

#[test]
fn report_shows_retitle_as_old_to_new() {
    let mut updated = change(K::Update, "Notes/Groceries.md");
    updated.previous_file = Some("Notes/Shopping list.md".into());
    let summary = PullSummary {
        updated: 1,
        changes: vec![updated],
        ..Default::default()
    };
    assert_eq!(
        render_pull_report(&summary, &id),
        vec![
            "Changes pulled from iCloud:",
            "",
            "        modified:  Notes/Shopping list.md -> Notes/Groceries.md",
            "",
            "0 added, 1 updated, 0 auto-merged, 0 deleted.",
        ]
    );
}

#[test]
fn report_renders_note_remarks_unmarked() {
    let summary = PullSummary {
        added: 1,
        changes: vec![with_remarks(
            change(K::Add, "Notes/Untitled.md"),
            vec![remark(
                "note",
                "title kept in apple-note-title: the title starts with a dot",
            )],
        )],
        ..Default::default()
    };
    assert_eq!(
        render_pull_report(&summary, &id),
        vec![
            "Changes pulled from iCloud:",
            "",
            "        new file:  Notes/Untitled.md",
            "                     title kept in apple-note-title: the title starts with a dot",
            "",
            "1 added, 0 updated, 0 auto-merged, 0 deleted.",
        ]
    );
}

#[test]
fn report_shows_deferred_rename_as_remark_not_arrow() {
    let mut updated = with_remarks(
        change(K::Update, "Notes/Shopping list.md"),
        vec![remark(
            "note",
            "rename deferred: this file should become \"Groceries.md\"",
        )],
    );
    updated.pending_rename = Some("Notes/Groceries.md".into());
    let summary = PullSummary {
        updated: 1,
        changes: vec![updated],
        ..Default::default()
    };
    assert_eq!(
        render_pull_report(&summary, &id),
        vec![
            "Changes pulled from iCloud:",
            "",
            "        modified:  Notes/Shopping list.md",
            "                     rename deferred: this file should become \"Groceries.md\"",
            "",
            "0 added, 1 updated, 0 auto-merged, 0 deleted.",
        ]
    );
}
