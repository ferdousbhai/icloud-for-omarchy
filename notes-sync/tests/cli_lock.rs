//! The vault lock shared with the Notes app (`cmd::lock`) and `vault-info`,
//! through the built binary.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

fn engine(runtime: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"));
    command
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("ICLOUD_NOTES_LOCK_FD")
        .env_remove("ICLOUD_NOTES_SYNC_CASSETTE");
    command
}

fn lock_path(runtime: &Path, vault: &Path) -> String {
    let o = engine(runtime)
        .args(["--quiet", "vault-info"])
        .arg(vault)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let info: Value = serde_json::from_slice(&o.stdout).unwrap();
    info["lockPath"].as_str().unwrap().to_owned()
}

/// Holds the lock the way the Notes app does: its own open of the file,
/// flocked, with its description written in.
fn hold(path: &str, owner: &str) -> File {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    file.try_lock().unwrap();
    file.set_len(0).unwrap();
    file.write_all(owner.as_bytes()).unwrap();
    file
}

/// The contract error object a machine-mode failure prints: the whole of
/// stdout (stderr carries no JSON).
fn json_error(o: &Output) -> Value {
    assert!(
        !String::from_utf8_lossy(&o.stderr).contains("\"ok\""),
        "an error object on stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let doc = serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null);
    assert_eq!(doc["ok"], false, "{doc}");
    doc["error"].clone()
}

/// `restore` on a vault that was never cloned: past the lock it fails with
/// `not_cloned_directory`, so the error code says whether it got the lock.
fn restore(runtime: &Path, vault: &Path, extra: &[&str]) -> Output {
    engine(runtime)
        .arg("--json")
        .args(extra)
        .args(["restore", "A.md"])
        .arg(vault)
        .output()
        .unwrap()
}

#[test]
fn the_lock_path_is_the_apps() {
    // The same vector notes/tests/backend_test.cpp checks NotesBackend::lockPath() against.
    let o = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .env("XDG_RUNTIME_DIR", "/run/user/test")
        .args(["--quiet", "vault-info", "/nonexistent/icloud-notes-vault"])
        .output()
        .unwrap();
    let info: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(info["lockPath"], "/run/user/test/icloud-notes-14b8d5b025dfa0cb.lock");
    assert_eq!(info["cloned"], false);

    // Without a runtime directory, beside the vault.
    let o = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .env_remove("XDG_RUNTIME_DIR")
        .args(["--quiet", "vault-info", "/nonexistent/icloud-notes-vault"])
        .output()
        .unwrap();
    let info: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(info["lockPath"], "/nonexistent/.icloud-notes-14b8d5b025dfa0cb.lock");
}

#[test]
fn a_run_refuses_at_once_while_the_notes_window_holds_the_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    let _app = hold(&lock_path(tmp.path(), &vault), "Notes (pid 4242)");

    let started = Instant::now();
    let o = restore(tmp.path(), &vault, &[]);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(o.status.code(), Some(1));
    let error = json_error(&o);
    assert_eq!(error["code"], "vault_busy");
    assert_eq!(
        error["message"],
        "Notes (pid 4242) is open and owns the notes vault while it is open."
    );
    assert!(error["hint"].as_str().unwrap().contains("--wait"));
}

#[test]
fn a_run_waits_for_another_holder_as_long_as_wait_says() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    let path = lock_path(tmp.path(), &vault);
    let background = hold(&path, "a background sync (icloud-notes --sync, pid 7)");

    let started = Instant::now();
    let o = restore(tmp.path(), &vault, &["--wait", "1"]);
    assert!(started.elapsed() >= Duration::from_secs(1));
    let error = json_error(&o);
    assert_eq!(error["code"], "vault_busy");
    assert_eq!(
        error["message"],
        "a background sync (icloud-notes --sync, pid 7) holds the notes vault."
    );

    // Released while it waits: the run goes ahead.
    let waiting = std::thread::spawn({
        let (runtime, vault) = (tmp.path().to_owned(), vault);
        move || restore(&runtime, &vault, &["--wait", "10"])
    });
    std::thread::sleep(Duration::from_millis(500));
    drop(background);
    assert_eq!(json_error(&waiting.join().unwrap())["code"], "not_cloned_directory");
    // And it cleared its description when done.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn read_only_commands_take_no_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    let _app = hold(&lock_path(tmp.path(), &vault), "Notes (pid 4242)");
    for args in [
        &["status"][..],
        &["history", "A.md"],
        &["diff", "A.md", "x"],
        &["push", "--dry-run"],
    ] {
        let o = engine(tmp.path())
            .arg("--json")
            .args(args)
            .arg(&vault)
            .output()
            .unwrap();
        assert_eq!(json_error(&o)["code"], "not_cloned_directory", "{args:?}");
    }
}

