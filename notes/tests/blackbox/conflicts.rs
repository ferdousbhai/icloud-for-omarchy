//! Conflict blocks in a note: `resolve` picks a side for each, `recover`
//! deals with markers that cannot be read as blocks.

use std::fs;

use serde_json::json;

use crate::world::{USAGE_HINT, World};

const BETA_HEAD: &str = "---\napple-note-id: id-b\n---\n# Beta\nshared\n";

#[test]
fn resolve_takes_a_side_for_each_block() {
    let Some(w) = World::seeded() else { return };
    let resolved = w.run(["resolve", "beta", "--choices", " local , both "]);
    assert_eq!(
        resolved.ok(),
        json!({"action": "resolve", "path": "Work/Beta.md", "choices": ["local", "both"]})
    );
    assert_eq!(resolved.summary(), "Resolved 2 conflict block(s) in Work/Beta.md");
    assert_eq!(
        w.read("Work/Beta.md"),
        format!("{BETA_HEAD}mine 1\nmiddle\nmine 2\ntheirs 2\n")
    );
    assert_eq!(w.json(["read", "beta"]).ok()["flags"], json!([]));
    let (message, hint) = w.json(["resolve", "beta", "--all", "remote"]).fails("no_conflicts");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("Work/Beta.md has no conflict to resolve", "")
    );
}

#[test]
fn resolve_all_picks_one_side_everywhere() {
    let Some(w) = World::seeded() else { return };
    let original = w.read("Work/Beta.md");
    for (choice, body) in [
        ("remote", "theirs 1\nmiddle\ntheirs 2\n"),
        ("local", "mine 1\nmiddle\nmine 2\n"),
        ("both", "mine 1\ntheirs 1\nmiddle\nmine 2\ntheirs 2\n"),
    ] {
        w.write("Work/Beta.md", &original);
        let resolved = w.json(["resolve", "beta", "--all", choice]).ok();
        assert_eq!(resolved["choices"], json!([choice, choice]));
        assert_eq!(w.read("Work/Beta.md"), format!("{BETA_HEAD}{body}"), "{choice}");
    }
    // A block with its base section: the base is dropped whatever is picked.
    w.write(
        "Work/Three.md",
        "# Three\n<<<<<<< local\nL\n||||||| base\nB\n=======\nR\n>>>>>>> remote\nend",
    );
    w.json(["resolve", "three", "--all", "both"]).ok();
    assert_eq!(w.read("Work/Three.md"), "# Three\nL\nR\nend");
}

#[test]
fn resolve_wants_one_valid_choice_per_block() {
    let Some(w) = World::seeded() else { return };
    let original = w.read("Work/Beta.md");
    for args in [
        vec!["resolve", "beta"],
        vec!["resolve", "beta", "--all", "local", "--choices", "local,local"],
    ] {
        let said = w.json(&args).fails("usage");
        assert_eq!(
            (said.0.as_str(), said.1.as_str()),
            (
                "resolve needs --all local|remote|both or --choices C1,C2,...",
                USAGE_HINT
            )
        );
    }
    for (args, bad) in [
        (vec!["--choices", "local,mine"], "mine"),
        (vec!["--all", "theirs"], "theirs"),
        (vec!["--all", "LOCAL"], "LOCAL"),
    ] {
        let (message, _) = w.json(["resolve", "beta"].into_iter().chain(args)).fails("usage");
        assert_eq!(message, format!("a choice is local, remote or both, not \"{bad}\""));
    }
    for (choices, given) in [("local", 1), ("local,remote,both", 3), (",", 0)] {
        let (message, hint) = w
            .json(["resolve", "beta", "--choices", choices])
            .fails("choices_mismatch");
        assert_eq!(
            message,
            format!("Work/Beta.md has 2 conflict blocks, and {given} choices were given")
        );
        assert_eq!(hint, "`icloud-notes read NOTE --json` lists the blocks.");
    }
    let (message, hint) = w
        .json(["resolve", "tangle", "--all", "local"])
        .fails("conflicts_unreadable");
    assert_eq!(
        message,
        "Work/Tangle.md has conflict markers the version picker cannot read"
    );
    assert_eq!(
        hint,
        "Use `icloud-notes recover NOTE --strip` (keep every line) or --synced (the last synced text)."
    );
    w.json(["resolve", "alpha", "--all", "local"]).fails("no_conflicts");
    w.json(["resolve", "no such note", "--all", "local"]).fails("not_found");
    assert_eq!(w.read("Work/Beta.md"), original);
}

