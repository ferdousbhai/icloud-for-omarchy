//! `icloud-notes <command>` runs the command line and `icloud-notes --sync`
//! the timer's background sync (see `icloud_notes::cli`). This binary links
//! no toolkit, so the timer's run every minute and a command start in
//! milliseconds. Started as the app (no arguments, or a first one that is
//! no command), it becomes `icloud-notes-app`, the window, in its place:
//! from the same directory as this binary, else from `PATH`. So does
//! `export-pdf`, which lays out a page.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use icloud_notes::cli::{self, Ran};

/// Becomes the app with this run's arguments. Only returns when it could
/// not: the binary it tried, and why.
fn become_the_app() -> (PathBuf, io::Error) {
    let name = "icloud-notes-app";
    let app = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(name))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name));
    let failed = Command::new(&app).args(std::env::args_os().skip(1)).exec();
    (app, failed)
}

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|arg| cli::argument(&arg)).collect();
    let code = match args.first().map(String::as_str) {
        Some("--sync") => cli::run_background_sync(),
        Some(first) if cli::is_cli_invocation(first) => match cli::run(&args) {
            Ran::Exit(code) => code,
            Ran::NeedsApp => {
                let (app, why) = become_the_app();
                cli::report_no_app(&args, &app, &why)
            }
        },
        _ => {
            let (app, why) = become_the_app();
            cli::say(&format!(
                "icloud-notes: cannot open the app ({}: {why}); run `icloud-notes help` for the command line",
                app.display()
            ));
            1
        }
    };
    std::process::exit(code.into());
}
