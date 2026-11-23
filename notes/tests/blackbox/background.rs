//! `icloud-notes --sync`, what the systemd timer runs every minute: one step
//! of the engine's `background`, with a line for the journal only when there
//! is something to say. Everything it prints goes to stdout.

use crate::world::World;

/// `--sync` with the stub switched by `env`: its exit code and what it printed.
fn background(w: &World, env: &[(&str, &str)]) -> (i32, String) {
    let run = env
        .iter()
        .fold(w.cmd(["--sync"]), |c, (k, v)| {
            c.env(&format!("ICLOUD_NOTES_SYNC_STUB_{k}"), v)
        })
        .run();
    assert_eq!(run.err, "", "{run:#?}");
    (run.code, run.out)
}

const RAN: &str = "$ icloud-notes-sync --quiet background --every 900\n";
const HINT: &str = "run `icloud-session sign-in` (a person signs in in the window)";

#[test]
fn a_vault_that_is_not_cloned_is_nothing_to_sync() {
    let Some(w) = World::new() else { return };
    assert_eq!(background(&w, &[]), (0, String::new()));
    assert!(!w.vault.exists() && !w.lock_path().exists());
    assert_eq!(w.stub_log(), [""; 0]);
}

#[test]
fn a_sync_that_ran_is_logged() {
    let Some(w) = World::seeded() else { return };
    let vault = w.vault.display();
    assert_eq!(
        background(&w, &[]),
        (
            0,
            format!(
                "{RAN}Fetching changes from iCloud...\n(exit 0)\npush:\nstub push ok\npull:\nstub pull ok\nSynced {vault}.\n"
            )
        )
    );
    assert_eq!(background(&w, &[("STEP", "changes")]).0, 0);
    assert_eq!(w.stub_log(), ["background", "background"]);
    assert_eq!(
        background(&w, &[("PUSH_REFUSED", "1")]),
        (
            1,
            format!(
                "{RAN}Fetching changes from iCloud...\n(exit 1)\npush:\nNotes/Big.md: server rejected the delete: BAD_REQUEST\n\
                 Pushed 0 note(s)\npush failed: Notes/Big.md: server rejected the delete: BAD_REQUEST\npull:\nstub pull ok\n\
                 Sync failed for {vault}.\n"
            )
        )
    );
}

#[test]
fn nothing_due_is_nothing_to_say() {
    let Some(w) = World::seeded() else { return };
    for step in ["idle", "settling"] {
        assert_eq!(background(&w, &[("STEP", step)]), (0, String::new()), "{step}");
    }
}

#[test]
fn nobody_to_sync_as_is_a_skip() {
    let Some(mut w) = World::seeded() else { return };
    assert_eq!(
        background(&w, &[("STEP", "signed_out")]),
        (0, "sign in to iCloud required; skipped.\n".into())
    );
    assert_eq!(
        background(&w, &[("STEP", "session_unavailable")]),
        (
            0,
            "icloud-session is not available, so the sign-in is unknown; skipped.\n".into()
        )
    );
    // The sign-in is the engine's to ask about, when a sync is due: this
    // command runs it whatever icloud-session would say.
    w.session.set_signed_in(false);
    assert!(background(&w, &[]).1.ends_with(".\n"));
    w.session.stop();
    assert!(background(&w, &[]).1.starts_with(RAN));
    assert_eq!(w.stub_log(), ["background", "background"]);
}

#[test]
fn no_network_is_a_skip_in_one_line() {
    let Some(w) = World::seeded() else { return };
    assert_eq!(
        background(&w, &[("OFFLINE", "1")]),
        (
            0,
            "iCloud could not be reached (offline: dns error: no network); skipped, the next sync tries again.\n"
                .into()
        )
    );
    assert_eq!(background(&w, &[("PUSH_FAIL", "network")]).0, 0);
}

#[test]
fn a_refused_sign_in_pauses_the_sync() {
    let Some(w) = World::seeded() else { return };
    assert_eq!(
        background(&w, &[("EXPIRED", "1")]),
        (
            1,
            format!(
                "{RAN}(exit 1)\npush failed: sign in to iCloud required\n{HINT}\n\
                 iCloud refused the sign-in; icloud-session was told. Sync paused until you sign in.\n"
            )
        )
    );
    assert_eq!(w.session.reports(), 1);
    // Apple refused a session icloud-session still holds: nobody is told again.
    let rejected = "Apple refused the iCloud session although icloud-sessiond still holds it (HTTP 401)";
    assert_eq!(
        background(&w, &[("EXPIRED", "1"), ("SIGNIN", "rejected")]),
        (
            1,
            format!(
                "{RAN}(exit 1)\npush failed: {rejected}\n{HINT}\n\
                 Apple refused the iCloud session icloud-session holds; sign in again ({HINT}).\n"
            )
        )
    );
    // The push failed for another reason, then the pull was rejected: the
    // rejection is what is logged, not a report nobody made.
    let (code, out) = background(
        &w,
        &[
            ("PUSH_FAIL", "cloudkit_request_failed"),
            ("PULL_FAIL", "session_rejected"),
        ],
    );
    assert_eq!(code, 1);
    assert!(
        out.ends_with("Apple refused the iCloud session icloud-session holds; sign in again ().\n"),
        "{out}"
    );
    assert!(!out.contains("icloud-session was told"), "{out}");
    assert_eq!(w.session.reports(), 1);
}

#[test]
fn a_missing_engine_is_an_error() {
    let Some(w) = World::seeded() else { return };
    let run = w
        .cmd(["--sync"])
        .env("ICLOUD_NOTES_SYNC_BIN", w.scratch().join("no-engine"))
        .run();
    assert_eq!(
        (run.code, run.out.as_str(), run.err.as_str()),
        (
            1,
            "the sync engine (icloud-notes-sync) is missing. (Reinstall icloud-notes: sudo pacman -S icloud-notes)\n",
            ""
        )
    );
}

#[test]
fn a_lock_that_cannot_be_opened_is_an_error() {
    let Some(w) = World::seeded() else { return };
    let nowhere = w.scratch().join("no-such-dir");
    let name = w.lock_path().file_name().unwrap().to_str().unwrap().to_string();
    let run = w.cmd(["--sync"]).env("XDG_RUNTIME_DIR", &nowhere).run();
    assert_eq!(
        (run.code, run.out),
        (
            1,
            format!(
                "Could not open the sync lock {}/{name}; not syncing.\n",
                nowhere.display()
            )
        )
    );
    assert_eq!(w.stub_log(), [""; 0]);
}
