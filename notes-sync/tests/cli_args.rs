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

/// A machine-mode failure: exactly one JSON document on stdout,
/// `{"ok":false,"error":{"code","message","hint"?}}`, and nothing on
/// stderr. Returns the `error` object.
fn machine_error(o: &Output) -> Value {
    assert_eq!(stderr(o), "", "stderr must be empty");
    let doc: Value = serde_json::from_str(&stdout(o)).expect("one JSON document on stdout");
    let mut keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["error", "ok"], "{doc}");
    assert_eq!(doc["ok"], false);
    let error = doc["error"].clone();
    for key in error.as_object().unwrap().keys() {
        assert!(["code", "message", "hint"].contains(&key.as_str()), "{key} in {error}");
    }
    error
}

/// clap's plain `<bin> <version>` in every mode, as in every iCloud tool.
#[test]
fn version_is_plain_text_in_every_mode() {
    let want = format!("icloud-notes-sync {}", env!("CARGO_PKG_VERSION"));
    for args in [
        &["--version"][..],
        &["-V"],
        &["--styled", "--version"],
        &["--quiet", "--version"],
        &["--version", "--json"],
    ] {
        let o = run_here(args);
        assert_eq!(o.status.code(), Some(0), "{args:?}");
        assert_eq!(stdout(&o).trim(), want, "{args:?}");
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
fn usage_error_exits_2_with_the_error_object_on_stdout() {
    for flag in ["--json", "--quiet"] {
        let o = run_here(&[flag, "pull", "--nope"]);
        assert_eq!(o.status.code(), Some(2));
        let error = machine_error(&o);
        assert_eq!(error["code"], "usage");
        assert_eq!(error["hint"], "run `icloud-notes-sync --help`");
    }
    // Piped without a flag: still the machine form.
    let piped = run_here(&["pull", "--nope"]);
    assert_eq!(piped.status.code(), Some(2));
    assert_eq!(machine_error(&piped)["code"], "usage");

    let human = run_here(&["--styled", "pull", "--nope"]);
    assert_eq!(human.status.code(), Some(2));
    assert!(stdout(&human).is_empty());
    assert!(stderr(&human).starts_with("error:"), "{}", stderr(&human));
}

#[test]
fn no_command_is_a_usage_error() {
    let o = run_here(&["--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(machine_error(&o)["message"], "no command given");
    let human = run_here(&["--styled"]);
    assert_eq!(human.status.code(), Some(2));
    assert!(stderr(&human).contains("error: no command given"), "{}", stderr(&human));
}

#[test]
fn status_outside_a_vault_is_a_known_error() {
    let tmp = tempfile::tempdir().unwrap();
    let o = run(&["--json", "status"], tmp.path());
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(machine_error(&o)["code"], "not_cloned_directory");

    let human = run(&["--styled", "status"], tmp.path());
    assert_eq!(human.status.code(), Some(3));
    assert!(stdout(&human).is_empty());
    assert!(stderr(&human).starts_with("error: "), "{}", stderr(&human));
    assert!(
        stderr(&human).contains("\nhint: Run \"icloud-notes-sync clone"),
        "{}",
        stderr(&human)
    );
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
    let error = machine_error(&o);
    assert_eq!(error["code"], "session_service");
    assert!(error["hint"].is_string());
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
    let error = machine_error(&o);
    assert_eq!(error["code"], "usage");
    assert_eq!(error["hint"], "run `icloud-notes-sync --help`");
}

/// The `tiny-pull-update` recording's vault, which has one epoch of
/// history for `Notes/Test Note.md`; history reads only the vault.
fn history_vault() -> tempfile::TempDir {
    let vault = tempfile::tempdir().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential/expected/tiny-pull-update/vault"),
        vault.path(),
    );
    vault
}

fn history(vault: &Path, flags: &[&str]) -> Output {
    let mut args = flags.to_vec();
    args.extend(["history", "Notes/Test Note.md"]);
    args.push(vault.to_str().unwrap());
    run_here(&args)
}

/// Piped with no flag the result is the envelope, `ok` first; `--quiet` is
/// exactly its `data` (what the Notes app reads).
#[test]
fn piped_output_is_the_envelope_and_quiet_is_its_data() {
    let vault = history_vault();
    let piped = history(vault.path(), &[]);
    assert_eq!(piped.status.code(), Some(0), "{}", stderr(&piped));
    assert!(stdout(&piped).starts_with("{\n  \"ok\": true,"), "{}", stdout(&piped));
    let envelope: Value = serde_json::from_str(&stdout(&piped)).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["summary"], "1 epoch");
    assert_eq!(envelope["data"]["mode"], "epochs");
    assert_eq!(envelope["data"]["epochs"].as_array().unwrap().len(), 1);
    assert_eq!(stderr(&piped), "");

    let json = history(vault.path(), &["--json"]);
    assert_eq!(serde_json::from_str::<Value>(&stdout(&json)).unwrap(), envelope);

    let quiet = history(vault.path(), &["--quiet"]);
    assert_eq!(quiet.status.code(), Some(0));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&quiet)).unwrap(),
        envelope["data"]
    );
}

