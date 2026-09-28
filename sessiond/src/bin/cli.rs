//! `icloud-session status | sign-in | sign-out | validate`
//!
//! Talks to icloud-sessiond over D-Bus. JSON on stdout; errors on stderr
//! with a non-zero exit, 2 meaning "sign in to iCloud required".

use std::process::ExitCode;

use icloud_session::{Error, Session, Status};
use serde_json::json;

const USAGE: &str = "usage: icloud-session <command>

commands:
  status     the daemon's properties: signed_in, apple_id, dsid, expires_at, signing_in
  sign-in    open the sign-in window and wait until it closes, then print status
  sign-out   forget the account and the sign-in window's profile, then print status
  validate   the session's webservices (the daemon revalidates when older than 10 minutes)

JSON goes to stdout. Exit codes: 0 ok, 1 error, 2 sign-in required, 64 usage.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["status"] => run(icloud_session::status().map(|s| status_json(&s))),
        ["sign-in"] => match sign_in() {
            Ok(s) if s.signed_in => print(&status_json(&s)),
            Ok(s) => {
                println!("{}", status_json(&s));
                eprintln!("icloud-session: sign-in did not complete");
                ExitCode::from(2)
            }
            Err(e) => fail(e),
        },
        ["sign-out"] => run(icloud_session::sign_out()
            .and_then(|()| icloud_session::status())
            .map(|s| status_json(&s))),
        ["validate"] => run(validate()),
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["-V" | "--version"] => {
            println!("icloud-session {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn status_json(s: &Status) -> serde_json::Value {
    serde_json::to_value(s).expect("status serializes")
}

/// `SignIn()`, then waits for the window to close (`SigningIn` false).
fn sign_in() -> Result<Status, Error> {
    let mut watch = icloud_session::watch()?;
    let before = icloud_session::status()?;
    if !before.signing_in {
        icloud_session::sign_in()?;
    }
    eprintln!("icloud-session: finish signing in in the \"Sign in to iCloud\" window (it may be on another workspace)");
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
    eprintln!("icloud-session: {e}");
    match e {
        Error::SignInRequired => ExitCode::from(2),
        _ => ExitCode::FAILURE,
    }
}

fn print(value: &serde_json::Value) -> ExitCode {
    println!("{value}");
    ExitCode::SUCCESS
}
