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

#[test]
fn a_file_moved_in_icloud_into_a_folder_renamed_here_lands_in_it_once() {
    for renamed in ["Papers", "docs"] {
        let bed = Bed::new();
        let docs = bed.cloud.folder(ROOT_ID, "Docs");
        let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
        bed.settle();
        fs::rename(bed.root().join("Docs"), bed.root().join(renamed)).unwrap();
        let node = bed.cloud.0.borrow().nodes[&a.id].clone();
        bed.cloud.move_to(&node, &docs.id).unwrap();
        bed.settle();
        let inside = format!("{renamed}/a.txt");
        assert_eq!(bed.cloud.paths(), [renamed, inside.as_str()], "no second folder");
        assert_eq!(bed.read(&inside).as_deref(), Some("one"));
    }
}

#[test]
fn a_file_renamed_here_with_a_new_one_in_its_place_keeps_its_identity() {
    for moved_there in [false, true] {
        let bed = Bed::new();
        let docs = bed.cloud.folder(ROOT_ID, "Docs");
        let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
        bed.settle();
        fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
        bed.write("a.txt", "a new file");
        if moved_there {
            // iCloud's move wins over the rename here, as when nothing new
            // took the old name.
            let node = bed.cloud.0.borrow().nodes[&a.id].clone();
            bed.cloud.move_to(&node, &docs.id).unwrap();
        }
        bed.settle();
        let (moved, at) = if moved_there {
            (["Docs", "Docs/a.txt", "a.txt"], "Docs/a.txt")
        } else {
            (["Docs", "a.txt", "b.txt"], "b.txt")
        };
        assert_eq!(bed.cloud.paths(), moved);
        assert_eq!(bed.cloud.id_at(at).as_deref(), Some(a.id.as_str()), "{at}");
        assert_eq!(bed.cloud.text(at).as_deref(), Some("one"));
        assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("a new file"));
    }
}

#[test]
fn a_folder_made_on_both_sides_under_names_of_another_case_is_one_folder() {
    let bed = Bed::new();
    bed.write("keep.txt", "k");
    bed.settle();
    let q = bed.cloud.folder(ROOT_ID, "photos");
    bed.cloud.file(&q.id, "there.txt", "from icloud", bed.time());
    bed.write("Photos/here.txt", "from here");
    bed.settle();
    assert_eq!(
        bed.cloud.paths(),
        ["keep.txt", "photos", "photos/here.txt", "photos/there.txt"]
    );
    assert_eq!(bed.cloud.id_at("photos").as_deref(), Some(q.id.as_str()));
}

#[test]
fn a_file_renamed_here_by_case_and_edited_on_both_sides_keeps_both_under_two_names() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "c.md", "one", bed.time());
    bed.settle();
    bed.write("c.md", "mine");
    bed.cloud.edit(&a.id, "theirs", bed.time());
    fs::rename(bed.root().join("c.md"), bed.root().join("C.MD")).unwrap();
    bed.settle();
    // Not "c.md" and "C.MD", which iCloud takes as one name.
    let paths = bed.cloud.paths();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert_eq!(bed.cloud.text("c.md").as_deref(), Some("theirs"));
    let copy = paths
        .iter()
        .find(|p| p.contains("conflicted copy"))
        .expect("a conflicted copy");
    assert_eq!(bed.cloud.text(copy).as_deref(), Some("mine"));
}

fn move_there(bed: &Bed, id: &str, parent: &str) {
    let node = bed.cloud.0.borrow().nodes[id].clone();
    bed.cloud.move_to(&node, parent).unwrap();
}

