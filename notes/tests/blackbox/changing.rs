//! The commands that change the vault's files: new, write and its guards,
//! rename, move, delete, toggle, attach, detach, export-pdf and the folder
//! commands. Each is checked by what it prints and by the files it leaves.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use crate::world::{USAGE_HINT, World, column};

const ALPHA: &str = "---\napple-note-id: id-a\n---\n# Alpha\n- [ ] milk\n- eggs\nplain line\n";

/// The names in a directory, sorted.
fn names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries.map(|e| e.unwrap().file_name().into_string().unwrap()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

// ---- new --------------------------------------------------------------------

#[test]
fn new_makes_a_note_in_the_default_folder_title_first() {
    let Some(w) = World::seeded() else { return };
    let made = w.run(["new", "Groceries", "--body", "bread\ncheese"]);
    assert_eq!(
        made.ok(),
        json!({"action": "new", "path": "Notes/Groceries.md", "folder": "Notes", "file": "Groceries.md", "title": "Groceries"})
    );
    assert_eq!(made.summary(), "Created Notes/Groceries.md");
    assert_eq!(w.read("Notes/Groceries.md"), "# Groceries\nbread\ncheese\n");
    // No body: the title line alone. A body's own final newline is kept as one.
    assert_eq!(w.json(["new", "Bare"]).ok()["path"], "Notes/Bare.md");
    assert_eq!(w.read("Notes/Bare.md"), "# Bare\n");
    w.json(["new", "Ended", "--body", "line\n"]).ok();
    assert_eq!(w.read("Notes/Ended.md"), "# Ended\nline\n");
    w.json(["new", "Empty body", "--body", ""]).ok();
    assert_eq!(w.read("Notes/Empty body.md"), "# Empty body\n");
    // A new note is untracked until a sync.
    assert_eq!(w.json(["read", "Notes/Bare.md"]).ok()["flags"], json!(["new"]));
}

#[test]
fn new_takes_a_folder_and_cleans_the_title() {
    let Some(w) = World::seeded() else { return };
    assert_eq!(
        w.json(["new", "Root note", "--folder", ""]).ok()["path"],
        "Root note.md"
    );
    assert_eq!(w.read("Root note.md"), "# Root note\n");
    assert_eq!(w.json(["new", "In work", "--folder", "work"]).ok()["folder"], "Work");
    // Slashes cannot be in a file's name; the ends are trimmed; .md is not doubled.
    let made = w.json(["new", "  Q1/Q2 plan  "]).ok();
    assert_eq!(
        (&made["file"], &made["title"]),
        (&json!("Q1Q2 plan.md"), &json!("Q1Q2 plan"))
    );
    assert_eq!(w.read("Notes/Q1Q2 plan.md"), "# Q1Q2 plan\n");
    let made = w.json(["new", "Named.MD"]).ok();
    assert_eq!(
        (&made["file"], &made["title"]),
        (&json!("Named.MD"), &json!("Named.MD"))
    );
    assert_eq!(w.read("Notes/Named.MD"), "# Named\n");
    let (message, hint) = w.json(["new", "X", "--folder", "Nope"]).fails(4, "not_found");
    assert_eq!(message, "no folder \"Nope\"");
    assert!(hint.contains("icloud-notes new-folder"), "{hint}");
    for (title, message) in [
        ("  ", "the title is empty"),
        ("/", "the title is empty"),
        (".hidden", "A name cannot start with a dot: those are skipped by sync."),
    ] {
        let said = w.json(["new", title]).fails(2, "usage");
        assert_eq!((said.0.as_str(), said.1.as_str()), (message, USAGE_HINT), "{title:?}");
    }
    assert!(!w.exists("Notes/.hidden.md"));
}

#[test]
fn new_never_replaces_a_note() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w.json(["new", "Alpha", "--body", "other"]).fails(2, "exists");
    assert_eq!(message, "a note named \"Alpha.md\" is already in Notes");
    assert_eq!(hint, "Edit it with `icloud-notes write`, or pick another title.");
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
    let (message, _) = w.json(["new", "Twin", "--folder", ""]).fails(2, "exists");
    assert_eq!(message, "a note named \"Twin.md\" is already in the vault root");
}

#[test]
fn new_goes_to_the_root_without_a_default_folder() {
    let Some(w) = World::new() else { return };
    // The engine names a default folder that is not there.
    w.state("in-body", &[], Some("Notes"));
    assert_eq!(w.json(["new", "First"]).ok()["path"], "First.md");
    assert!(!w.exists("Notes"));
}

#[test]
fn new_in_a_filename_vault_leaves_the_title_to_the_name() {
    let Some(w) = World::new() else { return };
    w.state("filename", &[], None);
    w.json(["new", "Empty"]).ok();
    assert_eq!(fs::read(w.path("Empty.md")).unwrap(), b"");
    w.json(["new", "Filled", "--body", "just the text"]).ok();
    assert_eq!(w.read("Filled.md"), "just the text\n");
}

#[test]
fn the_text_comes_from_an_option_a_file_or_stdin_once() {
    let Some(w) = World::seeded() else { return };
    let file = w.scratch().join("body.txt");
    fs::write(&file, "from a file").unwrap();
    w.json(["new".as_ref(), "Filed".as_ref(), "--file".as_ref(), file.as_os_str()])
        .ok();
    assert_eq!(w.read("Notes/Filed.md"), "# Filed\nfrom a file\n");
    w.cmd(["new", "Piped", "--stdin"]).stdin("from stdin\n").run().ok();
    assert_eq!(w.read("Notes/Piped.md"), "# Piped\nfrom stdin\n");
    w.cmd(["write", "piped", "--stdin"])
        .stdin("# Piped\nrewritten")
        .run()
        .ok();
    assert_eq!(w.read("Notes/Piped.md"), "# Piped\nrewritten\n");
    for args in [
        vec!["new", "Twice", "--body", "a", "--stdin"],
        vec!["new", "Twice", "--body", "a", "--file", "/dev/null"],
        vec!["write", "alpha", "--stdin", "--file", "/dev/null"],
    ] {
        let (message, _) = w.cmd(&args).stdin("x").run().fails(2, "usage");
        assert_eq!(message, "give the text once: --body, --file or --stdin", "{args:?}");
    }
    assert!(!w.exists("Notes/Twice.md"));
    let missing = w.scratch().join("no-such-file.txt");
    let (message, hint) = w
        .json([
            "new".as_ref(),
            "Unread".as_ref(),
            "--file".as_ref(),
            missing.as_os_str(),
        ])
        .fails(1, "error");
    assert_eq!(
        (message, hint.as_str()),
        (
            format!("cannot read {}: No such file or directory", missing.display()),
            ""
        )
    );
    assert!(!w.exists("Notes/Unread.md"));
    let (message, _) = w.json(["write", "alpha"]).fails(2, "usage");
    assert_eq!(message, "write needs the text: --body TEXT, --file PATH or --stdin");
}

