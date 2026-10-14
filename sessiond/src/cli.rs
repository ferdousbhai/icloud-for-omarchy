//! `icloud-session status | sign-in | authorize-find-my | set-password |
//! forget-password | sign-out | validate | skill [install|remove]`
//!
//! Talks to icloud-sessiond over D-Bus. Output follows the shared product
//! contract ([`icloud_session::output`]): a short text rendering on a
//! terminal, the JSON envelope `{"ok":true,"data","summary"}` when piped;
//! `--json`/`--styled`/`--quiet` (only `data`, the bare status or
//! webservices object)/`--ids-only`/`--count` choose. `skill` with no action
//! prints the skill's Markdown as it is, in every mode.
//!
//! A failure is `{"ok":false,"error":{"code","message","hint"?}}` on stdout in
//! a machine mode, `error:`/`hint:` lines on stderr in text mode, with a
//! non-zero exit in the table every iCloud tool shares (`icloud_session::cli`):
//!
//! - 1 error: a failure the daemon reported (`error`), `io`, a non-5xx
//!   `http`, `skill_unmanaged`;
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
//! Progress ("finish in the window", "checking the password") goes to stderr.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use icloud_session::cli::{EXIT_ERROR, EXIT_FIND_MY_AUTH, EXIT_SIGN_IN, EXIT_UNAVAILABLE};
use icloud_session::output::{self, CliError, OutputFlags, Response};
use icloud_session::{BUS_NAME, ERROR_PASSWORD_REJECTED, Error, INTERFACE, OBJECT_PATH, Session, Status};
use serde::Serialize;
use zeroize::Zeroizing;