#[test]
fn a_file_moved_here_into_a_folder_icloud_renamed_meanwhile_follows_it() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Docs/a.txt")).unwrap();
    bed.cloud.rename_item(&docs.id, "Papers");
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Papers", "Papers/a.txt"]);
    assert_eq!(bed.cloud.id_at("Papers/a.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn a_file_moved_here_into_a_new_folder_where_one_moved_away_reports_no_failure() {
    let bed = Bed::new();
    bed.cloud.folder(ROOT_ID, "Q");
    let c = bed.cloud.file(ROOT_ID, "c.md", "one", bed.time());
    bed.settle();
    fs::create_dir(bed.root().join("r")).unwrap();
    fs::rename(bed.root().join("Q"), bed.root().join("r/Q")).unwrap();
    fs::create_dir(bed.root().join("Q")).unwrap();
    fs::rename(bed.root().join("c.md"), bed.root().join("Q/c.md")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Q", "Q/c.md", "r", "r/Q"]);
    assert_eq!(bed.cloud.id_at("Q/c.md").as_deref(), Some(c.id.as_str()));
}

#[test]
fn a_file_renamed_in_icloud_onto_a_name_of_another_case_here_keeps_both() {
    let bed = Bed::new();
    let d = bed.cloud.file(ROOT_ID, "d", "one", bed.time());
    bed.settle();
    bed.write("A.TXT", "made here");
    bed.cloud.rename_item(&d.id, "a.txt");
    bed.settle();
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("one"));
    assert_eq!(bed.cloud.id_at("a.txt").as_deref(), Some(d.id.as_str()));
    let paths = bed.cloud.paths();
    assert_eq!(paths.len(), 2, "{paths:?}");
    let copy = paths
        .iter()
        .find(|p| p.contains("conflicted copy"))
        .expect("a conflicted copy");
    assert_eq!(bed.cloud.text(copy).as_deref(), Some("made here"));
}

#[test]
fn a_file_renamed_here_onto_a_name_of_another_case_in_icloud_is_not_sent_beside_it() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "d", "one", bed.time());
    bed.settle();
    bed.cloud.file(ROOT_ID, "a.txt", "made there", bed.time());
    fs::rename(bed.root().join("d"), bed.root().join("A.TXT")).unwrap();
    bed.settle();
    let paths = bed.cloud.paths();
    assert!(!paths.iter().any(|p| p == "A.TXT"), "{paths:?}");
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("made there"));
    assert_eq!(bed.local_paths(), paths);
    assert!(
        paths.iter().any(|p| bed.cloud.text(p).as_deref() == Some("one")),
        "{paths:?}"
    );
}

#[test]
fn two_folders_each_moved_into_the_other_follow_icloud() {
    let bed = Bed::new();
    let q = bed.cloud.folder(ROOT_ID, "Q");
    bed.cloud.file(&q.id, "q.txt", "in Q", bed.time());
    let r = bed.cloud.folder(ROOT_ID, "r");
    bed.cloud.file(&r.id, "r.txt", "in r", bed.time());
    bed.settle();
    // Here Q goes into r; in iCloud r goes into Q.
    fs::rename(bed.root().join("Q"), bed.root().join("r/Q")).unwrap();
    move_there(&bed, &r.id, &q.id);
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Q", "Q/q.txt", "Q/r", "Q/r/r.txt"]);
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}

#[test]
fn a_folder_moved_in_icloud_into_one_that_took_its_name_follows_it() {
    let bed = Bed::new();
    let r = bed.cloud.folder(ROOT_ID, "r");
    bed.cloud.file(&r.id, "in.txt", "inside", bed.time());
    let s = bed.cloud.folder(ROOT_ID, "s");
    bed.settle();
    // In iCloud r goes into s, and s takes the name r: r/r here.
    move_there(&bed, &r.id, &s.id);
    bed.cloud.rename_item(&s.id, "r");
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["r", "r/r", "r/r/in.txt"]);
    assert_eq!(bed.cloud.id_at("r/r").as_deref(), Some(r.id.as_str()));
}

#[test]
fn a_file_moved_here_over_a_removed_one_follows_a_move_in_icloud() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "a", bed.time());
    let d = bed.cloud.file(ROOT_ID, "d", "d", bed.time());
    let q = bed.cloud.folder(ROOT_ID, "Q");
    bed.settle();
    // Here d replaces a.txt; in iCloud d goes into Q.
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    fs::rename(bed.root().join("d"), bed.root().join("a.txt")).unwrap();
    move_there(&bed, &d.id, &q.id);
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Q", "Q/d"], "d once, a.txt removed");
    assert_eq!(bed.cloud.id_at("Q/d").as_deref(), Some(d.id.as_str()));
}