// ---- write ------------------------------------------------------------------

#[test]
fn write_replaces_the_body_and_keeps_the_id_block() {
    let Some(w) = World::seeded() else { return };
    let wrote = w.run(["write", "alpha", "--body", "# Alpha\nreplaced"]);
    assert_eq!(
        wrote.ok(),
        json!({"action": "write", "path": "Notes/Alpha.md", "changed": true})
    );
    assert_eq!(wrote.summary(), "Wrote Notes/Alpha.md");
    assert_eq!(
        w.read("Notes/Alpha.md"),
        "---\napple-note-id: id-a\n---\n# Alpha\nreplaced\n"
    );
    // The same text again writes nothing.
    w.set_age("Notes/Alpha.md", 500);
    let before = fs::metadata(w.path("Notes/Alpha.md")).unwrap().modified().unwrap();
    let again = w.run(["write", "alpha", "--body", "# Alpha\nreplaced\n"]);
    assert_eq!(
        again.ok(),
        json!({"action": "write", "path": "Notes/Alpha.md", "changed": false})
    );
    assert_eq!(again.summary(), "Notes/Alpha.md unchanged");
    assert_eq!(
        fs::metadata(w.path("Notes/Alpha.md")).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(
        w.text(["write", "alpha", "--body", "# Alpha\nreplaced\n"]).out,
        "Notes/Alpha.md unchanged\n"
    );
    // A block with blank lines after it comes back byte for byte.
    w.write(
        "Notes/Spaced.md",
        "--- \napple-note-id: id-s\nother: kept\n---\n\n\n# Spaced\nold\n",
    );
    w.json(["write", "Notes/Spaced.md", "--body", "# Spaced\nnew"]).ok();
    assert_eq!(
        w.read("Notes/Spaced.md"),
        "--- \napple-note-id: id-s\nother: kept\n---\n\n\n# Spaced\nnew\n"
    );
    // Nothing is left beside the note by the write.
    assert_eq!(names(&w.path("Notes")), ["Alpha.md", "Spaced.md", "Twin.md"]);
}

#[test]
fn write_appends_on_a_line_of_its_own() {
    let Some(w) = World::seeded() else { return };
    w.json(["write", "alpha", "--append", "--body", "jam"]).ok();
    assert_eq!(w.read("Notes/Alpha.md"), format!("{ALPHA}jam\n"));
    // A body that does not end its last line gets the newline first.
    w.write("Notes/Open.md", "# Open\nno newline");
    w.json(["write", "Notes/Open.md", "--append", "--body", "more\n"]).ok();
    assert_eq!(w.read("Notes/Open.md"), "# Open\nno newline\nmore\n");
    w.write("Notes/Blank.md", "---\napple-note-id: id-z\n---\n");
    w.json(["write", "Notes/Blank.md", "--append", "--body", "first"]).ok();
    assert_eq!(w.read("Notes/Blank.md"), "---\napple-note-id: id-z\n---\nfirst\n");
}

#[test]
fn write_refuses_a_read_only_note() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w
        .json(["write", "grid", "--body", "x", "--force", "--yes"])
        .fails(1, "read_only");
    assert_eq!(message, "Work/Grid.md is read-only here: this note is too large");
    assert_eq!(hint, "Edit it in Apple Notes; the changes still sync here.");
    assert_eq!(w.read("Work/Grid.md"), "---\napple-note-id: id-g\n---\n# Grid\nhuge\n");
}

#[test]
fn write_never_empties_a_note_unasked() {
    let Some(w) = World::seeded() else { return };
    let empty = w.scratch().join("empty.txt");
    fs::write(&empty, "").unwrap();
    let hint = "To delete it, use `icloud-notes delete NOTE --yes`; to empty it anyway, pass --yes.";
    let message = "the text is empty, and the next push would delete Notes/Alpha.md in iCloud";
    for source in [
        vec!["--body", ""],
        vec!["--stdin"],
        vec!["--file", empty.to_str().unwrap()],
        vec!["--body", "\n\n"],
    ] {
        let said = w
            .json(["write", "alpha"].into_iter().chain(source.iter().copied()))
            .fails(2, "usage");
        assert_eq!((said.0.as_str(), said.1.as_str()), (message, hint), "{source:?}");
    }
    // --force is for the guardrail, not for this.
    w.json(["write", "alpha", "--body", "", "--force"]).fails(2, "usage");
    w.text(["write", "alpha", "--body", ""]).fails_in_text(2, message, hint);
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
    w.json(["write", "alpha", "--yes", "--body", ""]).ok();
    assert_eq!(w.read("Notes/Alpha.md"), "---\napple-note-id: id-a\n---\n");
}

#[test]
fn write_empties_a_filename_vaults_note_without_asking() {
    let Some(w) = World::new() else { return };
    // There the title is the file's name, so an empty body is still a note.
    w.state("filename", &[&["id-n", "Named.md"]], None);
    w.write("Named.md", "---\napple-note-id: id-n\n---\ntext\n");
    assert_eq!(w.json(["write", "named", "--body", ""]).ok()["changed"], true);
    assert_eq!(w.read("Named.md"), "---\napple-note-id: id-n\n---\n");
}

