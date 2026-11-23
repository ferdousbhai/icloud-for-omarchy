//! The output contract: the envelope when piped, the flags that pick another
//! mode and how they combine, what `--ids-only` and `--count` print for each
//! kind of result, and where a failure goes.

use serde_json::json;

use crate::world::{World, column};

#[test]
fn piped_output_is_the_envelope_and_json_asks_for_the_same() {
    let Some(w) = World::seeded() else { return };
    let piped = w.run(["list"]);
    let data = piped.ok();
    assert_eq!(column(&data, "path"), World::SEEDED);
    assert_eq!(piped.summary(), "7 notes");
    assert_eq!(w.json(["list"]).doc(), piped.doc());
    // Never a `meta` key, and the document ends with one newline.
    assert!(piped.out.ends_with("\n}\n"), "{piped:#?}");
}

#[test]
fn quiet_prints_the_data_alone() {
    let Some(w) = World::seeded() else { return };
    let quiet = w.run(["--quiet", "list"]);
    assert_eq!((quiet.code, quiet.err.as_str()), (0, ""));
    assert_eq!(quiet.doc(), w.run(["list"]).data());
    assert_eq!(w.run(["folders", "--quiet"]).doc(), w.run(["folders"]).data());
    // An object too: a change's result.
    let made = w.run(["--quiet", "new", "Quiet"]);
    assert_eq!(
        made.doc(),
        json!({"action": "new", "path": "Notes/Quiet.md", "folder": "Notes", "file": "Quiet.md", "title": "Quiet"})
    );
}

#[test]
fn the_strongest_flag_wins() {
    let Some(w) = World::seeded() else { return };
    let ids = w.run(["list", "--ids-only"]);
    let envelope = w.run(["list"]);
    let quiet = w.run(["--quiet", "list"]);
    let text = w.run(["--styled", "list"]);
    assert_eq!(ids.out, World::SEEDED.join("\n") + "\n");
    assert_eq!(
        w.run(["--json", "--count", "--ids-only", "--quiet", "--styled", "list"])
            .out,
        ids.out
    );
    assert_eq!(w.run(["--quiet", "--count", "--json", "--styled", "list"]).out, "7\n");
    assert_eq!(w.run(["--json", "--quiet", "--styled", "list"]).out, quiet.out);
    assert_eq!(w.run(["--styled", "--json", "list"]).doc(), envelope.doc());
    assert!(text.out.starts_with("Notes/Twin.md\tSame title\t"), "{text:#?}");
    assert_eq!(text.err, "");
}

#[test]
fn ids_only_prints_what_each_command_is_about() {
    let Some(w) = World::seeded() else { return };
    let ids = |args: &[&str]| {
        let r = w.run(args);
        assert_eq!((r.code, r.err.as_str()), (0, ""), "{r:#?}");
        r.out
    };
    assert_eq!(ids(&["--ids-only", "search", "eggs"]), "Notes/Alpha.md\n");
    assert_eq!(ids(&["--ids-only", "read", "alpha"]), "Notes/Alpha.md\n");
    // The folders, not the vault root.
    assert_eq!(ids(&["--ids-only", "folders"]), "Notes\nWork\n");
    // A change: the note or the folder it touched.
    assert_eq!(ids(&["--ids-only", "new", "Made"]), "Notes/Made.md\n");
    assert_eq!(ids(&["--ids-only", "toggle", "alpha", "2"]), "Notes/Alpha.md\n");
    assert_eq!(ids(&["--ids-only", "move", "made", "Work"]), "Work/Made.md\n");
    assert_eq!(ids(&["--ids-only", "new-folder", "Box", "--in", "Work"]), "Work/Box\n");
    assert_eq!(ids(&["--ids-only", "delete-folder", "Work/Box", "--yes"]), "Work/Box\n");
    assert_eq!(
        ids(&["--ids-only", "delete", "Work/Made.md", "--yes"]),
        "Work/Made.md\n"
    );
    // Nothing found is nothing printed.
    assert_eq!(ids(&["--ids-only", "search", "no such text"]), "");
}

#[test]
fn ids_only_on_a_result_without_ids_prints_the_data_and_says_so() {
    let Some(w) = World::seeded() else { return };
    let status = w.run(["status", "--ids-only"]);
    assert_eq!(status.code, 0);
    assert_eq!(
        status.err,
        "note: this command has no ids to list; printing its result instead\n"
    );
    assert_eq!(status.doc(), w.run(["status", "--quiet"]).doc());
    let sync = w.run(["sync", "--ids-only"]);
    assert_eq!(
        sync.err,
        "note: this command has no ids to list; printing its result instead\n"
    );
    assert_eq!(sync.doc()["ok"], true);
}

