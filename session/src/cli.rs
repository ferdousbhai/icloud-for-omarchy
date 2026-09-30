//! What every iCloud command line shares (docs/CLI.md): the exit codes, the
//! error on stderr, the yes/no prompt, and (the `clap` feature) parsing
//! that answers a usage error the same way.

use std::io::{BufRead, IsTerminal, Write};

pub const EXIT_OK: u8 = 0;
pub const EXIT_ERROR: u8 = 1;
/// iCloud sign-in required: `icloud-session sign-in`.
pub const EXIT_SIGN_IN: u8 = 2;
/// Has changes or differences; not an error.
pub const EXIT_CHANGES: u8 = 3;
/// Find My needs the Apple password: `icloud-session authorize-find-my`.
pub const EXIT_FIND_MY_AUTH: u8 = 4;
pub const EXIT_USAGE: u8 = 64;
/// An internal error, a bug.
pub const EXIT_INTERNAL: u8 = 70;

/// Reports an error on stderr and returns `exit_code`. With `json`, one line
/// `{"error":{"code","message","exit_code","hint"?}}`; else `tool: message`,
/// then the hint on a line of its own.
pub fn report(tool: &str, json: bool, code: &str, exit_code: u8, message: &str, hint: Option<&str>) -> u8 {
    if json {
        let mut error = serde_json::json!({ "code": code, "message": message, "exit_code": exit_code });
        if let Some(hint) = hint {
            error["hint"] = hint.into();
        }
        eprintln!("{}", serde_json::json!({ "error": error }));
    } else {
        eprintln!("{tool}: {message}");
        if let Some(hint) = hint {
            eprintln!("{hint}");
        }
    }
    exit_code
}

/// Asks `question [y/N]` on stderr and reads the answer from stdin: true
/// for `y` or `yes` in any case. False, without asking, when stdin is not
/// a terminal (a caller refuses there unless given `--yes`).
pub fn confirm(question: &str) -> bool {
    let stdin = std::io::stdin();
    stdin.is_terminal() && ask(question, stdin.lock(), std::io::stderr())
}

fn ask(question: &str, mut input: impl BufRead, mut output: impl Write) -> bool {
    let _ = write!(output, "{question} [y/N] ");
    let _ = output.flush();
    let mut answer = String::new();
    if input.read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Parses the process arguments. `--help` and `--version` print and give
/// `Err(EXIT_OK)`; a usage error gives `Err(EXIT_USAGE)` after clap's
/// message, or the JSON error when `--json` is among the arguments.
#[cfg(feature = "clap")]
pub fn parse<P: clap::Parser>(tool: &str) -> Result<P, u8> {
    P::try_parse().map_err(|e| {
        if !e.use_stderr() {
            let _ = e.print();
            return EXIT_OK;
        }
        if !std::env::args_os().skip(1).any(|a| a == "--json") {
            let _ = e.print();
            return EXIT_USAGE;
        }
        // clap's error without its usage and "try --help" lines, on one line.
        let rendered = e.render().to_string();
        let error = rendered.split("\n\n").next().unwrap_or_default();
        let message = error.strip_prefix("error: ").unwrap_or(error);
        let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
        report(tool, true, "usage", EXIT_USAGE, &message, None)
    })
}

#[cfg(test)]
mod tests {
    use super::ask;

    #[test]
    fn only_y_or_yes_confirms() {
        for yes in ["y\n", "Y\n", "yes\n", "YES\n", "Yes\n", "  yEs  \n", "y"] {
            assert!(ask("Go?", yes.as_bytes(), Vec::new()), "{yes:?}");
        }
        for no in ["\n", "", "n\n", "no\n", "yeah\n", "y es\n", "ok\n"] {
            assert!(!ask("Go?", no.as_bytes(), Vec::new()), "{no:?}");
        }
    }

    #[test]
    fn asks_the_question() {
        let mut out = Vec::new();
        ask("Delete 2 items?", "n\n".as_bytes(), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "Delete 2 items? [y/N] ");
    }
}
