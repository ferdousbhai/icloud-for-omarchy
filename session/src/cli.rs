//! What every iCloud command line shares (docs/CLI.md): the exit codes, the
//! error on stderr, the yes/no prompt, and (the `clap` feature) parsing
//! that answers a usage error the same way. Tools that adopted the full
//! output contract use [`crate::output`] for parsing and reporting instead.

use std::io::{BufRead, IsTerminal, Write};

pub const EXIT_OK: u8 = 0;
pub const EXIT_ERROR: u8 = 1;
/// Invalid usage or request; a destructive command without `--yes`.
pub const EXIT_USAGE: u8 = 2;
/// Not signed in: `icloud-session sign-in`.
pub const EXIT_SIGN_IN: u8 = 3;
/// Find My needs the Apple password: `icloud-session authorize-find-my`.
/// The same exit code as a sign-in (not configured / not signed in); the
/// JSON `code` (`find_my_auth_required`) still tells the two apart.
pub const EXIT_FIND_MY_AUTH: u8 = 3;
/// The thing asked for does not exist.
pub const EXIT_NOT_FOUND: u8 = 4;
/// A service or a required tool is unavailable: no network, offline, the
/// daemon unreachable (`network`, `offline`, `session_service`, `http`).
pub const EXIT_UNAVAILABLE: u8 = 5;
/// An internal error, a bug. The same exit code as [`EXIT_ERROR`]; kept so
/// call sites can say which they mean.
pub const EXIT_INTERNAL: u8 = 1;

/// Reports an error on stderr and returns `exit_code`. With `json`, one line
/// of the shared product contract, `{"ok":false,"error":{"code","message","hint"?}}`
/// in that key order (the process exit code carries the number); else
/// `tool: message`, then the hint on a line of its own.
///
/// Legacy: for tools not yet on [`crate::output`], whose
/// [`CliError::report`](crate::output::CliError::report) puts a machine-mode
/// error on stdout and text-mode `error:`/`hint:` lines on stderr.
pub fn report(tool: &str, json: bool, code: &str, exit_code: u8, message: &str, hint: Option<&str>) -> u8 {
    eprintln!("{}", render(tool, json, code, message, hint));
    exit_code
}

fn render(tool: &str, json: bool, code: &str, message: &str, hint: Option<&str>) -> String {
    if json {
        crate::output::compact(&crate::output::ErrorEnvelope::new(code, message, hint))
    } else {
        match hint {
            Some(hint) => format!("{tool}: {message}\n{hint}"),
            None => format!("{tool}: {message}"),
        }
    }
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
/// message, or the JSON error (with a ``run `<tool> --help` `` hint) when
/// `--json` is among the arguments.
///
/// Legacy: for tools not yet on [`crate::output`]; those use
/// [`crate::output::parse`].
#[cfg(feature = "clap")]
pub fn parse<P: clap::Parser>(tool: &str) -> Result<P, u8> {
    P::try_parse().map_err(|e| {
        if !e.use_stderr() {
            let _ = e.print();
            return EXIT_OK;
        }
        if !std::env::args_os()
            .skip(1)
            .take_while(|a| a != "--")
            .any(|a| a == "--json")
        {
            let _ = e.print();
            return EXIT_USAGE;
        }
        let message = crate::output::usage_message(&e);
        let hint = crate::output::usage_hint(tool);
        report(tool, true, "usage", EXIT_USAGE, &message, Some(&hint))
    })
}

#[cfg(test)]
mod tests {
    use super::{ask, render};

    #[test]
    fn the_error_line_has_a_fixed_key_order() {
        assert_eq!(
            render("t", true, "not_found", "no \"x\"", Some("run `t list`")),
            r#"{"ok":false,"error":{"code":"not_found","message":"no \"x\"","hint":"run `t list`"}}"#
        );
        assert_eq!(
            render("t", true, "usage", "bad", None),
            r#"{"ok":false,"error":{"code":"usage","message":"bad"}}"#
        );
        assert_eq!(render("t", false, "usage", "bad", Some("try this")), "t: bad\ntry this");
        assert_eq!(render("t", false, "usage", "bad", None), "t: bad");
    }

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
