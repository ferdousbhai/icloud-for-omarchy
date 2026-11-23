//! Which first arguments are not the command line's: with none, or one that
//! is no command and no global flag, the binary is the app. Started here
//! on a display no one sees (the scratch world's Broadway server), it is
//! known by the vault's lock, which the window holds while it is open.

use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::world::{Started, World};

/// Waits for a window to hold the vault's lock; `false` if `started` ended
/// instead, or none did in time.
fn opens_the_window(w: &World, started: &mut Started) -> bool {
    let begun = Instant::now();
    while begun.elapsed() < Duration::from_secs(30) {
        if started.exited() {
            return false;
        }
        let lock = w.json(["status"]).ok()["lock"].clone();
        if lock["app_open"] == true {
            assert!(lock["held_by"].as_str().unwrap().starts_with("Notes (pid "), "{lock}");
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn no_arguments_open_the_app() {
    let Some(w) = World::seeded() else { return };
    let mut app = w.spawn([""; 0]);
    assert!(opens_the_window(&w, &mut app));
    // While it is open the command line reads, and leaves changes to it.
    w.run(["list"]).ok();
    let (message, _) = w.json(["new", "While open"]).fails("vault_busy");
    assert!(
        message.ends_with(" is open and owns the notes vault while it is open."),
        "{message}"
    );
    // A second launch shows the first one and ends.
    let second = w.cmd([""; 0]).run();
    assert_eq!(second.code, 0);
    assert!(!app.exited(), "the first is still open");
    drop(app);
    assert_eq!(
        w.json(["status"]).ok()["lock"],
        json!({"held_by": null, "app_open": false})
    );
}

#[test]
fn open_opens_the_app() {
    let Some(w) = World::seeded() else { return };
    let mut app = w.spawn(["open"]);
    assert!(opens_the_window(&w, &mut app));
}

#[test]
fn a_first_argument_that_is_no_command_opens_the_app() {
    let Some(w) = World::seeded() else { return };
    // Not usage errors: an unknown word, an unknown short option, and an
    // option of a command before any command.
    for first in ["bogus", "-x", "--folder=Notes"] {
        let mut app = w.spawn([first]);
        assert!(opens_the_window(&w, &mut app), "{first}");
    }
}

#[test]
fn without_a_session_bus_a_second_launch_still_ends() {
    let Some(w) = World::seeded() else { return };
    let mut app = w.spawn([""; 0]);
    assert!(opens_the_window(&w, &mut app));
    let second = w
        .cmd([""; 0])
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .run();
    assert_eq!(second.code, 0, "{second:?}");
    assert!(
        second.err.contains("Notes is open on this vault already."),
        "{second:?}"
    );
    assert!(!app.exited(), "the first is still open");
}

#[test]
fn panes_saved_narrower_than_their_least_still_open() {
    let Some(w) = World::seeded() else { return };
    let config = w.scratch().join("config/icloud-notes");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("window.json"), r#"{"folder_pane":10,"note_pane":10}"#).unwrap();
    let mut app = w.spawn([""; 0]);
    assert!(opens_the_window(&w, &mut app));
    // The panes are laid out after the lock is taken.
    thread::sleep(Duration::from_secs(1));
    assert!(!app.exited(), "the window is still open");
}

/// Polls `done` for up to 20 seconds.
fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let begun = Instant::now();
    while begun.elapsed() < Duration::from_secs(20) {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn opened_while_another_process_syncs_the_window_takes_the_vault_once_it_is_done() {
    let Some(w) = World::seeded() else { return };
    let held = w.hold_lock("a background sync (icloud-notes --sync, pid 1)");
    let mut app = w.spawn([""; 0]);
    thread::sleep(Duration::from_secs(2));
    assert!(!app.exited());
    assert_eq!(
        w.json(["status"]).ok()["lock"],
        json!({"held_by": "a background sync (icloud-notes --sync, pid 1)", "app_open": false})
    );
    assert!(w.stub_log().is_empty(), "its first sync waits: {:?}", w.stub_log());
    drop(held);
    assert!(opens_the_window(&w, &mut app));
    assert!(eventually(|| !w.stub_log().is_empty()), "the sync that waited ran");
}