const AFTER_HELP: &str = "\
Output: short text on a terminal, a JSON envelope {\"ok\":true,\"data\",\"summary\"} when piped.
--json forces the envelope, --styled text, --quiet prints only the data (the status object, or
validate's {dsid, apple_id, webservices}), --ids-only the webservice keys (validate) or the
paths (skill install/remove), --count their number. Flags combine: ids-only > count > quiet >
json > styled. `skill` alone prints the skill's Markdown in every mode. A failure is
{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or error:/hint: lines on stderr
in text mode.

Exit codes:
  0 ok
  1 error (a failure icloud-sessiond reported), io, http 4xx, skill_unmanaged
  2 usage
  3 not signed in or credentials rejected: sign_in_required, sign_in_not_completed,
    find_my_auth_required, find_my_auth_not_completed, password_rejected, session_rejected
  5 a service unavailable: session_service (icloud-sessiond unreachable),
    keyring_unavailable, network, offline, bad_response, http 5xx";

#[derive(Parser)]
#[command(
    name = "icloud-session",
    version,
    about = "The one iCloud sign-in every app uses, through icloud-sessiond.",
    after_help = AFTER_HELP
)]
struct Args {
    #[command(flatten)]
    out: OutputFlags,
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
    /// (sign_in_not_completed, no status printed) when the window closed
    /// without a sign-in.
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
    /// (find_my_auth_not_completed, no status printed) when Find My is still
    /// not authorized.
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
    /// again, then prints status. The password is checked with a Find My
    /// sign-in first and stored only if Apple accepts it (exit 3,
    /// password_rejected, when Apple refuses it). It is read from the
    /// terminal without echo, or from stdin when that is not a terminal,
    /// e.g. from a password manager:
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
    /// Prints the skill (SKILL.md, Markdown) built into icloud-session, as it is (in every
    /// output mode). `install` copies it to ~/.agents/skills/icloud/ (Codex, Pi and others) and
    /// links it into ~/.claude/skills/ when Claude Code is installed; `remove` removes what
    /// install created. Only a copy icloud-session wrote (marked .managed-by-icloud-session) is
    /// ever changed; a new release refreshes it.
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

type Outcome = Result<Response, CliError>;

pub fn main() -> ExitCode {
    let args = match output::parse::<Args>() {
        Ok(args) => args,
        Err(code) => return ExitCode::from(code),
    };
    let mode = args.out.mode();
    if let Some(home) = crate::agent_skill::home() {
        crate::agent_skill::refresh(&home, &SKILL);
    }
    let outcome = match args.command {
        Command::Skill { action: None } => {
            print!("{}", SKILL.body);
            return ExitCode::SUCCESS;
        }
        Command::Skill { action: Some(action) } => skill(action),
        Command::Status => icloud_session::status()
            .map(|s| status_response(&s, None))
            .map_err(failure),
        Command::SignIn { no_wait: true } => open_now(icloud_session::sign_in),
        Command::SignIn { no_wait: false } => sign_in(),
        Command::AuthorizeFindMy { no_wait: true } => open_now(icloud_session::authorize_find_my),
        Command::AuthorizeFindMy { no_wait: false } => authorize_find_my(),
        Command::SetPassword => set_password(),
        Command::ForgetPassword => call_daemon("ForgetPassword", &())
            .and_then(|()| icloud_session::status())
            .map(|s| status_response(&s, Some("Removed the stored password from the keyring.")))
            .map_err(failure),
        Command::SignOut => icloud_session::sign_out()
            .and_then(|()| icloud_session::status())
            .map(|s| status_response(&s, Some("Signed out.")))
            .map_err(failure),
        Command::Validate => validate().map_err(failure),
    };
    ExitCode::from(match outcome {
        Ok(response) => response.print(mode),
        Err(e) => e.report(mode),
    })
}

// ------------------------------------------------------------ rendering

/// The status as the result: `data` is the bare status object; the summary
/// its first line (or `headline`, what the command did), the text all of it.
fn status_response(s: &Status, headline: Option<&str>) -> Response {
    let lines = status_lines(s);
    let summary = headline.map_or_else(|| lines[0].clone(), str::to_string);
    let mut text = Vec::new();
    if let Some(h) = headline {
        text.push(h.to_string());
    }
    text.extend(lines);
    Response::new(s, summary).human(text.join("\n"))
}

/// "Signed in as …", expiry, Find My, the password; or "Not signed in" and why.
fn status_lines(s: &Status) -> Vec<String> {
    let mut lines = Vec::new();
    if s.signed_in {
        let who = s.apple_id.as_deref().unwrap_or("an unknown Apple ID");
        lines.push(match &s.full_name {
            Some(name) => format!("Signed in as {who} ({name})"),
            None => format!("Signed in as {who}"),
        });
        lines.push(match s.expires_at {
            Some(t) => format!("Session expires {}", icloud_session::time::rfc3339(t as i64)),
            None => "Session has no expiry date (signed in without \"Keep me signed in\")".into(),
        });
        lines.push(if s.find_my_authorized {
            "Find My: authorized".into()
        } else {
            "Find My: not authorized (icloud-session authorize-find-my)".into()
        });
        lines.push(if s.find_my_password_stored {
            "Password: stored in the keyring (Find My re-authorizes itself)".into()
        } else {
            "Password: not stored".into()
        });
    } else {
        lines.push(match &s.sign_out_reason {
            Some(reason) => format!("Not signed in: {reason}"),
            None => "Not signed in".into(),
        });
    }
    if s.signing_in {
        lines.push("The sign-in window is open.".into());
    }
    lines
}

// ------------------------------------------------------------ commands

const WINDOW_NOTE: &str = "icloud-session: finish in the \"Sign in to iCloud\" window (it may be on another workspace)";

/// Opens the window (unless one is open) and returns the status at once.
fn open_now(open: fn() -> Result<(), Error>) -> Outcome {
    let run = || -> Result<Status, Error> {
        if !icloud_session::status()?.signing_in {
            open()?;
        }
        eprintln!("{WINDOW_NOTE}");
        icloud_session::status()
    };
    let s = run().map_err(failure)?;
    Ok(status_response(
        &s,
        Some("Sign-in window opened; poll `icloud-session status` until signing_in is false."),
    ))
}

fn sign_in() -> Outcome {
    let s = open_window(icloud_session::sign_in).map_err(failure)?;
    if s.signed_in {
        return Ok(status_response(&s, None));
    }
    let message = match &s.sign_out_reason {
        Some(reason) => format!("sign-in did not complete: the window closed and you are still signed out ({reason})"),
        None => "sign-in did not complete: the window closed and you are still signed out".into(),
    };
    Err(CliError::new("sign_in_not_completed", EXIT_SIGN_IN, message)
        .hint("run `icloud-session sign-in` again and finish in the window"))
}

fn authorize_find_my() -> Outcome {
    let s = open_window(icloud_session::authorize_find_my).map_err(failure)?;
    if s.find_my_authorized {
        return Ok(status_response(&s, Some("Find My is authorized.")));
    }
    let e = match (&s.apple_id, s.signed_in) {
        (Some(who), true) => CliError::new(
            "find_my_auth_not_completed",
            EXIT_FIND_MY_AUTH,
            format!("Find My authorization did not complete: still not authorized for {who}"),
        )
        .hint("run `icloud-session authorize-find-my` again (to stop being asked: `icloud-session set-password`)"),
        _ => CliError::new(
            "find_my_auth_not_completed",
            EXIT_FIND_MY_AUTH,
            "Find My authorization did not complete: not signed in",
        )
        .hint(SIGN_IN_HINT),
    };
    Err(e)
}

/// `SignIn()` or `AuthorizeFindMy()`, then waits for the window to close
/// (`SigningIn` false). A window already open is waited for instead.
fn open_window(open: fn() -> Result<(), Error>) -> Result<Status, Error> {
    let mut watch = icloud_session::watch()?;
    let before = icloud_session::status()?;
    if !before.signing_in {
        open()?;
    }
    eprintln!("{WINDOW_NOTE}");
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

#[derive(Serialize)]
struct Validated {
    dsid: String,
    apple_id: String,
    webservices: std::collections::HashMap<String, String>,
}

fn validate() -> Result<Response, Error> {
    let session = Session::connect()?;
    let webservices = session.webservices()?;
    let mut keys: Vec<String> = webservices.urls.keys().cloned().collect();
    keys.sort();
    let summary = format!(
        "Apple accepts the sign-in of {}: {} services",
        session.apple_id(),
        keys.len()
    );
    let human = format!(
        "Apple ID: {} (dsid {})\nServices: {}",
        session.apple_id(),
        session.dsid(),
        keys.join(", ")
    );
    let data = Validated {
        dsid: session.dsid().to_string(),
        apple_id: session.apple_id().to_string(),
        webservices: webservices.urls,
    };
    Ok(Response::new(data, summary).human(human).ids(keys))
}

#[derive(Serialize)]
struct SkillOutcome {
    paths: Vec<std::path::PathBuf>,
    notices: Vec<String>,
}

fn skill(action: SkillAction) -> Outcome {
    let home = crate::agent_skill::home()
        .ok_or_else(|| CliError::new("error", EXIT_ERROR, "no home directory to install the skill into"))?;
    let (result, done) = match action {
        SkillAction::Install => (crate::agent_skill::install(&home, &SKILL), "Installed"),
        SkillAction::Remove => (crate::agent_skill::remove(&home, &SKILL), "Removed"),
    };
    let out = result.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => CliError::new("skill_unmanaged", EXIT_ERROR, e.to_string())
            .hint("move that path aside, then run `icloud-session skill install` again"),
        _ => CliError::new("io", EXIT_ERROR, e.to_string()),
    })?;
    let ids: Vec<String> = out.paths.iter().map(|p| p.display().to_string()).collect();
    let summary = match ids.len() {
        0 => format!("{done} nothing: the icloud skill was already in that state"),
        1 => format!("{done} the icloud skill: 1 path"),
        n => format!("{done} the icloud skill: {n} paths"),
    };
    let mut human = vec![summary.clone()];
    human.extend(ids.iter().map(|p| format!("  {p}")));
    human.extend(out.notices.iter().map(|n| format!("note: {n}")));
    let data = SkillOutcome {
        paths: out.paths,
        notices: out.notices,
    };
    Ok(Response::new(data, summary).human(human.join("\n")).ids(ids))
}