#[test]
fn ids_only_and_count_list_the_history() {
    let vault = history_vault();
    let quiet: Value = serde_json::from_str(&stdout(&history(vault.path(), &["--quiet"]))).unwrap();
    let id = quiet["epochs"][0]["id"].as_str().unwrap().to_owned();

    let ids = history(vault.path(), &["--ids-only"]);
    assert_eq!(ids.status.code(), Some(0));
    assert_eq!(stdout(&ids), format!("{id}\n"));
    let count = history(vault.path(), &["--count"]);
    assert_eq!(stdout(&count), "1\n");
    // Combined, by precedence: ids-only > count > quiet > json > styled.
    assert_eq!(
        stdout(&history(vault.path(), &["--json", "--count", "--styled"])),
        "1\n"
    );
    assert_eq!(
        stdout(&history(vault.path(), &["--count", "--ids-only"])),
        format!("{id}\n")
    );
}

#[test]
fn styled_history_is_the_text_report() {
    let vault = history_vault();
    let o = history(vault.path(), &["--styled"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("changed: note"), "{}", stdout(&o));
    assert!(!stdout(&o).contains('{'));
}

/// vault-info has nothing to list by id: --ids-only prints its data with a
/// note on stderr; --count is the number of tracked notes.
#[test]
fn vault_info_counts_its_notes() {
    let vault = history_vault();
    let count = run_here(&["--count", "vault-info", vault.path().to_str().unwrap()]);
    assert_eq!(stdout(&count), "1\n");
    let ids = run_here(&["--ids-only", "vault-info", vault.path().to_str().unwrap()]);
    let data: Value = serde_json::from_str(&stdout(&ids)).unwrap();
    assert_eq!(data["cloned"], true);
    assert!(stderr(&ids).starts_with("note: "), "{}", stderr(&ids));
}

/// A failed `sync` half is reported inside the data (no separate error
/// object), and the run exits with the worse half: here both halves find
/// no vault (exit 3).
#[test]
fn a_failed_sync_half_is_in_the_data_only() {
    let tmp = tempfile::tempdir().unwrap();
    let o = run(&["--quiet", "sync", "."], tmp.path());
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(!stderr(&o).contains("\"ok\""), "{}", stderr(&o));
    let data: Value = serde_json::from_str(&stdout(&o)).expect("one JSON document on stdout");
    for half in ["push", "pull"] {
        assert_eq!(data[half]["ok"], false, "{data}");
        assert_eq!(data[half]["exit_code"], 3, "{data}");
        assert_eq!(data[half]["error"]["code"], "not_cloned_directory", "{data}");
    }
    assert!(data["vault_info"].is_object(), "{data}");

    let envelope: Value = serde_json::from_str(&stdout(&run(&["--json", "sync", "."], tmp.path()))).unwrap();
    assert_eq!(envelope["data"]["push"], data["push"]);
    assert_eq!(
        envelope["summary"],
        "push failed (not_cloned_directory), pull failed (not_cloned_directory)"
    );

    let human = run(&["--styled", "sync", "."], tmp.path());
    assert_eq!(human.status.code(), Some(3));
    assert_eq!(stderr(&human).matches("error: ").count(), 2, "{}", stderr(&human));
}
