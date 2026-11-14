//! A sync stopped at every durable step, as a kill would stop it, then run
//! again: each scenario is run once whole for its list of crash points, then
//! once per point, stopped there and synced again twice. Each must end where
//! the whole run ended: the same files with the same text on both sides,
//! nothing else in either trash, a further sync with nothing to do, and a
//! state that reads back. (Debug builds only: release builds have no hook.)
#![cfg(debug_assertions)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};

use common::{Bed, crash_at, quiet};
use icloud_drive::crash;
use icloud_drive::remote::ROOT_ID;
use icloud_drive::state::State;

type Tree = BTreeMap<String, Option<String>>;

/// Both sides and both trashes.
#[derive(Debug, PartialEq, Eq)]
struct View {
    here: Tree,
    there: Tree,
    trashed_here: Vec<String>,
    trashed_there: Vec<String>,
}

fn view(bed: &Bed) -> View {
    View {
        here: bed.local_tree(),
        there: bed.cloud.tree(),
        trashed_here: bed.trashed_here(),
        trashed_there: bed.cloud.trashed_texts(),
    }
}

/// A sync until it has nothing left to do, which must come by the second.
fn settle(bed: &Bed, what: &str) {
    for _ in 0..2 {
        bed.try_sync(false).unwrap_or_else(|e| panic!("{what}: {e}"));
    }
    let last = bed.try_sync(false).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(last.done.is_empty(), "{what}: not settled: {:?}", last.done);
}

/// The points the sync after `setup` reaches.
fn trace(setup: fn(&Bed)) -> Vec<&'static str> {
    let bed = Bed::new();
    setup(&bed);
    crash::arm(None);
    let outcome = bed.try_sync(false);
    let trace = crash::disarm();
    let outcome = outcome.unwrap();
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    trace
}

