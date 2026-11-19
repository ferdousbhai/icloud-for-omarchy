//! sync, pull, push and clone through the stub engine: what is checked
//! before the engine runs, how it is run, how its answer becomes the result
//! (the runs, the log, the failure and the exit code), when icloud-session
//! is told about a refused sign-in, and a change's `--push`.

use std::fs;

use serde_json::{Value, json};

use crate::world::{Cmd, DSID, Run, SIGN_IN_HINT, World};

const OFFLINE: &str = "iCloud could not be reached (offline: dns error: no network)";
const NETWORK_HINT: &str = "check the network connection and try again";
const REFUSED: &str = "Notes/Big.md: server rejected the delete: BAD_REQUEST";
const SESSION_HINT: &str = "check that icloud-sessiond is installed and the D-Bus session bus is running";

/// The stub's switches, by their short names.
fn stub<'w>(cmd: Cmd<'w>, switches: &[(&str, &str)]) -> Cmd<'w> {
    switches
        .iter()
        .fold(cmd, |c, (k, v)| c.env(&format!("ICLOUD_NOTES_SYNC_STUB_{k}"), v))
}

/// A sync that ran and failed: exit `exit`, nothing on stderr, and the
/// result in an envelope whose own `ok` is true, with `ok: false` and the
/// failure inside. Gives the data.
#[track_caller]
fn failed(run: &Run, exit: i32, code: &str) -> Value {
    assert_eq!((run.code, run.err.as_str()), (exit, ""), "{run:#?}");
    let data = run.data();
    assert_eq!(
        (&data["ok"], &data["error"]["code"]),
        (&json!(false), &json!(code)),
        "{run:#?}"
    );
    data
}

fn ran(command: &str, exit: i32, result: Value) -> Value {
    json!({"command": command, "ok": exit == 0, "exit_code": exit, "result": result})
}

