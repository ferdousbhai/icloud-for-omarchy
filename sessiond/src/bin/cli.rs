//! `icloud-session status | sign-in | authorize-find-my | set-password |
//! forget-password | sign-out | validate`
//!
//! Talks to icloud-sessiond over D-Bus. JSON on stdout; errors on stderr
//! with a non-zero exit, in the table every iCloud tool shares: 1 error,
//! 2 sign-in required, 4 Find My not authorized, 64 usage. With `--json`
//! an error is one JSON line on stderr:
//! `{"error":{"code","message","exit_code","hint"?}}`.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use icloud_session::{BUS_NAME, Error, INTERFACE, OBJECT_PATH, Session, Status};
use serde_json::json;
use zeroize::Zeroizing;

const EXIT_ERROR: u8 = 1;
const EXIT_SIGN_IN: u8 = 2;
const EXIT_FIND_MY_AUTH: u8 = 4;
const EXIT_USAGE: u8 = 64;

/// `--json` anywhere on the command line: errors as JSON (the output
/// already is).
static JSON: AtomicBool = AtomicBool::new(false);

/// Each command's name, its synopsis and what it does (`<command> --help`).
const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "status",
        "status",
        "The daemon's properties: signed_in, apple_id, dsid, expires_at (Unix seconds; null for a\n\
         sign-in without \"Keep me signed in\"), signing_in, find_my_authorized, find_my_password_stored.",
    ),
    (
        "sign-in",
        "sign-in [--no-wait]",
        "Open the sign-in window (Apple's page: password and 2FA, typed by a person) and wait until it\n\
         closes, then print status. Exits 2 when the window closed without a sign-in. --no-wait prints\n\
         status at once instead; poll `status` until signing_in is false.",
    ),
    (
        "authorize-find-my",
        "authorize-find-my [--no-wait]",
        "Open the sign-in window on Find My, where Apple asks for the password again, and wait until it\n\
         closes, then print status. Exits 4 when Find My is still not authorized. --no-wait as for sign-in.",
    ),
    (
        "set-password",
        "set-password",
        "Store the Apple ID password in the keyring (GNOME Keyring), so the daemon re-authorizes Find My\n\
         by itself when it asks for the password again. The password is checked with a Find My sign-in\n\
         first and stored only if Apple accepts it. It is read from the terminal without echo, or from\n\
         stdin when that is not a terminal, e.g. from a password manager:\n\
         bw get password \"Apple ID\" | icloud-session set-password\n\
         op read \"op://Private/Apple ID/password\" | icloud-session set-password",
    ),
    (
        "forget-password",
        "forget-password",
        "Remove the stored password from the keyring, then print status.",
    ),
    (
        "sign-out",
        "sign-out",
        "Forget the account and the sign-in window's profile, then print status (a stored password stays\n\
         until forget-password). Every iCloud app is signed out.",
    ),
    (
        "validate",
        "validate",
        "The session's webservices: {dsid, apple_id, webservices} (the daemon revalidates when older than\n\
         10 minutes). Exits 2 when Apple no longer accepts the sign-in.",
    ),
];

const FOOTER: &str = "--json: errors as one JSON line on stderr, {\"error\":{\"code\",\"message\",\"exit_code\"}} (output is JSON anyway).
Exit codes: 0 ok, 1 error, 2 sign-in required (or sign-in not completed), 4 Find My not authorized
(or its authorization not completed), 64 usage.";

fn usage_text() -> String {
    let mut out = String::from("usage: icloud-session <command> [--json]\n\ncommands:\n");
    for (_, synopsis, help) in COMMANDS {
        out.push_str(&format!("  {synopsis}\n"));
        for line in help.lines() {
            out.push_str(&format!("      {}\n", line.trim_start()));
        }
    }
    out.push_str("\n`icloud-session <command> --help` shows one command.\n");
    out.push_str(FOOTER);
    out
}

