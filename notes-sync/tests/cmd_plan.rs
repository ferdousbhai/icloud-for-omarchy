//! Ports icloud-md `src/notes/pushPlan.test.ts`. Human text is plain (no
//! colours) and names `icloud-notes-sync`; the two chalk colour-forcing
//! tests are not ported (the port prints no ANSI colours).

use icloud_notes_sync::cmd::plan::{
    PlanEntry, PlanEntryKind as K, PlanResolution as R, RenderPlanOptions, SerializedPlanEntry, count_unchanged_notes,
    render_plan,
};

fn e(kind: K, file: &str, resolution: R) -> SerializedPlanEntry {
    PlanEntry::new(kind, file, resolution).serialize()
}

fn er(kind: K, file: &str, resolution: R, reason: &str) -> SerializedPlanEntry {
    let mut s = e(kind, file, resolution);
    s.reason = Some(reason.into());
    s
}

fn id(f: &str) -> String {
    f.to_owned()
}

fn render(entries: &[SerializedPlanEntry]) -> Vec<String> {
    render_plan(entries, &id, RenderPlanOptions::default())
}

#[test]
fn nothing_to_push_for_an_empty_plan() {
    assert_eq!(render(&[]), vec!["Nothing to push."]);
}

#[test]
fn nothing_to_push_when_every_entry_is_a_noop() {
    let entries = [e(K::Update, "Clean.md", R::Noop), e(K::Update, "AlsoClean.md", R::Noop)];
    assert_eq!(render(&entries), vec!["Nothing to push."]);
}

#[test]
fn lists_ready_entries_plus_summary() {
    let entries = [
        e(K::Create, "New.md", R::Ready),
        e(K::Update, "Edited.md", R::Ready),
        e(K::Delete, "Gone.md", R::Ready),
    ];
    assert_eq!(
        render(&entries),
        vec![
            "",
            "        new file: New.md",
            "        modified: Edited.md",
            "        deleted:  Gone.md",
            "",
            "1 to create, 1 changed, 1 to delete.",
        ]
    );
}

#[test]
fn refused_and_conflict_entries_get_reason_lines_and_separate_tallies() {
    let entries = [
        er(K::Update, "Refused.md", R::Refused, "this note has an attachment"),
        er(
            K::Delete,
            "Stale.md",
            R::Conflict,
            "changed remotely since the last pull - run \"pull\" first",
        ),
    ];
    assert_eq!(
        render(&entries),
        vec![
            "",
            "        modified: Refused.md",
            "                  ! this note has an attachment",
            "        deleted:  Stale.md",
            "                  ! changed remotely since the last pull - run \"pull\" first",
            "",
            "0 to create, 0 changed, 0 to delete. (1 conflict(s), 1 refused)",
        ]
    );
}

#[test]
fn noop_entries_are_omitted_but_their_neighbours_kept() {
    let entries = [e(K::Update, "Clean.md", R::Noop), e(K::Update, "Edited.md", R::Ready)];
    assert_eq!(
        render(&entries),
        vec![
            "",
            "        modified: Edited.md",
            "",
            "0 to create, 1 changed, 0 to delete."
        ]
    );
}

#[test]
fn preview_mode_adds_heading_hints_and_unchanged_remark() {
    let entries = [
        e(K::Update, "Edited.md", R::Ready),
        er(K::Update, "Refused.md", R::Refused, "this note has an attachment"),
    ];
    let lines = render_plan(
        &entries,
        &id,
        RenderPlanOptions {
            preview: true,
            unchanged: Some(41),
        },
    );
    assert_eq!(
        lines,
        vec![
            "Changes not yet pushed to iCloud:",
            "  (use \"icloud-notes-sync push\" to send them)",
            "  (use \"icloud-notes-sync restore <file>\" to discard a local edit)",
            "",
            "        modified: Edited.md",
            "        modified: Refused.md",
            "                  ! this note has an attachment",
            "",
            "0 to create, 1 changed, 0 to delete. (1 refused)",
            "41 other notes match the last pull.",
        ]
    );
}

#[test]
fn single_unchanged_note_is_grammatical() {
    let entries = [e(K::Update, "Edited.md", R::Ready)];
    let lines = render_plan(
        &entries,
        &id,
        RenderPlanOptions {
            preview: false,
            unchanged: Some(1),
        },
    );
    assert_eq!(lines.last().unwrap(), "1 other note matches the last pull.");
}

#[test]
fn unchanged_remark_omitted_when_zero() {
    let entries = [e(K::Update, "Edited.md", R::Ready)];
    let lines = render_plan(
        &entries,
        &id,
        RenderPlanOptions {
            preview: true,
            unchanged: Some(0),
        },
    );
    assert_eq!(lines.last().unwrap(), "0 to create, 1 changed, 0 to delete.");
}

#[test]
fn nothing_to_push_folds_in_the_unchanged_count() {
    let opts = |n| RenderPlanOptions {
        preview: true,
        unchanged: Some(n),
    };
    assert_eq!(
        render_plan(&[], &id, opts(42)),
        vec!["Nothing to push; all 42 notes match the last pull."]
    );
    assert_eq!(
        render_plan(&[], &id, opts(1)),
        vec!["Nothing to push; all 1 note matches the last pull."]
    );
    assert_eq!(render_plan(&[], &id, opts(0)), vec!["Nothing to push."]);
}

