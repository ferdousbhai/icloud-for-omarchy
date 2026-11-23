//! The commands that only read: status, folders, list, read and search, how
//! a NOTE and a FOLDER argument are understood, and what of the vault's
//! files counts as a note, a title, an id.

use std::fs;

use serde_json::{Value, json};

use crate::world::{World, column, iso, strings};

fn note<'a>(list: &'a Value, path: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|n| n["path"] == path)
        .unwrap_or_else(|| panic!("no {path} in {list}"))
}

// ---- status ---------------------------------------------------------------

#[test]
fn status_reports_the_vault_the_sign_in_and_the_lock() {
    let Some(w) = World::seeded() else { return };
    let status = w.run(["status"]);
    assert_eq!(
        status.ok(),
        json!({
            "vault": w.vault,
            "cloned": true,
            "left_icloud": false,
            "title_mode": "in-body",
            "sync_tool": true,
            "sign_in_known": true,
            "signed_in": true,
            "apple_id": "someone@example.com",
            // No known end to the session.
            "sign_in_days_left": -1,
            "lock": {"held_by": null, "app_open": false},
            "notes": 7,
            "folders": 2,
            "flagged": {
                "conflict": ["Work/Tangle.md", "Work/Beta.md"],
                "new": ["Notes/Twin.md", "Twin.md"],
                "read-only": ["Work/Grid.md"],
            },
        })
    );
    assert_eq!(status.summary(), "7 notes, signed in as someone@example.com");
    let text = w.text(["status"]);
    assert_eq!(
        text.out,
        format!(
            "Vault:       {}\nSigned in:   yes, as someone@example.com\nSync tool:   {}\nLock:        free\n\
             Notes:       7 in 2 folders\n  conflict: 2\n  new: 2\n  read-only: 1\n",
            w.vault.display(),
            crate::world::stub_engine().display()
        )
    );
    assert_eq!((text.code, text.err.as_str()), (0, ""));
}

#[test]
fn status_counts_the_whole_days_the_sign_in_has_left() {
    let Some(w) = World::seeded() else { return };
    let days_left = |seconds: i64| {
        w.session.set_expires_in(seconds);
        w.json(["status"]).ok()["sign_in_days_left"].as_i64().unwrap()
    };
    assert_eq!(days_left(3 * 86400 + 43200), 3);
    assert_eq!(days_left(86400 - 600), 0);
    // An end that has passed says nothing: -1, as with no known end.
    assert_eq!(days_left(-600), -1);
}

#[test]
fn status_signed_out_prints_its_data_and_exits_1() {
    let Some(w) = World::seeded() else { return };
    w.session.set_signed_in(false);
    let status = w.run(["status"]);
    assert_eq!((status.code, status.err.as_str()), (1, ""));
    let data = status.data();
    assert_eq!(
        (
            &data["sign_in_known"],
            &data["signed_in"],
            &data["sign_in_days_left"],
            &data["notes"]
        ),
        (&json!(true), &json!(false), &json!(-2), &json!(7))
    );
    assert_eq!(status.summary(), "7 notes, signed out");
    let text = w.text(["status"]);
    assert_eq!(text.code, 1);
    assert!(
        text.out.contains("\nSigned in:   no (icloud-session sign-in)\n"),
        "{text:#?}"
    );
}

#[test]
fn status_without_icloud_session_says_the_sign_in_is_unknown() {
    let Some(mut w) = World::seeded() else { return };
    w.session.stop();
    let status = w.run(["status"]);
    let data = status.ok();
    assert_eq!(
        (
            &data["sign_in_known"],
            &data["signed_in"],
            &data["apple_id"],
            &data["sign_in_days_left"]
        ),
        (&json!(false), &Value::Null, &Value::Null, &json!(-2))
    );
    assert_eq!(status.summary(), "7 notes, sign-in unknown");
    let text = w.text(["status"]);
    assert_eq!(text.code, 0);
    assert!(
        text.out
            .contains("\nSigned in:   unknown (icloud-session is not available)\n"),
        "{text:#?}"
    );
    // No bus at all is the same.
    let no_bus = w
        .cmd(["status"])
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .run();
    assert_eq!(no_bus.ok()["sign_in_known"], false);
}

