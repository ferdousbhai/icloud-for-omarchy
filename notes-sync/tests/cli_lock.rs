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
        .args(["--json", "vault-info"])
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

fn json_error(o: &Output) -> Value {
    let line = String::from_utf8_lossy(&o.stderr)
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned();
    serde_json::from_str::<Value>(&line).unwrap_or(Value::Null)["error"].clone()
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
        .args(["--json", "vault-info", "/nonexistent/icloud-notes-vault"])
        .output()
        .unwrap();
    let info: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(info["lockPath"], "/run/user/test/icloud-notes-14b8d5b025dfa0cb.lock");
    assert_eq!(info["cloned"], false);

    // Without a runtime directory, beside the vault.
    let o = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .env_remove("XDG_RUNTIME_DIR")
        .args(["--json", "vault-info", "/nonexistent/icloud-notes-vault"])
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
        let (runtime, vault) = (tmp.path().to_owned(), vault.clone());
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
        .args(["--json", "vault-info"])
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
        .args(["--json", "vault-info"])
        .current_dir(vault.join(".icloud-md"))
        .output()
        .unwrap();
    let inside: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(inside["notes"], info["notes"]);
}
