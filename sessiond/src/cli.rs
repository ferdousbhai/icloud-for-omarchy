//! `icloud-session status | sign-in | authorize-find-my | set-password |
//! forget-password | sign-out | validate | skill [install|remove]`
//!
//! Talks to icloud-sessiond over D-Bus. JSON on stdout; errors on stderr
//! with a non-zero exit, in the table every iCloud tool shares
//! (`icloud_session::cli`):
//!
//! - 1 error: a failure the daemon reported (`error`), `io`, a non-5xx `http`;
//! - 2 usage;
//! - 3 not signed in or credentials rejected: `sign_in_required`,
//!   `sign_in_not_completed`, `find_my_auth_required`,
//!   `find_my_auth_not_completed`, `password_rejected` (Apple refused the
//!   password given to `set-password`), `session_rejected` (Apple keeps
//!   answering 401/421 although the daemon still holds the sign-in);
//! - 5 a service unavailable: `session_service` (the daemon or the session
//!   bus unreachable), `keyring_unavailable`, `network`, `offline`,
//!   `bad_response`, `http` 5xx.
//!
//! With `--json` an error is one JSON line on stderr:
//! `{"ok":false,"error":{"code","message","hint"?}}`.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_FIND_MY_AUTH, EXIT_SIGN_IN, EXIT_UNAVAILABLE};
use icloud_session::{BUS_NAME, Error, INTERFACE, OBJECT_PATH, Session, Status};
use serde_json::json;
use zeroize::Zeroizing;

const TOOL: &str = "icloud-session";

/// `--json`: errors as JSON (the output already is).
static JSON: AtomicBool = AtomicBool::new(false);

const AFTER_HELP: &str = "\
--json: errors as one JSON line on stderr, {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}}
(output is JSON anyway).
Exit codes: 0 ok; 1 error (a failure icloud-sessiond reported); 2 usage; 3 not signed in or
credentials rejected (sign_in_required, sign_in_not_completed, find_my_auth_required,
find_my_auth_not_completed, password_rejected, session_rejected); 5 a service unavailable
(session_service: daemon unreachable; keyring_unavailable, network, offline, bad_response, HTTP 5xx).";

