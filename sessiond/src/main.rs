//! `icloud-sessiond`: the D-Bus user service that owns the iCloud session.
//! Started by D-Bus activation; exits when idle.

mod apple;
mod cookies;
mod daemon;
mod files;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
