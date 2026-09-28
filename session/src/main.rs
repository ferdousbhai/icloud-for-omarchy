//! `icloud-session status | validate | reauthenticate [directory]`
//!
//! JSON on stdout; errors on stderr with a non-zero exit, 2 meaning
//! "sign in to iCloud required".

use std::path::Path;
use std::process::ExitCode;

use icloud_session::{Error, Session, format_time, status};
use serde_json::json;

const USAGE: &str = "usage: icloud-session <command>

commands:
  status                      signed-in state from local files, no network (always exits 0)
  validate                    webservices from the shared cache, validating with Apple if stale
  reauthenticate [directory]  run `icloud-md reauthenticate [directory]` interactively

JSON goes to stdout. Exit codes: 0 ok, 1 error, 2 sign-in required, 64 usage.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["status"] => print(&serde_json::to_value(status()).expect("status serializes")),
        ["validate"] => run(validate()),
        ["reauthenticate"] => run(reauthenticate(None)),
        ["reauthenticate", dir] => run(reauthenticate(Some(Path::new(dir)))),
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

fn validate() -> Result<serde_json::Value, Error> {
    let session = Session::load()?;
    let webservices = session.webservices()?;
    Ok(json!({
        "dsid": session.dsid(),
        "apple_id": session.apple_id(),
        "validated_at": session.validated_at().map(format_time),
        "webservices": webservices.urls,
    }))
}

fn reauthenticate(dir: Option<&Path>) -> Result<serde_json::Value, Error> {
    match dir {
        Some(dir) => Session::reauthenticate_in(dir)?,
        None => Session::reauthenticate()?,
    }
    Ok(serde_json::to_value(status()).expect("status serializes"))
}

fn run(result: Result<serde_json::Value, Error>) -> ExitCode {
    match result {
        Ok(value) => print(&value),
        Err(e) => {
            eprintln!("icloud-session: {e}");
            match e {
                Error::SignInRequired => ExitCode::from(2),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

fn print(value: &serde_json::Value) -> ExitCode {
    println!("{value}");
    ExitCode::SUCCESS
}