fn check(setup: fn(&Bed)) {
    quiet();
    let reference = Bed::new();
    setup(&reference);
    crash::arm(None);
    reference.try_sync(false).unwrap();
    let points = crash::disarm();
    settle(&reference, "the run with no stop");
    let want = view(&reference);
    assert!(!points.is_empty());
    let mut failed = Vec::new();
    for (at, name) in points.iter().enumerate() {
        let what = format!("stopped at {name} (point {at} of {})", points.len());
        let bed = Bed::new();
        setup(&bed);
        assert_eq!(
            crash_at(&bed, false, || crash::arm(Some(at))),
            Some(*name),
            "{what}: the run did not replay"
        );
        let resumed = catch_unwind(AssertUnwindSafe(|| {
            settle(&bed, &what);
            let got = view(&bed);
            assert_eq!(got, want, "{what}");
            let state = State::open(&bed.dir.path().join("state")).unwrap();
            let rows: BTreeSet<String> = state.rows().unwrap().into_iter().map(|r| r.path).collect();
            let here: BTreeSet<String> = got.here.keys().cloned().collect();
            assert_eq!(rows, here, "{what}: the state reads back as both sides");
            assert!(state.pending_moves().unwrap().is_empty(), "{what}");
        }));
        if resumed.is_err() {
            failed.push(what);
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} stops did not end as the whole run: {failed:#?}",
        failed.len(),
        points.len()
    );
}

// ------------------------------------------------------------ the scenarios

/// Edits on each side and both, new items on each side, removals on each
/// side, and a file iCloud replaced with a new one of its name.
fn contents(bed: &Bed) {
    let c = &bed.cloud;
    c.file(ROOT_ID, "a.txt", "a1", bed.time());
    let b = c.file(ROOT_ID, "b.txt", "b1", bed.time());
    let x = c.file(ROOT_ID, "x.txt", "x1", bed.time());
    c.file(ROOT_ID, "d.txt", "d1", bed.time());
    let e = c.file(ROOT_ID, "e.txt", "e1", bed.time());
    let f = c.file(ROOT_ID, "f.txt", "f1", bed.time());
    bed.settle();
    bed.write("a.txt", "a2 from here");
    c.edit(&b.id, "b2 from there", bed.time());
    bed.write("x.txt", "x2 from here");
    c.edit(&x.id, "x2 from there", bed.time());
    fs::remove_file(bed.root().join("d.txt")).unwrap();
    c.remove(&e.id);
    c.remove(&f.id);
    c.file(ROOT_ID, "f.txt", "f2 a new file", bed.time());
    bed.write("New here/n.txt", "new here");
    let there = c.folder(ROOT_ID, "New there");
    c.file(&there.id, "t.txt", "new there", bed.time());
}

/// Renames and moves on each side, of files and of folders.
fn moves(bed: &Bed) {
    let c = &bed.cloud;
    let docs = c.folder(ROOT_ID, "Docs");
    c.file(&docs.id, "in.txt", "inside", bed.time());
    let mine = c.folder(ROOT_ID, "Mine");
    c.file(&mine.id, "m.txt", "mine", bed.time());
    c.file(ROOT_ID, "r.txt", "renamed here", bed.time());
    let s = c.file(ROOT_ID, "s.txt", "renamed there", bed.time());
    c.file(ROOT_ID, "into.txt", "moved into a new folder here", bed.time());
    c.file(ROOT_ID, "Case.txt", "a case-only rename here", bed.time());
    let taken = c.file(ROOT_ID, "taken.txt", "renamed there onto a name new here", bed.time());
    bed.settle();
    fs::rename(bed.root().join("r.txt"), bed.root().join("r2.txt")).unwrap();
    c.rename_item(&s.id, "s2.txt");
    fs::create_dir_all(bed.root().join("Fresh/deeper")).unwrap();
    fs::rename(bed.root().join("into.txt"), bed.root().join("Fresh/deeper/into2.txt")).unwrap();
    fs::rename(bed.root().join("Case.txt"), bed.root().join("case.txt")).unwrap();
    c.rename_item(&docs.id, "Papers");
    fs::rename(bed.root().join("Mine"), bed.root().join("Ours")).unwrap();
    c.rename_item(&taken.id, "untracked.txt");
    bed.write("untracked.txt", "made here meanwhile");
}

/// A folder removed in iCloud, one removed here, and one removed in iCloud
/// that gained a file here.
fn folders_removed(bed: &Bed) {
    let c = &bed.cloud;
    let old = c.folder(ROOT_ID, "Old");
    c.file(&old.id, "o.txt", "removed there", bed.time());
    let gone = c.folder(ROOT_ID, "Gone");
    c.file(&gone.id, "g.txt", "removed here", bed.time());
    let kept = c.folder(ROOT_ID, "Kept");
    c.file(&kept.id, "k.txt", "removed there", bed.time());
    bed.settle();
    c.remove(&old.id);
    fs::remove_dir_all(bed.root().join("Gone")).unwrap();
    c.remove(&kept.id);
    bed.write("Kept/added.txt", "added here meanwhile");
}

/// The first sync, into a folder that is not there yet.
fn first_sync_into_nothing(bed: &Bed) {
    let c = &bed.cloud;
    let docs = c.folder(ROOT_ID, "Docs");
    c.file(&docs.id, "a.txt", "from icloud", bed.time());
    c.file(ROOT_ID, "empty.txt", "", bed.time());
}

/// The first sync, with both sides holding things, some the same.
fn first_sync_of_both(bed: &Bed) {
    let c = &bed.cloud;
    let at = bed.time();
    c.file(ROOT_ID, "same.txt", "same", at);
    bed.write("same.txt", "same");
    local_mtime(bed, "same.txt", at);
    c.file(ROOT_ID, "differ.txt", "theirs", bed.time());
    bed.write("differ.txt", "mine!!");
    let docs = c.folder(ROOT_ID, "Docs");
    c.file(&docs.id, "a.txt", "from icloud", bed.time());
    bed.write("Docs/b.txt", "from here");
    bed.write("Mine/c.txt", "from here");
}

fn local_mtime(bed: &Bed, path: &str, ms: i64) {
    icloud_drive::local::set_mtime(&bed.root().join(path), ms).unwrap();
}

/// An edit here whose replace iCloud refuses as stale: an edit there its
/// size and date did not show.
fn stale_replace(bed: &Bed) {
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "ONE", a.mtime_ms);
    bed.write("a.txt", "mine");
}

