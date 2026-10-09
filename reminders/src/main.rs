//! `icloud-reminders <command>` runs the command line (see
//! `icloud_reminders::cli`). This binary never calls into GTK, so the
//! linker leaves GTK and libadwaita out of it: the background timer runs it
//! every minute, and a command starts in milliseconds. With no command it
//! opens the app by running `icloud-reminders-app` (the window,
//! `src/bin/app.rs`) in its place: from the same directory as this binary,
//! else from `PATH`.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP: &str = "icloud-reminders-app";

fn main() {
    if std::env::args_os().len() <= 1 {
        std::process::exit(open_app());
    }
    std::process::exit(icloud_reminders::cli::run().into());
}

/// Replaces this process with the app; returns only if that failed.
fn open_app() -> i32 {
    let app = beside_this_binary().unwrap_or_else(|| PathBuf::from(APP));
    let err = Command::new(&app).exec();
    eprintln!(
        "icloud-reminders: cannot open the app ({}: {err}); run `icloud-reminders help` for the command line",
        app.display()
    );
    icloud_session::cli::EXIT_ERROR.into()
}

fn beside_this_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent().unwrap_or(Path::new("/")).join(APP);
    app.is_file().then_some(app)
}
