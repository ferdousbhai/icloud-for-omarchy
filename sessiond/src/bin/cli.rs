//! `icloud-session status | sign-in | authorize-find-my | set-password |
//! forget-password | sign-out | validate`
//!
//! Talks to icloud-sessiond over D-Bus. JSON on stdout; errors on stderr
//! with a non-zero exit, in the table every iCloud tool shares: 1 error,
//! 2 sign-in required, 4 Find My not authorized, 64 usage
//! (`icloud_session::cli`). With `--json` an error is one JSON line on stderr:
//! `{"error":{"code","message","exit_code","hint"?}}`.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_FIND_MY_AUTH, EXIT_SIGN_IN};
use icloud_session::{BUS_NAME, Error, INTERFACE, OBJECT_PATH, Session, Status};
use serde_json::json;
use zeroize::Zeroizing;

const TOOL: &str = "icloud-session";

/// `--json`: errors as JSON (the output already is).
static JSON: AtomicBool = AtomicBool::new(false);

const AFTER_HELP: &str = "\
--json: errors as one JSON line on stderr, {\"error\":{\"code\",\"message\",\"exit_code\",\"hint\"?}}
(output is JSON anyway).
Exit codes: 0 ok, 1 error, 2 sign-in required (or sign-in not completed), 4 Find My not authorized
(or its authorization not completed), 64 usage.";

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
    /// The daemon's properties: signed_in, apple_id, dsid, expires_at (Unix
    /// seconds; null for a sign-in without "Keep me signed in"), signing_in,
    /// find_my_authorized, find_my_password_stored.
    #[command(after_help = AFTER_HELP)]
    Status,
    /// Open the sign-in window and wait for it, then print status.
    ///
    /// Opens the sign-in window (Apple's page: password and 2FA, typed by a
    /// person) and waits until it closes, then prints status. Exits 2 when
    /// the window closed without a sign-in.
    #[command(after_help = AFTER_HELP)]
    SignIn {
        /// Print status at once instead; poll `status` until signing_in is false.
        #[arg(long)]
        no_wait: bool,
    },
    /// Open the sign-in window on Find My's password page, then print status.
    ///
    /// Opens the sign-in window on Find My, where Apple asks for the password
    /// again, and waits until it closes, then prints status. Exits 4 when
    /// Find My is still not authorized.
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
    /// only if Apple accepts it. It is read from the terminal without echo,
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
    /// revalidates when older than 10 minutes). Exits 2 when Apple no longer
    /// accepts the sign-in.
    #[command(after_help = AFTER_HELP)]
    Validate,
}

fn main() -> ExitCode {
    let args = match cli::parse::<Args>(TOOL) {
        Ok(args) => args,
        Err(code) => return ExitCode::from(code),
    };
    JSON.store(args.json, Ordering::Relaxed);
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
            &message,
            Some("run `icloud-session sign-in` (a person signs in in the window)"),
        ),
        Error::FindMyAuthRequired => fail_with(
            "find_my_auth_required",
            EXIT_FIND_MY_AUTH,
            &message,
            Some("run `icloud-session authorize-find-my` (to stop being asked: `icloud-session set-password`)"),
        ),
        Error::Service(_) => fail_with("session_service", EXIT_ERROR, &message, None),
        Error::Network(_) => fail_with("network", EXIT_ERROR, &message, None),
        Error::Http { .. } => fail_with("http", EXIT_ERROR, &message, None),
        Error::Io(_) => fail_with("io", EXIT_ERROR, &message, None),
    }
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
            "sign in first (icloud-session sign-in)",
            None,
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

/// Calls a daemon method that answers nothing, mapping its errors to a
/// message and an exit code.
fn call_daemon<B>(method: &str, body: &B) -> Result<(), ExitCode>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let conn = zbus::blocking::Connection::session().map_err(|e| fail(Error::from(e)))?;
    match conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, body) {
        Ok(_) => Ok(()),
        Err(zbus::Error::MethodError(name, message, _)) => {
            let message = message.unwrap_or_else(|| name.to_string());
            Err(match name.as_str().rsplit('.').next() {
                Some("SignInRequired") => fail_with(
                    "sign_in_required",
                    EXIT_SIGN_IN,
                    "sign in first (icloud-session sign-in)",
                    None,
                ),
                _ => fail_with("error", EXIT_ERROR, &message, None),
            })
        }
        Err(e) => Err(fail(Error::from(e))),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }
}
