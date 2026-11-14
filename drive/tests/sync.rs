//! The engine end to end against the in-memory Drive of `common`.

mod common;

use std::fs;

use common::Bed;
use icloud_drive::engine::Error;
use icloud_drive::local;
use icloud_drive::remote::{self, ROOT_ID, Remote};
use icloud_drive::state::State;

#[test]
fn the_first_sync_brings_both_sides_together() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "from icloud", bed.time());
    bed.cloud.file(ROOT_ID, "empty.txt", "", bed.time());
    bed.write("mine/b.txt", "from here");
    bed.settle();
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("from icloud"));
    assert_eq!(bed.read("empty.txt").as_deref(), Some(""));
    assert_eq!(bed.cloud.text("mine/b.txt").as_deref(), Some("from here"));
}

#[test]
fn a_downloaded_file_keeps_iclouds_date() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "x", 1_700_000_000_000);
    bed.settle();
    let entry = local::stat(&bed.root().join("a.txt")).unwrap().unwrap();
    assert_eq!(entry.mtime_ns, 1_700_000_000_000 * 1_000_000);
}

#[test]
fn an_edit_on_either_side_reaches_the_other_and_keeps_the_id() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "two from icloud", bed.time());
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("two from icloud"));
    bed.write("a.txt", "three from here");
    bed.settle();
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("three from here"));
    assert_eq!(bed.cloud.id_at("a.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn edits_on_both_sides_keep_both_files() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "icloud's", bed.time());
    bed.write("a.txt", "mine");
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("icloud's"));
    let copy = bed
        .local_paths()
        .into_iter()
        .find(|p| p.contains("conflicted copy desk"))
        .expect("a conflicted copy");
    assert!(copy.starts_with("a (") && copy.ends_with(").txt"), "{copy}");
    assert_eq!(bed.read(&copy).as_deref(), Some("mine"));
    assert_eq!(bed.cloud.text(&copy).as_deref(), Some("mine"));
}

#[test]
fn an_icloud_edit_its_size_and_date_hide_still_keeps_both_files() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // Same size, same date (a copy that kept its time): only the etag shows it.
    bed.cloud.edit(&a.id, "ONE", a.mtime_ms);
    bed.write("a.txt", "mine");
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("ONE"));
    let copy = bed
        .local_paths()
        .into_iter()
        .find(|p| p.contains("conflicted copy desk"))
        .expect("a conflicted copy");
    assert_eq!(bed.read(&copy).as_deref(), Some("mine"));
    assert_eq!(bed.cloud.text(&copy).as_deref(), Some("mine"));
}

#[test]
fn a_removal_goes_to_the_other_sides_trash() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let b = bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    bed.cloud.remove(&b.id);
    bed.settle();
    assert!(bed.cloud.0.borrow().trashed.contains(&a.id));
    assert_eq!(
        fs::read_to_string(bed.dir.path().join("Trash/files/b.txt")).unwrap(),
        "two"
    );
    assert!(bed.local_paths().is_empty());
}

#[test]
fn an_edit_beats_a_removal() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let b = bed.cloud.file(ROOT_ID, "b.txt", "one", bed.time());
    bed.settle();
    // Removed in iCloud, edited here: it goes back up.
    bed.cloud.remove(&a.id);
    bed.write("a.txt", "edited here");
    // Removed here, edited in iCloud: it comes back down.
    fs::remove_file(bed.root().join("b.txt")).unwrap();
    bed.cloud.edit(&b.id, "edited there", bed.time());
    bed.settle();
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("edited here"));
    assert_eq!(bed.read("b.txt").as_deref(), Some("edited there"));
}

