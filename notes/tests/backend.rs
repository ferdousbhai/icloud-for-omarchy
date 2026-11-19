//! What only a window reaches of the backend: a save under a running sync
//! (or one waiting for another process's lock), unsaved edits meeting a note
//! that changed on disk or went away, and the warning before a save. (The command line's own use of the backend is
//! held by tests/blackbox/.) The engine here is a script with fixed answers.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use icloud_notes::backend::{Backend, Event, Role};
use icloud_notes::sync::{Kind, Started};
use icloud_notes::vault::Vault;

const ID: &str = "---\napple-note-id: id-q\n---\n";

struct World {
    dir: tempfile::TempDir,
    backend: Backend,
}

impl World {
    /// A cloned vault holding `Q.md` (tracked, with `body`), open in a
    /// backend whose engine tracks it and answers a pull with nothing new.
    fn new(body: &str) -> World {
        World::titled("in-body", body)
    }

    /// [`World::new`] in a vault whose titles are `title_mode`'s.
    fn titled(title_mode: &str, body: &str) -> World {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir_all(vault.join(".icloud-notes")).unwrap();
        fs::write(vault.join(".icloud-notes/state.json"), "{}").unwrap();
        fs::write(vault.join("Q.md"), format!("{ID}{body}")).unwrap();
        let engine = dir.path().join("engine");
        let info = format!(
            r#"{{"titleMode":"{title_mode}","defaultFolderDir":"","vault":"{}","notes":[{{"id":"id-q","file":"Q.md"}},{{"id":"id-r","file":"R.md","readOnlyReason":"is locked"}}]}}"#,
            vault.display()
        );
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n*vault-info*) echo '{info}' ;;\n*) echo '{{\"ok\":true,\"data\":{{\"added\":0}},\"summary\":\"pulled\"}}' ;;\nesac\n"
        );
        fs::write(&engine, script).unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
        let mut backend = Backend::with_engine(Vault::at(vault.to_string_lossy()), Role::App, Some(engine));
        backend.open_note("Q.md");
        assert_eq!(backend.current_note(), "Q.md");
        backend.take_events();
        World { dir, backend }
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join("vault").join(file)
    }

    fn read(&self, file: &str) -> String {
        fs::read_to_string(self.path(file)).unwrap_or_default()
    }

    fn write(&self, file: &str, text: &str) {
        fs::write(self.path(file), text).unwrap();
    }

    /// A pull during which `meanwhile` happens: saves made in it wait.
    fn pull_with(&mut self, meanwhile: impl FnOnce(&mut World)) {
        let Started::Running(running) = self.backend.start(Kind::Pull) else {
            panic!("the engine starts");
        };
        meanwhile(self);
        let runs = self.backend.finish(running.wait());
        assert!(runs.len() == 1 && runs[0].ok && runs[0].command == "pull", "{runs:?}");
    }
}

fn exists(path: &Path) -> bool {
    path.exists()
}

#[test]
fn a_save_writes_the_body_under_the_id_block_and_keeps_the_notes_own_characters() {
    let mut w = World::new("# Q\nno\u{a0}break kept\u{2028}soft break\nlast\n");
    // An editor hands back a space and a newline for them.
    assert!(
        w.backend
            .save_note("# Q\nno break kept\nsoft break\nlast, edited\n", None)
    );
    assert_eq!(
        w.read("Q.md"),
        format!("{ID}# Q\nno\u{a0}break kept\u{2028}soft break\nlast, edited\n")
    );
    assert_eq!(w.backend.take_events(), [Event::VaultChanged]);
    // The same text again writes nothing.
    assert!(
        w.backend
            .save_note("# Q\nno break kept\nsoft break\nlast, edited\n", None)
    );
    assert_eq!(w.backend.take_events(), []);
}

