//! Ports icloud-md `src/cli/output.test.ts`. `Error` variants stand in for
//! the TS test's `TestKnownError`; `Error::Internal` for a plain `Error`.
//! Not ported: the stack-trace assertions (the port has no stack) and
//! "a thrown non-Error value" (Rust can't throw one).

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

#[test]
fn known_error_prints_message_and_hint_and_returns_1() {
    let mut err = Vec::new();
    assert_eq!(HUMAN.emit_error_to(&known_error(), &mut err), 1);
    let printed = text(&err);
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"missing-note.md\" isn't a tracked note in /some/dir."));
    assert!(lines[1].contains("Check the file name"));
}

#[test]
fn known_error_is_structured_json_in_json_mode() {
    let mut err = Vec::new();
    assert_eq!(JSON.emit_error_to(&known_error(), &mut err), 1);
    let payload: Value = serde_json::from_str(&text(&err)).unwrap();
    assert_eq!(payload["error"]["code"], "untracked_file");
    assert_eq!(
        payload["error"]["message"],
        "\"missing-note.md\" isn't a tracked note in /some/dir."
    );
    assert_eq!(
        payload["error"]["hint"],
        "Check the file name (it's case-sensitive) and try again."
    );
    assert_eq!(payload["error"]["exit_code"], 1);
    assert_eq!(text(&err).lines().count(), 1, "one line, for agents reading stderr");
}

#[test]
fn json_payload_omits_hint_when_there_is_none() {
    let mut err = Vec::new();
    let error = Error::RequestedAccountMismatch {
        requested: "a@example.com".into(),
        actual: "b@example.com".into(),
    };
    JSON.emit_error_to(&error, &mut err);
    let payload: Value = serde_json::from_str(&text(&err)).unwrap();
    assert!(payload["error"].get("hint").is_none());
}

#[test]
fn internal_error_returns_70() {
    let mut err = Vec::new();
    assert_eq!(
        HUMAN.emit_error_to(&Error::Internal("a genuine bug".into()), &mut err),
        70
    );
    let printed = text(&err);
    assert_eq!(printed.lines().count(), 1);
    assert!(printed.contains("a genuine bug"));
}

#[test]
fn internal_error_is_structured_json_with_exit_70() {
    let mut err = Vec::new();
    assert_eq!(
        JSON.emit_error_to(&Error::Internal("a genuine bug".into()), &mut err),
        70
    );
    let payload: Value = serde_json::from_str(&text(&err)).unwrap();
    assert_eq!(payload["error"]["code"], "internal");
    assert_eq!(payload["error"]["message"], "a genuine bug");
    assert_eq!(payload["error"]["exit_code"], 70);
}

#[test]
fn usage_error_prints_nothing_for_humans_but_returns_64() {
    let mut err = Vec::new();
    assert_eq!(HUMAN.emit_usage_error_to("unknown option '--nope'", &mut err), 64);
    assert!(err.is_empty());
}

#[test]
fn usage_error_is_structured_json_in_json_mode() {
    let mut err = Vec::new();
    assert_eq!(JSON.emit_usage_error_to("unknown option '--nope'", &mut err), 64);
    let payload: Value = serde_json::from_str(&text(&err)).unwrap();
    assert_eq!(
        payload,
        serde_json::json!({"error": {"code": "usage", "message": "unknown option '--nope'", "exit_code": 64}})
    );
}

#[test]
fn sign_in_required_exits_2_and_keeps_the_sign_in_marker() {
    let mut err = Vec::new();
    assert_eq!(HUMAN.emit_error_to(&Error::SignInRequired, &mut err), 2);
    assert!(text(&err).contains("icloud-md reauthenticate"));
}