#[test]
fn a_rename_or_move_here_is_not_an_upload() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    bed.settle();
    let uploads = bed.cloud.0.borrow().uploads;
    fs::create_dir_all(bed.root().join("new/deeper")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("new/deeper/b.txt")).unwrap();
    fs::rename(bed.root().join("Docs"), bed.root().join("Papers")).unwrap();
    bed.settle();
    assert_eq!(
        bed.cloud.0.borrow().uploads,
        uploads,
        "moves must not send content again"
    );
    assert_eq!(bed.cloud.id_at("new/deeper/b.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.id_at("Papers").as_deref(), Some(docs.id.as_str()));
    assert_eq!(bed.cloud.text("Papers/in.txt").as_deref(), Some("inside"));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}

#[test]
fn a_folder_renamed_here_with_a_file_moved_into_it_stays_one_folder() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "zDocs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("zDocs"), bed.root().join("Papers")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Papers/a.txt")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.id_at("Papers").as_deref(), Some(docs.id.as_str()));
    assert_eq!(bed.cloud.id_at("Papers/a.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.paths(), ["Papers", "Papers/a.txt", "Papers/in.txt"]);
}

#[test]
fn a_folder_renamed_in_icloud_with_a_file_moved_into_it_stays_one_folder() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    let apple = bed.cloud.file(ROOT_ID, "Apple.txt", "one", bed.time());
    bed.settle();
    bed.cloud.rename_item(&docs.id, "Zed");
    let node = bed.cloud.0.borrow().nodes[&apple.id].clone();
    bed.cloud.move_to(&node, &docs.id).unwrap();
    bed.settle();
    assert_eq!(bed.local_paths(), ["Zed", "Zed/Apple.txt", "Zed/in.txt"]);
    assert_eq!(bed.read("Zed/Apple.txt").as_deref(), Some("one"));
}

#[test]
fn a_rename_in_icloud_is_a_rename_here() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.rename_item(&docs.id, "Papers");
    bed.cloud.edit(&a.id, "two", bed.time());
    bed.settle();
    assert_eq!(bed.read("Papers/a.txt").as_deref(), Some("two"));
    assert!(!bed.root().join("Docs").exists());
    assert!(!bed.dir.path().join("Trash").exists(), "nothing was trashed");
}

#[test]
fn a_listing_that_fails_stops_the_sync_and_removes_nothing() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    // Something changes below Docs, and its listing comes back broken.
    bed.cloud.file(&docs.id, "b.txt", "two", bed.time());
    bed.cloud.0.borrow_mut().broken_listing = Some(docs.id.clone());
    let result = bed.try_sync(false);
    assert!(
        matches!(result, Err(Error::Remote(remote::Error::Listing { .. }))),
        "{result:?}"
    );
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("one"));
    bed.cloud.0.borrow_mut().broken_listing = None;
    bed.settle();
    assert_eq!(bed.read("Docs/b.txt").as_deref(), Some("two"));
}

#[test]
fn many_removals_at_once_wait_to_be_allowed() {
    let bed = Bed::new();
    for n in 0..30 {
        bed.cloud.file(ROOT_ID, &format!("f{n}.txt"), "x", bed.time());
    }
    bed.settle();
    for n in 0..30 {
        fs::remove_file(bed.root().join(format!("f{n}.txt"))).unwrap();
    }
    let result = bed.try_sync(false);
    assert!(
        matches!(
            result,
            Err(Error::TooManyDeletions {
                deletions: 30,
                total: 30
            })
        ),
        "{result:?}"
    );
    assert_eq!(bed.cloud.paths().len(), 30, "nothing was removed");
    // A dry run is how they are looked at: it lists them and removes nothing.
    let looked = bed.run(true, false).unwrap();
    assert_eq!(looked.done.len(), 30, "{:?}", looked.done);
    assert_eq!(bed.cloud.paths().len(), 30);
    bed.try_sync(true).unwrap();
    assert!(bed.cloud.paths().is_empty());
}

#[test]
fn a_missing_folder_is_not_read_as_everything_deleted() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::remove_dir_all(bed.root()).unwrap();
    assert!(matches!(bed.try_sync(true), Err(Error::FolderMissing(_))));
    assert_eq!(bed.cloud.paths(), ["a.txt"]);
}