/// A package is found, and one found before is gone from iCloud.
fn packages(bed: &Bed) {
    let c = &bed.cloud;
    let old = c.file(ROOT_ID, "old.pages", "bundle", bed.time());
    c.0.borrow_mut().packages.push(old.id.clone());
    c.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.sync();
    bed.sync();
    c.remove(&old.id);
    let new = c.file(ROOT_ID, "new.pages", "bundle", bed.time());
    c.0.borrow_mut().packages.push(new.id.clone());
}

/// A move here that a sync stopped just after noting, then put back here:
/// the note is void and is dropped.
fn a_noted_move_put_back(bed: &Bed) {
    bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/b.txt")).unwrap();
    stop_at_first(bed, "state.set_pending_move");
    fs::rename(bed.root().join("Docs/b.txt"), bed.root().join("a.txt")).unwrap();
}

/// A move here that iCloud did half of (the folder, not the name), then
/// moved on here again.
fn a_half_done_move(bed: &Bed) {
    bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.folder(ROOT_ID, "Other");
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/b.txt")).unwrap();
    bed.cloud.0.borrow_mut().rename_fails_once = true;
    bed.try_sync(false).unwrap();
    fs::rename(bed.root().join("Docs/b.txt"), bed.root().join("Other/c.txt")).unwrap();
}

/// Runs a sync stopped at the first point of this name.
fn stop_at_first(bed: &Bed, name: &'static str) {
    assert_eq!(crash_at(bed, false, || crash::arm_named(name)), Some(name));
}

const SCENARIOS: &[fn(&Bed)] = &[
    contents,
    moves,
    folders_removed,
    first_sync_into_nothing,
    first_sync_of_both,
    stale_replace,
    packages,
    a_noted_move_put_back,
    a_half_done_move,
];

#[test]
fn every_point_is_reached_by_a_scenario() {
    quiet();
    let reached: BTreeSet<&str> = SCENARIOS.iter().flat_map(|setup| trace(*setup)).collect();
    let all: BTreeSet<&str> = crash::POINTS.iter().copied().collect();
    let missed: Vec<_> = all.difference(&reached).collect();
    assert!(missed.is_empty(), "no scenario reaches {missed:?}");
}

#[test]
fn a_sync_stopped_while_carrying_over_edits_new_items_and_removals() {
    check(contents);
}

#[test]
fn a_sync_stopped_while_moving_and_renaming() {
    check(moves);
}

#[test]
fn a_sync_stopped_while_removing_folders() {
    check(folders_removed);
}

#[test]
fn a_first_sync_into_a_missing_folder_stopped() {
    check(first_sync_into_nothing);
}

#[test]
fn a_first_sync_of_two_full_sides_stopped() {
    check(first_sync_of_both);
}

#[test]
fn a_sync_stopped_while_keeping_both_after_a_stale_replace() {
    check(stale_replace);
}

#[test]
fn a_sync_stopped_while_setting_packages_aside() {
    check(packages);
}

#[test]
fn a_sync_stopped_after_dropping_a_void_move_note() {
    check(a_noted_move_put_back);
}

#[test]
fn a_sync_stopped_while_finishing_a_half_done_move() {
    check(a_half_done_move);
}

#[test]
fn a_move_stopped_after_making_its_folder_here_follows_a_rename_of_that_folder_by_case() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    let q = bed.cloud.folder(ROOT_ID, "Q");
    let node = bed.cloud.0.borrow().nodes[&a.id].clone();
    use icloud_drive::remote::Remote;
    bed.cloud.move_to(&node, &q.id).unwrap();
    // Stopped once the folder is made here, before it is recorded.
    stop_at_first(&bed, "local.mkdir");
    bed.cloud.rename_item(&q.id, "q");
    settle(&bed, "after the stop");
    assert_eq!(bed.cloud.paths(), ["q", "q/a.txt"]);
    assert_eq!(bed.local_paths(), ["q", "q/a.txt"], "not Q beside q");
}