#[test]
fn write_asks_before_dropping_an_attachment() {
    let Some(w) = World::seeded() else { return };
    w.write("Work/attachments/two.pdf", "pdf");
    let body = "# Pics\n![pic.png](attachments/pic.png)\n[two](attachments/two.pdf)\n";
    w.json(["write", "pics", "--body", body]).ok();
    let linked = w.read("Work/Pics.md");
    let (message, hint) = w
        .json(["write", "pics", "--body", "# Pics\n[two](attachments/two.pdf)\n"])
        .fails(2, "usage");
    assert_eq!(
        message,
        "the text no longer links the attachment pic.png of Work/Pics.md, and the next push would remove it on all your devices"
    );
    assert_eq!(
        hint,
        "Keep the link(s) as `icloud-notes read` shows them, or pass --yes to remove the attachment."
    );
    let (message, hint) = w
        .json(["write", "pics", "--body", "# Pics\n", "--force"])
        .fails(2, "usage");
    assert_eq!(
        message,
        "the text no longer links the attachments pic.png, two.pdf of Work/Pics.md, and the next push would remove them on \
         all your devices"
    );
    assert!(hint.ends_with("pass --yes to remove the attachments."), "{hint}");
    assert_eq!(w.read("Work/Pics.md"), linked);
    // Keeping the links, or appending, asks nothing.
    w.json(["write", "pics", "--body", format!("{body}one more line\n").as_str()])
        .ok();
    w.json(["write", "pics", "--append", "--body", "appended"]).ok();
    let done = w
        .json([
            "write",
            "pics",
            "--yes",
            "--body",
            "# Pics\n[two](attachments/two.pdf)\n",
        ])
        .ok();
    assert_eq!(done["changed"], true);
    assert_eq!(
        column(&w.json(["read", "pics"]).ok()["attachments"], "name"),
        ["two.pdf"]
    );
    assert!(w.exists("Work/attachments/pic.png"), "the file is the push's to remove");
}

#[test]
fn write_holds_back_text_the_next_push_would_refuse() {
    let Some(w) = World::seeded() else { return };
    let markers = "# Alpha\n<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n";
    let hint = "Nothing was written. Pass --force to write it anyway (as Ctrl+S does in Notes).";
    let (message, said) = w.json(["write", "alpha", "--body", markers]).fails(1, "guardrail");
    assert_eq!(
        message,
        "Unresolved conflict markers present. Push will refuse this note until they are resolved."
    );
    assert_eq!(said, hint);
    // --yes is no --force.
    w.json(["write", "alpha", "--body", markers, "--yes"])
        .fails(1, "guardrail");
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
    w.json(["write", "alpha", "--force", "--body", markers]).ok();
    assert_eq!(
        w.read("Notes/Alpha.md"),
        format!("---\napple-note-id: id-a\n---\n{markers}")
    );
    // A note that already carries markers is not held back for keeping them.
    w.json(["write", "alpha", "--body", format!("{markers}more\n").as_str()])
        .ok();
    w.json(["write", "beta", "--append", "--body", "still conflicted"]).ok();
}

#[test]
fn write_warns_about_a_second_note_with_the_same_id() {
    let Some(w) = World::seeded() else { return };
    w.write("Notes/Copy.md", "---\napple-note-id: id-a\n---\n# Copy\n");
    let (message, _) = w
        .json(["write", "Notes/Copy.md", "--body", "# Copy\nedited\n"])
        .fails(1, "guardrail");
    assert_eq!(
        message,
        "Another note in this folder (Alpha.md) carries the same apple-note-id. Pushing two files with one id is ambiguous, \
         so keep only one."
    );
    // Both at once, a blank line between them.
    let (message, _) = w
        .json([
            "write",
            "Notes/Copy.md",
            "--body",
            "# Copy\n<<<<<<< local\n=======\n>>>>>>> remote\n",
        ])
        .fails(1, "guardrail");
    assert_eq!(
        message,
        "Unresolved conflict markers present. Push will refuse this note until they are resolved.\n\nAnother note in this \
         folder (Alpha.md) carries the same apple-note-id. Pushing two files with one id is ambiguous, so keep only one."
    );
    // The same id in another folder is not this check's business, nor is a
    // copy Syncthing made, on either side.
    w.write("Work/Far.md", "---\napple-note-id: id-a\n---\n# Far\n");
    w.json(["write", "Work/Far.md", "--body", "# Far\nedited\n"]).ok();
    fs::remove_file(w.path("Notes/Copy.md")).unwrap();
    w.write("Notes/Alpha.sync-conflict-20261114-101500-ABCDEF7.md", ALPHA);
    w.json(["write", "Notes/Alpha.md", "--append", "--body", "kept"]).ok();
    w.json([
        "write",
        "Notes/Alpha.sync-conflict-20261114-101500-ABCDEF7.md",
        "--append",
        "--body",
        "kept",
    ])
    .ok();
}

// ---- rename, move, delete -----------------------------------------------------

#[test]
fn rename_retitles_the_first_line_of_an_in_body_note() {
    let Some(w) = World::seeded() else { return };
    let renamed = w.run(["rename", "alpha", "  Q1/Q2 plan "]);
    assert_eq!(
        renamed.ok(),
        json!({"action": "rename", "path": "Notes/Alpha.md", "from": "Notes/Alpha.md", "title": "Q1/Q2 plan"})
    );
    assert_eq!(renamed.summary(), "Renamed Notes/Alpha.md to \"Q1/Q2 plan\"");
    // The file keeps its name; the heading keeps its marks.
    assert_eq!(
        w.read("Notes/Alpha.md"),
        "---\napple-note-id: id-a\n---\n# Q1/Q2 plan\n- [ ] milk\n- eggs\nplain line\n"
    );
    w.write("Notes/Deep.md", "### Deep\nbody\n");
    w.json(["rename", "Notes/Deep.md", "Deeper"]).ok();
    assert_eq!(w.read("Notes/Deep.md"), "### Deeper\nbody\n");
    // A bare first line stays bare; an empty note gains a heading.
    w.write("Notes/Bare.md", "bare title\nbody\n");
    w.json(["rename", "Notes/Bare.md", "Still bare"]).ok();
    assert_eq!(w.read("Notes/Bare.md"), "Still bare\nbody\n");
    w.write("Notes/Void.md", "---\napple-note-id: id-v\n---\n");
    w.json(["rename", "Notes/Void.md", "Filled"]).ok();
    assert_eq!(w.read("Notes/Void.md"), "---\napple-note-id: id-v\n---\n# Filled\n");
}