#[test]
fn a_save_is_refused_once_the_file_changed_and_the_edits_are_merged_instead() {
    let mut w = World::new("# D\none\nbetween\ntwo\n");
    w.write("Q.md", &format!("{ID}# D\none\nbetween\nfrom iCloud\n"));
    assert!(!w.backend.save_note("# D\none, mine\nbetween\ntwo\n", None));
    assert_eq!(w.backend.take_events(), [Event::NoteChangedOnDisk]);
    assert_eq!(
        w.read("Q.md"),
        format!("{ID}# D\none\nbetween\nfrom iCloud\n"),
        "nothing was written over it"
    );
    // Edits to other lines merge on their own.
    assert!(
        w.backend
            .keep_edits_as_conflict("# D\none\nbetween\ntwo\n", "# D\none, mine\nbetween\ntwo\n")
    );
    assert_eq!(w.read("Q.md"), format!("{ID}# D\none, mine\nbetween\nfrom iCloud\n"));
    assert_eq!(w.backend.take_events(), [Event::VaultChanged]);
    // The same line changed on both sides becomes a block to pick from.
    w.write("Q.md", &format!("{ID}# D\none, theirs\nbetween\nfrom iCloud\n"));
    assert!(w.backend.keep_edits_as_conflict(
        "# D\none, mine\nbetween\nfrom iCloud\n",
        "# D\none, mine too\nbetween\nfrom iCloud\n"
    ));
    assert_eq!(
        w.read("Q.md"),
        format!(
            "{ID}# D\n<<<<<<< local\none, mine too\n||||||| base\none, mine\n=======\none, theirs\n>>>>>>> remote\n\
             between\nfrom iCloud\n"
        )
    );
    assert_eq!(w.backend.note_conflicts().len(), 1);
    assert_eq!(w.backend.take_events(), [], "a conflict is not worth a sync");
    // Nothing to keep when the file is what the editor started from.
    let loaded = w.backend.note_body().to_string();
    assert!(!w.backend.keep_edits_as_conflict(&loaded, "anything"));
}

#[test]
fn a_save_under_a_sync_waits_and_is_written_once_it_is_done() {
    let mut w = World::new("# Q\none\n\ntwo\n");
    // The sync leaves the note alone: saved as asked.
    w.pull_with(|w| {
        assert!(!w.backend.save_note("# Q\none, mine\n\ntwo\n", None), "it waits");
        assert_eq!(w.read("Q.md"), format!("{ID}# Q\none\n\ntwo\n"));
    });
    assert_eq!(w.read("Q.md"), format!("{ID}# Q\none, mine\n\ntwo\n"));
    assert!(
        w.backend
            .take_events()
            .contains(&Event::QueuedSaveWritten("# Q\none, mine\n\ntwo\n".into()))
    );
    // The sync changed the note: the edits are merged into what it wrote.
    w.pull_with(|w| {
        assert!(!w.backend.save_note("# Q\none, mine again\n\ntwo\n", None));
        w.write("Q.md", &format!("{ID}# Q\none, mine\n\ntwo from iCloud\n"));
    });
    assert_eq!(w.read("Q.md"), format!("{ID}# Q\none, mine again\n\ntwo from iCloud\n"));
    // An edit undone before the sync ends, or dropped by the window, leaves nothing to write.
    let before = w.read("Q.md");
    w.pull_with(|w| {
        w.backend.save_note("# Q\nan edit\n", None);
        let loaded = w.backend.note_body().to_string();
        w.backend.save_note(&loaded, None);
    });
    w.pull_with(|w| {
        w.backend.save_note("# Q\nan edit\n", None);
        w.backend.drop_queued_save();
    });
    assert_eq!(w.read("Q.md"), before);
}

#[test]
fn closing_stops_the_windows_own_sync_and_writes_the_save_that_waits_for_it() {
    let mut w = World::new("# Q\none\n");
    // A pull that would never end by itself.
    let engine = w.dir.path().join("engine");
    let script = fs::read_to_string(&engine).unwrap();
    fs::write(&engine, script.replace("*) echo", "*) exec sleep 600; echo")).unwrap();
    let Started::Running(running) = w.backend.start(Kind::Pull) else {
        panic!("the engine starts");
    };
    let stopper = running.stopper();
    let waiting = std::thread::spawn(move || running.wait());
    assert!(!w.backend.save_note("# Q\none, mine\n", None), "it waits");
    assert!(w.backend.close_writes_queued_save());
    stopper.stop();
    w.backend.write_queued_save_on_close();
    assert_eq!(w.read("Q.md"), format!("{ID}# Q\none, mine\n"));
    assert!(!w.backend.sync_running());
    assert_ne!(waiting.join().unwrap().exit_code, 0, "the run was killed");
    // Stopping a run that is over already does nothing.
    stopper.stop();
}