#[test]
fn an_emptied_folder_is_not_read_as_everything_deleted_unless_allowed() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    // What an unmounted disk's mount point looks like.
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    fs::remove_file(bed.root().join("b.txt")).unwrap();
    assert!(matches!(bed.try_sync(false), Err(Error::TooManyDeletions { .. })));
    assert_eq!(bed.cloud.paths(), ["a.txt", "b.txt"]);
    bed.try_sync(true).unwrap();
    assert!(bed.cloud.paths().is_empty());
}

#[test]
fn a_file_dated_in_the_future_is_still_sent() {
    let bed = Bed::new();
    bed.write("a.txt", "from another clock");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let later = now.as_millis() as i64 + 3_600_000;
    local::set_mtime(&bed.root().join("a.txt"), later).unwrap();
    let outcome = bed.sync();
    assert!(outcome.pending.is_empty(), "{:?}", outcome.pending);
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("from another clock"));
}

#[test]
fn unchanged_folders_are_not_listed_again() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let other = bed.cloud.folder(ROOT_ID, "Other");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.cloud.file(&other.id, "b.txt", "two", bed.time());
    bed.settle();
    let before = bed.cloud.0.borrow().listings;
    bed.sync();
    assert_eq!(bed.cloud.0.borrow().listings - before, 1, "only the top folder");
    bed.cloud.file(&docs.id, "c.txt", "three", bed.time());
    let before = bed.cloud.0.borrow().listings;
    bed.sync();
    assert_eq!(bed.cloud.0.borrow().listings - before, 2, "the top folder and Docs");
    assert_eq!(bed.read("Docs/c.txt").as_deref(), Some("three"));
}

#[test]
fn a_package_is_left_alone_and_reported() {
    let bed = Bed::new();
    let doc = bed.cloud.file(ROOT_ID, "letter.pages", "bundle", bed.time());
    bed.cloud.0.borrow_mut().packages.push(doc.id.clone());
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let outcome = bed.sync();
    assert!(outcome.problems.is_empty());
    let again = bed.sync();
    assert!(again.done.is_empty(), "{:?}", again.done);
    assert!(
        again.skipped.iter().any(|s| s.starts_with("letter.pages")),
        "{:?}",
        again.skipped
    );
    assert_eq!(bed.local_paths(), ["a.txt"]);
    assert_eq!(bed.cloud.paths(), ["a.txt", "letter.pages"]);
}

#[test]
fn a_folder_removed_here_takes_its_contents_to_recently_deleted_as_one() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.cloud.file(ROOT_ID, "keep.txt", "k", bed.time());
    bed.settle();
    fs::remove_dir_all(bed.root().join("Docs")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["keep.txt"]);
}

#[test]
fn a_file_changed_while_a_removal_was_planned_is_kept() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.remove(&docs.id);
    bed.write("Docs/new.txt", "made here meanwhile");
    bed.settle();
    assert_eq!(bed.cloud.text("Docs/new.txt").as_deref(), Some("made here meanwhile"));
    assert_eq!(bed.local_paths(), ["Docs", "Docs/new.txt"]);
}

#[test]
fn a_same_size_edit_or_a_rename_in_icloud_is_seen_though_no_folder_etag_changes() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let deep = bed.cloud.folder(&docs.id, "deep");
    let a = bed.cloud.file(&deep.id, "a.txt", "one", bed.time());
    let b = bed.cloud.file(&deep.id, "b.txt", "bee", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "two", bed.time());
    bed.cloud.rename_item(&b.id, "c.txt");
    bed.settle();
    assert_eq!(bed.read("Docs/deep/a.txt").as_deref(), Some("two"));
    assert_eq!(bed.read("Docs/deep/c.txt").as_deref(), Some("bee"));
}

#[test]
fn when_the_feed_starts_over_everything_is_listed_again() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "two", bed.time());
    bed.cloud.0.borrow_mut().feed_expired = true;
    bed.settle();
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("two"));
}

#[test]
fn a_synced_file_that_becomes_unsyncable_is_left_alone_on_both_sides() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    // a.txt becomes a link; "B.txt" appears beside b.txt (one name to iCloud).
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    std::os::unix::fs::symlink("b.txt", bed.root().join("a.txt")).unwrap();
    bed.write("B.txt", "capital");
    let outcome = bed.sync();
    assert!(outcome.done.is_empty(), "{:?}", outcome.done);
    assert_eq!(bed.cloud.paths(), ["a.txt", "b.txt"]);
    assert!(bed.cloud.0.borrow().trashed.is_empty());
    assert_eq!(outcome.skipped.len(), 3, "{:?}", outcome.skipped);
}