#[test]
fn rename_refuses_what_it_cannot_do_safely() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w.json(["rename", "grid", "Other"]).fails(1, "read_only");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("This note is read-only here. Rename it in Apple Notes.", "")
    );
    let (message, _) = w.json(["rename", "alpha", "  "]).fails(1, "error");
    assert_eq!(message, "Title is empty.");
    // The title line holds the attachment's link: retitling would drop it.
    w.write("Work/Shot.md", "![pic.png](attachments/pic.png)\nbody\n");
    let (message, _) = w.json(["rename", "Work/Shot.md", "Named"]).fails(1, "error");
    assert_eq!(
        message,
        "The title line holds an attachment. Edit the note's text instead."
    );
    assert_eq!(w.read("Work/Shot.md"), "![pic.png](attachments/pic.png)\nbody\n");
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
}

#[test]
fn rename_renames_the_file_in_a_filename_vault() {
    let Some(w) = World::new() else { return };
    w.state(
        "filename",
        &[&["id-o", "Old.md"], &["id-r", "Locked.md", "has a drawing"]],
        None,
    );
    w.write("Old.md", "---\napple-note-id: id-o\n---\ntext\n");
    w.write("Taken.md", "taken\n");
    w.write("Locked.md", "---\napple-note-id: id-r\n---\n");
    let renamed = w.run(["rename", "old", " Re/named "]);
    assert_eq!(
        renamed.ok(),
        json!({"action": "rename", "path": "Renamed.md", "from": "Old.md", "title": "Re/named"})
    );
    assert_eq!(renamed.summary(), "Renamed Old.md to \"Re/named\"");
    assert_eq!(w.read("Renamed.md"), "---\napple-note-id: id-o\n---\ntext\n");
    assert!(!w.exists("Old.md"));
    // Its own name again is nothing to do.
    assert_eq!(w.json(["rename", "renamed", "Renamed"]).ok()["path"], "Renamed.md");
    let (message, _) = w.json(["rename", "renamed", "Taken"]).fails(2, "exists");
    assert_eq!(message, "A note with that name already exists.");
    let (message, hint) = w.json(["rename", "renamed", ".dot"]).fails(2, "usage");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("A name cannot start with a dot: those are skipped by sync.", USAGE_HINT)
    );
    w.json(["rename", "locked", "Free"]).fails(1, "read_only");
    assert_eq!(
        names(&w.vault),
        [".icloud-notes", "Locked.md", "Renamed.md", "Taken.md"]
    );
}

#[test]
fn move_puts_a_note_in_another_folder() {
    let Some(w) = World::seeded() else { return };
    let moved = w.run(["move", "alpha", "work"]);
    assert_eq!(
        moved.ok(),
        json!({"action": "move", "path": "Work/Alpha.md", "from": "Notes/Alpha.md"})
    );
    assert_eq!(moved.summary(), "Moved Notes/Alpha.md to Work/Alpha.md");
    assert_eq!(w.read("Work/Alpha.md"), ALPHA);
    assert!(!w.exists("Notes/Alpha.md"));
    // To the root, and where it already is.
    assert_eq!(w.json(["move", "Work/Alpha.md", ""]).ok()["path"], "Alpha.md");
    let stays = w.json(["move", "Alpha.md", "/"]).ok();
    assert_eq!(
        (&stays["path"], &stays["from"]),
        (&json!("Alpha.md"), &json!("Alpha.md"))
    );
    // A note's attachments stay where they are: the next push moves them.
    assert_eq!(w.json(["move", "pics", "Notes"]).ok()["path"], "Notes/Pics.md");
    assert!(w.exists("Work/attachments/pic.png") && !w.exists("Notes/attachments"));
}

#[test]
fn move_refuses_a_taken_name_a_missing_folder_and_a_read_only_note() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w.json(["move", "Twin.md", "Notes"]).fails(2, "exists");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("A note with that name already exists in that folder.", "")
    );
    let (message, _) = w.json(["move", "alpha", "Nope"]).fails(4, "not_found");
    assert_eq!(message, "no folder \"Nope\"");
    let (message, _) = w.json(["move", "grid", "Notes"]).fails(1, "read_only");
    assert_eq!(message, "This note is read-only here. Move it in Apple Notes.");
    w.json(["move", "no such note", "Notes"]).fails(4, "not_found");
    assert!(w.exists("Twin.md") && w.exists("Work/Grid.md"));
}