#[test]
fn status_needs_no_vault() {
    let Some(w) = World::new() else { return };
    let status = w.run(["status"]);
    let data = status.ok();
    assert_eq!(
        (
            &data["cloned"],
            &data["notes"],
            &data["folders"],
            &data["flagged"],
            &data["title_mode"]
        ),
        (&json!(false), &json!(0), &json!(0), &json!({}), &json!("in-body"))
    );
    assert_eq!(status.summary(), "not cloned, signed in as someone@example.com");
    assert!(!w.vault.exists(), "status makes no vault");
    let text = w.text(["status"]).out;
    assert!(
        text.starts_with(&format!("Vault:       {} (not cloned)\n", w.vault.display())),
        "{text}"
    );
    assert!(text.ends_with("Notes:       0 in 0 folders\n"), "{text}");
    assert_eq!(w.run(["status", "--count"]).out, "0\n");
}

#[test]
fn status_without_the_engine_knows_no_tracked_note() {
    let Some(w) = World::seeded() else { return };
    let missing = w.scratch().join("no-engine");
    let status = w.cmd(["status"]).env("ICLOUD_NOTES_SYNC_BIN", &missing).run();
    let data = status.ok();
    assert_eq!(
        (&data["sync_tool"], &data["title_mode"], &data["notes"]),
        (&json!(false), &json!("in-body"), &json!(7))
    );
    // Nothing says which notes the engine tracks: a note with an id looks
    // like another vault's, one without is new.
    let mut foreign = strings(&data["flagged"]["foreign-id"]);
    foreign.sort();
    assert_eq!(
        foreign,
        [
            "Notes/Alpha.md",
            "Work/Beta.md",
            "Work/Grid.md",
            "Work/Pics.md",
            "Work/Tangle.md"
        ]
    );
    assert_eq!(strings(&data["flagged"]["new"]), ["Notes/Twin.md", "Twin.md"]);
    assert!(data["flagged"].get("read-only").is_none());
    let text = w.cmd(["status"]).styled().env("ICLOUD_NOTES_SYNC_BIN", &missing).run();
    assert!(text.out.contains("\nSync tool:   missing\n"), "{text:#?}");
    // An engine that gives no answer about the vault leaves it as unknown.
    let silent = w
        .cmd(["list", "--flag", "foreign-id", "--count"])
        .env("ICLOUD_NOTES_SYNC_STUB_VAULT_INFO_FAIL", "1")
        .run();
    assert_eq!(silent.out, "5\n");
    // A file that is there but cannot be run is no engine either.
    fs::write(&missing, "#!/bin/sh\n").unwrap();
    assert_eq!(
        w.cmd(["status"]).env("ICLOUD_NOTES_SYNC_BIN", &missing).run().ok()["sync_tool"],
        false
    );
}

// ---- folders --------------------------------------------------------------

#[test]
fn folders_lists_the_root_then_the_default_folder_then_the_rest() {
    let Some(w) = World::seeded() else { return };
    let folders = w.run(["folders"]);
    assert_eq!(
        folders.ok(),
        json!([
            {"folder": "", "name": "All Notes", "notes": 7, "default": false},
            {"folder": "Notes", "name": "Notes", "notes": 2, "default": true},
            {"folder": "Work", "name": "Work", "notes": 4, "default": false},
        ])
    );
    assert_eq!(folders.summary(), "2 folders");
}

#[test]
fn folders_sort_by_name_with_numbers_as_numbers_and_children_after_their_parent() {
    let Some(w) = World::new() else { return };
    w.state("in-body", &[], Some("Notes"));
    for folder in [
        "apple",
        "Banana",
        "10 Later",
        "2 Sooner",
        "Notes/Sub",
        "Notes",
        "apple/zeta",
        "apple/Alpha",
        "Élan",
    ] {
        fs::create_dir_all(w.path(folder)).unwrap();
    }
    w.write("apple/zeta/Deep.md", "# Deep\n");
    w.write("apple/One.md", "# One\n");
    w.write("Root.md", "# Root\n");
    // Not folders: hidden directories, and where attachments are kept.
    w.write(".hidden/Note.md", "# Hidden\n");
    w.write("apple/attachments/a.md", "# Attached\n");
    w.write("apple/Attachments/b.md", "# Attached\n");
    let folders = w.run(["folders"]).ok();
    assert_eq!(
        column(&folders, "folder"),
        [
            "",
            "Notes",
            "Notes/Sub",
            "2 Sooner",
            "10 Later",
            "apple",
            "apple/Alpha",
            "apple/zeta",
            "Banana",
            "Élan"
        ]
    );
    assert_eq!(
        column(&folders, "name"),
        [
            "All Notes",
            "Notes",
            "Sub",
            "2 Sooner",
            "10 Later",
            "apple",
            "Alpha",
            "zeta",
            "Banana",
            "Élan"
        ]
    );
    // A note counts toward every folder above it, the root included.
    let counts: Vec<i64> = folders
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["notes"].as_i64().unwrap())
        .collect();
    assert_eq!(counts, [3, 0, 0, 0, 0, 2, 0, 1, 0, 0]);
    assert_eq!(w.run(["folders"]).summary(), "9 folders");
    assert_eq!(w.run(["folders", "--count"]).out, "9\n");
    assert_eq!(w.run(["list", "--count"]).out, "3\n");
    assert_eq!(w.json(["status"]).ok()["folders"], 9);
}