#[test]
fn count_unchanged_notes_treats_creates_as_untracked_and_noops_as_unchanged() {
    let entries = [
        e(K::Create, "New.md", R::Ready),
        e(K::Update, "Edited.md", R::Ready),
        e(K::Update, "Cosmetic.md", R::Noop),
        er(K::Delete, "Gone.md", R::Conflict, "changed remotely"),
    ];
    assert_eq!(count_unchanged_notes(&entries, 10), 8);
    assert_eq!(count_unchanged_notes(&[], 10), 10);
}

#[test]
fn format_path_applies_inside_reason_lines() {
    let entries = [er(
        K::Update,
        "Recipes/Pie.md",
        R::Refused,
        "this note has an attachment - run \"icloud-notes restore Recipes/Pie.md\" to discard your local edit",
    )];
    let lines = render_plan(&entries, &|f| format!("../{f}"), RenderPlanOptions::default());
    assert!(lines[1].contains("modified: ../Recipes/Pie.md"), "{lines:?}");
    assert!(lines[2].contains("restore ../Recipes/Pie.md"), "{lines:?}");
}

#[test]
fn pending_rename_shows_target_and_tallies_as_conflict() {
    let mut entry = er(
        K::Rename,
        "Notes/Shopping list.md",
        R::Conflict,
        "rename deferred by a previous pull and not yet performed",
    );
    entry.pending_rename = Some("Notes/Groceries.md".into());
    assert_eq!(
        render(&[entry]),
        vec![
            "",
            "        rename:   Notes/Shopping list.md -> Notes/Groceries.md",
            "                  ! rename deferred by a previous pull and not yet performed",
            "",
            "0 to create, 0 changed, 0 to delete. (1 conflict(s))",
        ]
    );
}

#[test]
fn count_unchanged_ignores_pending_renames() {
    let mut rename = er(K::Rename, "Edited.md", R::Conflict, "pending");
    rename.pending_rename = Some("Renamed.md".into());
    let entries = [rename, e(K::Update, "Edited.md", R::Ready)];
    assert_eq!(count_unchanged_notes(&entries, 10), 9);
}

#[test]
fn folder_create_has_trailing_slash_and_own_count() {
    let mut folder = e(K::CreateFolder, "Recipes", R::Ready);
    folder.folder_title = Some("Recipes".into());
    let lines = render(&[folder, e(K::Create, "Recipes/cake.md", R::Ready)]);
    assert!(
        lines.iter().any(|l| l.contains("new dir:") && l.contains("Recipes/")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("1 to create, 0 changed, 0 to delete, 1 new folder.")),
        "{lines:?}"
    );
}

#[test]
fn folder_count_omitted_without_folder_creates() {
    let lines = render(&[e(K::Create, "Notes/a.md", R::Ready)]);
    assert!(lines.iter().any(|l| l.contains("1 to create, 0 changed, 0 to delete.")));
    assert!(!lines.iter().any(|l| l.contains("new folder")));
}

#[test]
fn serialize_carries_folder_title_only_for_folder_create() {
    let mut folder = PlanEntry::new(K::CreateFolder, "Recipes", R::Ready);
    folder.folder_title = Some("Recipes".into());
    assert_eq!(
        serde_json::to_value(folder.serialize()).unwrap(),
        serde_json::json!({"kind": "createFolder", "file": "Recipes", "resolution": "ready", "folderTitle": "Recipes"})
    );
    let plain = serde_json::to_value(PlanEntry::new(K::Create, "Notes/a.md", R::Ready).serialize()).unwrap();
    assert!(plain.get("folderTitle").is_none());
}

#[test]
fn remark_shown_under_a_ready_entry() {
    let mut entry = e(K::Update, "Untitled.md", R::Ready);
    entry.remark = Some("retitled to \"Groceries\" - the next \"pull\" renames this file to match".into());
    assert_eq!(
        render(&[entry]),
        vec![
            "",
            "        modified: Untitled.md",
            "                  retitled to \"Groceries\" - the next \"pull\" renames this file to match",
            "",
            "0 to create, 1 changed, 0 to delete.",
        ]
    );
}

#[test]
fn remark_never_displaces_a_refusal_reason() {
    let mut entry = er(K::Update, "Untitled.md", R::Refused, "this note has an attachment");
    entry.remark = Some("ignored".into());
    assert_eq!(
        render(&[entry]),
        vec![
            "",
            "        modified: Untitled.md",
            "                  ! this note has an attachment",
            "",
            "0 to create, 0 changed, 0 to delete. (1 refused)",
        ]
    );
}

#[test]
fn serialize_carries_remark_and_omits_it_when_absent() {
    let mut entry = PlanEntry::new(K::Update, "Untitled.md", R::Ready);
    entry.remark = Some("retitled to \"Groceries\"".into());
    assert_eq!(
        serde_json::to_string(&entry.serialize()).unwrap(),
        r#"{"kind":"update","file":"Untitled.md","resolution":"ready","remark":"retitled to \"Groceries\""}"#
    );
    assert_eq!(
        serde_json::to_string(&PlanEntry::new(K::Update, "Edited.md", R::Ready).serialize()).unwrap(),
        r#"{"kind":"update","file":"Edited.md","resolution":"ready"}"#
    );
}