fn error(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

// ---- what is checked first ----------------------------------------------------

#[test]
fn syncing_needs_a_cloned_vault_the_engine_and_a_sign_in_in_that_order() {
    let Some(mut w) = World::new() else { return };
    let nowhere = w.scratch().join("no-engine");
    for command in ["sync", "pull", "push"] {
        let (message, _) = w
            .cmd([command])
            .env("ICLOUD_NOTES_SYNC_BIN", &nowhere)
            .run()
            .fails(3, "not_cloned");
        assert_eq!(message, format!("no notes cloned at {}", w.vault.display()));
    }
    w.state("in-body", &[], None);
    w.session.set_signed_in(false);
    for command in ["sync", "pull", "push", "clone"] {
        // The engine before the lock, the lock before the sign-in.
        if command != "clone" {
            w.cmd([command])
                .env("ICLOUD_NOTES_SYNC_BIN", &nowhere)
                .run()
                .fails(5, "sync_tool_missing");
            let held = w.hold_lock("Notes (pid 1)");
            w.run([command]).fails(1, "vault_busy");
            drop(held);
        }
    }
    for command in ["sync", "pull", "push"] {
        let (message, hint) = w.run([command]).fails(3, "sign_in_required");
        assert_eq!(
            (message.as_str(), hint.as_str()),
            ("sign in to iCloud required", SIGN_IN_HINT)
        );
        w.text([command])
            .fails_in_text(3, "sign in to iCloud required", SIGN_IN_HINT);
    }
    w.session.stop();
    for command in ["sync", "pull", "push"] {
        let (message, hint) = w.run([command]).fails(5, "session_service");
        assert_eq!(
            (message.as_str(), hint.as_str()),
            (
                "icloud-session is not available, so the sign-in is unknown",
                SESSION_HINT
            )
        );
    }
    let no_bus = w
        .cmd(["sync"])
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .run();
    no_bus.fails(5, "session_service");
    // The engine ran for none of it, and nobody was asked to sign in.
    assert_eq!(w.stub_log(), [""; 0]);
    assert_eq!((w.session.sign_ins(), w.session.reports()), (0, 0));
}

// ---- runs that work --------------------------------------------------------------

#[test]
fn sync_is_one_engine_run_reported_as_a_push_and_a_pull() {
    let Some(w) = World::seeded() else { return };
    let log = "$ icloud-notes-sync --quiet sync\nFetching changes from iCloud...\n(exit 0)\npush:\nstub push ok\npull:\nstub pull ok";
    let sync = w.run(["sync"]);
    assert_eq!(
        sync.ok(),
        json!({
            "ok": true,
            "runs": [ran("push", 0, json!({"stub": "push", "ok": true})), ran("pull", 0, json!({"stub": "pull", "ok": true}))],
            "log": log,
        })
    );
    assert_eq!(sync.summary(), "sync done");
    assert_eq!(w.stub_log(), ["sync"]);
    // Text: the log.
    let text = w.text(["sync"]);
    assert_eq!((text.code, text.out, text.err.as_str()), (0, format!("{log}\n"), ""));
}

#[test]
fn pull_and_push_are_runs_of_their_own() {
    let Some(w) = World::seeded() else { return };
    let pull = w.run(["pull"]);
    // The engine's progress lines (one per page and note) are in nobody's log.
    assert_eq!(
        pull.ok(),
        json!({
            "ok": true,
            "runs": [ran("pull", 0, json!({"stub": "pull", "ok": true}))],
            "log": "$ icloud-notes-sync --json pull\n(exit 0)\nstub pull ok",
        })
    );
    assert_eq!(pull.summary(), "pull done");
    let push = w.run(["push"]);
    assert_eq!(
        push.ok()["log"],
        "$ icloud-notes-sync --json push\n(exit 0)\nstub push ok"
    );
    assert_eq!(push.summary(), "push done");
    assert_eq!(w.stub_log(), ["pull", "push"]);
    let text = w.text(["pull"]);
    assert_eq!(
        (text.out.as_str(), text.err.as_str()),
        ("$ icloud-notes-sync --json pull\n(exit 0)\nstub pull ok\n", "")
    );
}

#[test]
fn what_a_pull_brought_is_there_when_the_command_ends() {
    let Some(w) = World::seeded() else { return };
    let added = w.path("Notes/Added.md");
    let sync = w.cmd(["sync"]).env("ICLOUD_NOTES_SYNC_STUB_PULL_ADDS", &added).run();
    let data = sync.ok();
    assert_eq!(data["runs"][1]["result"]["added"], 1);
    assert!(
        data["log"]
            .as_str()
            .unwrap()
            .ends_with(&format!("pull:\nstub pull ok\nadded: {}", added.display()))
    );
    assert_eq!(w.read("Notes/Added.md"), "# Added\nfrom iCloud\n");
}

#[test]
fn a_change_left_for_the_next_push_is_no_failure() {
    let Some(w) = World::seeded() else { return };
    let sync = stub(w.cmd(["sync"]), &[("PUSH_SKIPPED", "1")]).run();
    let data = sync.ok();
    assert_eq!(data["ok"], true);
    assert!(data.get("error").is_none());
    assert_eq!(
        data["runs"][0]["result"]["entries"][0]["outcome"],
        json!({
            "succeeded": true,
            "skipped": true,
            "message": "Notes/Alpha.md: changed here while it was being pushed - left for the next push",
        })
    );
    assert!(
        data["log"]
            .as_str()
            .unwrap()
            .contains("left for the next push\nPushed 0 note(s)\npull:")
    );
}

// ---- runs that fail --------------------------------------------------------------

#[test]
fn no_network_is_said_in_this_tools_words() {
    let Some(w) = World::seeded() else { return };
    let offline = error("offline", "offline: dns error: no network");
    let sync = stub(w.cmd(["sync"]), &[("OFFLINE", "1")]).run();
    assert_eq!(
        failed(&sync, 5, "offline"),
        json!({
            "ok": false,
            "error": {"code": "offline", "message": OFFLINE, "hint": NETWORK_HINT},
            // The pull was left out: it is reported as failing the way the push did.
            "runs": [ran("push", 5, offline.clone()), ran("pull", 5, offline)],
            "log": format!("$ icloud-notes-sync --quiet sync\n(exit 5)\npush: {OFFLINE}; the next sync tries again."),
        })
    );
    assert_eq!(sync.summary(), format!("sync failed: {OFFLINE}"));
    // In text: the log on stdout, the failure on stderr.
    let text = stub(w.cmd(["sync"]).styled(), &[("OFFLINE", "1")]).run();
    assert_eq!(text.code, 5);
    assert_eq!(
        text.out,
        format!("$ icloud-notes-sync --quiet sync\n(exit 5)\npush: {OFFLINE}; the next sync tries again.\n")
    );
    assert_eq!(text.err, format!("error: {OFFLINE}\nhint: {NETWORK_HINT}\n"));
    // A pull alone: the engine's error document, whatever its layout and key order.
    let pull = stub(w.cmd(["pull"]), &[("OFFLINE", "1")]).run();
    let data = failed(&pull, 5, "offline");
    assert_eq!(
        data["error"],
        json!({"code": "offline", "message": OFFLINE, "hint": NETWORK_HINT})
    );
    assert_eq!(
        data["log"],
        format!("$ icloud-notes-sync --json pull\n(exit 5)\npull: {OFFLINE}; the next sync tries again.")
    );
    assert_eq!(
        failed(
            &stub(w.cmd(["sync"]), &[("PUSH_FAIL", "network:5")]).run(),
            5,
            "network"
        )["error"]["hint"],
        NETWORK_HINT
    );
    assert_eq!(w.session.reports(), 0);
}

#[test]
fn the_engines_own_error_comes_through_with_its_exit() {
    let Some(w) = World::seeded() else { return };
    let mismatch = [
        ("EXIT", "3"),
        ("ERROR", "account_mismatch"),
        ("MESSAGE", "another account's vault"),
        ("HINT", "use its own vault"),
    ];
    let said = json!({"code": "account_mismatch", "message": "another account's vault", "hint": "use its own vault"});
    // An engine that never started its halves: both runs fail with its
    // error. Its stderr is in the log.
    let sync = stub(w.cmd(["sync"]), &mismatch).run();
    assert_eq!(
        failed(&sync, 3, "account_mismatch"),
        json!({
            "ok": false,
            "error": said,
            "runs": [ran("push", 3, json!({"error": said})), ran("pull", 3, json!({"error": said}))],
            "log": "$ icloud-notes-sync --quiet sync\nstub: failing with account_mismatch\n(exit 3)\n\
                    sync failed: another account's vault\nuse its own vault",
        })
    );
    assert_eq!(sync.summary(), "sync failed: another account's vault");
    assert_eq!(
        failed(&stub(w.cmd(["pull"]), &mismatch).run(), 3, "account_mismatch")["error"],
        said
    );
    let text = stub(w.cmd(["push"]).styled(), &mismatch).run();
    assert_eq!(
        (text.code, text.err.as_str()),
        (3, "error: another account's vault\nhint: use its own vault\n")
    );
    assert!(
        text.out
            .ends_with("(exit 3)\npush failed: another account's vault\nuse its own vault\n"),
        "{text:#?}"
    );
    // Not a sign-in, whatever the exit code says.
    assert_eq!(w.session.reports(), 0);
    let unavailable = stub(w.cmd(["pull"]), &[("EXIT", "5"), ("ERROR", "cloudkit_request_failed")]).run();
    let data = failed(&unavailable, 5, "cloudkit_request_failed");
    assert_eq!(
        data["error"],
        json!({"code": "cloudkit_request_failed", "message": "stub failure"})
    );
}

#[test]
fn the_worse_half_of_a_sync_decides() {
    let Some(w) = World::seeded() else { return };
    let halves = |push: &str, pull: &str| stub(w.cmd(["sync"]), &[("PUSH_FAIL", push), ("PULL_FAIL", pull)]).run();
    // The higher exit code; 3 beats every other; a tie goes to the pull.
    failed(
        &halves("cloudkit_request_failed:1", "bad_response:5"),
        5,
        "bad_response",
    );
    failed(
        &halves("bad_response:5", "cloudkit_request_failed:1"),
        5,
        "bad_response",
    );
    failed(&halves("bad_response:5", "account_mismatch:3"), 3, "account_mismatch");
    failed(&halves("account_mismatch:3", "bad_response:5"), 3, "account_mismatch");
    let tie = halves("first:4", "second:4");
    let data = failed(&tie, 4, "second");
    assert_eq!(
        data["error"],
        json!({"code": "second", "message": "stub pull failed with second"})
    );
    assert_eq!(
        data["runs"],
        json!([
            ran("push", 4, error("first", "stub push failed with first")),
            ran("pull", 4, error("second", "stub pull failed with second"))
        ])
    );
    assert_eq!(
        data["log"],
        "$ icloud-notes-sync --quiet sync\n(exit 4)\npush failed: stub push failed with first\npull failed: stub pull failed with second"
    );
    // One half failing fails the sync, with the other half's result kept.
    let one = stub(w.cmd(["sync"]), &[("PULL_FAIL", "bad_response:5")]).run();
    let data = failed(&one, 5, "bad_response");
    assert_eq!(data["runs"][0], ran("push", 0, json!({"stub": "push", "ok": true})));
}

#[test]
fn a_failure_that_says_nothing_is_sync_failed() {
    let Some(w) = World::seeded() else { return };
    // Exit 1 and plain text on stderr: no error document, no code.
    let silent = stub(w.cmd(["pull"]), &[("EXPIRED", "1"), ("SIGNIN", "marker")]).run();
    assert_eq!(
        failed(&silent, 1, "sync_failed"),
        json!({
            "ok": false,
            "error": {"code": "sync_failed", "message": "pull failed; see the log"},
            "runs": [ran("pull", 1, Value::Null)],
            "log": "$ icloud-notes-sync --json pull\nRun \"icloud-md reauthenticate\" to sign in interactively.\n(exit 1)",
        })
    );
    assert_eq!(silent.summary(), "pull failed: pull failed; see the log");
    assert_eq!(w.session.reports(), 0);
}

#[test]
fn a_change_icloud_refused_fails_the_push_and_the_pull_still_runs() {
    let Some(w) = World::seeded() else { return };
    let sync = stub(w.cmd(["sync"]), &[("PUSH_REFUSED", "1")]).run();
    let data = failed(&sync, 1, "push_rejected");
    assert_eq!(data["error"], json!({"code": "push_rejected", "message": REFUSED}));
    assert_eq!(
        data["runs"],
        json!([
            ran("push", 1, error("push_rejected", REFUSED)),
            ran("pull", 0, json!({"stub": "pull", "ok": true}))
        ])
    );
    assert_eq!(
        data["log"],
        format!(
            "$ icloud-notes-sync --quiet sync\nFetching changes from iCloud...\n(exit 1)\npush:\n{REFUSED}\nPushed 0 note(s)\n\
             push failed: {REFUSED}\npull:\nstub pull ok"
        )
    );
    // A push alone exits 1 with a result that worked but for one entry.
    let push = stub(w.cmd(["push"]), &[("PUSH_REFUSED", "1")]).run();
    let data = failed(&push, 1, "push_rejected");
    assert_eq!(data["error"]["message"], REFUSED);
    assert_eq!(
        data["log"],
        format!("$ icloud-notes-sync --json push\n(exit 1)\n{REFUSED}\nPushed 0 note(s)\npush failed: {REFUSED}")
    );
}

// ---- a refused sign-in ---------------------------------------------------------------

#[test]
fn a_refused_sign_in_is_reported_to_icloud_session() {
    let Some(w) = World::seeded() else { return };
    let message = "sign in to iCloud required: iCloud refused the sign-in; icloud-session was told";
    let engine_said =
        json!({"error": {"code": "sign_in_required", "message": "sign in to iCloud required", "hint": SIGN_IN_HINT}});
    let pull = stub(w.cmd(["pull"]), &[("EXPIRED", "1")]).run();
    assert_eq!(
        failed(&pull, 3, "sign_in_required"),
        json!({
            "ok": false,
            "error": {"code": "sign_in_required", "message": message, "hint": SIGN_IN_HINT},
            "runs": [ran("pull", 3, engine_said.clone())],
            "log": format!("$ icloud-notes-sync --json pull\n(exit 3)\npull failed: sign in to iCloud required\n{SIGN_IN_HINT}"),
        })
    );
    assert_eq!(w.session.reports(), 1);
    // In a sync the pull is not run after it, and not reported.
    let sync = stub(w.cmd(["sync"]), &[("EXPIRED", "1")]).run();
    assert_eq!(
        failed(&sync, 3, "sign_in_required")["runs"],
        json!([ran("push", 3, engine_said)])
    );
    assert_eq!(sync.summary(), format!("sync failed: {message}"));
    assert_eq!(w.session.reports(), 2);
    // Exit 3 with no error document at all is taken for one too.
    let bare = stub(w.cmd(["pull"]), &[("EXPIRED", "1"), ("SIGNIN", "code")]).run();
    let data = failed(&bare, 3, "sign_in_required");
    assert_eq!(data["error"]["message"], message);
    assert_eq!(data["runs"], json!([ran("pull", 3, Value::Null)]));
    assert_eq!(
        data["log"],
        "$ icloud-notes-sync --json pull\nsign in to iCloud required\n(exit 3)"
    );
    assert_eq!(w.session.reports(), 3);
    assert_eq!(w.session.sign_ins(), 0, "a command never opens the sign-in window");
}

#[test]
fn a_session_apple_rejected_is_not_reported_again() {
    let Some(w) = World::seeded() else { return };
    let rejected = "Apple refused the iCloud session although icloud-sessiond still holds it (HTTP 401)";
    let pull = stub(w.cmd(["pull"]), &[("EXPIRED", "1"), ("SIGNIN", "rejected")]).run();
    let data = failed(&pull, 3, "session_rejected");
    assert_eq!(
        data["error"],
        json!({"code": "session_rejected", "message": rejected, "hint": SIGN_IN_HINT})
    );
    // The push failed first (a CloudKit 4xx), then the pull was rejected:
    // that is the failure, as the engine said it.
    let both = stub(
        w.cmd(["sync"]),
        &[
            ("PUSH_FAIL", "cloudkit_request_failed:1"),
            ("PULL_FAIL", "session_rejected:3"),
        ],
    )
    .run();
    let data = failed(&both, 3, "session_rejected");
    assert_eq!(
        data["error"],
        json!({"code": "session_rejected", "message": "stub pull failed with session_rejected"})
    );
    assert!(!both.out.contains("icloud-session was told"));
    assert_eq!(w.session.reports(), 0);
}

#[test]
fn a_sign_in_icloud_session_vouches_for_is_tried_once_more() {
    let Some(w) = World::seeded() else { return };
    w.session.vouch();
    // The pull is refused; icloud-session says the sign-in is good; the
    // retry (a whole sync) fails its pull for another reason, and that is
    // the failure reported.
    let retried = stub(
        w.cmd(["pull"]),
        &[("EXPIRED", "pull"), ("PULL_FAIL", "cloudkit_transient:5")],
    )
    .run();
    let data = failed(&retried, 5, "cloudkit_transient");
    let commands: Vec<&str> = data["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, ["pull", "push", "pull"]);
    assert_eq!(w.session.reports(), 1);
    assert_eq!(w.stub_log(), ["pull", "sync"]);
    // Still refused after the retry: reported once more, and that is all.
    let again = stub(w.cmd(["sync"]), &[("EXPIRED", "1")]).run();
    failed(&again, 3, "sign_in_required");
    assert_eq!(w.stub_log(), ["pull", "sync", "sync", "sync"]);
    assert_eq!(w.session.reports(), 3);
}

// ---- a push that would delete most of the vault -------------------------------------------

#[test]
fn only_a_yes_on_sync_or_push_confirms_mass_deletions() {
    let Some(w) = World::seeded() else { return };
    let too_many = [("TOO_MANY", "1")];
    let message = "5 of the 6 notes tracked here have no file any more (or an empty one). Pushing that would move them all to \
                   Recently Deleted, so nothing was pushed.";
    let sync = stub(w.cmd(["sync"]), &too_many).run();
    let data = failed(&sync, 2, "too_many_deletions");
    // The engine's message and hint, whose commands are this tool's.
    assert_eq!(data["error"]["message"], message);
    let hint = data["error"]["hint"].as_str().unwrap();
    assert!(
        hint.contains("\"icloud-notes push --dry-run\"") && hint.contains("\"icloud-notes push --yes\""),
        "{hint}"
    );
    // The pull ran all the same.
    assert_eq!(
        (&data["runs"][0]["ok"], &data["runs"][1]["ok"]),
        (&json!(false), &json!(true))
    );
    let push = stub(w.cmd(["push"]), &too_many).run();
    assert_eq!(failed(&push, 2, "too_many_deletions")["error"], data["error"]);
    let text = stub(w.cmd(["push"]).styled(), &too_many).run();
    assert_eq!((text.code, text.err), (2, format!("error: {message}\nhint: {hint}\n")));
    // A change's --push never confirms them.
    let changed = stub(w.cmd(["toggle", "alpha", "2", "--push"]), &too_many).run();
    assert_eq!(
        (changed.code, &changed.data()["sync"]["error"]["code"]),
        (2, &json!("too_many_deletions"))
    );
    let background = stub(w.cmd(["--sync"]), &too_many).run();
    assert_eq!(background.code, 1);
    assert_eq!(w.stub_log(), ["sync", "push", "push", "sync", "background"]);
    // --yes goes to the engine, from sync and from push.
    stub(w.cmd(["sync", "--yes"]), &too_many).run().ok();
    stub(w.cmd(["push", "--yes"]), &too_many).run().ok();
    assert_eq!(w.stub_log()[5..], ["sync --yes", "push --yes"]);
}

// ---- a change's --push ----------------------------------------------------------------------

#[test]
fn a_change_with_push_syncs_after_it_under_the_same_lock() {
    let Some(w) = World::seeded() else { return };
    let pushed = w.run(["new", "Pushed", "--push"]);
    let data = pushed.ok();
    assert_eq!(data["path"], "Notes/Pushed.md");
    assert_eq!(data["sync"], w.run(["sync"]).ok());
    assert_eq!(pushed.summary(), "Created Notes/Pushed.md; synced");
    assert_eq!(w.stub_log(), ["sync", "sync"]);
    // Text: the change's line, then the sync's log.
    let text = w.text(["toggle", "alpha", "2", "--push"]);
    assert_eq!(
        (text.code, text.out.as_str(), text.err.as_str()),
        (
            0,
            "- [x] milk\n$ icloud-notes-sync --quiet sync\nFetching changes from iCloud...\n(exit 0)\npush:\nstub push ok\npull:\nstub pull ok\n",
            ""
        )
    );
    // Every command that changes a note or a folder takes it.
    for args in [
        vec!["write", "alpha", "--append", "--body", "x", "--push"],
        vec!["rename", "alpha", "Alpha 2", "--push"],
        vec!["move", "alpha", "Work", "--push"],
        vec!["resolve", "beta", "--all", "local", "--push"],
        vec!["recover", "tangle", "--strip", "--push"],
        vec!["new-folder", "Pushed folder", "--push"],
        vec!["rename-folder", "Pushed folder", "Renamed folder", "--push"],
        vec!["delete-folder", "Renamed folder", "--yes", "--push"],
        vec!["delete", "Work/Alpha.md", "--yes", "--push"],
    ] {
        assert_eq!(w.run(&args).ok()["sync"]["ok"], true, "{args:?}");
    }
    assert_eq!(w.stub_log().len(), 12);
    // A change that was refused syncs nothing.
    w.run(["toggle", "beta", "1", "--push"]).fails(1, "not_a_list_item");
    assert_eq!(w.stub_log().len(), 12);
}

#[test]
fn a_change_stays_done_when_its_push_fails() {
    let Some(mut w) = World::seeded() else { return };
    // The sync ran and failed: its result, with the failure in it.
    let offline = stub(w.cmd(["new", "Offline", "--push"]), &[("OFFLINE", "1")]).run();
    assert_eq!((offline.code, offline.err.as_str()), (5, ""));
    let data = offline.data();
    assert_eq!(
        data["sync"]["error"],
        json!({"code": "offline", "message": format!("new done here, but not synced: {OFFLINE}"), "hint": NETWORK_HINT})
    );
    assert_eq!(
        (&data["sync"]["ok"], data["sync"]["runs"].as_array().unwrap().len()),
        (&json!(false), 2)
    );
    assert_eq!(offline.summary(), "Created Notes/Offline.md; not synced");
    assert_eq!(w.read("Notes/Offline.md"), "# Offline\n");
    let text = stub(w.cmd(["new", "Offline 2", "--push"]).styled(), &[("OFFLINE", "1")]).run();
    assert_eq!(text.code, 5);
    assert!(
        text.out
            .starts_with("Created Notes/Offline 2.md\n$ icloud-notes-sync --quiet sync\n(exit 5)\n"),
        "{text:#?}"
    );
    assert_eq!(
        text.err,
        format!("error: new done here, but not synced: {OFFLINE}\nhint: {NETWORK_HINT}\n")
    );
    // The sync never started: only the failure.
    w.session.set_signed_in(false);
    let signed_out = w.run(["write", "offline", "--body", "# Offline\nkept\n", "--push"]);
    assert_eq!(signed_out.code, 3);
    let data = signed_out.data();
    assert_eq!((&data["action"], &data["changed"]), (&json!("write"), &json!(true)));
    assert_eq!(
        data["sync"],
        json!({"ok": false, "error": {
            "code": "sign_in_required",
            "message": "write done here, but not synced: sign in to iCloud required",
            "hint": SIGN_IN_HINT,
        }})
    );
    assert_eq!(w.read("Notes/Offline.md"), "# Offline\nkept\n");
    w.session.stop();
    let unknown = w.run(["toggle", "alpha", "2", "--push"]);
    assert_eq!(
        (unknown.code, &unknown.data()["sync"]["error"]["code"]),
        (5, &json!("session_service"))
    );
    let missing = w
        .cmd(["toggle", "alpha", "2", "--push"])
        .env("ICLOUD_NOTES_SYNC_BIN", w.scratch().join("no-engine"))
        .run();
    assert_eq!(
        (missing.code, &missing.data()["sync"]["error"]["code"]),
        (5, &json!("sync_tool_missing"))
    );
    assert_eq!(w.stub_log(), ["sync", "sync"]);
}

// ---- clone ---------------------------------------------------------------------------------------

#[test]
fn clone_fills_a_new_vault_as_the_signed_in_account() {
    let Some(w) = World::new() else { return };
    let (engine, log) = w.recording_engine();
    let vault = w.vault.display().to_string();
    let cloned = w
        .cmd(["clone"])
        .env("ICLOUD_NOTES_SYNC_BIN", &engine)
        .env("ICLOUD_NOTES_SYNC_STUB_CLONE", "1")
        .run();
    let command = format!("--json clone --account {DSID} --non-interactive {vault}");
    assert_eq!(
        cloned.ok(),
        json!({
            "ok": true,
            "runs": [ran("clone", 0, json!({"written": 1}))],
            "log": format!("$ icloud-notes-sync {command}\n(exit 0)\nCloned 1 notes"),
        })
    );
    assert_eq!(cloned.summary(), "clone done");
    assert_eq!(w.run(["clone", "--count"]).code, 2, "cloned once");
    let ran_engine = fs::read_to_string(&log).unwrap();
    assert!(
        ran_engine.contains(&format!("cwd={vault} lock=yes argv={command}\n")),
        "{ran_engine}"
    );
    assert_eq!(w.read("Notes/Cloned.md"), "---\napple-note-id: id-c\n---\n# Cloned\n");
    assert_eq!(w.run(["--ids-only", "list"]).out, "Notes/Cloned.md\n");
    let (message, hint) = w.run(["clone"]).fails(2, "already_cloned");
    assert_eq!(message, format!("{vault} is already cloned"));
    assert_eq!(hint, "Run `icloud-notes pull` to fetch what changed.");
}

#[test]
fn clone_signed_out_does_nothing_but_make_the_directory() {
    let Some(w) = World::new() else { return };
    w.session.set_signed_in(false);
    w.run(["clone"]).fails(3, "sign_in_required");
    assert!(w.vault.is_dir() && !w.exists(".icloud-notes"));
    assert_eq!(w.session.sign_ins(), 0, "never a sign-in window");
}

#[test]
fn a_clone_that_fails_reports_the_engines_error() {
    let Some(w) = World::new() else { return };
    let partial = stub(w.cmd(["clone"]), &[("CLONE", "partial")]).run();
    let data = failed(&partial, 1, "cloudkit_request_failed");
    assert_eq!(
        data["error"],
        json!({"code": "cloudkit_request_failed", "message": "Attachment download failed: HTTP 410"})
    );
    assert_eq!(partial.summary(), "clone failed: Attachment download failed: HTTP 410");
    // What it wrote is there, for a pull to finish.
    assert!(w.exists("Notes/Cloned.md"));
    w.run(["pull"]).ok();
}