// ---- list -----------------------------------------------------------------

#[test]
fn list_describes_every_note_newest_first() {
    let Some(w) = World::seeded() else { return };
    let list = w.run(["list"]);
    let data = list.ok();
    assert_eq!(column(&data, "path"), World::SEEDED);
    let alpha = note(&data, "Notes/Alpha.md");
    let modified = fs::metadata(w.path("Notes/Alpha.md")).unwrap().modified().unwrap();
    let ms = modified.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    assert_eq!(
        alpha,
        &json!({
            "path": "Notes/Alpha.md",
            "folder": "Notes",
            "file": "Alpha.md",
            "title": "Alpha",
            "snippet": "milk",
            "modified": iso(ms),
            "modified_ms": ms,
            "id": "id-a",
            "flags": [],
        })
    );
    // A note in the vault's root, with no id.
    let twin = note(&data, "Twin.md");
    assert_eq!(
        (&twin["folder"], &twin["id"], &twin["flags"]),
        (&json!(""), &Value::Null, &json!(["new"]))
    );
    // Text: path, title, time and flags, tab-separated.
    let lines: Vec<String> = w.text(["list"]).out.lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 7);
    assert_eq!(lines[6], format!("Notes/Alpha.md\tAlpha\t{}\t", iso(ms)));
    assert_eq!(lines[4].split('\t').collect::<Vec<_>>()[3], "read-only");
}

#[test]
fn list_takes_a_folder_with_what_is_below_it() {
    let Some(w) = World::seeded() else { return };
    w.write("Work/Child/Alpha.md", "# Nested Alpha\n");
    w.set_age("Work/Child/Alpha.md", 1000);
    // Markdown kept as an attachment is no note.
    w.write("Work/attachments/document.md", "# Not a note\n");
    w.write("Work/Child/Attachments/other.md", "# Not a note\n");
    w.write("Work/.git/config.md", "# Not a note\n");
    let work = [
        "Work/Tangle.md",
        "Work/Pics.md",
        "Work/Grid.md",
        "Work/Beta.md",
        "Work/Child/Alpha.md",
    ];
    for folder in ["Work", "work", "/Work/", " WORK "] {
        assert_eq!(
            column(&w.json(["list", "--folder", folder]).ok(), "path"),
            work,
            "{folder:?}"
        );
    }
    assert_eq!(
        column(&w.json(["list", "--folder", "Work/Child"]).ok(), "path"),
        ["Work/Child/Alpha.md"]
    );
    let nested = w.json(["list", "--folder", "work/child"]).ok();
    assert_eq!(
        (&nested[0]["folder"], &nested[0]["file"]),
        (&json!("Work/Child"), &json!("Alpha.md"))
    );
    // The root, by either name, is the whole vault, each note once.
    for root in ["", "/"] {
        assert_eq!(
            w.json(["list", "--folder", root]).ok().as_array().unwrap().len(),
            8,
            "{root:?}"
        );
    }
    assert_eq!(w.run(["list"]).summary(), "8 notes");
    assert_eq!(w.json(["list", "--folder", "Work/Child"]).summary(), "1 note");
    let (message, hint) = w.json(["list", "--folder", "Nope"]).fails("not_found");
    assert_eq!(message, "no folder \"Nope\"");
    assert_eq!(
        hint,
        "`icloud-notes folders` lists them; `icloud-notes new-folder` makes one."
    );
}

#[test]
fn list_filters_by_flag() {
    let Some(w) = World::seeded() else { return };
    assert_eq!(
        column(&w.json(["list", "--flag", "conflict"]).ok(), "path"),
        ["Work/Tangle.md", "Work/Beta.md"]
    );
    assert_eq!(
        column(&w.json(["list", "--flag", "read-only"]).ok(), "path"),
        ["Work/Grid.md"]
    );
    assert_eq!(
        column(&w.json(["list", "--flag", "new", "--folder", "Notes"]).ok(), "path"),
        ["Notes/Twin.md"]
    );
    // A flag nobody carries, or that does not exist, matches nothing.
    assert_eq!(w.json(["list", "--flag", "tables"]).ok(), json!([]));
    assert_eq!(w.json(["list", "--flag", "CONFLICT"]).ok(), json!([]));
}

