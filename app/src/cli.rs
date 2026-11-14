//! The apps' command-line helpers, on top of `icloud_session::output`.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use icloud_session::cli::{EXIT_ERROR, EXIT_USAGE};
use icloud_session::output::{self, CliError, Mode, SIGN_IN_HINT};
use serde_json::Value;

/// `main` of an app's command-line binary `name` (`icloud-reminders`): with
/// a command it runs `cli`; with none it opens the app by running
/// `<name>-app` in its place, from the same directory as this binary, else
/// from `PATH`.
pub fn main(name: &str, cli: fn() -> u8) -> ! {
    if std::env::args_os().len() > 1 {
        std::process::exit(cli().into());
    }
    let app_name = format!("{name}-app");
    let app = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(&app_name))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(&app_name));
    // Only returns on failure; on success the app replaces this process.
    let err = Command::new(&app).exec();
    eprintln!(
        "{name}: cannot open the app ({}: {err}); run `{name} help` for the command line",
        app.display()
    );
    std::process::exit(EXIT_ERROR.into());
}

pub fn usage(msg: impl Into<String>) -> CliError {
    CliError::new("usage", EXIT_USAGE, msg)
}

pub fn other(msg: impl Into<String>) -> CliError {
    CliError::new("error", EXIT_ERROR, msg)
}

/// No `--data-dir`, and no environment to find the default one from.
pub fn no_data_dir() -> CliError {
    other("no data directory (no $XDG_DATA_HOME or $HOME)").hint("pass --data-dir DIR")
}

/// The shared sign-in hint, plus the app's own button.
pub fn sign_in_hint() -> String {
    format!("{SIGN_IN_HINT}, or Sign In in the app")
}

/// `e` with `hint` in place of the shared sign-in hint, if it carries that.
pub fn with_sign_in_hint(e: CliError, hint: impl Into<String>) -> CliError {
    if e.hint.as_deref() == Some(SIGN_IN_HINT) {
        e.hint(hint)
    } else {
        e
    }
}

/// An icloud-session failure as every iCloud tool reports it
/// (`output::session_failure`), its sign-in hint naming the app's button too.
pub fn session_failure(e: &icloud_session::Error) -> CliError {
    with_sign_in_hint(output::session_failure(e), sign_in_hint())
}

/// Asks unless `yes`, and only when someone can answer
/// ([`output::interactive`]: text mode, stdin and stdout a terminal);
/// otherwise refuses (exit 2) without `--yes`.
pub fn confirm(mode: Mode, cmd: &str, yes: bool, question: &str) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    if !output::interactive(mode) {
        return Err(
            usage(format!("{cmd} asks before it acts, and nobody can be asked here"))
                .hint(format!("pass --yes to {cmd} without asking")),
        );
    }
    if icloud_session::cli::confirm(question) {
        Ok(())
    } else {
        Err(CliError::new("cancelled", EXIT_ERROR, "cancelled"))
    }
}

/// A problem that did not stop the command, on stderr.
pub fn warning(msg: &str) {
    eprintln!("{}", output::terminal_safe(&format!("warning: {msg}")));
}

/// A write or sync that happened but left a problem behind: on stderr, and
/// as `warning` in the JSON. The command still succeeds.
pub fn warn(out: &mut Value, w: Option<String>) {
    if let Some(w) = w {
        warning(&w);
        out["warning"] = Value::String(w);
    }
}

/// Lowercase, with typographic apostrophes as plain ones ("Dous’s" = "dous's").
pub fn fold(s: &str) -> String {
    s.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sign_in_hint_is_swapped() {
        let http = |status| icloud_session::Error::Http {
            status,
            body: "x".into(),
        };
        let rejected = session_failure(&http(421));
        assert_eq!((rejected.code.as_str(), rejected.exit), ("session_rejected", 3));
        assert_eq!(rejected.hint, Some(sign_in_hint()));
        let offline = session_failure(&icloud_session::Error::Offline("x".into()));
        assert_eq!((offline.code.as_str(), offline.exit), ("offline", 5));
        assert_eq!(offline.hint.as_deref(), Some(output::NETWORK_HINT));
        assert_eq!(session_failure(&http(404)).hint, None);
    }

    #[test]
    fn confirm_refuses_where_nobody_can_answer() {
        assert!(confirm(Mode::Json, "delete", true, "Delete?").is_ok());
        let e = confirm(Mode::Json, "delete", false, "Delete?").unwrap_err();
        assert_eq!((e.code.as_str(), e.exit), ("usage", 2));
        assert_eq!(e.hint.as_deref(), Some("pass --yes to delete without asking"));
    }

    #[test]
    fn fold_ignores_case_and_apostrophe_style() {
        assert_eq!(fold("Dous\u{2019}s iPhone"), "dous's iphone");
    }
}