#[test]
fn a_file_moved_out_of_a_folder_whose_rename_icloud_refuses_is_not_sent_twice() {
    let bed = Bed::new();
    let r = bed.cloud.folder(ROOT_ID, "r");
    let a = bed.cloud.file(&r.id, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("r"), bed.root().join("P")).unwrap();
    fs::rename(bed.root().join("P/a.txt"), bed.root().join("a.txt")).unwrap();
    bed.cloud.0.borrow_mut().refuses_moves = true;
    bed.try_sync(false).unwrap();
    assert_eq!(bed.cloud.paths(), ["r", "r/a.txt"], "a.txt not sent as new");
    bed.cloud.0.borrow_mut().refuses_moves = false;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["P", "a.txt"]);
    assert_eq!(bed.cloud.id_at("a.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn a_folder_icloud_removed_and_made_again_comes_back_in_the_same_sync() {
    let bed = Bed::new();
    let q = bed.cloud.folder(ROOT_ID, "Q");
    let r = bed.cloud.folder(&q.id, "r");
    bed.settle();
    bed.cloud.remove(&r.id);
    bed.cloud.folder(&q.id, "r");
    bed.sync();
    assert_eq!(bed.local_paths(), ["Q", "Q/r"]);
}

#[test]
fn a_file_moved_here_into_a_new_folder_icloud_has_in_another_case_goes_into_that_one() {
    let bed = Bed::new();
    let c = bed.cloud.file(ROOT_ID, "c.md", "one", bed.time());
    bed.settle();
    let made = bed.cloud.folder(ROOT_ID, "s s");
    fs::create_dir(bed.root().join("S S")).unwrap();
    fs::rename(bed.root().join("c.md"), bed.root().join("S S/c.md")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["s s", "s s/c.md"]);
    assert_eq!(bed.cloud.id_at("s s").as_deref(), Some(made.id.as_str()));
    assert_eq!(bed.cloud.id_at("s s/c.md").as_deref(), Some(c.id.as_str()));
}

#[test]
fn a_folder_icloud_replaced_under_another_case_while_edited_here_is_one_folder() {
    let bed = Bed::new();
    let s = bed.cloud.folder(ROOT_ID, "s s");
    bed.cloud.file(&s.id, "c.md", "one", bed.time());
    bed.settle();
    bed.cloud.remove(&s.id);
    let again = bed.cloud.folder(ROOT_ID, "S S");
    bed.write("s s/c.md", "edited here");
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["S S", "S S/c.md"]);
    assert_eq!(bed.cloud.id_at("S S").as_deref(), Some(again.id.as_str()));
    assert_eq!(bed.cloud.text("S S/c.md").as_deref(), Some("edited here"));
}

#[test]
fn a_conflicted_copy_named_like_one_in_icloud_in_another_case_is_not_put_beside_it() {
    let bed = Bed::new();
    let c = bed.cloud.file(ROOT_ID, "c.md", "one", bed.time());
    bed.settle();
    // Both sides edit c.md; iCloud already has an earlier conflicted copy
    // under the name this one would take, in another case.
    bed.cloud
        .file(ROOT_ID, "C (conflicted copy desk DATE).MD", "older copy", bed.time());
    bed.cloud.edit(&c.id, "theirs", bed.time());
    bed.write("c.md", "mine");
    let date = &icloud_session::time::rfc3339(icloud_session::time::now_ms() / 1000)[..10];
    let older = bed.cloud.id_at("C (conflicted copy desk DATE).MD").unwrap();
    bed.cloud
        .rename_item(&older, &format!("C (conflicted copy desk {date}).MD"));
    bed.settle();
    let paths = bed.cloud.paths();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert_eq!(bed.local_paths(), paths);
}

#[test]
fn a_file_renamed_in_icloud_onto_a_removed_ones_name_and_moved_here_is_not_sent_twice() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "b.txt", "b", bed.time());
    let c = bed.cloud.file(ROOT_ID, "c.md", "c", bed.time());
    bed.settle();
    // In iCloud b.txt goes and c.md takes its name; here c.md goes into a new folder.
    let b = bed.cloud.id_at("b.txt").unwrap();
    bed.cloud.remove(&b);
    bed.cloud.rename_item(&c.id, "b.txt");
    fs::create_dir(bed.root().join("P")).unwrap();
    fs::rename(bed.root().join("c.md"), bed.root().join("P/c.md")).unwrap();
    bed.settle();
    let texts: Vec<_> = bed.cloud.tree().into_values().flatten().collect();
    assert_eq!(texts, ["c"], "{:?}", bed.cloud.paths());
}