#[test]
fn notes_are_flagged_for_what_sync_would_make_of_them() {
    let Some(w) = World::new() else { return };
    w.state(
        "in-body",
        &[
            &["id-t", "Table.md"],
            &["id-m", "NoId.md"],
            &["id-r", "Locked.md", "has a drawing"],
            &["id-c", "Both.md"],
        ],
        None,
    );
    let table = "| a | b |\n| - | - |\n";
    w.write("Table.md", format!("---\napple-note-id: id-t\n---\n# Table\n{table}"));
    // One line with a pipe is prose, and so are two short ones.
    w.write("Pipe.md", "# Pipe\nthis | that\nplain\na|\n|b\n");
    // Tracked, but its id is gone from the file.
    w.write("NoId.md", "# NoId\n");
    // Carries an id the vault's state does not know.
    w.write("Foreign.md", "---\napple-note-id: id-x\n---\n# Foreign\n");
    w.write("Locked.md", "---\napple-note-id: id-r\n---\n# Locked\n");
    // Conflict, then tables, then read-only: the order the flags come in.
    w.write(
        "Both.md",
        format!("---\napple-note-id: id-c\n---\n# Both\n{table}<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n"),
    );
    // A copy Syncthing made, of a tracked note and of none.
    w.write(
        "Table.sync-conflict-20261114-101500-ABCDEF7.md",
        format!("---\napple-note-id: id-t\n---\n# Table\n{table}"),
    );
    w.write("Plain.sync-conflict-20261114-101500-ABCDEF7.md", "# Plain copy\n");
    // Not Syncthing's shape (a device id of 6 characters): an ordinary new note.
    w.write("Near.sync-conflict-20261114-101500-ABCDEF.md", "# Near\n");
    // A lone ======= is a rule, but it still counts as a marker line.
    w.write("Rule.md", "# Rule\n=======\n");
    // Over 2 MiB a file is listed, but not read through for markers or tables.
    let filler = "x".repeat(2 * 1024 * 1024);
    w.write("Huge.md", format!("# Huge\n{table}<<<<<<< local\n{filler}\n"));
    let list = w.json(["list"]).ok();
    let flags = |path: &str| strings(&note(&list, path)["flags"]);
    assert_eq!(flags("Huge.md"), ["new"]);
    assert_eq!(flags("Table.md"), ["tables"]);
    assert_eq!(flags("Pipe.md"), ["new"]);
    assert_eq!(flags("NoId.md"), ["missing-id"]);
    assert_eq!(flags("Foreign.md"), ["foreign-id"]);
    assert_eq!(flags("Locked.md"), ["read-only"]);
    assert_eq!(flags("Both.md"), ["conflict", "tables"]);
    assert_eq!(
        flags("Table.sync-conflict-20261114-101500-ABCDEF7.md"),
        ["tables", "conflict-copy"]
    );
    assert_eq!(
        flags("Plain.sync-conflict-20261114-101500-ABCDEF7.md"),
        ["conflict-copy"]
    );
    assert_eq!(flags("Near.sync-conflict-20261114-101500-ABCDEF.md"), ["new"]);
    assert_eq!(flags("Rule.md"), ["conflict", "new"]);
    assert_eq!(note(&list, "Foreign.md")["id"], "id-x");
    assert_eq!(note(&list, "NoId.md")["id"], Value::Null);
    let status = w.json(["status"]).ok();
    let flagged = crate::world::sorted_keys(&status["flagged"]);
    assert_eq!(
        flagged,
        [
            "conflict",
            "conflict-copy",
            "foreign-id",
            "missing-id",
            "new",
            "read-only",
            "tables"
        ]
    );
    let text = w.text(["list", "--flag", "tables", "--folder", ""]).out;
    assert!(
        text.lines()
            .any(|l| l.starts_with("Both.md\tBoth\t") && l.ends_with("\tconflict,tables")),
        "{text}"
    );
}

// ---- read -----------------------------------------------------------------