// ------------------------------------------------------------ errors

const SIGN_IN_HINT: &str = "run `icloud-session sign-in` (a person signs in in the window)";

/// A client-library error as the contract's code, exit code, message and hint.
fn failure(e: Error) -> CliError {
    let message = e.to_string();
    match e {
        Error::SignInRequired => CliError::new(
            "sign_in_required",
            EXIT_SIGN_IN,
            icloud_session::sign_in_required_message(),
        )
        .hint(SIGN_IN_HINT),
        Error::FindMyAuthRequired => CliError::new("find_my_auth_required", EXIT_FIND_MY_AUTH, message)
            .hint("run `icloud-session authorize-find-my` (to stop being asked: `icloud-session set-password`)"),
        Error::KeyringUnavailable(_) => CliError::new("keyring_unavailable", EXIT_UNAVAILABLE, message)
            .hint("unlock the keyring (GNOME Keyring), then try again"),
        Error::Service(_) => CliError::new("session_service", EXIT_UNAVAILABLE, message)
            .hint("check that icloud-sessiond is installed and the D-Bus session bus is running"),
        // Apple refused the password given to set-password.
        Error::Daemon { name, message } if name == ERROR_PASSWORD_REJECTED => {
            CliError::new("password_rejected", EXIT_SIGN_IN, message)
                .hint("check the Apple ID password, then run `icloud-session set-password` again")
        }
        // The daemon was reached and failed: not an unreachable daemon.
        Error::Daemon { .. } => CliError::new("error", EXIT_ERROR, message),
        Error::Network(_) => {
            CliError::new("network", EXIT_UNAVAILABLE, message).hint("check the network connection and try again")
        }
        Error::Offline(_) => {
            CliError::new("offline", EXIT_UNAVAILABLE, message).hint("check the network connection and try again")
        }
        Error::BadResponse(_) => CliError::new("bad_response", EXIT_UNAVAILABLE, message).hint("try again later"),
        Error::Http { status: 401 | 421, .. } => CliError::new(
            "session_rejected",
            EXIT_SIGN_IN,
            format!("Apple refused the iCloud session although icloud-sessiond still holds it ({message})"),
        )
        .hint(SIGN_IN_HINT),
        Error::Http { status: 500..=599, .. } => {
            CliError::new("http", EXIT_UNAVAILABLE, message).hint("iCloud is having trouble; try again later")
        }
        Error::Http { .. } => CliError::new("http", EXIT_ERROR, message),
        Error::Io(_) => CliError::new("io", EXIT_ERROR, message),
        // `icloud_session::Error` is non-exhaustive: a variant added later.
        _ => CliError::new("error", EXIT_ERROR, message),
    }
}