#[test]
fn a_replace_whose_listing_fails_is_still_recorded_and_makes_no_conflict() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.write("a.txt", "two from here");
    bed.cloud.0.borrow_mut().listing_fails_after_replace = true;
    let outcome = bed.try_sync(false).unwrap();
    assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("two from here"));
    bed.cloud.0.borrow_mut().listing_fails_after_replace = false;
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.local_paths(), ["a.txt"], "no conflicted copy");
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "not sent again");
}

#[test]
fn a_download_that_was_not_recorded_makes_no_conflict() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    let at = bed.time();
    bed.cloud.edit(&a.id, "two from icloud", at);
    // What a sync stopped between putting the file in place and recording it leaves.
    fs::write(bed.root().join("a.txt"), "two from icloud").unwrap();
    local::set_mtime(&bed.root().join("a.txt"), at).unwrap();
    bed.settle();
    assert_eq!(bed.local_paths(), ["a.txt"], "no conflicted copy");
    assert_eq!(bed.read("a.txt").as_deref(), Some("two from icloud"));
}

#[test]
fn what_is_not_synced_is_kept_for_status_and_a_package_that_goes_is_dropped() {
    let bed = Bed::new();
    let doc = bed.cloud.file(ROOT_ID, "letter.pages", "bundle", bed.time());
    bed.cloud.0.borrow_mut().packages.push(doc.id.clone());
    bed.cloud.file(ROOT_ID, ".hidden", "h", bed.time());
    bed.write("a.txt", "one");
    std::os::unix::fs::symlink("a.txt", bed.root().join("link")).unwrap();
    bed.sync();
    assert_eq!(bed.not_synced(), [".hidden", "letter.pages", "link"]);
    bed.cloud.remove(&doc.id);
    fs::remove_file(bed.root().join("link")).unwrap();
    bed.sync();
    assert_eq!(bed.not_synced(), [".hidden"]);
}

#[test]
fn a_folder_removed_here_stays_in_icloud_while_it_holds_something_never_synced() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.cloud.file(&docs.id, ".hidden", "h", bed.time());
    bed.cloud.file(ROOT_ID, "keep.txt", "k", bed.time());
    bed.settle_paths(&["Docs", "Docs/.hidden", "Docs/a.txt", "keep.txt"]);
    fs::remove_dir_all(bed.root().join("Docs")).unwrap();
    bed.settle_paths(&["Docs", "Docs/.hidden", "keep.txt"]);
    assert_eq!(bed.local_paths(), ["Docs", "keep.txt"]);
}

#[test]
fn a_file_moved_into_a_folder_both_sides_made_does_not_stop_the_sync() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // Offline: a phone makes "Trip", and so does this computer, which also
    // moves a synced file into it.
    bed.cloud.folder(ROOT_ID, "Trip");
    fs::create_dir(bed.root().join("Trip")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Trip/a.txt")).unwrap();
    bed.write("other.txt", "unrelated");
    bed.settle();
    assert_eq!(bed.cloud.id_at("Trip/a.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.text("other.txt").as_deref(), Some("unrelated"));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}

#[test]
fn a_file_moved_into_a_folder_icloud_replaced_does_not_stop_the_sync() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // In iCloud the folder is removed and another of its name made; here a
    // synced file is moved into the folder meanwhile.
    bed.cloud.remove(&docs.id);
    let again = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&again.id, "new.txt", "new", bed.time());
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/a.txt")).unwrap();
    bed.write("other.txt", "unrelated");
    bed.settle();
    assert_eq!(bed.cloud.text("other.txt").as_deref(), Some("unrelated"));
    assert_eq!(bed.cloud.text("Docs/a.txt").as_deref(), Some("one"));
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("one"));
    assert_eq!(bed.read("Docs/new.txt").as_deref(), Some("new"));
}

