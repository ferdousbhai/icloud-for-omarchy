//! The vault's lock: the file every program that changes the vault takes
//! (the window, a command, the background sync, the engine), where it is,
//! what its holder writes in it, who waits for whom, and how the engine a
//! command starts gets the lock the command holds.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::world::{USAGE_HINT, World};

const WINDOW: &str = "Notes (pid 4242)";
const BACKGROUND: &str = "a background sync (icloud-notes --sync, pid 7)";

/// Waits for the lock file to hold a holder's description, and gives it.
fn holder(w: &World) -> String {
    let started = Instant::now();
    loop {
        let text = fs::read_to_string(w.lock_path()).unwrap_or_default();
        if !text.is_empty() {
            return text;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "nobody took the lock");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_lock_file_is_named_for_the_vault_and_stays() {
    let Some(w) = World::seeded() else { return };
    assert!(!w.lock_path().exists());
    // Reading takes no lock.
    w.run(["list"]).ok();
    w.run(["read", "alpha"]).ok();
    assert!(!w.lock_path().exists());
    w.json(["new", "Locked once"]).ok();
    // In the runtime directory, named by the FNV-1a hash of the vault's
    // path; kept, private, and emptied when let go.
    let meta = fs::metadata(w.lock_path()).expect("the lock file is where the rule puts it");
    assert_eq!((meta.len(), meta.permissions().mode() & 0o777), (0, 0o600));
    assert_eq!(fs::read_dir(w.scratch().join("run")).unwrap().count(), 1);
    // A vault reached by another spelling of its path has the same lock.
    let _held = w.hold_lock(WINDOW);
    let spelled = format!("{}/vault/../vault/Notes/..", w.scratch().display());
    w.json(["--vault", &spelled, "new", "Other spelling"])
        .fails(1, "vault_busy");
}

#[test]
fn without_a_runtime_directory_the_lock_is_beside_the_vault() {
    let Some(w) = World::seeded() else { return };
    w.cmd(["new", "No runtime dir"]).env("XDG_RUNTIME_DIR", "").run().ok();
    let name = w.lock_path().file_name().unwrap().to_str().unwrap().to_string();
    assert!(w.scratch().join(format!(".{name}")).is_file());
    assert!(!w.lock_path().exists());
}

#[test]
fn a_lock_that_cannot_be_opened_is_an_error() {
    let Some(w) = World::seeded() else { return };
    let nowhere = w.scratch().join("no-such-dir");
    let name = w.lock_path().file_name().unwrap().to_str().unwrap().to_string();
    let (message, hint) = w
        .cmd(["new", "Unlocked"])
        .env("XDG_RUNTIME_DIR", &nowhere)
        .run()
        .fails(1, "vault_lock");
    assert_eq!(
        (message, hint.as_str()),
        (
            format!("could not open the vault's lock {}/{name}", nowhere.display()),
            ""
        )
    );
    assert!(!w.exists("Notes/Unlocked.md"));
}

#[test]
fn the_window_is_never_waited_for() {
    let Some(w) = World::seeded() else { return };
    let _held = w.hold_lock(WINDOW);
    let busy = w.json(["new", "While open"]);
    let (message, hint) = busy.fails(1, "vault_busy");
    assert_eq!(
        message,
        "Notes (pid 4242) is open and owns the notes vault while it is open."
    );
    assert_eq!(
        hint,
        "Make the change in Notes, or quit it and retry (or --wait SECS for it to close)."
    );
    assert!(busy.elapsed < Duration::from_millis(900), "at once: {busy:#?}");
    w.text(["delete", "alpha", "--yes"]).fails_in_text(1, &message, &hint);
    assert!(!w.exists("Notes/While open.md") && w.exists("Notes/Alpha.md"));
    // Unless asked to.
    let waited = w.json(["--wait", "1", "new", "While open"]);
    waited.fails(1, "vault_busy");
    assert!(waited.elapsed >= Duration::from_millis(900), "{waited:#?}");
    // A sync is refused the same way, before the engine runs.
    for command in ["sync", "pull", "push"] {
        w.json([command]).fails(1, "vault_busy");
    }
    assert_eq!(w.stub_log(), [""; 0]);
}

#[test]
fn reading_goes_on_while_the_vault_is_locked() {
    let Some(w) = World::seeded() else { return };
    let _held = w.hold_lock(WINDOW);
    for args in [
        vec!["list"],
        vec!["folders"],
        vec!["read", "alpha"],
        vec!["search", "eggs"],
    ] {
        w.run(&args).ok();
    }
    // The engine's own read-only commands too: it locks where it writes.
    for args in [
        vec!["history", "alpha"],
        vec!["diff", "alpha", "e9"],
        vec!["push", "--dry-run"],
    ] {
        assert_eq!(w.run(&args).code, 0, "{args:?}");
    }
    let lock = w.json(["status"]).ok()["lock"].clone();
    assert_eq!(lock, json!({"held_by": WINDOW, "app_open": true}));
    assert!(w.text(["status"]).out.contains("\nLock:        Notes (pid 4242)\n"));
    // status only looked: the holder's words are still in the file.
    assert_eq!(fs::read_to_string(w.lock_path()).unwrap(), WINDOW);
}

#[test]
fn another_holder_is_waited_for_as_long_as_asked() {
    let Some(w) = World::seeded() else { return };
    let held = w.hold_lock(BACKGROUND);
    assert_eq!(
        w.json(["status"]).ok()["lock"],
        json!({"held_by": BACKGROUND, "app_open": false})
    );
    let waited = w.json(["--wait", "1", "new", "Later"]);
    let (message, hint) = waited.fails(1, "vault_busy");
    assert_eq!(message, format!("{BACKGROUND} holds the notes vault."));
    assert_eq!(hint, "Retry later, or pass --wait SECS.");
    assert!(waited.elapsed >= Duration::from_millis(900), "{waited:#?}");
    let at_once = w.json(["--wait=0", "new", "Later"]);
    at_once.fails(1, "vault_busy");
    assert!(at_once.elapsed < Duration::from_millis(900), "{at_once:#?}");
    assert!(!w.exists("Notes/Later.md"));
    // Let go while a command waits: it goes ahead.
    thread::scope(|s| {
        s.spawn(|| {
            thread::sleep(Duration::from_millis(700));
            drop(held);
        });
        let after = w.json(["--wait", "20", "new", "Later"]);
        after.ok();
        assert!(after.elapsed >= Duration::from_millis(600), "{after:#?}");
    });
    assert_eq!(w.read("Notes/Later.md"), "# Later\n");
}

#[test]
fn a_holder_that_wrote_nothing_is_another_sync() {
    let Some(w) = World::seeded() else { return };
    let _held = w.hold_lock("");
    assert_eq!(
        w.json(["status"]).ok()["lock"],
        json!({"held_by": "another sync", "app_open": false})
    );
    let (message, _) = w.json(["--wait", "0", "new", "Later"]).fails(1, "vault_busy");
    assert_eq!(message, "another sync holds the notes vault.");
    assert!(w.text(["status"]).out.contains("\nLock:        another sync\n"));
}

#[test]
fn wait_wants_whole_seconds() {
    let Some(w) = World::seeded() else { return };
    for bad in ["x", "1.5", "-1", "", "1s"] {
        let (message, hint) = w.json(["new", "T", "--wait", bad]).fails(2, "usage");
        assert_eq!(
            (message, hint.as_str()),
            (format!("--wait wants whole seconds, not \"{bad}\""), USAGE_HINT)
        );
    }
    assert!(!w.exists("Notes/T.md"));
    // A command that takes no lock does not look at it.
    w.run(["--wait", "x", "list"]).ok();
}

#[test]
fn a_command_says_who_it_is_while_it_holds_the_lock() {
    let Some(w) = World::seeded() else { return };
    let gate = w.scratch().join("gate");
    fs::write(&gate, "").unwrap();
    thread::scope(|s| {
        let sync = s.spawn(|| w.cmd(["sync"]).env("ICLOUD_NOTES_SYNC_STUB_GATE", &gate).run());
        let said = holder(&w);
        let pid = said
            .strip_prefix("the icloud-notes command line (pid ")
            .and_then(|p| p.strip_suffix(')'))
            .expect(&said);
        assert!(pid.parse::<u32>().is_ok(), "{said}");
        assert_eq!(
            w.json(["status"]).ok()["lock"],
            json!({"held_by": said, "app_open": false})
        );
        let (message, _) = w.json(["--wait", "0", "new", "Meanwhile"]).fails(1, "vault_busy");
        assert_eq!(message, format!("{said} holds the notes vault."));
        fs::remove_file(&gate).unwrap();
        sync.join().unwrap().ok();
    });
    assert_eq!(fs::read_to_string(w.lock_path()).unwrap(), "");
    assert_eq!(w.json(["status"]).ok()["lock"]["held_by"], Value::Null);
}

#[test]
fn the_background_sync_says_who_it_is_and_never_waits() {
    let Some(w) = World::seeded() else { return };
    let gate = w.scratch().join("gate");
    fs::write(&gate, "").unwrap();
    thread::scope(|s| {
        let sync = s.spawn(|| w.cmd(["--sync"]).env("ICLOUD_NOTES_SYNC_STUB_GATE", &gate).run());
        let said = holder(&w);
        let pid = said
            .strip_prefix("a background sync (icloud-notes --sync, pid ")
            .and_then(|p| p.strip_suffix(')'))
            .expect(&said);
        assert!(pid.parse::<u32>().is_ok(), "{said}");
        // A second one finds the vault busy and leaves, saying nothing.
        let second = w.run(["--sync"]);
        assert_eq!((second.code, second.out.as_str(), second.err.as_str()), (0, "", ""));
        assert!(second.elapsed < Duration::from_secs(5));
        fs::remove_file(&gate).unwrap();
        assert_eq!(sync.join().unwrap().code, 0);
    });
    assert_eq!(w.stub_log(), ["background"]);
    // Nor does it run while the window is open.
    let _held = w.hold_lock(WINDOW);
    let skipped = w.run(["--sync"]);
    assert_eq!((skipped.code, skipped.out.as_str(), skipped.err.as_str()), (0, "", ""));
    assert_eq!(w.stub_log(), ["background"]);
}

#[test]
fn the_engine_is_handed_the_lock_the_command_holds() {
    let Some(w) = World::seeded() else { return };
    let (engine, log) = w.recording_engine();
    for args in [
        vec!["sync"],
        vec!["pull"],
        vec!["push"],
        vec!["--sync"],
        vec!["toggle", "alpha", "2", "--push"],
    ] {
        assert_eq!(
            w.cmd(&args).env("ICLOUD_NOTES_SYNC_BIN", &engine).run().code,
            0,
            "{args:?}"
        );
    }
    let runs = fs::read_to_string(&log).unwrap();
    let syncing: Vec<&str> = runs.lines().filter(|l| !l.contains(" vault-info ")).collect();
    // Started in the vault, with ICLOUD_NOTES_LOCK_FD naming the descriptor.
    let at = format!("cwd={} lock=yes argv=", w.vault.display());
    assert_eq!(
        syncing,
        [
            format!("{at}--quiet sync"),
            format!("{at}--json pull"),
            format!("{at}--json push"),
            format!("{at}--quiet background --every 900"),
            format!("{at}--quiet sync"),
        ]
    );
    // And the real engine takes it: handed the descriptor it gets the vault,
    // without it it finds the vault busy (the stub runs both and fails otherwise).
    let taken = w.cmd(["pull"]).env("ICLOUD_NOTES_SYNC_STUB_TAKE_LOCK", "1").run();
    assert_eq!(taken.ok()["ok"], true, "{taken:#?}");
}
