//! `--json` vs human output and error reporting. Ports icloud-md
//! `src/cli/output.ts`.
//!
//! Every emitter has a `*_to` form writing to explicit streams (tests) and a
//! plain form writing to stdout/stderr.

use std::io::Write;

use serde::Serialize;

use super::errors::{EXIT_USAGE, Error};

/// `OutputContext`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputContext {
    pub json: bool,
}

/// `JSON.stringify(value, null, 2)`.
pub fn to_json_pretty<T: Serialize>(value: &T) -> String {
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

    /// `emitError`: report on stderr, return the exit code. With `--json`
    /// the report is one line, the error object every iCloud tool prints:
    /// `{"error":{"code","message","exit_code","hint"?}}`.
    pub fn emit_error(&self, error: &Error) -> i32 {
        self.emit_error_to(error, &mut std::io::stderr())
    }

    pub fn emit_error_to(&self, error: &Error, stderr: &mut dyn Write) -> i32 {
        let code = error.exit_code();
        if self.json {
            let _ = writeln!(
                stderr,
                "{}",
                error_json(&error.code(), &error.to_string(), code, error.hint())
            );
        } else {
            let _ = writeln!(stderr, "{error}");
            if let Some(hint) = error.hint() {
                let _ = writeln!(stderr, "{hint}");
            }
        }
        code
    }

    /// `emitUsageError`: clap already printed the human form; `--json` gets a
    /// structured one. Always 64.
    pub fn emit_usage_error(&self, message: &str) -> i32 {
        self.emit_usage_error_to(message, &mut std::io::stderr())
    }

    pub fn emit_usage_error_to(&self, message: &str, stderr: &mut dyn Write) -> i32 {
        if self.json {
            let _ = writeln!(stderr, "{}", error_json("usage", message, EXIT_USAGE, None));
        }
        EXIT_USAGE
    }
}

/// The one-line `--json` error object.
pub fn error_json(code: &str, message: &str, exit_code: i32, hint: Option<String>) -> String {
    let mut error = serde_json::Map::new();
    error.insert("code".into(), code.into());
    error.insert("message".into(), message.into());
    error.insert("exit_code".into(), exit_code.into());
    if let Some(hint) = hint {
        error.insert("hint".into(), hint.into());
    }
    serde_json::json!({ "error": error }).to_string()
}