#[test]
fn a_folder_replaced_by_a_file_here_leaves_nothing_for_later() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    fs::remove_dir_all(bed.root().join("Docs")).unwrap();
    bed.write("Docs", "now a file");
    // No "Not a directory" problem for the file that was in the folder.
    bed.settle();
    assert!(bed.cloud.0.borrow().trashed.contains(&a.id));
    let copy = bed
        .local_paths()
        .into_iter()
        .find(|p| p.contains("conflicted copy desk"))
        .expect("the file is kept beside iCloud's folder");
    assert_eq!(bed.cloud.text(&copy).as_deref(), Some("now a file"));
}

#[test]
fn a_move_icloud_refuses_is_left_alone_and_shown_while_the_rest_syncs() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
    bed.write("other.txt", "unrelated");
    bed.cloud.0.borrow_mut().refuses_moves = true;
    for _ in 0..2 {
        let outcome = bed.try_sync(false).unwrap();
        assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
        assert!(
            outcome.problems[0].contains("NAME_NOT_ALLOWED"),
            "{:?}",
            outcome.problems
        );
        // Everything else is synced; the file is neither removed in iCloud
        // nor sent again under its new name.
        assert_eq!(bed.cloud.text("other.txt").as_deref(), Some("unrelated"));
        assert_eq!(bed.cloud.paths(), ["a.txt", "other.txt"]);
        assert_eq!(bed.local_paths(), ["b.txt", "other.txt"]);
        assert!(bed.cloud.0.borrow().trashed.is_empty());
        // And `status` says so.
        let state = State::open(&bed.dir.path().join("state")).unwrap();
        let shown = state.not_synced().unwrap();
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(shown[0].0, "b.txt");
        assert!(
            shown[0].1.starts_with("it could not be moved or renamed in iCloud: ")
                && shown[0].1.contains("NAME_NOT_ALLOWED"),
            "{shown:?}"
        );
        assert!(
            outcome.skipped.iter().any(|s| s.starts_with("b.txt: ")),
            "{:?}",
            outcome.skipped
        );
    }
    // Once iCloud takes it, it is the same file under its new name.
    bed.cloud.0.borrow_mut().refuses_moves = false;
    bed.settle();
    assert_eq!(bed.cloud.id_at("b.txt").as_deref(), Some(a.id.as_str()));
    assert!(bed.not_synced().is_empty());
}

#[test]
fn a_move_that_fails_for_want_of_a_network_ends_the_sync() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
    bed.write("other.txt", "unrelated");
    bed.cloud.0.borrow_mut().moves_offline = true;
    let result = bed.try_sync(false);
    assert!(
        matches!(
            result,
            Err(Error::Remote(remote::Error::Session(icloud_session::Error::Offline(_))))
        ),
        "{result:?}"
    );
    assert_eq!(bed.cloud.paths(), ["a.txt"], "nothing went on without the move");
    assert!(bed.not_synced().is_empty());
    bed.cloud.0.borrow_mut().moves_offline = false;
    bed.settle();
}

#[test]
fn a_move_here_that_stopped_half_way_in_icloud_is_finished_not_undone() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    let uploads = bed.cloud.0.borrow().uploads;
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/b.txt")).unwrap();
    // The move into the folder is done; the rename is not.
    bed.cloud.0.borrow_mut().rename_fails_once = true;
    let outcome = bed.try_sync(false).unwrap();
    assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
    assert_eq!(bed.cloud.paths(), ["Docs", "Docs/a.txt"]);
    assert_eq!(bed.local_paths(), ["Docs", "Docs/b.txt"]);
    bed.settle();
    assert_eq!(bed.local_paths(), ["Docs", "Docs/b.txt"], "the rename here stands");
    assert_eq!(bed.cloud.id_at("Docs/b.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.0.borrow().nodes[&a.id].parent, docs.id);
    assert_eq!(bed.cloud.0.borrow().uploads, uploads);
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}

#[test]
fn a_file_that_goes_while_its_stale_replace_is_refused_is_one_files_problem() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "ONE", a.mtime_ms);
    bed.write("a.txt", "mine");
    bed.write("other.txt", "unrelated");
    bed.cloud.0.borrow_mut().stale_removes = Some(bed.root().join("a.txt"));
    let outcome = bed.try_sync(false).unwrap();
    assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
    assert_eq!(bed.cloud.text("other.txt").as_deref(), Some("unrelated"));
}