// ------------------------------------------------------------ set-password

type Secret = Zeroizing<String>;

fn set_password() -> Outcome {
    let status = icloud_session::status().map_err(failure)?;
    let Some(apple_id) = status.apple_id.filter(|_| status.signed_in) else {
        return Err(CliError::new(
            "sign_in_required",
            EXIT_SIGN_IN,
            "not signed in: the password is checked against the signed-in Apple ID",
        )
        .hint(SIGN_IN_HINT));
    };
    let password = read_password(&apple_id).map_err(|m| CliError::new("error", EXIT_ERROR, m))?;
    if password.is_empty() {
        return Err(CliError::new(
            "error",
            EXIT_ERROR,
            "the password is empty; nothing stored",
        ));
    }
    eprintln!("icloud-session: checking the password with a Find My sign-in…");
    // The password travels to the daemon in the D-Bus message only.
    let result = call_daemon("SetPassword", &(password.as_str(),));
    drop(password);
    result.map_err(failure)?;
    let s = icloud_session::status().map_err(failure)?;
    Ok(status_response(
        &s,
        Some(&format!(
            "Apple accepted the password; stored in the keyring for {apple_id}. Find My now \
             re-authorizes itself (`icloud-session forget-password` to undo)."
        )),
    ))
}

/// From the terminal without echo, else all of stdin (one trailing newline
/// dropped). The terminal prompt is the password's source, not a question:
/// it is asked whenever stdin is a terminal, in every output mode.
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

/// Calls a daemon method that answers nothing. A D-Bus error the client
/// library has no variant for (e.g. `PasswordRejected`) arrives as
/// [`Error::Daemon`].
fn call_daemon<B>(method: &str, body: &B) -> Result<(), Error>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let conn = zbus::blocking::Connection::session()?;
    conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use icloud_session::{ERROR_PASSWORD_REJECTED, Error};

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }

    #[test]
    fn errors_map_to_the_shared_codes_and_exits() {
        let daemon = |name: &str| Error::Daemon {
            name: name.into(),
            message: "why".into(),
        };
        let cases = [
            (Error::Service("gone".into()), "session_service", 5),
            (daemon("io.github.ferdousbhai.ICloudSession.Error.Failed"), "error", 1),
            (daemon(ERROR_PASSWORD_REJECTED), "password_rejected", 3),
            (Error::KeyringUnavailable("locked".into()), "keyring_unavailable", 5),
            (Error::FindMyAuthRequired, "find_my_auth_required", 3),
            (Error::BadResponse("html".into()), "bad_response", 5),
            (Error::Offline("x".into()), "offline", 5),
            (Error::Network("x".into()), "network", 5),
            (http(401), "session_rejected", 3),
            (http(503), "http", 5),
            (http(404), "http", 1),
        ];
        for (e, code, exit) in cases {
            let what = format!("{e:?}");
            let f = super::failure(e);
            assert_eq!((f.code.as_str(), f.exit), (code, exit), "{what}");
        }
        let f = super::failure(daemon(ERROR_PASSWORD_REJECTED));
        assert_eq!(f.message, "why");
    }

    fn http(status: u16) -> Error {
        Error::Http {
            status,
            body: String::new(),
        }
    }
}
