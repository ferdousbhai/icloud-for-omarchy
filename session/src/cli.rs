//! What every iCloud command line shares (docs/CLI.md): the exit codes, the
//! error on stderr, the yes/no prompt, and (the `clap` feature) parsing
//! that answers a usage error the same way. Tools that adopted the full
//! output contract use [`crate::output`] for parsing and reporting instead.
//!
//! Omarchy's command lines exit 0 on success and 1 on any failure, so a
//! script needs nothing but the exit status to know whether a call worked.
//! What kind of failure it was is the stable `code` of the JSON error, which
//! goes to stderr; the numbered exit codes the tools once had (2 usage, 3
//! signed out, 4 not found, 5 unavailable) are gone.

use std::io::{BufRead, IsTerminal, Write};

pub const EXIT_OK: u8 = 0;
/// Every failure, whatever its kind: the JSON `code` tells them apart.
pub const EXIT_ERROR: u8 = 1;
/// Once 2. Kept, as 1, so the crates that pin this one by git `rev` still
/// build after a bump; the deprecation names what to use.
#[deprecated(note = "every failure exits 1: use EXIT_ERROR, and tell failures apart by the JSON `code`")]
pub const EXIT_USAGE: u8 = EXIT_ERROR;
/// Once 3.
#[deprecated(note = "every failure exits 1: use EXIT_ERROR, and tell failures apart by the JSON `code`")]
pub const EXIT_SIGN_IN: u8 = EXIT_ERROR;
/// Once 3.
#[deprecated(note = "every failure exits 1: use EXIT_ERROR, and tell failures apart by the JSON `code`")]
pub const EXIT_FIND_MY_AUTH: u8 = EXIT_ERROR;
/// Once 4.
#[deprecated(note = "every failure exits 1: use EXIT_ERROR, and tell failures apart by the JSON `code`")]
pub const EXIT_NOT_FOUND: u8 = EXIT_ERROR;
/// Once 5.
#[deprecated(note = "every failure exits 1: use EXIT_ERROR, and tell failures apart by the JSON `code`")]
pub const EXIT_UNAVAILABLE: u8 = EXIT_ERROR;
/// Always 1.
#[deprecated(note = "every failure exits 1: use EXIT_ERROR")]
pub const EXIT_INTERNAL: u8 = EXIT_ERROR;

/// The exit code for a process that ends with `exit`: 0 stays 0, and every
/// other number, from a caller that still passes the old ones, becomes 1.
pub fn exit_status(exit: u8) -> u8 {
    if exit == EXIT_OK { EXIT_OK } else { EXIT_ERROR }
}

/// Reports an error on stderr and returns the exit code, 1 (any non-zero
/// `exit_code` becomes 1, see [`exit_status`]). With `json`, one line of the
/// shared product contract, `{"ok":false,"error":{"code","message","hint"?}}`
/// in that key order; else `tool: message`, then the hint on a line of its
/// own.
///
/// Legacy: for tools not yet on [`crate::output`], whose
/// [`CliError::report`](crate::output::CliError::report) puts a machine-mode
/// error on stdout and text-mode `error:`/`hint:` lines on stderr.
#[deprecated(note = "use icloud_session::output")]
pub fn report(tool: &str, json: bool, code: &str, exit_code: u8, message: &str, hint: Option<&str>) -> u8 {
    eprintln!("{}", render(tool, json, code, message, hint));
    let _ = exit_code;
    EXIT_ERROR
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
/// `Err(EXIT_OK)`; a usage error gives `Err(EXIT_ERROR)` after clap's
/// message, or the JSON error (with a ``run `<tool> --help` `` hint) when
/// `--json` is among the arguments.
///
/// Legacy: for tools not yet on [`crate::output`]; those use
/// [`crate::output::parse`].
#[cfg(feature = "clap")]
#[deprecated(note = "use icloud_session::output")]
#[allow(deprecated)]
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
            return EXIT_ERROR;
        }
        let message = crate::output::usage_message(&e);
        let hint = crate::output::usage_hint(tool);
        report(tool, true, "usage", EXIT_ERROR, &message, Some(&hint))
    })
}

#[cfg(test)]
mod tests {
    use super::{EXIT_ERROR, EXIT_OK, ask, exit_status, render};

    #[test]
    fn a_process_exits_0_or_1_and_nothing_else() {
        assert_eq!(exit_status(EXIT_OK), 0);
        for old in 1..=u8::MAX {
            assert_eq!(exit_status(old), EXIT_ERROR, "{old}");
        }
    }

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