#[test]
fn read_gives_the_note_its_body_and_what_hangs_on_it() {
    let Some(w) = World::seeded() else { return };
    let read = w.run(["read", "Notes/Alpha.md"]);
    let listed = note(&w.json(["list"]).ok(), "Notes/Alpha.md").clone();
    assert_eq!(
        read.ok(),
        json!({
            "path": "Notes/Alpha.md",
            "folder": "Notes",
            "file": "Alpha.md",
            "title": "Alpha",
            "modified": listed["modified"],
            "modified_ms": listed["modified_ms"],
            "id": "id-a",
            "flags": [],
            "read_only": null,
            "body": "# Alpha\n- [ ] milk\n- eggs\nplain line\n",
            "attachments": [],
            "conflicts": [],
            "conflicts_unreadable": false,
            "has_synced_copy": false,
        })
    );
    assert_eq!(read.summary(), "Alpha (Notes/Alpha.md)");
    // --raw adds the whole file, ID block included.
    let raw = w.json(["read", "alpha", "--raw"]).ok();
    assert_eq!(
        raw["text"],
        "---\napple-note-id: id-a\n---\n# Alpha\n- [ ] milk\n- eggs\nplain line\n"
    );
    assert_eq!(raw["body"], "# Alpha\n- [ ] milk\n- eggs\nplain line\n");
    assert_eq!(w.text(["read", "alpha", "--raw"]).out, raw["text"].as_str().unwrap());
    assert_eq!(w.json(["read", "grid"]).ok()["read_only"], "is too large");
    assert_eq!(w.json(["read", "grid"]).ok()["flags"], json!(["read-only"]));
}

#[test]
fn read_lays_out_the_conflict_blocks() {
    let Some(w) = World::seeded() else { return };
    let beta = w.json(["read", "beta"]).ok();
    assert_eq!(
        beta["conflicts"],
        json!([
            {"local": ["mine 1"], "remote": ["theirs 1"], "before": ["# Beta", "shared"], "after": ["middle"]},
            {"local": ["mine 2"], "remote": ["theirs 2"], "before": ["middle"], "after": []},
        ])
    );
    assert_eq!(
        (&beta["conflicts_unreadable"], &beta["has_synced_copy"]),
        (&json!(false), &json!(false))
    );
    // Markers nested in one another cannot be read as blocks; the last
    // synced text is there to go back to.
    let tangle = w.json(["read", "tangle"]).ok();
    assert_eq!(
        (
            &tangle["conflicts"],
            &tangle["conflicts_unreadable"],
            &tangle["has_synced_copy"]
        ),
        (&json!([]), &json!(true), &json!(true))
    );
    // A block with its base section, markers indented, and at most two
    // lines of context on each side.
    w.write(
        "Work/Three.md",
        "# Three\none\ntwo\nthree\n  <<<<<<< local\nL1\nL2\n||||||| base\nB\n=======\n>>>>>>> remote  \nfour\nfive\nsix\n",
    );
    let three = w.json(["read", "three"]).ok();
    assert_eq!(
        three["conflicts"],
        json!([{"local": ["L1", "L2"], "remote": [], "before": ["two", "three"], "after": ["four", "five"]}])
    );
    // A lone ======= is a rule in prose, not a conflict the picker cannot read.
    w.write("Work/Rule.md", "# Rule\n=======\n");
    let rule = w.json(["read", "rule"]).ok();
    assert_eq!(
        (&rule["conflicts"], &rule["conflicts_unreadable"]),
        (&json!([]), &json!(false))
    );
}

#[test]
fn read_lists_the_attachments_the_text_links_and_the_folder_holds() {
    let Some(w) = World::seeded() else { return };
    for name in [
        "pic.png",
        "scan 1.JPG",
        "plan (a).pdf",
        "notes.txt",
        "draw.svg",
        "clip.webp",
    ] {
        w.write(&format!("Notes/attachments/{name}"), "x");
    }
    w.write("outside.png", "x");
    w.write(
        "Notes/Links.md",
        "# Links\n![pic.png](attachments/pic.png) and ![again](attachments/pic.png)\n\
         ![scan](attachments/scan%201.JPG)\n[plan](attachments/plan%20(a).pdf)\n[notes](attachments/notes.txt)\n\
         ![d](attachments/draw.svg)![c](attachments/clip.webp)\n\
         [gone](attachments/missing.png)\n[out](attachments/../../outside.png)\n[web](https://example.com/attachments/pic.png)\n",
    );
    let read = w.json(["read", "links"]).ok();
    let attachments = read["attachments"].as_array().unwrap();
    // Each file once, in the order the text links them; only files that
    // are really in the note's attachments folder.
    assert_eq!(
        column(&read["attachments"], "name"),
        [
            "pic.png",
            "scan 1.JPG",
            "plan (a).pdf",
            "notes.txt",
            "draw.svg",
            "clip.webp"
        ]
    );
    let images: Vec<bool> = attachments.iter().map(|a| a["image"].as_bool().unwrap()).collect();
    assert_eq!(images, [true, true, false, false, true, true]);
    assert_eq!(attachments[1]["path"], json!(w.path("Notes/attachments/scan 1.JPG")));
    // A note in the vault's root keeps its attachments in the root's folder.
    w.write("attachments/root.gif", "x");
    w.write("Rooted.md", "# Rooted\n![r](attachments/root.gif)\n");
    assert_eq!(
        w.json(["read", "rooted"]).ok()["attachments"],
        json!([{"name": "root.gif", "path": w.path("attachments/root.gif"), "image": true}])
    );
}