#[test]
fn a_file_moved_here_into_a_folder_renamed_on_both_sides_follows_icloud_with_no_failure() {
    let bed = Bed::new();
    let r = bed.cloud.folder(ROOT_ID, "r");
    let s = bed.cloud.file(ROOT_ID, "s.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("r"), bed.root().join("Q")).unwrap();
    fs::rename(bed.root().join("s.txt"), bed.root().join("Q/s.txt")).unwrap();
    bed.cloud.rename_item(&r.id, "P");
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["P", "P/s.txt"]);
    assert_eq!(bed.cloud.id_at("P/s.txt").as_deref(), Some(s.id.as_str()));
}

#[test]
fn a_file_renamed_in_icloud_to_the_name_of_one_moved_away_here_keeps_both_in_step() {
    let bed = Bed::new();
    let c = bed.cloud.file(ROOT_ID, "c.md", "c", bed.time());
    let d = bed.cloud.file(ROOT_ID, "d", "d", bed.time());
    let q = bed.cloud.folder(ROOT_ID, "Q");
    bed.settle();
    // Here d goes into a new folder; in iCloud into Q, and c.md takes its name.
    fs::create_dir(bed.root().join("r")).unwrap();
    fs::rename(bed.root().join("d"), bed.root().join("r/d")).unwrap();
    move_there(&bed, &d.id, &q.id);
    bed.cloud.rename_item(&c.id, "d");
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Q", "Q/d", "d", "r"]);
    assert_eq!(bed.cloud.id_at("Q/d").as_deref(), Some(d.id.as_str()));
    assert_eq!(bed.cloud.id_at("d").as_deref(), Some(c.id.as_str()));
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
}

#[test]
fn a_folder_renamed_here_to_the_name_of_a_folder_new_in_icloud_keeps_both() {
    let bed = Bed::new();
    let p = bed.cloud.folder(ROOT_ID, "P");
    let x = bed.cloud.file(&p.id, "x.txt", "x", bed.time());
    bed.settle();
    fs::rename(bed.root().join("P"), bed.root().join("r")).unwrap();
    let r = bed.cloud.folder(ROOT_ID, "r");
    bed.cloud.file(&r.id, "y.txt", "y", bed.time());
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert!(bed.cloud.0.borrow().trashed.is_empty(), "{:?}", bed.cloud.paths());
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
    assert!(bed.cloud.id_at("r/y.txt").is_some());
    let moved = bed.cloud.0.borrow().path_of(&x.id);
    assert!(moved.ends_with("/x.txt") && moved != "r/x.txt", "{moved}");
}

#[test]
fn a_file_renamed_here_to_the_name_of_a_file_new_in_icloud_keeps_both() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "a", bed.time());
    bed.settle();
    fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
    bed.cloud.file(ROOT_ID, "b.txt", "theirs", bed.time());
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert!(bed.cloud.0.borrow().trashed.is_empty());
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
    assert_eq!(bed.cloud.text("b.txt").as_deref(), Some("theirs"));
    assert!(bed.cloud.0.borrow().nodes[&a.id].name.contains("conflicted copy"));
}

#[test]
fn a_folder_moved_here_into_a_new_folder_of_its_old_name_moves_in_icloud_too() {
    let bed = Bed::new();
    let p = bed.cloud.folder(ROOT_ID, "P");
    let x = bed.cloud.file(&p.id, "x.txt", "x", bed.time());
    bed.settle();
    fs::create_dir(bed.root().join("Q")).unwrap();
    fs::rename(bed.root().join("P"), bed.root().join("Q/P")).unwrap();
    fs::create_dir(bed.root().join("P")).unwrap();
    fs::rename(bed.root().join("Q"), bed.root().join("P/Q")).unwrap();
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["P", "P/Q", "P/Q/P", "P/Q/P/x.txt"]);
    assert_eq!(bed.cloud.id_at("P/Q/P").as_deref(), Some(p.id.as_str()));
    assert_eq!(bed.cloud.id_at("P/Q/P/x.txt").as_deref(), Some(x.id.as_str()));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
}

