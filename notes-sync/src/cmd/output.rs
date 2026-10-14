//! How the CLI reports: the shared product contract of
//! [`icloud_session::output`] (human text on a terminal, a JSON envelope
//! when piped, `--quiet` for the bare result, errors as the contract error
//! object on stdout in machine modes and `error:`/`hint:` on stderr for a
//! person). This module holds what is notes-sync's own: turning an
//! [`Error`] into a [`CliError`], usage errors, and where status lines go.
//!
//! Every emitter has a `*_to` form writing to explicit streams (tests) and a
//! plain form writing to stdout/stderr.

use std::io::Write;

use icloud_session::output::{CliError, Mode, usage_hint};
use serde_json::Value;

use super::errors::{EXIT_USAGE, Error};

/// The name errors are reported under.
pub const TOOL: &str = "icloud-notes-sync";

/// A status line ("Fetching changes from iCloud..."): on stdout ahead of the
/// report for a person, on stderr in a machine mode, so stdout stays one
/// JSON document.
pub fn status(mode: Mode, message: &str) {
    status_to(mode, message, &mut std::io::stdout(), &mut std::io::stderr());
}

pub fn status_to(mode: Mode, message: &str, stdout: &mut dyn Write, stderr: &mut dyn Write) {
    let _ = if mode.is_machine() {
        writeln!(stderr, "{message}")
    } else {
        writeln!(stdout, "{message}")
    };
}

/// A usage error found after parsing (clap's own are reported by
/// `icloud_session::output::parse`): code `usage`, exit 2, with the
/// ``run `icloud-notes-sync --help` `` hint.
pub fn usage_error(message: impl Into<String>) -> CliError {
    CliError::new("usage", EXIT_USAGE, message).hint(usage_hint(TOOL))
}

/// The error's code, message, hint and exit code. A sign-in error says why
/// the account was signed out, when icloud-sessiond knows (one D-Bus round
/// trip).
impl From<&Error> for CliError {
    fn from(error: &Error) -> CliError {
        let message = match error {
            Error::SignInRequired => match icloud_session::status().ok().filter(|s| !s.signed_in) {
                Some(status) => status.sign_in_prompt(&error.to_string()),
                None => error.to_string(),
            },
            _ => error.to_string(),
        };
        let cli = CliError::new(&error.code(), error.exit_code(), message);
        match error.hint() {
            Some(hint) => cli.hint(hint),
            None => cli,
        }
    }
}

impl From<Error> for CliError {
    fn from(error: Error) -> CliError {
        CliError::from(&error)
    }
}

/// The contract error object's inner part, `{"code","message","hint"?}`, as
/// a value inside a result (each failed `sync` half carries one).
pub fn error_json(error: &CliError) -> Value {
    let mut object = serde_json::json!({
        "code": error.code,
        "message": error.message,
    });
    if let Some(hint) = &error.hint {
        object["hint"] = hint.as_str().into();
    }
    object
}