// ---- NOTE and FOLDER ---------------------------------------------------------

#[test]
fn a_note_is_named_by_path_id_or_a_title_only_it_has() {
    let Some(w) = World::seeded() else { return };
    let absolute = w.path("Notes/Alpha.md");
    for name in [
        "Notes/Alpha.md",
        "Notes/Alpha",
        "./Notes/Alpha.md",
        "  Notes/Alpha.md  ",
        absolute.to_str().unwrap(),
        "id-a",
        "Alpha",
        "ALPHA",
        " alpha ",
    ] {
        assert_eq!(w.run(["--ids-only", "read", name]).out, "Notes/Alpha.md\n", "{name:?}");
    }
    // A title is not the file's name once they differ; both find the note.
    w.write("Work/File name.md", "# Shown title\n");
    assert_eq!(w.run(["--ids-only", "read", "shown TITLE"]).out, "Work/File name.md\n");
    assert_eq!(w.run(["--ids-only", "read", "file NAME"]).out, "Work/File name.md\n");
    // A path is exact: its case matters, and only the title may differ in it.
    w.json(["read", "notes/alpha.md"]).fails("not_found");
    w.json(["read", "ID-A"]).fails("not_found");
    let (message, hint) = w.json(["read", "same title"]).fails("ambiguous");
    assert_eq!(message, "\"same title\" names 2 notes: Notes/Twin.md, Twin.md");
    assert_eq!(hint, "Name the note by its path.");
    // A path, with or without .md, is looked for before any title.
    assert_eq!(w.run(["--ids-only", "read", "Twin"]).out, "Twin.md\n");
    assert_eq!(w.run(["--ids-only", "read", "Twin.md"]).out, "Twin.md\n");
    assert_eq!(w.run(["--ids-only", "read", "Notes/Twin"]).out, "Notes/Twin.md\n");
    // Two notes carrying one id: the id names neither.
    w.write("Work/Copy.md", "---\napple-note-id: id-a\n---\n# Copy\n");
    let (message, _) = w.json(["read", "id-a"]).fails("ambiguous");
    assert!(message.starts_with("\"id-a\" names 2 notes: "), "{message}");
    let (message, _) = w.json(["read", "  no such  "]).fails("not_found");
    assert_eq!(message, "no note \"  no such  \"");
}

// ---- search ---------------------------------------------------------------

#[test]
fn search_finds_text_and_titles_whatever_their_case() {
    let Some(w) = World::seeded() else { return };
    let found = w.run(["search", "  EGGS "]);
    assert_eq!(
        found.ok(),
        json!([{"path": "Notes/Alpha.md", "folder": "Notes", "file": "Alpha.md", "title": "Alpha", "snippet": "eggs"}])
    );
    // The query as trimmed, in the summary.
    assert_eq!(found.summary(), "1 note matching \"EGGS\"");
    // The snippet is the first line holding the text, as prose.
    w.write(
        "Work/Deep.md",
        "# Deep\nfirst line\n- [x] the **needle** in [a link](https://example.com)\nneedle again\n",
    );
    assert_eq!(w.json(["search", "needle"]).ok()[0]["snippet"], "the needle in a link");
    // The whole file is searched, its ID block too.
    let by_id = w.json(["search", "id-g"]).ok();
    assert_eq!(
        (&by_id[0]["path"], &by_id[0]["snippet"]),
        (&json!("Work/Grid.md"), &json!("apple-note-id: id-g"))
    );
    // Titles are searched as well as text.
    let mut twins = column(&w.json(["search", "same TITLE"]).ok(), "path");
    twins.sort();
    assert_eq!(twins, ["Notes/Twin.md", "Twin.md"]);
    assert_eq!(
        w.json(["search", "same title"]).summary(),
        "2 notes matching \"same title\""
    );
    assert_eq!(w.json(["search", "no such text"]).ok(), json!([]));
}