#[test]
fn a_folder_moved_here_into_a_new_folder_of_its_old_name_waits_while_icloud_refuses_and_the_rest_syncs() {
    let bed = Bed::new();
    bed.cloud.folder(ROOT_ID, "P");
    bed.settle();
    fs::create_dir(bed.root().join("Q")).unwrap();
    fs::rename(bed.root().join("P"), bed.root().join("Q/P")).unwrap();
    fs::create_dir(bed.root().join("P")).unwrap();
    fs::rename(bed.root().join("Q"), bed.root().join("P/Q")).unwrap();
    bed.write("other.txt", "unrelated");
    bed.cloud.0.borrow_mut().refuses_moves = true;
    let outcome = bed.try_sync(false).unwrap();
    assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
    assert_eq!(bed.cloud.text("other.txt").as_deref(), Some("unrelated"));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
    bed.cloud.0.borrow_mut().refuses_moves = false;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["P", "P/Q", "P/Q/P", "other.txt"]);
}

#[test]
fn a_file_moved_here_over_one_removed_takes_its_name_in_icloud() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "a", bed.time());
    let b = bed.cloud.file(ROOT_ID, "b.txt", "b", bed.time());
    bed.settle();
    fs::remove_file(bed.root().join("b.txt")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("b.txt")).unwrap();
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.cloud.id_at("b.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.text("b.txt").as_deref(), Some("a"));
    assert_eq!(
        bed.cloud.0.borrow().trashed,
        std::slice::from_ref(&b.id),
        "only the one removed"
    );
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
}

#[test]
fn a_folder_renamed_here_by_case_while_a_file_in_it_is_renamed_in_icloud_stays_one_folder() {
    let bed = Bed::new();
    let q = bed.cloud.folder(ROOT_ID, "Q");
    let d = bed.cloud.file(&q.id, "d", "d", bed.time());
    bed.cloud.file(&q.id, "b.txt", "b", bed.time());
    bed.settle();
    bed.cloud.rename_item(&d.id, "c.md");
    fs::rename(bed.root().join("Q"), bed.root().join("q")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["q", "q/b.txt", "q/c.md"]);
    assert_eq!(bed.cloud.id_at("q").as_deref(), Some(q.id.as_str()));
}

#[test]
fn a_folder_made_again_here_and_then_renamed_is_one_folder_renamed() {
    let bed = Bed::new();
    let q = bed.cloud.folder(ROOT_ID, "Q");
    bed.cloud.file(&q.id, "b.txt", "b", bed.time());
    bed.settle();
    // Removed and made again here (the file brought back into it), then renamed.
    fs::rename(bed.root().join("Q/b.txt"), bed.root().join("b.txt")).unwrap();
    fs::remove_dir(bed.root().join("Q")).unwrap();
    fs::create_dir(bed.root().join("Q")).unwrap();
    fs::rename(bed.root().join("b.txt"), bed.root().join("Q/b.txt")).unwrap();
    bed.settle();
    fs::rename(bed.root().join("Q"), bed.root().join("q")).unwrap();
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["q", "q/b.txt"]);
    assert_eq!(bed.cloud.id_at("q").as_deref(), Some(q.id.as_str()));
    assert_eq!(bed.cloud.0.borrow().uploads, uploads);
}

#[test]
fn a_new_file_where_a_half_done_move_left_another_waits_for_the_move() {
    let bed = Bed::new();
    bed.cloud.folder(ROOT_ID, "P");
    let b = bed.cloud.file(ROOT_ID, "b.txt", "b", bed.time());
    bed.settle();
    // b.txt moved here to P/a.txt; iCloud moves it, then refuses the name.
    fs::rename(bed.root().join("b.txt"), bed.root().join("P/a.txt")).unwrap();
    bed.write("P/b.txt", "a new b");
    bed.cloud.0.borrow_mut().refused_names = vec!["a.txt".into()];
    bed.try_sync(false).unwrap();
    let names: Vec<String> = bed.cloud.paths();
    assert_eq!(names, ["P", "P/b.txt"], "one P/b.txt in iCloud");
    bed.cloud.0.borrow_mut().refused_names.clear();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["P", "P/a.txt", "P/b.txt"]);
    assert_eq!(bed.cloud.id_at("P/a.txt").as_deref(), Some(b.id.as_str()));
    assert_eq!(bed.cloud.text("P/b.txt").as_deref(), Some("a new b"));
}

