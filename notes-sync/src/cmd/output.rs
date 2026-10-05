//! `--json` vs human output and error reporting. Originally derived from
//! icloud-md.
//!
//! Every emitter has a `*_to` form writing to explicit streams (tests) and a
//! plain form writing to stdout/stderr.

use std::io::Write;

use serde::Serialize;

use super::errors::{EXIT_USAGE, Error};

/// The name errors are reported under.
pub const TOOL: &str = "icloud-notes-sync";

/// `OutputContext`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputContext {
    pub json: bool,
}

/// `JSON.stringify(value, null, 2)`.
fn to_json_pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("result serializes")
}

impl OutputContext {
    /// `emitResult`: JSON on stdout in `--json` mode, else the human
    /// renderer.
    pub fn emit_result<T: Serialize>(&self, result: &T, render_human: impl FnOnce(&T)) {
        self.emit_result_to(result, render_human, &mut std::io::stdout());
    }

    pub fn emit_result_to<T: Serialize>(&self, result: &T, render_human: impl FnOnce(&T), stdout: &mut dyn Write) {
        if self.json {
            let _ = writeln!(stdout, "{}", to_json_pretty(result));
        } else {
            render_human(result);
        }
    }

    /// `makeStatusSink`: status lines to stdout for a human, stderr in
    /// `--json` mode.
    pub fn status(&self, message: &str) {
        self.status_to(message, &mut std::io::stdout(), &mut std::io::stderr());
    }

    pub fn status_to(&self, message: &str, stdout: &mut dyn Write, stderr: &mut dyn Write) {
        let _ = if self.json {
            writeln!(stderr, "{message}")
        } else {
            writeln!(stdout, "{message}")
        };
    }

    /// `emitError`: report on stderr (icloud-session's shared form: with
    /// `--json` one line, `{"error":{"code","message","exit_code","hint"?}}`),
    /// return the exit code.
    pub fn emit_error(&self, error: &Error) -> u8 {
        icloud_session::cli::report(
            TOOL,
            self.json,
            &error.code(),
            error.exit_code(),
            &error.to_string(),
            error.hint().as_deref(),
        )
    }

    /// A usage error found after parsing (clap's own go through
    /// `icloud_session::cli::parse`). Always 64.
    pub fn emit_usage_error(&self, message: &str) -> u8 {
        icloud_session::cli::report(TOOL, self.json, "usage", EXIT_USAGE, message, None)
    }
}