#[test]
fn search_wants_two_characters() {
    let Some(w) = World::seeded() else { return };
    for query in ["e", " e ", ""] {
        let (message, _) = w.json(["search", query]).fails("usage");
        assert_eq!(message, "search wants at least 2 characters", "{query:?}");
    }
    assert_eq!(w.run(["search", "eg", "--count"]).out, "1\n");
    // Counted as the app counts text: one emoji is two units, one accented letter one.
    w.write("Notes/Emoji.md", "# Emoji\nturtle 🐢 here\né\n");
    assert_eq!(w.run(["search", "🐢", "--ids-only"]).out, "Notes/Emoji.md\n");
    w.json(["search", "é"]).fails("usage");
}

#[test]
fn search_stops_at_a_hundred_notes() {
    let Some(w) = World::new() else { return };
    w.state("in-body", &[], None);
    for i in 0..105 {
        w.write(&format!("N{i}.md"), "# Same\nhaystack\n");
    }
    assert_eq!(w.run(["search", "haystack", "--count"]).out, "100\n");
    assert_eq!(
        w.json(["search", "haystack"]).summary(),
        "100 notes matching \"haystack\""
    );
    assert_eq!(w.run(["list", "--count"]).out, "105\n");
}

// ---- the files --------------------------------------------------------------

#[test]
fn the_id_block_is_the_fenced_head_of_the_file() {
    let Some(w) = World::new() else { return };
    w.state("in-body", &[], None);
    let read = |path: &str, text: &str| {
        w.write(path, text);
        let data = w.json(["read", path]).ok();
        (data["id"].clone(), data["body"].as_str().unwrap().to_string())
    };
    // The blank lines after the closing fence belong to the block.
    assert_eq!(
        read("A.md", "---\napple-note-id: id-1\n---\n\n\n# A\n\nbody\n"),
        (json!("id-1"), "# A\n\nbody\n".into())
    );
    // Fences may carry spaces; other keys are kept out of the body too.
    assert_eq!(
        read("B.md", "--- \ntags: [x]\napple-note-id:   id-2  \n ---\n# B\n"),
        (json!("id-2"), "# B\n".into())
    );
    // Quotes come off when they match.
    assert_eq!(
        read("C.md", "---\napple-note-id: \"id 3\"\n---\n# C\n").0,
        json!("id 3")
    );
    assert_eq!(read("D.md", "---\napple-note-id: 'id-4'\n---\n# D\n").0, json!("id-4"));
    assert_eq!(
        read("E.md", "---\napple-note-id: \"id-5'\n---\n# E\n").0,
        json!("\"id-5'")
    );
    // Only a top-level scalar is the id: not a nested key, not a block.
    assert_eq!(
        read("F.md", "---\nmeta:\n  apple-note-id: nested\n---\n# F\n").0,
        Value::Null
    );
    assert_eq!(
        read("G.md", "---\napple-note-id: |\n  block\n---\n# G\n").0,
        Value::Null
    );
    assert_eq!(read("H.md", "---\napple-note-id: >\n---\n# H\n").0, Value::Null);
    // The first one counts.
    assert_eq!(
        read("I.md", "---\napple-note-id: first\napple-note-id: second\n---\n# I\n").0,
        json!("first")
    );
    // A fence never closed is text, and so is one that is not the first line.
    assert_eq!(
        read("J.md", "---\napple-note-id: id-j\n# J\n"),
        (Value::Null, "---\napple-note-id: id-j\n# J\n".into())
    );
    assert_eq!(read("K.md", "\n---\napple-note-id: id-k\n---\n# K\n").0, Value::Null);
    // The block alone is a note with no body.
    assert_eq!(
        read("L.md", "---\napple-note-id: id-l\n---\n"),
        (json!("id-l"), String::new())
    );
}

#[test]
fn files_are_read_as_utf8_text() {
    let Some(w) = World::seeded() else { return };
    // Windows line ends and a byte order mark are not part of the text.
    w.write("Notes/Crlf.md", "\u{feff}# Crlf\r\nfirst body line\r\nsecond\r\n");
    let read = w.json(["read", "Notes/Crlf.md", "--raw"]).ok();
    assert_eq!(read["body"], "# Crlf\nfirst body line\nsecond\n");
    assert_eq!(read["text"], "# Crlf\nfirst body line\nsecond\n");
    assert_eq!(read["title"], "Crlf");
    // A write gives the file back as it was read, with the change.
    let wrote = w.json(["write", "Notes/Crlf.md", "--append", "--body", "third"]).ok();
    assert_eq!(wrote["changed"], true);
    assert_eq!(
        fs::read(w.path("Notes/Crlf.md")).unwrap(),
        b"# Crlf\nfirst body line\nsecond\nthird\n"
    );
    // Bytes that are no UTF-8 read as the replacement character.
    w.write("Notes/Latin.md", b"# Latin\ncaf\xe9\n");
    assert_eq!(
        w.json(["read", "Notes/Latin.md"]).ok()["body"],
        "# Latin\ncaf\u{fffd}\n"
    );
}

