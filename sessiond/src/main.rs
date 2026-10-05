//! `icloud-sessiond`: the D-Bus user service that owns the iCloud session.
//! Started by D-Bus activation; exits when idle.
//!
//! The same executable is the `icloud-session` CLI (src/cli.rs): the
//! package installs it once, as icloud-sessiond, and links icloud-session
//! to it, so the two share one copy of zbus, ureq and the rest.

use std::path::Path;
use std::process::ExitCode;

use icloud_sessiond::{cli, daemon};

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let invoked_as = args.next();
    if invoked_as
        .as_deref()
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|name| name == "icloud-session")
    {
        return cli::main();
    }
    let args: Vec<String> = args.map(|a| a.to_string_lossy().into_owned()).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("-V" | "--version") => {
            println!("icloud-sessiond {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(_) => {
            eprintln!("usage: icloud-sessiond   (normally started by D-Bus activation)");
            return ExitCode::from(64);
        }
    }
    let daemon = daemon::Daemon::new(daemon::Config::from_env());
    match daemon.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("icloud-sessiond: {e}");
            ExitCode::FAILURE
        }
    }
}