#[test]
fn a_waiting_save_is_merged_from_the_text_its_editor_started_from() {
    // The note was loaded again under the sync before the window saved: its
    // save names the text the editor started from, so the reloaded note is
    // not taken for it and what changed in between is kept.
    let loaded = "# Q\none\n\ntwo\n";
    let mut w = World::new(loaded);
    w.pull_with(|w| {
        w.write("Q.md", &format!("{ID}# Q\none\n\ntwo from iCloud\n"));
        w.backend.refresh();
        assert!(!w.backend.save_note("# Q\none, mine\n\ntwo\n", Some(loaded)));
    });
    assert_eq!(w.read("Q.md"), format!("{ID}# Q\none, mine\n\ntwo from iCloud\n"));
    // Without an editor's, a save queued again keeps the base of the first.
    w.write("Q.md", &format!("{ID}{loaded}"));
    w.backend.refresh();
    w.pull_with(|w| {
        w.backend.save_note("# Q\none, mine\n\ntwo\n", None);
        w.write("Q.md", &format!("{ID}# Q\none\n\ntwo from iCloud\n"));
        w.backend.refresh();
        w.backend.save_note("# Q\none, mine and more\n\ntwo\n", None);
    });
    assert_eq!(
        w.read("Q.md"),
        format!("{ID}# Q\none, mine and more\n\ntwo from iCloud\n")
    );
}

#[test]
fn edits_follow_a_note_the_sync_moved_and_outlive_one_it_deleted() {
    let mut w = World::new("# Moving\none\n\ntwo\n");
    fs::create_dir(w.path("Elsewhere")).unwrap();
    // Moved and retitled by the pull: found again by its id.
    w.pull_with(|w| {
        assert!(!w.backend.save_note("# Moving\none\n\ntwo, mine\n", None));
        fs::remove_file(w.path("Q.md")).unwrap();
        w.write("Elsewhere/Moved.md", &format!("{ID}# Moved\none\n\ntwo\n"));
    });
    assert_eq!(
        (w.backend.current_folder(), w.backend.current_note()),
        ("Elsewhere", "Moved.md")
    );
    assert_eq!(w.read("Elsewhere/Moved.md"), format!("{ID}# Moved\none\n\ntwo, mine\n"));
    // Deleted by the pull: the edits become a note of their own, without the id.
    w.backend.take_events();
    w.pull_with(|w| {
        assert!(!w.backend.save_note("# Moved\none, kept\n\ntwo, mine\n", None));
        fs::remove_file(w.path("Elsewhere/Moved.md")).unwrap();
    });
    assert_eq!(w.backend.current_note(), "Moved (unsaved edits).md");
    assert_eq!(
        w.read("Elsewhere/Moved (unsaved edits).md"),
        "# Moved\none, kept\n\ntwo, mine\n"
    );
    let told = w.backend.take_events();
    assert!(
        told.contains(&Event::EditsKeptAsNote(
            "\"Moved\" was deleted elsewhere, so your unsaved edits are in a new note, \"Moved (unsaved edits)\"."
                .into()
        )),
        "{told:?}"
    );
    assert!(w.backend.sync_log().contains("was deleted elsewhere"));
}

#[test]
fn edits_are_never_merged_into_a_note_that_holds_a_conflict() {
    let loaded = "# Clash\nline\n";
    let mut w = World::new(loaded);
    let theirs = format!("{ID}# Clash\n<<<<<<< local\na\n=======\nb\n>>>>>>> remote\n");
    w.write("Q.md", &theirs);
    assert!(w.backend.keep_edits_as_conflict(loaded, "# Clash\nline, mine\n"));
    assert_eq!(w.read("Q.md"), theirs, "the note is left as it is");
    assert_eq!(w.read("Clash (unsaved edits).md"), "# Clash\nline, mine\n");
    assert_eq!(w.backend.current_note(), "Clash (unsaved edits).md");
    let told = w.backend.take_events();
    assert!(
        told.contains(&Event::EditsKeptAsNote(
            "\"Clash\" has an unresolved conflict, so your unsaved edits were not merged into it. They are in a new note, \
             \"Clash (unsaved edits)\", and \"Clash\" is unchanged."
                .into()
        )),
        "{told:?}"
    );
    // A second time the name is taken: a number tells them apart.
    w.backend.open_note("Q.md");
    assert!(w.backend.keep_edits_as_conflict(loaded, "# Clash\nline, mine again\n"));
    assert!(exists(&w.path("Clash (unsaved edits 2).md")));
}