/// `sh -c` holding the lock on fd 9 (as the app does) and running the engine
/// with `ICLOUD_NOTES_LOCK_FD=9`.
fn restore_under_fd9(runtime: &Path, vault: &Path, lock: &str, flock: bool) -> Output {
    let script = format!(
        "exec 9<>\"$LOCK\"; {} ICLOUD_NOTES_LOCK_FD=9 exec \"$ENGINE\" --json restore A.md \"$VAULT\"",
        if flock { "flock -n 9 || exit 99;" } else { "" }
    );
    Command::new("sh")
        .args(["-c", &script])
        .env("LOCK", lock)
        .env("ENGINE", env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .env("VAULT", vault)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .unwrap()
}

#[test]
fn a_caller_holding_the_lock_passes_it_down() {
    if Command::new("flock").arg("--version").output().is_err() {
        eprintln!("no flock(1); skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    let path = lock_path(tmp.path(), &vault);
    File::create(&path).unwrap();

    let o = restore_under_fd9(tmp.path(), &vault, &path, true);
    assert_eq!(
        json_error(&o)["code"],
        "not_cloned_directory",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    // A descriptor that doesn't hold the lock is no way around it.
    let _app = hold(&path, "Notes (pid 4242)");
    let o = restore_under_fd9(tmp.path(), &vault, &path, false);
    assert_eq!(json_error(&o)["code"], "vault_busy");
    // Nor is one on another file.
    let other = tmp.path().join("other.lock");
    let o = restore_under_fd9(tmp.path(), &vault, other.to_str().unwrap(), true);
    assert_eq!(json_error(&o)["code"], "vault_busy");
}

#[test]
fn vault_info_reports_what_the_app_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(vault.join(".icloud-md/base")).unwrap();
    std::fs::write(
        vault.join(".icloud-md/state.json"),
        r#"{"layoutVersion":3,"titleMode":"filename","notes":{
            "id-a":{"file":"Notes/A.md","recordChangeTag":"t","modificationDate":1},
            "id-f":{"file":"F.md","recordChangeTag":"t","modificationDate":1,"unpublishableReason":"is too large"}},
          "folders":{"DefaultFolder-CloudKit":{"name":"Notes","dirName":"Notes"}}}"#,
    )
    .unwrap();
    std::fs::write(vault.join(".icloud-md/base/id-a.md"), "body\n").unwrap();

    let o = engine(tmp.path())
        .args(["--quiet", "vault-info"])
        .arg(&vault)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let info: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(info["cloned"], true);
    assert_eq!(info["titleMode"], "filename");
    assert_eq!(info["defaultFolderDir"], "Notes");
    assert_eq!(info["stateDir"], vault.join(".icloud-md").to_str().unwrap());
    assert_eq!(info["stateFile"], vault.join(".icloud-md/state.json").to_str().unwrap());
    assert_eq!(
        info["notes"],
        serde_json::json!([
            {"id": "id-a", "file": "Notes/A.md", "baseFile": ".icloud-md/base/id-a.md"},
            {"id": "id-f", "file": "F.md", "readOnlyReason": "is too large", "baseFile": ".icloud-md/base/id-f.md"},
        ])
    );

    // From inside the vault, without naming it.
    let o = engine(tmp.path())
        .args(["--quiet", "vault-info"])
        .current_dir(vault.join(".icloud-md"))
        .output()
        .unwrap();
    let inside: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(inside["notes"], info["notes"]);
}

/// Every path under `dir` with its contents, to show a run wrote nothing.
fn snapshot(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push((p.clone(), Vec::new()));
                stack.push(p);
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn set_mtime(path: &Path, ago: Duration) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - ago).unwrap();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// `background`, which the Notes app runs every minute: while the open app
/// holds the lock and with no session bus to reach icloud-session through
/// (so no way to the network), it decides from the vault alone, writes
/// nothing when nothing is due, and leaves a layout 3 vault where it is. A
/// change made since the last sync is due; with icloud-session out of reach
/// it is skipped before the lock is taken, and recorded as a failed sync,
/// so the same change waits for the next due one.
#[test]
fn background_decides_with_no_lock_and_no_network() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(vault.join(".icloud-md/base")).unwrap();
    std::fs::create_dir_all(vault.join("Notes")).unwrap();
    std::fs::write(
        vault.join(".icloud-md/state.json"),
        r#"{"layoutVersion":3,"titleMode":"in-body","account":{"appleId":"a@example.com","dsid":"1"},"notes":{
            "id-a":{"file":"Notes/A.md","recordChangeTag":"t","modificationDate":1,"folderRecordName":"DefaultFolder-CloudKit"}},
          "folders":{"DefaultFolder-CloudKit":{"name":"Notes","dirName":"Notes"}}}"#,
    )
    .unwrap();
    std::fs::write(vault.join(".icloud-md/base/id-a.md"), "body\n").unwrap();
    std::fs::write(vault.join("Notes/A.md"), "body\n").unwrap();
    let record = vault.join(".icloud-md/background.json");
    std::fs::write(
        &record,
        serde_json::json!({"lastRunMs": now_ms() - 120_000, "lastOk": true}).to_string(),
    )
    .unwrap();
    let _app = hold(&lock_path(tmp.path(), &vault), "Notes (pid 4242)");
    let background = || {
        let o = engine(tmp.path())
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/icloud-notes-test-bus",
            )
            .env_remove("ICLOUD_SESSION_MOCK")
            .args(["--quiet", "background"])
            .arg(&vault)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<Value>(&o.stdout).unwrap()
    };
    let before = snapshot(&vault);
    assert_eq!(background(), serde_json::json!({"synced": false, "step": "idle"}));
    assert_eq!(snapshot(&vault), before, "a run with nothing to do wrote to the vault");

    std::fs::write(vault.join("Notes/A.md"), "body edited\n").unwrap();
    assert_eq!(background(), serde_json::json!({"synced": false, "step": "settling"}));

    set_mtime(&vault.join("Notes/A.md"), Duration::from_secs(30));
    assert_eq!(
        background(),
        serde_json::json!({"synced": false, "step": "session_unavailable"})
    );
    let recorded: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(recorded["lastOk"], false, "{recorded}");
    assert!(
        recorded["lastRunMs"].as_i64().unwrap() > now_ms() - 10_000,
        "{recorded}"
    );
    assert_eq!(background(), serde_json::json!({"synced": false, "step": "idle"}));
    assert!(!vault.join(".icloud-notes").exists(), "background migrated the vault");
}
