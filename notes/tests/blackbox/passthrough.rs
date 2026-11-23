//! The engine's own commands: `push --dry-run`, `history`, `diff`, `restore`
//! and `leave` become the engine, in the vault, so its output and exit code
//! are the command's. What is checked is how the engine is run, and what is
//! refused before it is.

use std::fs;
use std::path::Path;

use serde_json::json;

use crate::world::World;

/// Runs `args` with the recording engine and gives the run with the one
/// engine command line that was not `vault-info`.
fn recorded(w: &World, args: &[&str]) -> (crate::world::Run, String) {
    let (engine, log) = w.recording_engine();
    let _ = fs::remove_file(&log);
    let run = w.cmd(args).env("ICLOUD_NOTES_SYNC_BIN", &engine).run();
    let lines = fs::read_to_string(&log).unwrap_or_default();
    let mut ran: Vec<&str> = lines.lines().filter(|l| !l.contains(" vault-info ")).collect();
    assert_eq!(ran.len(), 1, "{lines}");
    (run, ran.remove(0).to_string())
}

fn argv(vault: &Path, args: &str) -> String {
    // In the vault, and with no lock handed down: the engine takes it
    // itself where it writes.
    format!("cwd={} lock=no argv={args}", vault.display())
}

#[test]
fn the_engines_commands_run_the_engine_in_the_vault() {
    let Some(w) = World::seeded() else { return };
    let v = &w.vault;
    // Piped with no flag: the envelope, asked for as --json. The note is
    // named to the engine by its path in the vault, however it was named here.
    let (history, ran) = recorded(&w, &["history", "ALPHA"]);
    assert_eq!(ran, argv(v, "--json history Notes/Alpha.md"));
    assert_eq!(history.code, 0);
    assert_eq!(history.doc()["data"]["mode"], "epochs");
    // The engine's stderr is the command's.
    assert_eq!(history.err, "Reading history...\n");
    assert_eq!(
        recorded(&w, &["history", "id-a", "--records"]).1,
        argv(v, "--json history Notes/Alpha.md --records")
    );
    assert_eq!(
        recorded(&w, &["diff", "Notes/Alpha", "e9"]).1,
        argv(v, "--json diff Notes/Alpha.md e9")
    );
    assert_eq!(recorded(&w, &["push", "--dry-run"]).1, argv(v, "--json push --dry-run"));
    assert_eq!(
        recorded(&w, &["push", "--dry-run", "--yes"]).1,
        argv(v, "--json push --dry-run --yes")
    );
    assert_eq!(
        recorded(&w, &["push", "--yes", "--dry-run"]).1,
        argv(v, "--json push --dry-run --yes")
    );
}

#[test]
fn the_output_flags_and_the_wait_go_to_the_engine_as_given() {
    let Some(w) = World::seeded() else { return };
    let v = &w.vault;
    let (quiet, ran) = recorded(&w, &["--quiet", "history", "alpha"]);
    assert_eq!(ran, argv(v, "--quiet history Notes/Alpha.md"));
    assert_eq!(quiet.doc()["mode"], "epochs");
    let (text, ran) = recorded(&w, &["history", "alpha", "--styled"]);
    assert_eq!(ran, argv(v, "--styled history Notes/Alpha.md"));
    assert_eq!(text.out, "e9  2026-09-01T00:00:00Z  changed: Notes/Alpha.md\n");
    assert_eq!(
        recorded(&w, &["--count", "--ids-only", "push", "--dry-run"]).0.out,
        "stub-id\n"
    );
    assert_eq!(recorded(&w, &["history", "alpha", "--count"]).0.out, "1\n");
    let (_, ran) = recorded(&w, &["--wait", "5", "--quiet", "diff", "alpha", "e9"]);
    assert_eq!(ran, argv(v, "--quiet --wait 5 diff Notes/Alpha.md e9"));
    let (_, ran) = recorded(&w, &["diff", "alpha", "e9", "--json", "--wait=7"]);
    assert_eq!(ran, argv(v, "--json --wait 7 diff Notes/Alpha.md e9"));
}

#[test]
fn the_engines_exit_code_and_error_are_the_commands() {
    let Some(w) = World::seeded() else { return };
    let diff = w
        .cmd(["--json", "diff", "alpha", "e9"])
        .env("ICLOUD_NOTES_SYNC_STUB_EXIT", "1")
        .env("ICLOUD_NOTES_SYNC_STUB_ERROR", "some_code")
        .run();
    // The engine's exit, 1, and its own error document, in its own layout,
    // on stderr; nothing on stdout.
    assert_eq!((diff.code, diff.out.as_str()), (1, ""), "{diff:#?}");
    assert_eq!(
        diff.error_doc(),
        json!({"ok": false, "error": {"code": "some_code", "message": "stub failure"}})
    );
}