#[test]
fn an_in_body_vault_takes_titles_from_the_first_line_of_prose() {
    let Some(w) = World::new() else { return };
    w.state("in-body", &[], None);
    let preview = |text: &str| {
        w.write("File.md", text);
        let n = w.json(["list"]).ok()[0].clone();
        (
            n["title"].as_str().unwrap().to_string(),
            n["snippet"].as_str().unwrap().to_string(),
        )
    };
    let both = |title: &str, snippet: &str| (title.to_string(), snippet.to_string());
    assert_eq!(preview("# Heading\nnext line\nthird\n"), both("Heading", "next line"));
    assert_eq!(
        preview("\n\n   ## > Quoted **bold** title  \n\n- [ ] a `task`\n"),
        both("Quoted bold title", "a task")
    );
    assert_eq!(preview("12. numbered\n3) paren\n"), both("numbered", "paren"));
    assert_eq!(
        preview("* [x] done ~~struck~~ __under__ _it_\n+ plus\n"),
        both("done struck under it", "plus")
    );
    assert_eq!(
        preview("![image alt](attachments/a.png)\n[link text](https://example.com) after\n"),
        both("image alt", "link text after")
    );
    // Marker lines are no prose; a line of marks alone is empty.
    assert_eq!(
        preview("<<<<<<< local\nmine\n=======\ntheirs\n>>>>>>> remote\n"),
        both("mine", "theirs")
    );
    assert_eq!(preview("#\n**\nreal\n"), both("real", ""));
    // Nothing to take a title from: the file's name.
    assert_eq!(preview(""), both("File", ""));
    assert_eq!(preview("---\napple-note-id: x\n---\n"), both("File", ""));
    // Not a list marker without its space; a checkbox only after one.
    assert_eq!(preview("-dash\n[x] box\n"), both("-dash", "box"));
    // The snippet is cut at 140, counted as the app counts text.
    let long = "é".repeat(200);
    assert_eq!(preview(&format!("# T\n{long}\n")), both("T", &"é".repeat(140)));
    let (title, _) = preview(&format!("# {long}\nrest\n"));
    assert_eq!(title, long, "a title is not cut");
}

#[test]
fn a_filename_vault_takes_titles_from_the_file_names() {
    let Some(w) = World::new() else { return };
    w.state("filename", &[&["id-s", "Shopping list.md"]], None);
    w.write(
        "Shopping list.md",
        "---\napple-note-id: id-s\n---\n# Not the title\nmilk\n",
    );
    let status = w.json(["status"]).ok();
    assert_eq!(status["title_mode"], "filename");
    let list = w.json(["list"]).ok();
    assert_eq!(
        (&list[0]["title"], &list[0]["snippet"]),
        (&json!("Shopping list"), &json!("Not the title"))
    );
    assert_eq!(
        w.json(["read", "shopping LIST"]).summary(),
        "Shopping list (Shopping list.md)"
    );
    w.json(["read", "Not the title"]).fails("not_found");
    assert_eq!(w.json(["search", "shopping"]).ok()[0]["title"], "Shopping list");
}

#[test]
fn reading_needs_a_cloned_vault() {
    let Some(w) = World::new() else { return };
    let hint = "Run `icloud-notes clone` (signed in to icloud-session) first.";
    for args in [
        vec!["folders"],
        vec!["list"],
        vec!["read", "x"],
        vec!["search", "xy"],
        vec!["new", "T"],
        vec!["new-folder", "F"],
    ] {
        let said = w.json(&args).fails("not_cloned");
        assert_eq!(
            (said.0, said.1.as_str()),
            (format!("no notes cloned at {}", w.vault.display()), hint),
            "{args:?}"
        );
    }
    // A directory with notes in it but no engine state is not cloned either.
    w.write("Notes/Loose.md", "# Loose\n");
    w.json(["list"]).fails("not_cloned");
    w.text(["list"])
        .fails_in_text(&format!("no notes cloned at {}", w.vault.display()), hint);
    // The state of the engine's older layout counts.
    w.write(
        ".icloud-md/state.json",
        r#"{"layoutVersion":4,"titleMode":"in-body","notes":{}}"#,
    );
    assert_eq!(column(&w.json(["list"]).ok(), "path"), ["Notes/Loose.md"]);
}