#[test]
fn count_prints_one_number() {
    let Some(w) = World::seeded() else { return };
    let count = |args: &[&str]| {
        let r = w.run(args);
        assert_eq!((r.code, r.err.as_str()), (0, ""), "{r:#?}");
        r.out
    };
    assert_eq!(count(&["--count", "list"]), "7\n");
    assert_eq!(count(&["--count", "list", "--folder", "Work"]), "4\n");
    assert_eq!(count(&["--count", "list", "--flag", "no-such-flag"]), "0\n");
    assert_eq!(count(&["--count", "search", "eggs"]), "1\n");
    assert_eq!(count(&["--count", "search", "no such text"]), "0\n");
    // The folders without the root; the notes of the vault.
    assert_eq!(count(&["--count", "folders"]), "2\n");
    assert_eq!(count(&["--count", "status"]), "7\n");
    // One note read, one note changed.
    assert_eq!(count(&["--count", "read", "alpha"]), "1\n");
    assert_eq!(count(&["--count", "toggle", "alpha", "2"]), "1\n");
    // A sync: its runs (push, then pull); a pull alone is one.
    assert_eq!(count(&["--count", "sync"]), "2\n");
    assert_eq!(count(&["--count", "pull"]), "1\n");
}

#[test]
fn text_mode_prints_the_commands_own_text() {
    let Some(w) = World::seeded() else { return };
    let text = |args: &[&str]| {
        let r = w.text(args);
        assert_eq!((r.code, r.err.as_str()), (0, ""), "{r:#?}");
        r.out
    };
    assert_eq!(text(&["folders"]), "7\t(All Notes)\n2\tNotes\n4\tWork\n");
    assert_eq!(text(&["search", "EGGS"]), "Notes/Alpha.md\tAlpha\teggs\n");
    assert_eq!(text(&["read", "alpha"]), "# Alpha\n- [ ] milk\n- eggs\nplain line\n");
    // A change's one line is its summary; toggle prints the line it changed.
    assert_eq!(text(&["new", "Texted"]), "Created Notes/Texted.md\n");
    assert_eq!(text(&["toggle", "alpha", "3"]), "- [ ] eggs\n");
    assert_eq!(
        text(&["move", "texted", "Work"]),
        "Moved Notes/Texted.md to Work/Texted.md\n"
    );
    // Nothing to print is the summary.
    assert_eq!(text(&["search", "no such text"]), "0 notes matching \"no such text\"\n");
    assert_eq!(text(&["list", "--flag", "no-such-flag"]), "0 notes\n");
}

#[test]
fn control_characters_never_reach_a_terminal_or_an_id_line() {
    let Some(w) = World::seeded() else { return };
    // DEL, SOH and a C1 control in a title and a snippet; a tab and a
    // newline in a file's name.
    w.write("Notes/Odd.md", "# Odd\u{7f}title\u{1}\nbell\u{7}snip\u{85}pet zebra\n");
    w.write("Notes/tab\there\nnow.md", "# Named\n");
    let found = w.text(["search", "zebra"]);
    assert_eq!(found.out, "Notes/Odd.md\tOdd?title?\tbell?snip?pet zebra\n");
    // In JSON they are there as they are, escaped as JSON needs.
    let data = w.json(["search", "zebra"]).ok();
    assert_eq!(data[0]["title"], "Odd\u{7f}title\u{1}");
    assert_eq!(data[0]["snippet"], "bell\u{7}snip\u{85}pet zebra");
    // One id per line whatever the name holds.
    let ids = w.run(["--ids-only", "search", "Named"]);
    assert_eq!(ids.out, "Notes/tab here now.md\n");
    let listed = w.json(["search", "Named"]).ok();
    assert_eq!(listed[0]["path"], "Notes/tab\there\nnow.md");
}

#[test]
fn text_beyond_ascii_comes_through_whole() {
    let Some(w) = World::seeded() else { return };
    let body = "# Żółw 🐢\nnaïve café — 日本語\n";
    w.write("Notes/Żółw.md", body);
    let read = w.json(["read", "Notes/Żółw.md"]).ok();
    assert_eq!(
        (read["title"].as_str(), read["body"].as_str()),
        (Some("Żółw 🐢"), Some(body))
    );
    // Raw in the JSON text itself, not as \u escapes.
    assert!(w.run(["read", "żółw 🐢"]).out.contains("naïve café — 日本語"));
    assert_eq!(w.text(["read", "Notes/Żółw"]).out, body);
    assert_eq!(w.run(["--ids-only", "search", "日本"]).out, "Notes/Żółw.md\n");
}

#[test]
fn a_failure_is_the_error_object_in_every_machine_mode() {
    let Some(w) = World::seeded() else { return };
    let message = "no note \"nothing like it\"";
    let hint = "`icloud-notes list` shows every note's path; a title must match exactly.";
    for flags in [
        vec![],
        vec!["--json"],
        vec!["--quiet"],
        vec!["--ids-only"],
        vec!["--count"],
        vec!["--count", "--styled"],
    ] {
        let said = w
            .run(flags.iter().copied().chain(["read", "nothing like it"]))
            .fails("not_found");
        assert_eq!((said.0.as_str(), said.1.as_str()), (message, hint), "{flags:?}");
    }
    w.text(["read", "nothing like it"]).fails_in_text(message, hint);
    // A failure with no hint has no `hint` key (fails() checks), and no hint: line.
    let (said, hint) = w.json(["toggle", "alpha", "1"]).fails("not_a_list_item");
    assert_eq!(
        (said.as_str(), hint.as_str()),
        ("line 1 of Notes/Alpha.md is not a list item", "")
    );
    w.text(["toggle", "alpha", "1"])
        .fails_in_text("line 1 of Notes/Alpha.md is not a list item", "");
}