#[derive(Parser)]
#[command(
    name = "icloud-session",
    version,
    about = "The one iCloud sign-in every app uses, through icloud-sessiond. JSON on stdout.",
    after_help = AFTER_HELP
)]
struct Args {
    /// Errors as one JSON line on stderr (the output is JSON anyway).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The daemon's properties.
    ///
    /// The daemon's properties: signed_in, apple_id, full_name, dsid, expires_at (Unix
    /// seconds; null for a sign-in without "Keep me signed in"), signing_in,
    /// find_my_authorized, find_my_password_stored, sign_out_reason (why the
    /// account was last signed out; null while signed in or never signed in).
    #[command(after_help = AFTER_HELP)]
    Status,
    /// Open the sign-in window and wait for it, then print status.
    ///
    /// Opens the sign-in window (Apple's page: password and 2FA, typed by a
    /// person) and waits until it closes, then prints status. Exits 3
    /// (sign_in_not_completed) when the window closed without a sign-in.
    #[command(after_help = AFTER_HELP)]
    SignIn {
        /// Print status at once instead; poll `status` until signing_in is false.
        #[arg(long)]
        no_wait: bool,
    },
    /// Open the sign-in window on Find My's password page, then print status.
    ///
    /// Opens the sign-in window on Find My, where Apple asks for the password
    /// again, and waits until it closes, then prints status. Exits 3
    /// (find_my_auth_not_completed) when Find My is still not authorized.
    #[command(after_help = AFTER_HELP)]
    AuthorizeFindMy {
        /// Print status at once instead; poll `status` until signing_in is false.
        #[arg(long)]
        no_wait: bool,
    },
    /// Store the Apple ID password so Find My re-authorizes by itself.
    ///
    /// Stores the Apple ID password in the keyring (GNOME Keyring), so the
    /// daemon re-authorizes Find My by itself when it asks for the password
    /// again. The password is checked with a Find My sign-in first and stored
    /// only if Apple accepts it (exit 3, password_rejected, when Apple
    /// refuses it). It is read from the terminal without echo,
    /// or from stdin when that is not a terminal, e.g. from a password
    /// manager:
    ///
    ///     bw get password "Apple ID" | icloud-session set-password
    ///     op read "op://Private/Apple ID/password" | icloud-session set-password
    #[command(after_help = AFTER_HELP, verbatim_doc_comment)]
    SetPassword,
    /// Remove the stored password from the keyring, then print status.
    #[command(after_help = AFTER_HELP)]
    ForgetPassword,
    /// Forget the account and the sign-in window's profile, then print status.
    ///
    /// Forgets the account and the sign-in window's profile, then prints
    /// status (a stored password stays until forget-password). Every iCloud
    /// app is signed out.
    #[command(after_help = AFTER_HELP)]
    SignOut,
    /// The session's webservices: {dsid, apple_id, webservices}.
    ///
    /// The session's webservices: {dsid, apple_id, webservices} (the daemon
    /// validates first when older than 6 hours, and in the background when
    /// older than 10 minutes). Exits 3 (sign_in_required) when Apple no
    /// longer accepts the sign-in.
    #[command(after_help = AFTER_HELP)]
    Validate,
    /// Print the agent skill for the iCloud tools, or install it for your coding agents.
    ///
    /// Prints the skill (SKILL.md, Markdown) built into icloud-session. `install` copies it to
    /// ~/.agents/skills/icloud/ (Codex, Pi and others) and links it into ~/.claude/skills/ when
    /// Claude Code is installed; `remove` removes what install created. Only a copy icloud-session
    /// wrote (marked .managed-by-icloud-session) is ever changed; a new release refreshes it.
    #[command(after_help = AFTER_HELP)]
    Skill {
        #[command(subcommand)]
        action: Option<SkillAction>,
    },
}

#[derive(Subcommand)]
enum SkillAction {
    /// Install the skill, then print {paths, notices}.
    Install,
    /// Remove the installed skill and link, then print {paths, notices}.
    Remove,
}

const SKILL: crate::agent_skill::Skill = crate::agent_skill::Skill {
    name: "icloud",
    body: include_str!("../../docs/skills/icloud/SKILL.md"),
    cli: "icloud-session",
    version: env!("CARGO_PKG_VERSION"),
    adopts_legacy: false,
};

fn skill(action: Option<SkillAction>) -> ExitCode {
    let Some(action) = action else {
        print!("{}", SKILL.body);
        return ExitCode::SUCCESS;
    };
    let Some(home) = crate::agent_skill::home() else {
        return fail_with("error", EXIT_ERROR, "no home directory to install the skill into", None);
    };
    let result = match action {
        SkillAction::Install => crate::agent_skill::install(&home, &SKILL),
        SkillAction::Remove => crate::agent_skill::remove(&home, &SKILL),
    };
    match result {
        Ok(out) => print(&json!({ "paths": out.paths, "notices": out.notices })),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => fail_with(
            "skill_unmanaged",
            EXIT_ERROR,
            &e.to_string(),
            Some("move that path aside, then run `icloud-session skill install` again"),
        ),
        Err(e) => fail_with("io", EXIT_ERROR, &e.to_string(), None),
    }
}

