//! `leave`: the vault leaves iCloud for Omacloud (`icloud_app::leave`). It
//! refuses, changing nothing, until Omacloud can take the folder; then a last
//! sync, the state moves out, the marker goes, and nothing reaches iCloud
//! for that vault again.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn differential() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A stand-in `omacloud`: `status` answers `status`, anything else is
/// logged to `omacloud.log` and succeeds.
fn fake_omacloud(out: &Path, status: &str) -> PathBuf {
    let bin = out.join("omacloud");
    let log = out.join("omacloud.log");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nif [ \"$1\" = status ]; then cat <<'EOF'\n{status}\nEOF\nelse echo \"$@\" >> '{}'; echo '{{\"ok\":true,\"data\":null}}'; fi\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn ready_status(out: &Path) -> String {
    format!(
        r#"{{"ok":true,"data":{{"set_up":true,"folder":"{}","folders":null}}}}"#,
        out.display()
    )
}

/// The tiny clone at `out/vault`.
fn vault(out: &Path) -> PathBuf {
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    vault
}

fn run(out: &Path, omacloud: &Path, args: &[&str]) -> (i32, Value) {
    std::fs::create_dir_all(out.join("home")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(args)
        .env("HOME", out.join("home"))
        .env("XDG_DATA_HOME", out.join("data"))
        .env("XDG_RUNTIME_DIR", out.join("home"))
        .env("ICLOUD_OMACLOUD_BIN", omacloud)
        .env(
            "ICLOUD_NOTES_SYNC_CASSETTE",
            differential().join("cassettes/tiny-pull-noop.json"),
        )
        .env("ICLOUD_NOTES_SYNC_REQUEST_LOG", out.join("requests.json"))
        .env("ICLOUD_NOTES_SYNC_NOW", "1790000000000")
        .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
        .output()
        .unwrap();
    // A failure leaves stdout empty and its error object on stderr's last line.
    if !output.status.success() {
        assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
        let error = icloud_session::output::error_line(&output.stderr)
            .unwrap_or_else(|| panic!("no error object on stderr: {}", String::from_utf8_lossy(&output.stderr)));
        return (output.status.code().unwrap_or(-1), error);
    }
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout isn't JSON ({e}): {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap_or(-1), stdout)
}

fn requests(out: &Path) -> usize {
    std::fs::read_to_string(out.join("requests.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["requests"].as_array().map(Vec::len))
        .unwrap_or(0)
}

#[test]
fn leave_refuses_without_omacloud_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path();
    let vault = vault(out);
    let v = vault.to_str().unwrap();
    let cases = [
        (out.join("absent-omacloud"), "omacloud_missing"),
        (
            fake_omacloud(out, r#"{"ok":true,"data":{"set_up":false}}"#),
            "omacloud_not_set_up",
        ),
    ];
    for (bin, code) in cases {
        let (exit, json) = run(out, &bin, &["--json", "leave", v, "--yes"]);
        assert_eq!((exit, json["error"]["code"].as_str()), (1, Some(code)), "{json}");
        assert!(vault.join(".icloud-notes/state.json").is_file());
        assert!(!out.join("data/icloud-notes/left-icloud.json").exists());
    }
    let off = fake_omacloud(
        out,
        &format!(
            r#"{{"ok":true,"data":{{"set_up":true,"folder":"{}","folders":{{"synced":[]}}}}}}"#,
            out.display()
        ),
    );
    let (exit, json) = run(out, &off, &["--json", "leave", v, "--yes"]);
    assert_eq!((exit, json["error"]["code"].as_str()), (1, Some("omacloud_folder_off")));
    assert!(
        json["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("omacloud folders add ~/vault")
    );
    assert_eq!(requests(out), 0, "nothing reached iCloud");
}

/// While iCloud keeps the vault, a sync marks it so Omacloud leaves it alone.
#[test]
fn a_sync_marks_the_vault_as_icloud_s() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path();
    let vault = vault(out);
    assert!(!vault.join(".nosync").exists());
    let bin = fake_omacloud(out, &ready_status(out));
    let (exit, json) = run(out, &bin, &["--json", "sync", vault.to_str().unwrap()]);
    assert_eq!(exit, 0, "{json}");
    assert!(vault.join(".nosync").is_file());
}

#[test]
fn leave_needs_yes_where_nobody_can_be_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path();
    let vault = vault(out);
    let bin = fake_omacloud(out, &ready_status(out));
    let (exit, json) = run(out, &bin, &["--json", "leave", vault.to_str().unwrap()]);
    assert_eq!((exit, json["error"]["code"].as_str()), (1, Some("usage")));
    assert!(vault.join(".icloud-notes/state.json").is_file());
}

#[test]
fn leave_syncs_once_more_then_hands_the_folder_over() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path();
    let vault = vault(out);
    let v = vault.to_str().unwrap();
    let bin = fake_omacloud(out, &ready_status(out));
    std::fs::write(vault.join(".nosync"), "").unwrap();

    let (exit, json) = run(out, &bin, &["--json", "leave", v, "--yes"]);
    assert_eq!(exit, 0, "{json}");
    assert!(requests(out) > 0, "a last sync ran");
    let data = &json["data"];
    assert_eq!(data["folder"].as_str(), Some(v));
    assert_eq!(data["omacloud_synced"], true);
    assert_eq!(data["already"], false);
    // the state is kept outside the vault, the marker gone, the notes there
    assert!(!vault.join(".icloud-notes").exists());
    assert!(
        out.join("data/icloud-notes/left-icloud-state/.icloud-notes/state.json")
            .is_file()
    );
    assert!(!vault.join(".nosync").exists());
    assert!(vault.join("Notes/Test Note.md").is_file());
    assert_eq!(
        std::fs::read_to_string(out.join("omacloud.log")).unwrap().trim(),
        "sync --json"
    );

    // nothing reaches iCloud for this vault any more
    let before = requests(out);
    for args in [
        vec!["--json", "sync", v],
        vec!["--json", "pull", v],
        vec!["--json", "push", v],
        vec!["--json", "status", v],
        vec!["--json", "history", "Notes/Test Note.md", v],
        vec!["--json", "clone", v],
    ] {
        let (exit, json) = run(out, &bin, &args);
        assert_eq!(
            (exit, json["error"]["code"].as_str()),
            (1, Some("left_icloud")),
            "{args:?}: {json}"
        );
    }
    let (exit, json) = run(out, &bin, &["--json", "background", v]);
    assert_eq!((exit, json["data"]["step"].as_str()), (0, Some("left")));
    assert_eq!(requests(out), before);

    // the app still learns the vault's title mode
    let (exit, json) = run(out, &bin, &["--json", "vault-info", v]);
    assert_eq!(exit, 0);
    assert_eq!(
        (json["data"]["left"].as_bool(), json["data"]["cloned"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(json["data"]["titleMode"].as_str(), Some("in-body"));

    // leaving again is a no-op that says so
    let (exit, json) = run(out, &bin, &["--json", "leave", v]);
    assert_eq!((exit, json["data"]["already"].as_bool()), (0, Some(true)));
}
