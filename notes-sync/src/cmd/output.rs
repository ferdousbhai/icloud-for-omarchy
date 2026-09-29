//! `--json` vs human output and error reporting. Ports icloud-md
//! `src/cli/output.ts`. Owner: workstream D.

use serde::Serialize;

use super::errors::{EXIT_USAGE, Error};

/// `OutputContext`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputContext {
    pub json: bool,
}

impl OutputContext {
    /// `emitResult`: JSON (2-space, like `JSON.stringify(x, null, 2)`) on
    /// stdout in `--json` mode, else the human renderer.
    pub fn emit_result<T: Serialize>(&self, result: &T, render_human: impl FnOnce(&T)) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(result).expect("result serializes"));
        } else {
            render_human(result);
        }
    }

    /// `makeStatusSink`: status lines to stdout for a human, stderr in
    /// `--json` mode.
    pub fn status(&self, message: &str) {
        if self.json {
            eprintln!("{message}");
        } else {
            println!("{message}");
        }
    }

    /// `emitError`: report on stderr, return the exit code.
    pub fn emit_error(&self, error: &Error) -> i32 {
        let code = error.exit_code();
        if self.json {
            let mut payload = serde_json::Map::new();
            payload.insert("error".into(), error.name().into());
            payload.insert("message".into(), error.to_string().into());
            payload.insert("exitCode".into(), code.into());
            if let Some(hint) = error.hint() {
                payload.insert("hint".into(), hint.into());
            }
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&payload).expect("payload serializes")
            );
        } else {
            eprintln!("{error}");
            if let Some(hint) = error.hint() {
                eprintln!("{hint}");
            }
        }
        code
    }

    /// `emitUsageError`: clap already printed the human form; `--json` gets a
    /// structured one. Always 2.
    pub fn emit_usage_error(&self, message: &str) -> i32 {
        if self.json {
            let payload = serde_json::json!({ "error": "UsageError", "message": message, "exitCode": EXIT_USAGE });
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&payload).expect("payload serializes")
            );
        }
        EXIT_USAGE
    }
}
