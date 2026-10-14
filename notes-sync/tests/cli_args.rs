//! The CLI, running the built binary: arguments, help, and the exit-code
//! contract. Originally derived from icloud-md's tests.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn run(args: &[&str], cwd: &Path) -> Output {
    let runtime = tempfile::tempdir().unwrap(); // vault locks land here, not in the real runtime dir
    Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(args)
        .current_dir(cwd)
        .env_remove("ICLOUD_NOTES_SYNC_CASSETTE")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .expect("binary runs")
}

fn run_here(args: &[&str]) -> Output {
    run(args, Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn version_prints_the_package_version() {
    let o = run_here(&["--version"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(stdout(&o).trim(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn json_version_prints_an_object_in_either_flag_order() {
    for args in [["--json", "--version"], ["--version", "--json"]] {
        let o = run_here(&args);
        let parsed: Value = serde_json::from_str(&stdout(&o)).unwrap();
        assert_eq!(parsed, serde_json::json!({"version": env!("CARGO_PKG_VERSION")}));
    }
}

#[test]
fn vault_shape_flags_live_on_the_commands_that_own_them() {
    let clone = stdout(&run_here(&["clone", "--help"]));
    assert!(clone.contains("--filename-as-title"));
    assert!(clone.contains("--account"));
    assert!(clone.contains("--non-interactive"));

    let pull = stdout(&run_here(&["pull", "--help"]));
    assert!(pull.contains("--defer-renames"));
    assert!(!pull.contains("--filename-as-title"));
}

#[test]
fn usage_error_exits_2_with_a_json_payload() {
    let o = run_here(&["--json", "pull", "--nope"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    let payload: Value = serde_json::from_str(&stderr(&o)).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "usage");

    let human = run_here(&["pull", "--nope"]);
    assert_eq!(human.status.code(), Some(2));
}

#[test]
fn status_outside_a_vault_is_a_known_error() {
    let tmp = tempfile::tempdir().unwrap();
    let o = run(&["--json", "status"], tmp.path());
    assert_eq!(o.status.code(), Some(3));
    assert!(stdout(&o).is_empty());
    let payload: Value = serde_json::from_str(&stderr(&o)).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "not_cloned_directory");
}

/// icloud-sessiond out of reach is `session_service`, exit 5 (a service
/// unavailable), not an internal error.
#[test]
fn an_unreachable_session_daemon_is_session_service_exit_5() {
    let vault = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential/expected/tiny-clone/vault"),
        vault.path(),
    );
    let runtime = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(["--json", "pull"])
        .arg(vault.path())
        .env_remove("ICLOUD_NOTES_SYNC_CASSETTE")
        .env_remove("ICLOUD_SESSION_MOCK")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/icloud-notes-sync-test-bus",
        )
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .expect("binary runs");
    assert_eq!(o.status.code(), Some(5), "{}", stderr(&o));
    let payload: Value = serde_json::from_str(stderr(&o).lines().last().unwrap()).unwrap();
    assert_eq!(payload["error"]["code"], "session_service");
    assert!(payload["error"]["hint"].is_string());
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn diff_with_an_invalid_ref_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let o = run(&["--json", "diff", "Note.md", "a..b..c"], tmp.path());
    assert_eq!(o.status.code(), Some(2));
    let payload: Value = serde_json::from_str(&stderr(&o)).unwrap();
    assert_eq!(payload["error"]["code"], "usage");
}