#[test]
fn a_save_warns_of_what_it_would_cost_in_icloud() {
    let mut w = World::new("# Pics\n![pic.png](attachments/pic.png)\n[doc.pdf](attachments/doc.pdf)\n");
    w.write("Copy.md", &format!("{ID}# A copy\n"));
    w.write(
        "Copy.sync-conflict-20260101-101010-ABCDEFG.md",
        &format!("{ID}# Syncthing's\n"),
    );
    fs::create_dir(w.path("attachments")).unwrap();
    w.write("attachments/pic.png", "png");
    w.write("attachments/doc.pdf", "pdf");
    w.backend.refresh();
    let twin = "Another note in this folder (Copy.md) carries the same apple-note-id. Pushing two files with one id is \
                ambiguous, so keep only one.";
    let both = "# Pics\n![pic.png](attachments/pic.png)\n[doc.pdf](attachments/doc.pdf)\nmore\n";
    assert_eq!(w.backend.save_warning(both), twin);
    assert_eq!(w.backend.removed_attachments(both), [] as [&str; 0]);
    assert_eq!(
        w.backend.save_warning("# Pics\n[doc.pdf](attachments/doc.pdf)\n"),
        format!("{twin}\n\nThis removes the attachment pic.png from this note on all your devices.")
    );
    assert_eq!(
        w.backend.save_warning("# Pics\n<<<<<<< x\n"),
        format!(
            "Unresolved conflict markers present. Push will refuse this note until they are resolved.\n\n{twin}\n\nThis \
             removes the attachments pic.png, doc.pdf from this note on all your devices."
        )
    );
    // With the copy gone only Syncthing's is left, which the engine never syncs.
    fs::remove_file(w.path("Copy.md")).unwrap();
    w.backend.refresh();
    assert_eq!(w.backend.save_warning(both), "");
}

#[test]
fn a_read_only_note_is_never_written() {
    let mut w = World::new("# Q\n");
    w.write("R.md", "---\napple-note-id: id-r\n---\n# Locked\n");
    w.backend.refresh();
    w.backend.open_note("R.md");
    assert_eq!(w.backend.read_only_reason(), "is locked");
    assert_eq!(w.backend.note_flags("R.md"), ["read-only"]);
    assert!(w.backend.save_note("# Locked\nedited\n", None), "nothing to save");
    assert!(!w.backend.keep_edits_as_conflict("x", "y"));
    assert_eq!(w.backend.rename_current_note("New").unwrap_err().code, "read_only");
    assert_eq!(w.backend.move_current_note("").unwrap_err().code, "read_only");
    assert_eq!(w.read("R.md"), "---\napple-note-id: id-r\n---\n# Locked\n");
}

#[test]
fn a_pdf_is_laid_out_by_whoever_can_and_never_over_another() {
    let mut w = World::new("# Q\ntext\n");
    let laid_out = std::cell::RefCell::new(Vec::new());
    let render = |markdown: &str, pdf: &Path| {
        laid_out.borrow_mut().push(markdown.to_string());
        fs::write(pdf, "pdf")
    };
    assert_eq!(w.backend.export_pdf(&render).unwrap(), w.path("Q.pdf"));
    assert_eq!(*laid_out.borrow(), ["# Q\ntext\n"], "the body, without the id block");
    let refused = w.backend.export_pdf(&render).unwrap_err();
    assert_eq!(
        (refused.code, refused.message.as_str()),
        ("exists", "Already exists (not overwritten): Q.pdf")
    );
    assert_eq!((laid_out.borrow().len(), w.read("Q.pdf").as_str()), (1, "pdf"));
    // A renderer that fails, or says it worked and wrote nothing.
    fs::remove_file(w.path("Q.pdf")).unwrap();
    let mut cannot = |render: &dyn Fn(&str, &Path) -> std::io::Result<()>| {
        let refused = w.backend.export_pdf(render).unwrap_err();
        assert_eq!(
            (refused.code, refused.message.as_str()),
            ("error", "Could not write the PDF.")
        );
    };
    cannot(&|_, _| Err(std::io::Error::other("no printer")));
    cannot(&|_, _| Ok(()));
    // A note titled by its file's name gets the title as a heading.
    let mut w = World::titled("filename", "text\n");
    w.backend.export_pdf(&render).unwrap();
    assert_eq!(laid_out.borrow().last().unwrap(), "# Q\n\ntext\n");
}