#[test]
fn delete_moves_a_note_to_the_trash_when_told_to() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w.json(["delete", "alpha"]).fails(2, "usage");
    assert_eq!(
        message,
        "delete asks before it acts, and cannot ask here (no terminal, or a machine output mode)"
    );
    assert_eq!(hint, "Pass --yes to go ahead.");
    // Text mode with no terminal cannot ask either.
    w.text(["delete", "alpha"]).fails_in_text(2, &message, &hint);
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
    let deleted = w.run(["delete", "alpha", "--yes"]);
    assert_eq!(deleted.ok(), json!({"action": "delete", "path": "Notes/Alpha.md"}));
    assert_eq!(deleted.summary(), "Moved Notes/Alpha.md to the trash");
    assert!(!w.exists("Notes/Alpha.md"));
    // The desktop's trash: the file, and where it came from.
    assert_eq!(fs::read_to_string(w.trash().join("files/Alpha.md")).unwrap(), ALPHA);
    let info = fs::read_to_string(w.trash().join("info/Alpha.md.trashinfo")).unwrap();
    assert!(info.starts_with("[Trash Info]\n"), "{info}");
    assert!(
        info.contains(&format!("\nPath={}\n", w.path("Notes/Alpha.md").display())),
        "{info}"
    );
    assert!(info.contains("\nDeletionDate=20"), "{info}");
    // A read-only note can go too; a second file of the same name gets another.
    w.json(["delete", "grid", "--yes"]).ok();
    w.json(["delete", "Twin.md", "--yes"]).ok();
    w.json(["delete", "Notes/Twin.md", "--yes"]).ok();
    let trashed = names(&w.trash().join("files"));
    assert_eq!(trashed.len(), 4, "{trashed:?}");
    assert!(
        trashed.contains(&"Twin.md".to_string()) && trashed.contains(&"Grid.md".to_string()),
        "{trashed:?}"
    );
    w.json(["delete", "alpha", "--yes"]).fails(4, "not_found");
}

// ---- toggle -------------------------------------------------------------------