pub fn main() -> ExitCode {
    let args = match cli::parse::<Args>(TOOL) {
        Ok(args) => args,
        Err(code) => return ExitCode::from(code),
    };
    JSON.store(args.json, Ordering::Relaxed);
    if let Some(home) = crate::agent_skill::home() {
        crate::agent_skill::refresh(&home, &SKILL);
    }
    match args.command {
        Command::Status => run(icloud_session::status().map(|s| status_json(&s))),
        Command::SignIn { no_wait: true } => run(open_now(icloud_session::sign_in)),
        Command::SignIn { no_wait: false } => match open_window(icloud_session::sign_in) {
            Ok(s) if s.signed_in => print(&status_json(&s)),
            Ok(s) => {
                println!("{}", status_json(&s));
                fail_with("sign_in_not_completed", EXIT_SIGN_IN, "sign-in did not complete", None)
            }
            Err(e) => fail(e),
        },
        Command::AuthorizeFindMy { no_wait: true } => run(open_now(icloud_session::authorize_find_my)),
        Command::AuthorizeFindMy { no_wait: false } => match open_window(icloud_session::authorize_find_my) {
            Ok(s) if s.find_my_authorized => print(&status_json(&s)),
            Ok(s) => {
                println!("{}", status_json(&s));
                fail_with(
                    "find_my_auth_not_completed",
                    EXIT_FIND_MY_AUTH,
                    "Find My authorization did not complete",
                    None,
                )
            }
            Err(e) => fail(e),
        },
        Command::SetPassword => set_password(),
        Command::ForgetPassword => match call_daemon("ForgetPassword", &()) {
            Ok(()) => {
                eprintln!("icloud-session: removed the stored password from the keyring");
                run(icloud_session::status().map(|s| status_json(&s)))
            }
            Err(code) => code,
        },
        Command::SignOut => run(icloud_session::sign_out()
            .and_then(|()| icloud_session::status())
            .map(|s| status_json(&s))),
        Command::Validate => run(validate()),
        Command::Skill { action } => skill(action),
    }
}

/// Opens the window (unless one is open) and returns the status at once.
fn open_now(open: fn() -> Result<(), Error>) -> Result<serde_json::Value, Error> {
    if !icloud_session::status()?.signing_in {
        open()?;
    }
    eprintln!("icloud-session: finish in the \"Sign in to iCloud\" window (it may be on another workspace)");
    icloud_session::status().map(|s| status_json(&s))
}

/// Reports an error on stderr (one JSON line with `--json`) and returns its
/// exit code.
fn fail_with(code: &str, exit: u8, message: &str, hint: Option<&str>) -> ExitCode {
    ExitCode::from(cli::report(
        TOOL,
        JSON.load(Ordering::Relaxed),
        code,
        exit,
        message,
        hint,
    ))
}

fn status_json(s: &Status) -> serde_json::Value {
    serde_json::to_value(s).expect("status serializes")
}

/// `SignIn()` or `AuthorizeFindMy()`, then waits for the window to close
/// (`SigningIn` false). A window already open is waited for instead.
fn open_window(open: fn() -> Result<(), Error>) -> Result<Status, Error> {
    let mut watch = icloud_session::watch()?;
    let before = icloud_session::status()?;
    if !before.signing_in {
        open()?;
    }
    eprintln!("icloud-session: finish in the \"Sign in to iCloud\" window (it may be on another workspace)");
    let mut opened = before.signing_in;
    if let Some(current) = watch.current()
        && current.signing_in
    {
        opened = true;
    }
    for status in watch.by_ref() {
        if status.signing_in {
            opened = true;
        } else if opened {
            return Ok(status);
        }
    }
    icloud_session::status()
}

fn validate() -> Result<serde_json::Value, Error> {
    let session = Session::connect()?;
    let webservices = session.webservices()?;
    Ok(json!({
        "dsid": session.dsid(),
        "apple_id": session.apple_id(),
        "webservices": webservices.urls,
    }))
}

fn run(result: Result<serde_json::Value, Error>) -> ExitCode {
    match result {
        Ok(value) => print(&value),
        Err(e) => fail(e),
    }
}