#[test]
fn the_tree_lists_every_note_newest_first() {
    let mut w = World::new("# Q\n");
    fs::create_dir(w.path("Work")).unwrap();
    w.write("Work/Older.md", "# Older\n");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(w.path("Work/Older.md"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    w.backend.refresh();
    let notes = w.backend.all_notes();
    let listed: Vec<(&str, &str, &str)> = notes
        .iter()
        .map(|n| (n.path.as_str(), n.folder.as_str(), n.title.as_str()))
        .collect();
    assert_eq!(listed, [("Q.md", "", "Q"), ("Work/Older.md", "Work", "Older")]);
    let rows = icloud_notes::notetree::rows(
        w.backend.folders(),
        w.backend.folder_note_counts(),
        &notes,
        &["Work".to_string()],
        false,
        &[],
    );
    let shown: Vec<(&str, usize, i64, bool)> = rows
        .iter()
        .map(|r| (r.folder.as_str(), r.depth, r.count, r.note.is_some()))
        .collect();
    assert_eq!(shown, [("", 0, 2, false), ("Work", 0, 1, false), ("Work", 1, 0, true)]);
}

#[test]
fn the_windows_search_reads_the_first_notes_and_hands_them_to_recall() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    let most = icloud_notes::backend::WINDOW_SEARCH_NOTES;
    for n in 0..=most {
        fs::write(vault.join(format!("N{n}.md")), format!("# Note {n}\nneedle\n")).unwrap();
    }
    let mut b = Backend::with_engine(Vault::at(vault.to_string_lossy()), Role::App, None);
    // The command line searches every note, to its 100 answers.
    assert_eq!(b.search_vault("needle").len(), 100);
    // The window reads no more than its share, says so, and hands what it
    // read to Recall; `~` asks Recall alone, with the same text matches.
    // Without Recall it stops at its 100 answers, as the command line does.
    let found = b.search_window("needle", true);
    assert!(found.truncated);
    assert_eq!((found.hits.len(), found.read.len()), (100, most));
    assert_eq!(
        found.read[0].text,
        format!("# Note {}\nneedle\n", found.read[0].path[1..].trim_end_matches(".md"))
    );
    let alone = b.search_window("~ needle", false);
    assert!(!alone.truncated && alone.read.is_empty());
    assert_eq!(alone.hits, found.hits);
    assert!(
        b.search_window("~n", false).hits.is_empty(),
        "one character is no query"
    );
}

#[test]
fn a_run_waits_for_another_processs_lock_and_saves_wait_with_it() {
    let mut w = World::new("# Q\none\n");
    let lock = icloud_notes::vault::Vault::at(w.dir.path().join("vault").to_string_lossy()).lock_path();
    let mut other = icloud_notes::lock::VaultLock::new(lock);
    assert_eq!(other.try_lock("a background sync"), icloud_notes::lock::Taken::Locked);
    assert!(matches!(w.backend.begin(Kind::Pull), Started::Waiting));
    assert!(w.backend.sync_running());
    assert!(!w.backend.save_note("# Q\none, mine\n", None), "it waits");
    assert!(matches!(w.backend.go_on(), Started::Waiting), "the lock is still taken");
    assert_eq!(w.read("Q.md"), format!("{ID}# Q\none\n"));
    assert!(
        !w.backend.close_writes_queued_save(),
        "the run is not this process's to stop"
    );
    other.release();
    drop(other);
    let Started::Running(running) = w.backend.go_on() else {
        panic!("the engine starts once the lock is free");
    };
    assert!(w.backend.holds_lock());
    w.backend.finish(running.wait());
    assert_eq!(w.read("Q.md"), format!("{ID}# Q\none, mine\n"));
    assert!(matches!(w.backend.go_on(), Started::Nothing), "no run waits");
}