#[test]
fn a_download_stopped_beside_a_name_of_another_case_leaves_no_pair_of_names() {
    let bed = Bed::new();
    let b = bed.cloud.file(ROOT_ID, "b.txt", "one", bed.time());
    bed.settle();
    // Edited on both sides, and renamed here by case: iCloud's comes back
    // under its name, and this one must not stay beside it.
    bed.write("b.txt", "mine");
    bed.cloud.edit(&b.id, "theirs", bed.time());
    fs::rename(bed.root().join("b.txt"), bed.root().join("B.TXT")).unwrap();
    stop_at_first(&bed, "local.download_rename");
    settle(&bed, "after the stop");
    assert_eq!(bed.local_paths(), bed.cloud.paths());
    assert_eq!(bed.cloud.text("b.txt").as_deref(), Some("theirs"));
    assert_eq!(bed.local_paths().len(), 2, "{:?}", bed.local_paths());
}

#[test]
fn a_download_stopped_before_it_was_recorded_and_then_moved_here_is_not_sent_again() {
    for point in ["local.download_rename", "local.mkdir"] {
        let bed = Bed::new();
        let docs = bed.cloud.folder(ROOT_ID, "Docs");
        let a = bed.cloud.file(&docs.id, "a.txt", "from icloud", bed.time());
        stop_at_first(&bed, point);
        // Moved here before the next sync.
        fs::create_dir_all(bed.root().join("Docs")).unwrap();
        let moved = if point == "local.mkdir" { "Papers" } else { "Docs/b.txt" };
        let from = if point == "local.mkdir" { "Docs" } else { "Docs/a.txt" };
        if bed.root().join(from).exists() {
            fs::rename(bed.root().join(from), bed.root().join(moved)).unwrap();
        }
        let uploads = bed.cloud.0.borrow().uploads;
        settle(&bed, point);
        assert_eq!(bed.cloud.0.borrow().uploads, uploads, "{point}: nothing sent again");
        assert!(bed.cloud.0.borrow().trashed.is_empty(), "{point}");
        assert_eq!(bed.local_paths(), bed.cloud.paths(), "{point}");
        let texts: Vec<_> = bed.cloud.tree().into_values().flatten().collect();
        assert_eq!(texts, ["from icloud"], "{point}: once");
        let _ = a;
    }
}

#[test]
fn an_upload_stopped_before_it_was_recorded_and_then_moved_here_is_not_sent_again() {
    for point in ["remote.upload_new", "remote.create_folder"] {
        let bed = Bed::new();
        bed.cloud.folder(ROOT_ID, "Docs");
        bed.settle();
        bed.write("New/a.txt", "from here");
        if point == "remote.upload_new" {
            bed.sync();
            bed.write("Docs/b.txt", "from here too");
        }
        stop_at_first(&bed, point);
        if point == "remote.upload_new" {
            fs::rename(bed.root().join("Docs/b.txt"), bed.root().join("b.txt")).unwrap();
        } else {
            fs::rename(bed.root().join("New"), bed.root().join("Newer")).unwrap();
        }
        settle(&bed, point);
        assert!(bed.cloud.0.borrow().trashed.is_empty(), "{point}");
        assert_eq!(bed.local_paths(), bed.cloud.paths(), "{point}");
        let mut texts: Vec<_> = bed.cloud.tree().into_values().flatten().collect();
        texts.sort();
        let want: &[&str] = if point == "remote.upload_new" {
            &["from here", "from here too"]
        } else {
            &["from here"]
        };
        assert_eq!(texts, want, "{point}: once each");
    }
}

#[test]
fn a_replace_stopped_before_it_was_recorded_and_then_moved_here_is_kept_once() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.write("a.txt", "two from here");
    stop_at_first(&bed, "remote.replace");
    fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
    settle(&bed, "after the stop");
    assert_eq!(bed.cloud.paths(), ["b.txt"]);
    assert_eq!(bed.cloud.id_at("b.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.text("b.txt").as_deref(), Some("two from here"));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}

#[test]
fn an_upload_stopped_before_it_was_recorded_and_then_renamed_in_icloud_is_not_sent_again() {
    let bed = Bed::new();
    bed.write("c.md", "from here");
    stop_at_first(&bed, "remote.upload_new");
    let id = bed.cloud.id_at("c.md").unwrap();
    bed.cloud.rename_item(&id, "d.md");
    settle(&bed, "after the stop");
    assert_eq!(bed.cloud.paths(), ["d.md"]);
    assert_eq!(bed.local_paths(), ["d.md"]);
}