fn fail(e: Error) -> ExitCode {
    let message = e.to_string();
    match e {
        Error::SignInRequired => fail_with(
            "sign_in_required",
            EXIT_SIGN_IN,
            &icloud_session::sign_in_required_message(),
            Some(SIGN_IN_HINT),
        ),
        Error::FindMyAuthRequired => fail_with(
            "find_my_auth_required",
            EXIT_FIND_MY_AUTH,
            &message,
            Some("run `icloud-session authorize-find-my` (to stop being asked: `icloud-session set-password`)"),
        ),
        Error::KeyringUnavailable(_) => fail_with(
            "keyring_unavailable",
            EXIT_UNAVAILABLE,
            &message,
            Some("unlock the keyring (GNOME Keyring), then try again"),
        ),
        Error::Service(detail) => match service_failure(&detail) {
            ServiceFailure::Unreachable => fail_with(
                "session_service",
                EXIT_UNAVAILABLE,
                &message,
                Some("check that icloud-sessiond is installed and the D-Bus session bus is running"),
            ),
            ServiceFailure::Daemon { name, detail } => daemon_error(name, detail),
        },
        Error::Network(_) => fail_with("network", EXIT_UNAVAILABLE, &message, None),
        Error::BadResponse(_) => fail_with("bad_response", EXIT_UNAVAILABLE, &message, None),
        Error::Offline(_) => fail_with("offline", EXIT_UNAVAILABLE, &message, None),
        Error::Http { status: 401 | 421, .. } => fail_with(
            "session_rejected",
            EXIT_SIGN_IN,
            &format!("Apple refused the iCloud session although icloud-sessiond still holds it ({message})"),
            Some(SIGN_IN_HINT),
        ),
        Error::Http { status: 500..=599, .. } => fail_with("http", EXIT_UNAVAILABLE, &message, None),
        Error::Http { .. } => fail_with("http", EXIT_ERROR, &message, None),
        Error::Io(_) => fail_with("io", EXIT_ERROR, &message, None),
    }
}

const SIGN_IN_HINT: &str = "run `icloud-session sign-in` (a person signs in in the window)";

/// The error name of `ServiceError::PasswordRejected` (sessiond/src/daemon.rs).
const ERROR_PASSWORD_REJECTED: &str = "io.github.ferdousbhai.ICloudSession.Error.PasswordRejected";

/// What an [`Error::Service`] was: the daemon (or the bus) could not be
/// reached, or the daemon answered with an error of its own.
#[derive(Debug, PartialEq, Eq)]
enum ServiceFailure<'a> {
    /// No session bus, nothing owns or can start the name, or no reply.
    Unreachable,
    /// A D-Bus error the daemon replied with (`name`, `detail`).
    Daemon { name: &'a str, detail: &'a str },
}

/// Classifies the text of an [`Error::Service`]. `icloud_session` turns
/// every D-Bus method error into `Service("{error name}: {detail}")`, and
/// any other zbus error (no bus, a broken connection) into its plain
/// message, so the error name, when there is one, tells a daemon-side
/// failure from an unreachable daemon.
fn service_failure(text: &str) -> ServiceFailure<'_> {
    let Some((name, detail)) = text.split_once(": ") else {
        return ServiceFailure::Unreachable;
    };
    let is_error_name = name.contains('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !is_error_name || unreachable_error_name(name) {
        return ServiceFailure::Unreachable;
    }
    ServiceFailure::Daemon { name, detail }
}

/// The bus's own errors for a daemon it cannot reach or start, or a
/// connection that broke: not anything the daemon said.
fn unreachable_error_name(name: &str) -> bool {
    let Some(short) = name.strip_prefix("org.freedesktop.DBus.Error.") else {
        return false;
    };
    short.starts_with("Spawn.")
        || matches!(
            short,
            "ServiceUnknown"
                | "NameHasNoOwner"
                | "NoReply"
                | "TimedOut"
                | "Timeout"
                | "NoServer"
                | "NoNetwork"
                | "Disconnected"
                | "IOError"
                | "BadAddress"
                | "AuthFailed"
                | "AddressInUse"
                | "LimitsExceeded"
                | "NoMemory"
        )
}

/// An error the daemon replied with that `icloud_session` has no variant
/// for: Apple refusing a password (`password_rejected`, exit 3), else a
/// daemon-side failure (`error`, exit 1), not an unreachable daemon.
fn daemon_error(name: &str, detail: &str) -> ExitCode {
    if name == ERROR_PASSWORD_REJECTED {
        return fail_with(
            "password_rejected",
            EXIT_SIGN_IN,
            detail,
            Some("check the Apple ID password, then run `icloud-session set-password` again"),
        );
    }
    fail_with("error", EXIT_ERROR, &format!("icloud-sessiond: {detail}"), None)
}