#[test]
fn resolve_refuses_a_read_only_note() {
    let Some(w) = World::seeded() else { return };
    let conflicted = "---\napple-note-id: id-g\n---\n# Grid\n<<<<<<< local\na\n=======\nb\n>>>>>>> remote\n";
    w.write("Work/Grid.md", conflicted);
    let (message, _) = w.json(["resolve", "grid", "--all", "local"]).fails("read_only");
    assert_eq!(message, "This note is read-only here. Resolve it in Apple Notes.");
    assert_eq!(w.read("Work/Grid.md"), conflicted);
}

#[test]
fn recover_strip_keeps_every_line_after_a_backup() {
    let Some(w) = World::seeded() else { return };
    let original = w.read("Work/Tangle.md");
    let recovered = w.run(["recover", "tangle", "--strip"]);
    let data = recovered.ok();
    let backup = data["backup"].as_str().unwrap();
    // The backup: beside the engine's state, under the note's folder, named
    // for the note and the time.
    let dir = w.path(".icloud-notes/conflict-backups/Work");
    let name = backup.strip_prefix(&format!("{}/", dir.display())).expect(backup);
    let stamp = name
        .strip_prefix("Tangle (conflict backup ")
        .and_then(|n| n.strip_suffix(").md"))
        .expect(name);
    let shape: String = stamp
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    assert_eq!(shape, "0000-00-00 000000");
    assert_eq!(fs::read_to_string(backup).unwrap(), original);
    let message = format!(
        "Conflict markers removed. Read the note over: both versions' lines are kept. The note as it was is saved in \
         .icloud-notes/conflict-backups/Work/{name}."
    );
    assert_eq!(
        data,
        json!({"action": "recover", "path": "Work/Tangle.md", "how": "strip", "backup": backup, "message": message})
    );
    assert_eq!(recovered.summary(), message);
    assert_eq!(
        w.read("Work/Tangle.md"),
        "---\napple-note-id: id-u\n---\n# Tangle\na\nb\nc\nd\n"
    );
    // The backup is no note.
    assert_eq!(w.run(["list", "--count"]).out, "7\n");
    let (message, hint) = w.json(["recover", "tangle", "--strip"]).fails("no_conflicts");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("This note has no unreadable conflict markers.", "")
    );
}

#[test]
fn recover_synced_goes_back_to_the_last_synced_text() {
    let Some(w) = World::seeded() else { return };
    let original = w.read("Work/Tangle.md");
    let data = w.json(["recover", "tangle", "--synced"]).ok();
    assert_eq!(data["how"], "synced");
    assert!(
        data["message"]
            .as_str()
            .unwrap()
            .starts_with("Back to the last synced version. The note as it was is saved in ")
    );
    assert_eq!(fs::read_to_string(data["backup"].as_str().unwrap()).unwrap(), original);
    assert_eq!(
        w.read("Work/Tangle.md"),
        "---\napple-note-id: id-u\n---\n# Tangle\nsynced text\n"
    );
    // Backups never replace one another, even within one second.
    w.write("Work/Tangle.md", &original);
    let first = w.json(["recover", "tangle", "--strip"]).ok()["backup"]
        .as_str()
        .unwrap()
        .to_string();
    w.write("Work/Tangle.md", &original);
    let second = w.json(["recover", "tangle", "--strip"]).ok()["backup"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);
    assert!(fs::metadata(&first).is_ok() && fs::metadata(&second).is_ok());
}

#[test]
fn recover_refuses_what_it_cannot_recover() {
    let Some(w) = World::seeded() else { return };
    for args in [
        vec!["recover", "tangle"],
        vec!["recover", "tangle", "--strip", "--synced"],
    ] {
        let said = w.json(&args).fails("usage");
        assert_eq!(
            (said.0.as_str(), said.1.as_str()),
            ("recover needs --strip or --synced", USAGE_HINT)
        );
    }
    // Blocks the picker can read are resolve's.
    w.json(["recover", "beta", "--strip"]).fails("no_conflicts");
    w.json(["recover", "alpha", "--synced"]).fails("no_conflicts");
    // Unreadable markers, but nothing synced to go back to.
    w.write(
        "Notes/Alpha.md",
        "---\napple-note-id: id-a\n---\n# Alpha\n<<<<<<< local\n<<<<<<< local\nb\n",
    );
    let (message, hint) = w.json(["recover", "alpha", "--synced"]).fails("no_synced_copy");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("There is no last synced version of this note.", "")
    );
    w.write(
        "Work/Grid.md",
        "---\napple-note-id: id-g\n---\n# Grid\n>>>>>>> remote\n",
    );
    let (message, _) = w.json(["recover", "grid", "--strip"]).fails("read_only");
    assert_eq!(message, "This note is read-only here. Resolve it in Apple Notes.");
    assert!(
        !w.exists(".icloud-notes/conflict-backups"),
        "a refusal backs nothing up"
    );
}
