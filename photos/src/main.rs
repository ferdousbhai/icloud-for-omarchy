//! `icloud-photos`: the command-line interface (`src/cli.rs`), with no GTK
//! linked in, so a command starts in milliseconds instead of loading the
//! toolkit's hundred-odd shared libraries. With no arguments it opens the
//! app by running `icloud-photos-app` (`src/app.rs`) in its place.

mod cli;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// The window's binary, installed next to this one.
const APP_BIN: &str = "icloud-photos-app";

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        return cli::main();
    }
    let app = app_binary();
    // Only returns on failure; on success the app replaces this process.
    let e = Command::new(&app).arg0(APP_BIN).exec();
    eprintln!("icloud-photos: cannot open the app ({}): {e}", app.display());
    ExitCode::FAILURE
}

/// `$ICLOUD_PHOTOS_APP` (tests, development), else `icloud-photos-app`
/// beside this executable, else whichever one is on `PATH`.
fn app_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("ICLOUD_PHOTOS_APP").filter(|p| !p.is_empty()) {
        return p.into();
    }
    std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(APP_BIN))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| APP_BIN.into())
}