#[test]
fn a_folder_whose_move_icloud_refused_and_then_moved_into_a_folder_of_its_name_moves_once() {
    let bed = Bed::new();
    let r = bed.cloud.folder(ROOT_ID, "r");
    bed.cloud.file(&r.id, "a.txt", "a", bed.time());
    let s = bed.cloud.folder(ROOT_ID, "s s");
    bed.settle();
    fs::rename(bed.root().join("r"), bed.root().join("s s/r")).unwrap();
    bed.cloud.0.borrow_mut().refuses_moves = true;
    bed.try_sync(false).unwrap();
    bed.cloud.0.borrow_mut().refuses_moves = false;
    fs::rename(bed.root().join("s s"), bed.root().join("r")).unwrap();
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["r", "r/r", "r/r/a.txt"]);
    assert_eq!(bed.cloud.id_at("r").as_deref(), Some(s.id.as_str()));
    assert_eq!(bed.cloud.id_at("r/r").as_deref(), Some(r.id.as_str()));
    assert_eq!(bed.cloud.0.borrow().uploads, uploads);
}

#[test]
fn a_folder_moved_here_over_one_removed_takes_its_name_in_icloud() {
    let bed = Bed::new();
    let r = bed.cloud.folder(ROOT_ID, "r");
    let x = bed.cloud.file(&r.id, "x.txt", "x", bed.time());
    let s = bed.cloud.folder(ROOT_ID, "s s");
    bed.cloud.file(&s.id, "y.txt", "y", bed.time());
    bed.settle();
    fs::remove_dir_all(bed.root().join("r")).unwrap();
    fs::rename(bed.root().join("s s"), bed.root().join("r")).unwrap();
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["r", "r/y.txt"]);
    assert_eq!(bed.cloud.id_at("r").as_deref(), Some(s.id.as_str()));
    assert_eq!(bed.cloud.0.borrow().trashed, [r.id.clone(), x.id.clone()]);
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
}

#[test]
fn a_file_moved_here_over_one_removed_under_another_case_edited_in_icloud_keeps_both() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "a", bed.time());
    let q = bed.cloud.folder(ROOT_ID, "Q");
    let b = bed.cloud.file(&q.id, "b.txt", "b", bed.time());
    bed.settle();
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    fs::rename(bed.root().join("Q/b.txt"), bed.root().join("A.TXT")).unwrap();
    bed.cloud.edit(&a.id, "a edited there", bed.time());
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert!(bed.cloud.0.borrow().trashed.is_empty());
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "nothing sent again");
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("a edited there"));
    assert!(bed.cloud.0.borrow().nodes[&b.id].name.contains("conflicted copy"));
}

#[test]
fn a_folder_icloud_refused_to_make_is_not_taken_for_one_another_device_makes_later() {
    let bed = Bed::new();
    bed.settle();
    fs::create_dir(bed.root().join("P")).unwrap();
    bed.write("P/mine.txt", "mine");
    bed.cloud.0.borrow_mut().refused_names = vec!["P".into()];
    bed.try_sync(false).unwrap();
    bed.cloud.0.borrow_mut().refused_names.clear();
    // Another device makes its own P; here P is renamed.
    let theirs = bed.cloud.folder(ROOT_ID, "P");
    bed.cloud.file(&theirs.id, "theirs.txt", "theirs", bed.time());
    fs::rename(bed.root().join("P"), bed.root().join("Mine")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["Mine", "Mine/mine.txt", "P", "P/theirs.txt"]);
    assert_eq!(bed.cloud.id_at("P").as_deref(), Some(theirs.id.as_str()));
}