fn print(value: &serde_json::Value) -> ExitCode {
    println!("{value}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------------ set-password

type Secret = Zeroizing<String>;

fn set_password() -> ExitCode {
    let status = match icloud_session::status() {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let Some(apple_id) = status.apple_id.filter(|_| status.signed_in) else {
        return fail_with(
            "sign_in_required",
            EXIT_SIGN_IN,
            "sign in first: the password is checked against the signed-in Apple ID",
            Some(SIGN_IN_HINT),
        );
    };
    let password = match read_password(&apple_id) {
        Ok(p) if p.is_empty() => {
            return fail_with("error", EXIT_ERROR, "the password is empty; nothing stored", None);
        }
        Ok(p) => p,
        Err(message) => return fail_with("error", EXIT_ERROR, &message, None),
    };
    eprintln!("icloud-session: checking the password with a Find My sign-in…");
    // The password travels to the daemon in the D-Bus message only.
    let result = call_daemon("SetPassword", &(password.as_str(),));
    drop(password);
    match result {
        Ok(()) => {
            eprintln!(
                "icloud-session: Apple accepted the password; stored in the keyring for {apple_id}. \
                 Find My now re-authorizes itself (icloud-session forget-password to undo)"
            );
            run(icloud_session::status().map(|s| status_json(&s)))
        }
        Err(code) => code,
    }
}

/// From the terminal without echo, else all of stdin (one trailing newline
/// dropped).
fn read_password(apple_id: &str) -> Result<Secret, String> {
    if std::io::stdin().is_terminal() {
        let p = rpassword::prompt_password(format!("Apple ID password for {apple_id}: "))
            .map_err(|e| format!("reading the password: {e}"))?;
        return Ok(Zeroizing::new(p));
    }
    let mut p = Zeroizing::new(String::new());
    std::io::stdin()
        .read_to_string(&mut p)
        .map_err(|e| format!("reading the password from stdin: {e}"))?;
    Ok(strip_newline(p))
}

fn strip_newline(mut s: Secret) -> Secret {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// Calls a daemon method that answers nothing, reporting a failure as
/// [`fail`] does (a D-Bus error name the client library has no variant
/// for, e.g. `PasswordRejected`, is classified by [`service_failure`]).
fn call_daemon<B>(method: &str, body: &B) -> Result<(), ExitCode>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let conn = zbus::blocking::Connection::session().map_err(|e| fail(Error::from(e)))?;
    conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body)
        .map(drop)
        .map_err(|e| fail(Error::from(e)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }

    #[test]
    fn a_service_error_tells_an_unreachable_daemon_from_its_own_failure() {
        use super::ServiceFailure::{Daemon, Unreachable};
        use super::service_failure;
        // No bus, or a broken connection: zbus's own message.
        assert_eq!(
            service_failure("I/O error: No such file or directory (os error 2)"),
            Unreachable
        );
        assert_eq!(service_failure("Invalid D-Bus method reply"), Unreachable);
        // The bus cannot reach or start the daemon.
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.Spawn.ChildExited",
        ] {
            assert_eq!(service_failure(&format!("{name}: why")), Unreachable, "{name}");
        }
        // The daemon answered with an error.
        assert_eq!(
            service_failure("io.github.ferdousbhai.ICloudSession.Error.Failed: storing failed: x"),
            Daemon {
                name: "io.github.ferdousbhai.ICloudSession.Error.Failed",
                detail: "storing failed: x"
            }
        );
        assert_eq!(
            service_failure("org.freedesktop.DBus.Error.UnknownMethod: no such method"),
            Daemon {
                name: "org.freedesktop.DBus.Error.UnknownMethod",
                detail: "no such method"
            }
        );
    }
}
