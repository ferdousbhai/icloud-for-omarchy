//! The commands with the real engine instead of the stub, replaying iCloud
//! answers the engine's own tests recorded (its cassettes): what this tool
//! sends the engine and reads back from it, checked against what the engine
//! really takes and prints.

use std::fs;

use serde_json::json;

use crate::world::{CASSETTE_DSID, Run, World, cassette, column};

const NOTE: &str = "Notes/Test Note.md";

/// A world whose vault the real engine cloned from the recorded account.
fn cloned() -> Option<World> {
    let w = World::new()?;
    w.session.set_dsid(CASSETTE_DSID);
    let clone = replay(&w, &["clone"], "tiny-clone.json");
    let data = clone.ok();
    assert_eq!(
        (
            &data["runs"][0]["command"],
            &data["runs"][0]["ok"],
            &data["runs"][0]["result"]["written"]
        ),
        (&json!("clone"), &json!(true), &json!(1))
    );
    let log = data["log"].as_str().unwrap();
    assert!(
        log.starts_with(&format!(
            "$ icloud-notes-sync --json clone --account {CASSETTE_DSID} --non-interactive "
        )),
        "{log}"
    );
    assert!(log.contains("Cloned 1 notes") && !log.contains(":progress:"), "{log}");
    Some(w)
}

fn replay(w: &World, args: &[&str], name: &str) -> Run {
    w.cmd(args).replay(&cassette(name)).run()
}

#[test]
fn clone_writes_the_vault_the_commands_then_read() {
    let Some(w) = cloned() else { return };
    assert!(w.read(NOTE).contains("apple-note-id: "));
    // vault-info, as the note list reads it: the note is tracked (not
    // "new"), and its folder is the default one.
    let list = replay(&w, &["list"], "tiny-lookup.json").ok();
    assert_eq!(column(&list, "path"), [NOTE]);
    assert_eq!(list[0]["flags"], json!([]));
    assert!(list[0]["id"].is_string());
    let folders = replay(&w, &["folders"], "tiny-lookup.json").ok();
    assert_eq!(
        (&folders[1]["folder"], &folders[1]["default"]),
        (&json!("Notes"), &json!(true))
    );
    let status = replay(&w, &["status"], "tiny-lookup.json").ok();
    assert_eq!(
        (&status["cloned"], &status["title_mode"], &status["flagged"]),
        (&json!(true), &json!("in-body"), &json!({}))
    );
    assert_eq!(
        replay(&w, &["new", "Beside"], "tiny-lookup.json").ok()["path"],
        "Notes/Beside.md"
    );
}

#[test]
fn a_sync_with_nothing_to_do_reports_both_halves() {
    let Some(w) = cloned() else { return };
    let sync = replay(&w, &["sync"], "tiny-pull-noop.json");
    let data = sync.ok();
    let (push, pull) = (&data["runs"][0], &data["runs"][1]);
    assert_eq!(
        (&push["command"], &push["ok"], &push["exit_code"]),
        (&json!("push"), &json!(true), &json!(0))
    );
    assert_eq!(
        (
            &push["result"]["dryRun"],
            &push["result"]["pushed"],
            &push["result"]["unchanged"]
        ),
        (&json!(false), &json!(0), &json!(1))
    );
    assert_eq!(
        (&pull["command"], &pull["ok"], &pull["result"]["added"]),
        (&json!("pull"), &json!(true), &json!(0))
    );
    assert!(pull["result"]["changes"].is_array());
    let log = data["log"].as_str().unwrap();
    assert!(log.starts_with("$ icloud-notes-sync --quiet sync\n"), "{log}");
    assert!(
        log.contains("pull:\nAlready up to date.") && !log.contains(":progress:"),
        "{log}"
    );
    assert_eq!(replay(&w, &["--sync"], "tiny-pull-noop.json").code, 0);
}

#[test]
fn a_local_edit_is_planned_by_the_engines_dry_run() {
    let Some(w) = cloned() else { return };
    // A Syncthing conflict copy beside the note is no second note: to the
    // engine a warning, here a flag, and the note itself is still written.
    let copy = "Notes/Test Note.sync-conflict-20261114-101500-ABCDEF7.md";
    w.write(copy, w.read(NOTE));
    w.write(NOTE, format!("{}\nA line added locally.", w.read(NOTE)));
    let dry = replay(&w, &["--quiet", "push", "--dry-run"], "tiny-lookup.json");
    assert_eq!(dry.code, 0);
    let plan = dry.doc();
    assert_eq!(plan["dryRun"], true);
    assert_eq!(plan["entries"].as_array().unwrap().len(), 1);
    let entry = &plan["entries"][0];
    assert_eq!(
        (&entry["kind"], &entry["file"], &entry["resolution"]),
        (&json!("update"), &json!(NOTE), &json!("ready"))
    );
    assert_eq!(plan["notices"].as_array().unwrap().len(), 1);
    assert_eq!(plan["notices"][0]["level"], "warn");
    assert!(
        plan["notices"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{copy} is a Syncthing conflict copy"))
    );
    assert_eq!(
        replay(
            &w,
            &["--ids-only", "list", "--flag", "conflict-copy"],
            "tiny-lookup.json"
        )
        .out,
        format!("{copy}\n")
    );
    replay(&w, &["write", NOTE, "--append", "--body", "more"], "tiny-lookup.json").ok();
    assert!(w.read(NOTE).ends_with("A line added locally.\nmore\n"));
}

#[test]
fn history_and_diff_are_the_engines_own_answers() {
    let Some(w) = cloned() else { return };
    let history = replay(&w, &["--quiet", "history", "Test Note"], "tiny-lookup.json");
    assert_eq!(history.code, 0);
    assert_eq!(history.doc()["mode"], "epochs");
    assert!(history.doc()["epochs"].is_array());
    // An unknown version: the engine's error document and exit code.
    let diff = replay(&w, &["--json", "diff", "Test Note", "no-such-id"], "tiny-lookup.json");
    assert_eq!(diff.code, 4);
    assert_eq!(
        (&diff.doc()["ok"], &diff.doc()["error"]["code"]),
        (&json!(false), &json!("unknown_version_snapshot"))
    );
}

#[test]
fn a_vault_of_another_account_is_refused_by_the_engine_and_is_no_sign_in() {
    let Some(w) = cloned() else { return };
    let state = w.read(".icloud-notes/state.json");
    assert!(state.contains(&format!("\"{CASSETTE_DSID}\"")));
    fs::write(
        w.path(".icloud-notes/state.json"),
        state.replace(CASSETTE_DSID, "10000000002"),
    )
    .unwrap();
    let sync = replay(&w, &["sync"], "tiny-pull-noop.json");
    assert_eq!((sync.code, sync.err.as_str()), (3, ""));
    let data = sync.data();
    assert_eq!(
        (&data["ok"], &data["error"]["code"]),
        (&json!(false), &json!("account_mismatch"))
    );
    assert!(data["error"]["hint"].is_string());
    // The push had nothing to send and asked iCloud nothing; the pull is
    // the half that met the other account. Exit 3, and yet no sign-in.
    let runs = data["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!((&runs[0]["command"], &runs[0]["ok"]), (&json!("push"), &json!(true)));
    assert_eq!(
        (
            &runs[1]["ok"],
            &runs[1]["exit_code"],
            &runs[1]["result"]["error"]["code"]
        ),
        (&json!(false), &json!(3), &json!("account_mismatch"))
    );
    assert_eq!(w.session.reports(), 0);
}
