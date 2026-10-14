//! Output: what notes-sync hands icloud-session's shared output contract
//! (`icloud_session::output`, whose own tests cover the envelope and the
//! error object), where status lines go, and the exact bytes of an error
//! through the binary. Originally derived from icloud-md's tests.

use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::output::{error_json, status_to, usage_error};
use icloud_session::output::{CliError, Mode};

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
fn status_goes_to_stdout_for_humans_and_stderr_in_machine_modes() {
    for mode in [Mode::Json, Mode::Quiet, Mode::Ids, Mode::Count] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        status_to(mode, "machine status", &mut out, &mut err);
        assert_eq!((text(&out).as_str(), text(&err).as_str()), ("", "machine status\n"));
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    status_to(Mode::Human, "human status", &mut out, &mut err);
    assert_eq!((text(&out).as_str(), text(&err).as_str()), ("human status\n", ""));
}

#[test]
fn known_error_has_its_message_hint_code_and_exit_4() {
    let error = known_error();
    assert_eq!(error.exit_code(), 4);
    assert_eq!(error.code(), "untracked_file");
    assert_eq!(
        error.to_string(),
        "\"missing-note.md\" isn't a tracked note in /some/dir."
    );
    assert_eq!(
        error.hint().as_deref(),
        Some("Check the file name (it's case-sensitive) and try again.")
    );
    let cli = CliError::from(&error);
    assert_eq!(
        (cli.code.as_str(), cli.exit, cli.message.as_str(), cli.hint.as_deref()),
        (
            "untracked_file",
            4,
            "\"missing-note.md\" isn't a tracked note in /some/dir.",
            Some("Check the file name (it's case-sensitive) and try again.")
        )
    );
}

#[test]
fn an_error_without_a_hint_has_none() {
    let error = Error::RequestedAccountMismatch {
        requested: "a@example.com".into(),
        actual: "b@example.com".into(),
    };
    assert!(error.hint().is_none());
    let cli = CliError::from(error);
    assert!(cli.hint.is_none());
    let object = error_json(&cli);
    let mut keys: Vec<&String> = object.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["code", "message"]);
}

#[test]
fn error_json_is_code_message_hint() {
    let object = error_json(&CliError::from(known_error()));
    let mut keys: Vec<&String> = object.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["code", "hint", "message"]);
    assert_eq!(object["code"], "untracked_file");
}

#[test]
fn usage_errors_exit_2_and_point_at_help() {
    let e = usage_error("bad ref");
    assert_eq!(
        (e.code.as_str(), e.exit, e.message.as_str(), e.hint.as_deref()),
        ("usage", 2, "bad ref", Some("run `icloud-notes-sync --help`"))
    );
}

#[test]
fn internal_error_is_code_internal_exit_1() {
    let error = Error::Internal("a genuine bug".into());
    assert_eq!((error.code().as_str(), error.exit_code()), ("internal", 1));
    assert_eq!(error.to_string(), "a genuine bug");
}

#[test]
fn sign_in_required_exits_3_and_its_hint_names_the_command() {
    assert_eq!(Error::SignInRequired.exit_code(), 3);
    assert!(
        Error::SignInRequired
            .hint()
            .unwrap()
            .contains("Run `icloud-session sign-in`")
    );
}

/// The error object, byte for byte (pretty, `ok` first, `code`, `message`,
/// `hint` in that order), alone on stdout with stderr empty; the same error
/// in text mode as `error:` / `hint:` lines on stderr.
#[test]
fn the_error_object_is_the_shared_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().display();
    let expected = format!(
        "{{\n  \"ok\": false,\n  \"error\": {{\n    \"code\": \"not_cloned_directory\",\n    \"message\": \"{dir} doesn't \
         look like a cloned notes directory (no .icloud-notes/state.json).\",\n    \"hint\": \"Run \\\"icloud-notes-sync \
         clone <directory>\\\" first.\"\n  }}\n}}\n"
    );
    for flag in ["--json", "--quiet", "--count", "--ids-only"] {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
            .args([flag, "history", "missing-note.md"])
            .arg(tmp.path())
            .env("XDG_RUNTIME_DIR", tmp.path())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(3), "{flag}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), expected, "{flag}");
        assert_eq!(String::from_utf8_lossy(&o.stderr), "", "{flag}");
    }
    let human = std::process::Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(["--styled", "history", "missing-note.md"])
        .arg(tmp.path())
        .env("XDG_RUNTIME_DIR", tmp.path())
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(3));
    assert!(human.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&human.stderr),
        format!(
            "error: {dir} doesn't look like a cloned notes directory (no .icloud-notes/state.json).\n\
             hint: Run \"icloud-notes-sync clone <directory>\" first.\n"
        )
    );
}
