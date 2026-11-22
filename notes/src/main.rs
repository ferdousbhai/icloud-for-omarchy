//! `icloud-notes <command>` runs the command line and `icloud-notes --sync`
//! the timer's background sync (see `icloud_notes::cli`). This binary links
//! no toolkit, so the timer's run every minute and a command start in
//! milliseconds. `icloud-notes open` (the desktop entry's) opens the window:
//! it becomes `icloud-notes-app` in its place, found as every iCloud app's
//! window is (`icloud_app::cli::app_binary`: in /usr/lib/icloud-notes/ in the
//! package, beside this binary in a cargo build, else on `PATH`). So does a
//! start with no arguments, or a first one that is no command, and
//! `export-pdf`, which lays out a page.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use icloud_notes::cli::{self, Ran};

/// The app's one name, which its window binary is found by.
const NAME: &str = "icloud-notes";

/// Becomes the app with this run's arguments. Only returns when it could
/// not: the binary it tried, and why.
fn become_the_app() -> (PathBuf, io::Error) {
    let app = icloud_app::cli::app_binary(NAME);
    let failed = Command::new(&app).args(std::env::args_os().skip(1)).exec();
    (app, failed)
}

/// What a window that could not be opened says, as it always has.
fn no_window(why: &str) -> u8 {
    cli::say(&format!(
        "icloud-notes: {why}; run `icloud-notes help` for the command line"
    ));
    1
}

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|arg| cli::argument(&arg)).collect();
    let code = match args.first().map(String::as_str) {
        Some("--sync") => cli::run_background_sync(),
        // The window, with nothing after the word for it to take.
        Some("open") => no_window(&icloud_app::cli::open_app(NAME).message),
        Some(first) if cli::is_cli_invocation(first) => match cli::run(&args) {
            Ran::Exit(code) => code,
            Ran::NeedsApp => {
                let (app, why) = become_the_app();
                cli::report_no_app(&args, &app, &why)
            }
        },
        _ => {
            let (app, why) = become_the_app();
            no_window(&format!("cannot open the app ({}: {why})", app.display()))
        }
    };
    std::process::exit(code.into());
}