#[test]
fn toggle_ticks_unticks_and_plants_a_checkbox() {
    let Some(w) = World::seeded() else { return };
    let ticked = w.run(["toggle", "alpha", "2"]);
    assert_eq!(
        ticked.ok(),
        json!({"action": "toggle", "path": "Notes/Alpha.md", "line": 2, "text": "- [x] milk"})
    );
    assert_eq!(ticked.summary(), "Line 2 of Notes/Alpha.md: - [x] milk");
    assert_eq!(w.json(["toggle", "alpha", "3"]).ok()["text"], "- [ ] eggs");
    assert_eq!(
        w.read("Notes/Alpha.md"),
        "---\napple-note-id: id-a\n---\n# Alpha\n- [x] milk\n- [ ] eggs\nplain line\n"
    );
    assert_eq!(w.json(["toggle", "alpha", " +2 "]).ok()["text"], "- [ ] milk");
    // Every list marker, indented or not; lines are counted in the body.
    w.write(
        "Notes/Lists.md",
        "---\napple-note-id: id-l\n---\n* [X] star\n  + plus\n12. [ ] numbered\n\t3) paren\n- [x]tight\n-nospace\n",
    );
    let toggle = |line: &str| {
        w.json(["toggle", "Notes/Lists.md", line]).ok()["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(toggle("1"), "* [ ] star");
    assert_eq!(toggle("2"), "  + [ ] plus");
    assert_eq!(toggle("3"), "12. [x] numbered");
    assert_eq!(toggle("4"), "\t3) [ ] paren");
    // Not a checkbox without a space after it: the item gains one.
    assert_eq!(toggle("5"), "- [ ] [x]tight");
}

#[test]
fn toggle_leaves_everything_else_alone() {
    let Some(w) = World::seeded() else { return };
    for line in ["1", "4", "5", "99"] {
        let (message, hint) = w.json(["toggle", "alpha", line]).fails(1, "not_a_list_item");
        assert_eq!(
            (message, hint.as_str()),
            (format!("line {line} of Notes/Alpha.md is not a list item"), "")
        );
    }
    for line in ["0", "-1", "two", "1.5", ""] {
        // After `--`, so that -1 is an argument and not an option.
        let (message, _) = w.json(["toggle", "--", "alpha", line]).fails(2, "usage");
        assert_eq!(message, format!("LINE is a line number, 1 or more, not \"{line}\""));
    }
    let (message, _) = w.json(["toggle", "grid", "1"]).fails(1, "read_only");
    assert_eq!(message, "This note is read-only here: it is too large");
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
}

// ---- attach, detach -------------------------------------------------------------

#[test]
fn attach_copies_the_file_beside_the_note_and_links_it() {
    let Some(w) = World::seeded() else { return };
    let source = |name: &str, bytes: &[u8]| {
        let path = w.scratch().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    };
    let png = b"\x89PNG\r\n\x1a\n\0binary";
    let photo = source("photo.PNG", png);
    fs::set_permissions(&photo, fs::Permissions::from_mode(0o444)).unwrap();
    let attached = w.run(["attach".as_ref(), "alpha".as_ref(), photo.as_os_str()]);
    assert_eq!(
        attached.ok(),
        json!({
            "action": "attach",
            "path": "Notes/Alpha.md",
            "file": "Notes/attachments/photo.PNG",
            "link": "![photo.PNG](attachments/photo.PNG)",
        })
    );
    assert_eq!(attached.summary(), "Attached photo.PNG to Notes/Alpha.md");
    assert_eq!(fs::read(w.path("Notes/attachments/photo.PNG")).unwrap(), png);
    assert_eq!(
        w.read("Notes/Alpha.md"),
        format!("{ALPHA}![photo.PNG](attachments/photo.PNG)\n")
    );
    assert!(photo.exists(), "the source stays");
    // The copy can be written by its owner, whatever the source allowed.
    let mode = fs::metadata(w.path("Notes/attachments/photo.PNG"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o600, 0o600);
    // A name already taken there: another one, never over the file.
    let second = source("other/photo.PNG", b"second");
    let again = w
        .json(["attach".as_ref(), "Notes/Alpha.md".as_ref(), second.as_os_str()])
        .ok();
    assert_eq!(again["file"], "Notes/attachments/photo 2.PNG");
    assert_eq!(again["link"], "![photo 2.PNG](attachments/photo%202.PNG)");
    let third = w.json(["attach".as_ref(), "alpha".as_ref(), second.as_os_str()]).ok();
    assert_eq!(third["file"], "Notes/attachments/photo 3.PNG");
    assert_eq!(fs::read(w.path("Notes/attachments/photo.PNG")).unwrap(), png);
    assert_eq!(fs::read(w.path("Notes/attachments/photo 2.PNG")).unwrap(), b"second");
    assert_eq!(
        column(&w.json(["read", "alpha"]).ok()["attachments"], "name"),
        ["photo.PNG", "photo 2.PNG", "photo 3.PNG"]
    );
    // A note whose body does not end its last line gets the link on its own.
    w.write("Notes/Open.md", "# Open\nno newline");
    w.json(["attach".as_ref(), "Notes/Open.md".as_ref(), second.as_os_str()])
        .ok();
    assert_eq!(
        w.read("Notes/Open.md"),
        "# Open\nno newline\n![photo 4.PNG](attachments/photo%204.PNG)\n"
    );
}

#[test]
fn attach_links_a_file_the_way_the_engine_reads_it() {
    let Some(w) = World::seeded() else { return };
    let link = |name: &str| {
        let path = w.scratch().join(name);
        fs::write(&path, "x").unwrap();
        w.json(["attach".as_ref(), "alpha".as_ref(), path.as_os_str()]).ok()["link"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // Images are embedded, other files linked.
    for image in [
        "a.png", "b.jpg", "c.JPEG", "d.heic", "e.tiff", "f.tif", "g.gif", "h.webp",
    ] {
        assert_eq!(link(image), format!("![{image}](attachments/{image})"));
    }
    for other in ["i.svg", "j.pdf", "k.txt", "noext", ".png"] {
        assert_eq!(link(other), format!("[{other}](attachments/{other})"));
    }
    // Brackets and backslashes escaped in the text; the path percent-encoded,
    // with parentheses left alone only when they pair up.
    assert_eq!(
        link("plan [v1] (a).pdf"),
        "[plan \\[v1\\] (a).pdf](attachments/plan%20%5Bv1%5D%20(a).pdf)"
    );
    assert_eq!(
        link("receipt (1).png"),
        "![receipt (1).png](attachments/receipt%20(1).png)"
    );
    assert_eq!(
        link("open (paren.txt"),
        "[open (paren.txt](attachments/open%20%28paren.txt)"
    );
    assert_eq!(
        link("back)(wards.txt"),
        "[back)(wards.txt](attachments/back%29%28wards.txt)"
    );
    assert_eq!(
        link("back\\slash.txt"),
        "[back\\\\slash.txt](attachments/back%5Cslash.txt)"
    );
    assert_eq!(
        link("zażółć & co #1!.txt"),
        "[zażółć & co #1!.txt](attachments/za%C5%BC%C3%B3%C5%82%C4%87%20%26%20co%20%231!.txt)"
    );
    assert_eq!(link("it's*~-_.txt"), "[it's*~-_.txt](attachments/it's*~-_.txt)");
}

#[test]
fn attach_refuses_and_changes_nothing() {
    let Some(w) = World::seeded() else { return };
    let scratch = w.scratch().display().to_string();
    let source = |name: &str, bytes: &[u8]| {
        let path = w.scratch().join(name);
        fs::write(&path, bytes).unwrap();
        path.display().to_string()
    };
    let photo = source("photo.png", b"png");
    let (message, hint) = w.json(["attach", "grid", &photo]).fails(1, "read_only");
    assert_eq!(message, "Work/Grid.md is read-only here: this note is too large");
    assert_eq!(hint, "Add the attachment in Apple Notes; it still syncs here.");
    let (message, hint) = w
        .json(["attach", "alpha", &format!("{scratch}/nope.png")])
        .fails(4, "not_found");
    assert_eq!(
        (message, hint.as_str()),
        (format!("no file \"{scratch}/nope.png\""), "")
    );
    w.json(["attach", "no such note", &photo]).fails(4, "not_found");
    let (message, _) = w
        .json(["attach", "alpha", &format!("{scratch}/home")])
        .fails(1, "error");
    assert_eq!(message, format!("cannot read {scratch}/home: not a regular file"));
    let locked = source("locked.txt", b"secret");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (message, _) = w.json(["attach", "alpha", &locked]).fails(1, "error");
    assert_eq!(message, format!("cannot read {locked}: Permission denied"));
    let empty = source("empty.txt", b"");
    let (message, _) = w.json(["attach", "alpha", &empty]).fails(1, "error");
    assert_eq!(message, format!("{empty} is empty: there is nothing to attach"));
    // A name the engine would not take: control characters, or over 190 bytes.
    let long = format!("{}.txt", "n".repeat(187));
    for name in ["tab\tname.txt", long.as_str()] {
        let (message, hint) = w.json(["attach", "alpha", &source(name, b"x")]).fails(2, "usage");
        assert_eq!(
            message,
            format!(
                "\"{name}\" cannot be an attachment's name: rename the file first (no control characters, at most 190 bytes)"
            )
        );
        assert_eq!(hint, USAGE_HINT);
    }
    // 190 bytes is still a name.
    w.write("Work/Spare.md", "# Spare\n");
    let longest = format!("{}.txt", "n".repeat(186));
    w.json(["attach", "Work/Spare.md", &source(&longest, b"x")]).ok();
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
    assert!(!w.exists("Notes/attachments"));
    assert_eq!(names(&w.path("Work/attachments")), [longest.as_str(), "pic.png"]);
}

#[test]
fn attach_refuses_more_than_the_engine_uploads() {
    let Some(w) = World::seeded() else { return };
    // 100 MB and one byte, as a sparse file: nothing is read or copied.
    let big = w.scratch().join("big.bin");
    fs::File::create(&big).unwrap().set_len(100 * 1024 * 1024 + 1).unwrap();
    let (message, hint) = w
        .json(["attach".as_ref(), "alpha".as_ref(), big.as_os_str()])
        .fails(1, "error");
    assert_eq!(
        message,
        format!(
            "{} is larger than 100 MB, the most that can be added as an attachment",
            big.display()
        )
    );
    assert_eq!(hint, "Add it in Apple Notes instead.");
    assert!(!w.exists("Notes/attachments/big.bin"));
}

#[test]
fn detach_takes_the_link_out_and_leaves_the_file() {
    let Some(w) = World::seeded() else { return };
    w.write("Work/attachments/two.pdf", "pdf");
    w.write(
        "Work/Pics.md",
        "---\napple-note-id: id-p\n---\n# Pics\n![pic.png](attachments/pic.png)\nsee [two](attachments/two.pdf) here\n",
    );
    let before = w.read("Work/Pics.md");
    // Never unasked.
    let (message, hint) = w.json(["detach", "pics", "pic.png"]).fails(2, "usage");
    assert_eq!(
        message,
        "detach asks before it acts, and cannot ask here (no terminal, or a machine output mode)"
    );
    assert_eq!(hint, "Pass --yes to go ahead.");
    let (message, hint) = w.json(["detach", "pics", "nope.png", "--yes"]).fails(4, "not_found");
    assert_eq!(message, "Work/Pics.md has no attachment named \"nope.png\"");
    assert_eq!(hint, "Its attachments: pic.png, two.pdf.");
    let (_, hint) = w.json(["detach", "alpha", "x.png", "--yes"]).fails(4, "not_found");
    assert_eq!(hint, "The note has no attachments.");
    let (message, hint) = w.json(["detach", "grid", "x.png", "--yes"]).fails(1, "read_only");
    assert_eq!(message, "Work/Grid.md is read-only here: this note is too large");
    assert_eq!(
        hint,
        "Remove the attachment in Apple Notes; the change still syncs here."
    );
    w.json(["detach", "no such note", "x", "--yes"]).fails(4, "not_found");
    assert_eq!(w.read("Work/Pics.md"), before);
    // A link alone on its line goes with the line; one in a sentence leaves the sentence.
    let detached = w.run(["detach", "pics", "pic.png", "--yes"]);
    assert_eq!(
        detached.ok(),
        json!({"action": "detach", "path": "Work/Pics.md", "file": "Work/attachments/pic.png"})
    );
    assert_eq!(detached.summary(), "Removed the attachment pic.png from Work/Pics.md");
    assert_eq!(
        w.read("Work/Pics.md"),
        "---\napple-note-id: id-p\n---\n# Pics\nsee [two](attachments/two.pdf) here\n"
    );
    w.json(["detach", "pics", "two.pdf", "--yes"]).ok();
    assert_eq!(
        w.read("Work/Pics.md"),
        "---\napple-note-id: id-p\n---\n# Pics\nsee  here\n"
    );
    assert_eq!(names(&w.path("Work/attachments")), ["pic.png", "two.pdf"]);
}

// ---- a file that will not take a save ---------------------------------------------

#[test]
fn a_note_that_cannot_be_written_is_never_replaced() {
    // A save replaces the file, which the folder alone would allow: the
    // note's own permissions are what says no. An unreadable note reads as
    // nothing, and is not to be saved as nothing and the change.
    for mode in [0o444, 0o000] {
        let Some(w) = World::seeded() else { return };
        let note = w.path("Notes/Alpha.md");
        let set = |mode: u32| fs::set_permissions(&note, fs::Permissions::from_mode(mode)).unwrap();
        set(0o000);
        if fs::read(&note).is_ok() {
            return; // root, whom no permissions stop
        }
        set(mode);
        let picture = w.scratch().join("picture.png");
        fs::write(&picture, "png").unwrap();
        let changes: [(&[&str], &str); 5] = [
            (&["write", "--body", "# x\nnew"], "could not write Notes/Alpha.md"),
            (
                &["write", "--append", "--body", "more"],
                "could not write Notes/Alpha.md",
            ),
            (&["rename", "Renamed"], "Could not write the note."),
            (&["toggle", "2"], "could not write Notes/Alpha.md"),
            (&["attach", picture.to_str().unwrap()], "could not write Notes/Alpha.md"),
        ];
        for (args, refusal) in changes {
            if mode == 0o000 && args[0] == "toggle" {
                continue; // no line of it can be read, so none is there to toggle
            }
            let mut command = vec![args[0], "Notes/Alpha.md"];
            command.extend(&args[1..]);
            let (message, _) = w.json(&command).fails(1, "error");
            assert_eq!(message, refusal, "{mode:o} {command:?}");
            set(0o644);
            assert_eq!(w.read("Notes/Alpha.md"), ALPHA, "{mode:o} {command:?}");
            set(mode);
        }
        // Nothing is left beside it: no temporary file, no attachment.
        assert_eq!(names(&w.path("Notes")), ["Alpha.md", "Twin.md", "attachments"]);
        assert_eq!(names(&w.path("Notes/attachments")), [""; 0]);
        assert_eq!(fs::metadata(&note).unwrap().permissions().mode() & 0o777, mode);
    }
}

#[test]
fn a_note_with_a_very_long_name_is_saved() {
    // A file's name is at most 255 bytes; the save's temporary file beside
    // the note must not need a longer one than the note has.
    let Some(w) = World::seeded() else { return };
    let long = format!("Notes/{}.md", "n".repeat(245));
    w.write(&long, "# Long\n- [ ] one\n");
    w.json(["write", &long, "--append", "--body", "more"]).ok();
    assert_eq!(w.json(["toggle", &long, "2"]).ok()["text"], "- [x] one");
    assert_eq!(w.read(&long), "# Long\n- [x] one\nmore\n");
    // A new note needs no room at all beside its name.
    let title = "t".repeat(252);
    assert_eq!(w.json(["new", &title]).ok()["file"], format!("{title}.md"));
    assert_eq!(w.read(&format!("Notes/{title}.md")), format!("# {title}\n"));
    assert_eq!(names(&w.path("Notes")).len(), 4, "no temporary file is left");
}

// ---- export-pdf -----------------------------------------------------------------

#[test]
fn export_pdf_writes_a_pdf_beside_the_note_once() {
    let Some(w) = World::seeded() else { return };
    let pdf = w.path("Notes/Alpha.pdf");
    let exported = w.run(["export-pdf", "alpha"]);
    assert_eq!(
        exported.ok(),
        json!({"action": "export-pdf", "path": "Notes/Alpha.md", "pdf": pdf})
    );
    assert_eq!(exported.summary(), pdf.display().to_string());
    assert!(fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
    // Never over a file that is there.
    let (message, hint) = w.json(["export-pdf", "alpha"]).fails(2, "exists");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("Already exists (not overwritten): Alpha.pdf", "")
    );
    fs::remove_file(&pdf).unwrap();
    assert_eq!(w.text(["export-pdf", "alpha"]).out, format!("{}\n", pdf.display()));
    fs::remove_file(&pdf).unwrap();
    // The id of what it made is the PDF.
    assert_eq!(
        w.run(["--ids-only", "export-pdf", "Notes/Alpha.md"]).out,
        format!("{}\n", pdf.display())
    );
    assert_eq!(w.read("Notes/Alpha.md"), ALPHA);
    // It needs no display: this world starts none unless a window is.
    let headless = w.cmd(["export-pdf", "grid"]).run();
    assert_eq!(headless.ok()["pdf"], json!(w.path("Work/Grid.pdf")));
}

// ---- folders ----------------------------------------------------------------------

#[test]
fn new_folder_makes_a_directory() {
    let Some(w) = World::seeded() else { return };
    let made = w.run(["new-folder", " Archive ", "--in", "work"]);
    assert_eq!(made.ok(), json!({"action": "new-folder", "folder": "Work/Archive"}));
    assert_eq!(made.summary(), "Created folder Work/Archive");
    assert!(w.path("Work/Archive").is_dir());
    assert_eq!(w.json(["new-folder", "Top/level"]).ok()["folder"], "Toplevel");
    assert_eq!(w.json(["new-folder", "Rooted", "--in", "/"]).ok()["folder"], "Rooted");
    let (message, hint) = w.json(["new-folder", "Work"]).fails(2, "exists");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("folder \"Work\" already exists", "")
    );
    let (message, _) = w.json(["new-folder", "Archive", "--in", "Work"]).fails(2, "exists");
    assert_eq!(message, "folder \"Work/Archive\" already exists");
    w.json(["new-folder", "X", "--in", "Nope"]).fails(4, "not_found");
    for (name, message) in [
        ("  ", "the folder name is empty"),
        (".Old", "A name cannot start with a dot: those are skipped by sync."),
        (
            "Attachments",
            "That folder name holds note attachments: it is skipped by sync.",
        ),
        (
            "attachments",
            "That folder name holds note attachments: it is skipped by sync.",
        ),
    ] {
        let said = w.json(["new-folder", name]).fails(2, "usage");
        assert_eq!((said.0.as_str(), said.1.as_str()), (message, USAGE_HINT), "{name:?}");
    }
    assert!(!w.exists(".Old") && !w.exists("Attachments") && !w.exists("attachments"));
}

#[test]
fn rename_folder_renames_it_where_it_is() {
    let Some(w) = World::seeded() else { return };
    w.json(["new-folder", "Archive", "--in", "Work"]).ok();
    w.write("Work/Archive/Old.md", "# Old\n");
    let renamed = w.run(["rename-folder", "work/archive", "Kept"]);
    assert_eq!(
        renamed.ok(),
        json!({"action": "rename-folder", "folder": "Work/Kept", "from": "Work/Archive"})
    );
    assert_eq!(renamed.summary(), "Renamed folder Work/Archive to Work/Kept");
    assert_eq!(w.read("Work/Kept/Old.md"), "# Old\n");
    assert!(!w.exists("Work/Archive"));
    // Its own name is nothing to do.
    assert_eq!(
        w.json(["rename-folder", "Work/Kept", "Kept"]).ok()["folder"],
        "Work/Kept"
    );
    let (message, hint) = w.json(["rename-folder", "Notes", "Work"]).fails(2, "exists");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("A folder with that name already exists.", "")
    );
    for (name, code, exit, message) in [
        (
            ".Old",
            "usage",
            2,
            "A name cannot start with a dot: those are skipped by sync.",
        ),
        (
            "Attachments",
            "usage",
            2,
            "That folder name holds note attachments: it is skipped by sync.",
        ),
        (" ", "error", 1, "Name is empty."),
    ] {
        let said = w.json(["rename-folder", "Notes", name]).fails(exit, code);
        assert_eq!(said.0, message, "{name:?}");
    }
    let (message, _) = w.json(["rename-folder", "", "Everything"]).fails(1, "error");
    assert_eq!(message, "All Notes cannot be renamed.");
    w.json(["rename-folder", "Nope", "X"]).fails(4, "not_found");
    assert!(w.exists("Notes/Alpha.md"));
}

#[test]
fn delete_folder_moves_it_to_the_trash_when_told_to() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w.json(["delete-folder", "Work"]).fails(2, "usage");
    assert_eq!(
        message,
        "delete-folder asks before it acts, and cannot ask here (no terminal, or a machine output mode)"
    );
    assert_eq!(hint, "Pass --yes to go ahead.");
    // The root is refused before anything is asked.
    for args in [vec!["delete-folder", ""], vec!["delete-folder", "/", "--yes"]] {
        let (message, hint) = w.json(&args).fails(1, "error");
        assert_eq!(
            (message.as_str(), hint.as_str()),
            ("All Notes cannot be deleted.", ""),
            "{args:?}"
        );
    }
    w.json(["delete-folder", "Nope", "--yes"]).fails(4, "not_found");
    assert!(w.exists("Work/Beta.md"));
    let deleted = w.run(["delete-folder", "work", "--yes"]);
    assert_eq!(deleted.ok(), json!({"action": "delete-folder", "folder": "Work"}));
    assert_eq!(deleted.summary(), "Moved folder Work to the trash");
    assert!(!w.exists("Work"));
    assert!(w.trash().join("files/Work/Beta.md").is_file());
    assert!(w.trash().join("info/Work.trashinfo").is_file());
    assert_eq!(column(&w.json(["folders"]).ok(), "folder"), ["", "Notes"]);
}