fn command_help(name: &str) -> Option<String> {
    let (_, synopsis, help) = COMMANDS.iter().find(|(n, ..)| *n == name)?;
    let body: Vec<&str> = help.lines().map(str::trim_start).collect();
    Some(format!(
        "usage: icloud-session {synopsis} [--json]\n\n{}\n\n{FOOTER}",
        body.join("\n")
    ))
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    JSON.store(raw.iter().any(|a| a == "--json"), Ordering::Relaxed);
    let args: Vec<&str> = raw.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    let wants_help = args.iter().any(|a| matches!(*a, "-h" | "--help"));
    match args.as_slice() {
        ["-h" | "--help" | "help"] => {
            println!("{}", usage_text());
            return ExitCode::SUCCESS;
        }
        ["help", name] | [name, ..] if wants_help || args.first() == Some(&"help") => {
            return match command_help(name) {
                Some(text) => {
                    println!("{text}");
                    ExitCode::SUCCESS
                }
                None => usage(&format!("unknown command \"{name}\"")),
            };
        }
        _ => {}
    }
    match args.as_slice() {
        ["status"] => run(icloud_session::status().map(|s| status_json(&s))),
        ["sign-in", rest @ ..] => match no_wait(rest) {
            Some(true) => run(open_now(icloud_session::sign_in)),
            Some(false) => match open_window(icloud_session::sign_in) {
                Ok(s) if s.signed_in => print(&status_json(&s)),
                Ok(s) => {
                    println!("{}", status_json(&s));
                    fail_with("sign_in_not_completed", EXIT_SIGN_IN, "sign-in did not complete", None)
                }
                Err(e) => fail(e),
            },
            None => usage("sign-in takes only --no-wait"),
        },
        ["authorize-find-my", rest @ ..] => match no_wait(rest) {
            Some(true) => run(open_now(icloud_session::authorize_find_my)),
            Some(false) => match open_window(icloud_session::authorize_find_my) {
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
            None => usage("authorize-find-my takes only --no-wait"),
        },
        ["set-password"] => set_password(),
        ["forget-password"] => match call_daemon("ForgetPassword", &()) {
            Ok(()) => {
                eprintln!("icloud-session: removed the stored password from the keyring");
                run(icloud_session::status().map(|s| status_json(&s)))
            }
            Err(code) => code,
        },
        ["sign-out"] => run(icloud_session::sign_out()
            .and_then(|()| icloud_session::status())
            .map(|s| status_json(&s))),
        ["validate"] => run(validate()),
        ["-V" | "--version"] => {
            if JSON.load(Ordering::Relaxed) {
                println!("{}", json!({ "version": env!("CARGO_PKG_VERSION") }));
            } else {
                println!("icloud-session {}", env!("CARGO_PKG_VERSION"));
            }
            ExitCode::SUCCESS
        }
        [] => usage("no command given"),
        [name, ..] if COMMANDS.iter().any(|(n, ..)| n == name) => usage(&format!(
            "{name}: unexpected arguments (see icloud-session {name} --help)"
        )),
        [name, ..] => usage(&format!("unknown command \"{name}\"")),
    }
}

/// `--no-wait` or nothing; None for anything else.
fn no_wait(rest: &[&str]) -> Option<bool> {
    match rest {
        [] => Some(false),
        ["--no-wait"] => Some(true),
        _ => None,
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

/// A usage error: the usage on stderr for a person, or the JSON error.
fn usage(message: &str) -> ExitCode {
    if !JSON.load(Ordering::Relaxed) {
        eprintln!("{}\n", usage_text());
    }
    fail_with("usage", EXIT_USAGE, message, None)
}

/// Reports an error on stderr (one JSON line with `--json`) and returns its
/// exit code.
fn fail_with(code: &str, exit: u8, message: &str, hint: Option<&str>) -> ExitCode {
    if JSON.load(Ordering::Relaxed) {
        let mut error = json!({ "code": code, "message": message, "exit_code": exit });
        if let Some(hint) = hint {
            error["hint"] = json!(hint);
        }
        eprintln!("{}", json!({ "error": error }));
    } else {
        eprintln!("icloud-session: {message}");
        if let Some(hint) = hint {
            eprintln!("{hint}");
        }
    }
    ExitCode::from(exit)
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