#[test]
fn the_note_is_found_here_before_the_engine_runs() {
    let Some(w) = World::seeded() else { return };
    let (engine, log) = w.recording_engine();
    for args in [
        vec!["history", "no such note"],
        vec!["diff", "no such note", "e9"],
        vec!["restore", "no such note", "--yes"],
    ] {
        w.cmd(&args)
            .env("ICLOUD_NOTES_SYNC_BIN", &engine)
            .run()
            .fails("not_found");
    }
    w.cmd(["history", "same title"])
        .env("ICLOUD_NOTES_SYNC_BIN", &engine)
        .run()
        .fails("ambiguous");
    let ran = fs::read_to_string(&log).unwrap_or_default();
    assert!(ran.lines().all(|l| l.contains(" vault-info ")), "{ran}");
}

#[test]
fn restore_asks_first_and_keeps_the_yes_to_itself() {
    let Some(w) = World::seeded() else { return };
    let (message, hint) = w.run(["restore", "alpha"]).fails("usage");
    assert_eq!(
        message,
        "restore asks before it acts, and cannot ask here (no terminal, or a machine output mode)"
    );
    assert_eq!(hint, "Pass --yes to go ahead.");
    assert!(!w.read("Notes/Alpha.md").contains("restored"));
    let (restored, ran) = recorded(&w, &["restore", "alpha", "--yes"]);
    assert_eq!(ran, argv(&w.vault, "--json restore Notes/Alpha.md"));
    assert_eq!(restored.code, 0);
    assert!(w.read("Notes/Alpha.md").ends_with("restored\n"));
}

#[test]
fn leave_hands_the_vault_to_the_engine() {
    let Some(w) = World::seeded() else { return };
    let vault = w.vault.display().to_string();
    // The stub has no `leave`: what it was asked is what is checked.
    let (left, ran) = recorded(&w, &["leave"]);
    assert_eq!(ran, argv(&w.vault, &format!("--json leave {vault}")));
    assert_eq!(
        (left.code, left.err),
        (1, format!("stub: unknown args: --json leave {vault}\n"))
    );
    assert_eq!(
        recorded(&w, &["--styled", "leave", "--yes"]).1,
        argv(&w.vault, &format!("--styled leave {vault} --yes"))
    );
}

#[test]
fn without_the_engine_they_say_it_is_missing() {
    let Some(w) = World::seeded() else { return };
    let nowhere = w.scratch().join("no-engine");
    for args in [
        vec!["diff", "alpha", "e9"],
        vec!["history", "alpha"],
        vec!["push", "--dry-run"],
        vec!["restore", "alpha", "--yes"],
        vec!["leave"],
        vec!["sync"],
        vec!["pull"],
        vec!["push"],
    ] {
        let said = w
            .cmd(&args)
            .env("ICLOUD_NOTES_SYNC_BIN", &nowhere)
            .run()
            .fails("sync_tool_missing");
        assert_eq!(said.0, "the sync engine (icloud-notes-sync) is missing.", "{args:?}");
        assert_eq!(
            said.1, "Reinstall icloud-notes: sudo pacman -S icloud-notes",
            "{args:?}"
        );
    }
    // What the engine has no part in still works.
    w.cmd(["new", "Engineless"])
        .env("ICLOUD_NOTES_SYNC_BIN", &nowhere)
        .run()
        .ok();
}

#[test]
fn they_need_a_cloned_vault_except_leave() {
    let Some(w) = World::new() else { return };
    for args in [
        vec!["push", "--dry-run"],
        vec!["history", "x"],
        vec!["diff", "x", "e1"],
        vec!["restore", "x", "--yes"],
    ] {
        w.run(&args).fails("not_cloned");
    }
    // leave is the engine's to refuse, once there is a directory to run it in.
    let (message, hint) = w.run(["leave"]).fails("error");
    assert_eq!(
        (message, hint.as_str()),
        (format!("cannot enter {}", w.vault.display()), "")
    );
    fs::create_dir(&w.vault).unwrap();
    let left = w.run(["leave"]);
    assert_eq!(left.code, 1);
    assert!(left.err.starts_with("stub: unknown args: --json leave "), "{left:#?}");
}