#[test]
fn a_file_renamed_and_edited_in_icloud_arrives_with_its_edit() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.rename_item(&a.id, "b.txt");
    bed.cloud.edit(&a.id, "two from icloud", bed.time());
    bed.settle();
    assert_eq!(bed.read("b.txt").as_deref(), Some("two from icloud"));
    assert_eq!(bed.local_paths(), ["b.txt"]);
}

#[test]
fn a_file_renamed_here_and_edited_in_icloud_gets_the_edit() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
    bed.cloud.edit(&a.id, "two from icloud", bed.time());
    bed.settle();
    assert_eq!(bed.read("b.txt").as_deref(), Some("two from icloud"));
    assert_eq!(bed.cloud.id_at("b.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.local_paths(), ["b.txt"]);
}

#[test]
fn a_file_moved_into_a_folder_whose_rename_icloud_refuses_is_not_removed_in_icloud() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "zDocs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("zDocs"), bed.root().join("Papers")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Papers/a.txt")).unwrap();
    bed.cloud.0.borrow_mut().refuses_moves = true;
    for _ in 0..2 {
        bed.try_sync(false).unwrap();
        assert!(
            bed.cloud.0.borrow().trashed.is_empty(),
            "{:?}",
            bed.cloud.0.borrow().trashed
        );
        assert_eq!(bed.cloud.paths(), ["a.txt", "zDocs", "zDocs/in.txt"]);
    }
    bed.cloud.0.borrow_mut().refuses_moves = false;
    bed.settle();
    assert_eq!(bed.cloud.id_at("Papers/a.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn a_half_done_move_of_a_file_moved_again_here_goes_where_it_is_now() {
    let bed = Bed::new();
    bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.folder(ROOT_ID, "Other");
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/b.txt")).unwrap();
    bed.cloud.0.borrow_mut().rename_fails_once = true;
    bed.try_sync(false).unwrap();
    assert_eq!(bed.cloud.paths(), ["Docs", "Docs/a.txt", "Other"]);
    fs::rename(bed.root().join("Docs/b.txt"), bed.root().join("Other/c.txt")).unwrap();
    bed.settle();
    assert_eq!(bed.local_paths(), ["Docs", "Other", "Other/c.txt"]);
    assert_eq!(bed.cloud.id_at("Other/c.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn a_file_renamed_in_icloud_over_one_removed_there_is_not_sent_back_under_its_old_name() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let b = bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    bed.cloud.remove(&b.id);
    bed.cloud.rename_item(&a.id, "b.txt");
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["b.txt"]);
    assert_eq!(bed.read("b.txt").as_deref(), Some("one"));
}

#[test]
fn a_folder_renamed_in_icloud_over_one_removed_there_leaves_no_folder_behind() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "old.txt", "old", bed.time());
    let papers = bed.cloud.folder(ROOT_ID, "Papers");
    bed.cloud.file(&papers.id, "x.txt", "x", bed.time());
    bed.settle();
    bed.cloud.remove(&docs.id);
    bed.cloud.rename_item(&papers.id, "Docs");
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Docs", "Docs/x.txt"]);
}

#[test]
fn a_file_removed_here_while_its_stale_replace_is_refused_comes_back_as_icloud_has_it() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // iCloud's edit shows in nothing but the etag; the file here is edited,
    // then goes just as its replace is refused, so it cannot be set aside.
    bed.cloud.edit(&a.id, "ONE", a.mtime_ms);
    bed.write("a.txt", "mine");
    bed.cloud.0.borrow_mut().stale_removes = Some(bed.root().join("a.txt"));
    let outcome = bed.try_sync(false).unwrap();
    assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
    // The row is gone with the refusal, so the missing file is not read as
    // a removal of the version in iCloud that this computer never saw.
    bed.settle();
    assert!(
        bed.cloud.0.borrow().trashed.is_empty(),
        "{:?}",
        bed.cloud.0.borrow().trashed
    );
    assert_eq!(bed.read("a.txt").as_deref(), Some("ONE"));
}

