//! Output: `--json` results and error objects, human text, and which stream
//! each goes to. `Error::Internal` stands for an unexpected error. Originally derived from icloud-md's tests.

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::output::OutputContext;
use serde_json::Value;

const HUMAN: OutputContext = OutputContext { json: false };
const JSON: OutputContext = OutputContext { json: true };

fn text(buf: &[u8]) -> String {
    String::from_utf8(buf.to_vec()).unwrap()
}

fn known_error() -> Error {
    Error::UntrackedFile {
        file: "missing-note.md".into(),
        target_dir: "/some/dir".into(),
    }
}

#[test]
fn emit_result_calls_render_human_and_prints_nothing_itself() {
    let mut out = Vec::new();
    let mut rendered = None;
    HUMAN.emit_result_to(&42, |n| rendered = Some(*n), &mut out);
    assert_eq!(rendered, Some(42));
    assert!(out.is_empty());
}

#[test]
fn emit_result_prints_json_in_json_mode() {
    let mut out = Vec::new();
    let mut called = false;
    JSON.emit_result_to(&serde_json::json!({"a": 1, "b": "two"}), |_| called = true, &mut out);
    assert!(!called);
    let printed = text(&out);
    assert!(printed.ends_with('\n'));
    let parsed: Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(parsed, serde_json::json!({"a": 1, "b": "two"}));
}

#[test]
fn status_goes_to_stdout_for_humans_and_stderr_in_json_mode() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    HUMAN.status_to("human status", &mut out, &mut err);
    JSON.status_to("json status", &mut out, &mut err);
    assert_eq!(text(&out), "human status\n");
    assert_eq!(text(&err), "json status\n");
}

// Errors are printed by icloud-session's shared `cli::report` (its tests
// cover the line itself); here, what notes-sync hands it.

#[test]
fn known_error_has_its_message_hint_code_and_exit_1() {
    let error = known_error();
    assert_eq!(error.exit_code(), 1);
    assert_eq!(error.code(), "untracked_file");
    assert_eq!(
        error.to_string(),
        "\"missing-note.md\" isn't a tracked note in /some/dir."
    );
    assert_eq!(
        error.hint().as_deref(),
        Some("Check the file name (it's case-sensitive) and try again.")
    );
}

#[test]
fn an_error_without_a_hint_has_none() {
    let error = Error::RequestedAccountMismatch {
        requested: "a@example.com".into(),
        actual: "b@example.com".into(),
    };
    assert!(error.hint().is_none());
}

#[test]
fn internal_error_is_code_internal_exit_70() {
    let error = Error::Internal("a genuine bug".into());
    assert_eq!((error.code().as_str(), error.exit_code()), ("internal", 70));
    assert_eq!(error.to_string(), "a genuine bug");
}

#[test]
fn sign_in_required_exits_2_and_keeps_the_sign_in_marker() {
    assert_eq!(Error::SignInRequired.exit_code(), 2);
    assert!(
        Error::SignInRequired
            .hint()
            .unwrap()
            .contains("Sign in to iCloud again with icloud-session")
    );
}

/// The one-line JSON error, in its key order, through the binary.
#[test]
fn the_json_error_line_is_the_shared_one() {
    let tmp = tempfile::tempdir().unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(["--json", "history", "missing-note.md"])
        .arg(tmp.path())
        .env("XDG_RUNTIME_DIR", tmp.path())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let dir = tmp.path().display();
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "{{\"error\":{{\"code\":\"not_cloned_directory\",\"message\":\"{dir} doesn't look like a cloned notes directory \
             (no .icloud-notes/state.json).\",\"exit_code\":1,\"hint\":\"Run \\\"icloud-notes-sync clone <directory>\\\" first.\"}}}}\n"
        )
    );
    let human = std::process::Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(["history", "missing-note.md"])
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&human.stderr).starts_with("icloud-notes-sync: "));
}