#[test]
fn a_half_done_move_of_a_file_moved_back_here_is_moved_back_in_icloud() {
    let bed = Bed::new();
    bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/b.txt")).unwrap();
    bed.cloud.0.borrow_mut().rename_fails_once = true;
    bed.try_sync(false).unwrap();
    fs::rename(bed.root().join("Docs/b.txt"), bed.root().join("a.txt")).unwrap();
    bed.settle();
    assert_eq!(bed.local_paths(), ["Docs", "a.txt"]);
    assert_eq!(bed.cloud.id_at("a.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn a_move_icloud_refused_leaves_no_note_to_undo_a_later_move_made_in_icloud() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // A move here that iCloud refuses outright, then given up on here.
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/b.txt")).unwrap();
    bed.cloud.0.borrow_mut().refuses_moves = true;
    bed.try_sync(false).unwrap();
    bed.cloud.0.borrow_mut().refuses_moves = false;
    fs::rename(bed.root().join("Docs/b.txt"), bed.root().join("a.txt")).unwrap();
    // Before any sync sees that, a phone moves the file into the folder.
    let node = bed.cloud.0.borrow().nodes[&a.id].clone();
    bed.cloud.move_to(&node, &docs.id).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Docs", "Docs/a.txt"], "the phone's move stands");
    assert_eq!(bed.local_paths(), ["Docs", "Docs/a.txt"]);
}

#[test]
fn a_file_moved_into_a_folder_whose_rename_icloud_refuses_and_edited_stays_in_icloud_until_it_can_be_sent() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "zDocs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("zDocs"), bed.root().join("Papers")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Papers/a.txt")).unwrap();
    bed.write("Papers/a.txt", "edited after the move");
    bed.cloud.0.borrow_mut().refuses_moves = true;
    for _ in 0..2 {
        bed.try_sync(false).unwrap();
        assert!(
            bed.cloud.0.borrow().trashed.is_empty(),
            "{:?}",
            bed.cloud.0.borrow().trashed
        );
        assert_eq!(bed.cloud.paths(), ["a.txt", "zDocs", "zDocs/in.txt"]);
    }
    // Once the folder's move goes through, the usual rule: the old file is
    // removed in iCloud and the edited one sent as new.
    bed.cloud.0.borrow_mut().refuses_moves = false;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Papers", "Papers/a.txt", "Papers/in.txt"]);
    assert_eq!(bed.cloud.text("Papers/a.txt").as_deref(), Some("edited after the move"));
}

#[test]
fn a_replace_that_was_not_recorded_makes_no_conflict() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.write("a.txt", "two from here");
    // What a sync stopped between iCloud taking the new content and the
    // state recording it leaves: iCloud dates it at the replace.
    let at = bed.time();
    bed.cloud.edit(&a.id, "two from here", at);
    bed.settle();
    assert_eq!(bed.local_paths(), ["a.txt"], "no conflicted copy");
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("two from here"));
}

#[test]
fn a_file_put_here_that_icloud_has_with_another_date_is_one_file() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "the same", bed.time());
    bed.cloud.file(ROOT_ID, "b.txt", "theirs!!", bed.time());
    bed.write("a.txt", "the same");
    bed.write("b.txt", "mine!!!!");
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("the same"));
    let copies: Vec<String> = bed
        .local_paths()
        .into_iter()
        .filter(|p| p.contains("conflicted copy"))
        .collect();
    assert_eq!(copies.len(), 1, "only the file that differs is kept twice: {copies:?}");
    assert_eq!(bed.read(&copies[0]).as_deref(), Some("mine!!!!"));
    assert_eq!(bed.read("b.txt").as_deref(), Some("theirs!!"));
}
