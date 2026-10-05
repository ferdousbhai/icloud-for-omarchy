//! `icloud-findmy <command>` runs the command line (see
//! `icloud_findmy::cli`). This binary never calls into GTK, so the linker
//! leaves GTK, libadwaita and libshumate out of it and a command starts in
//! a few milliseconds instead of loading some 140 shared libraries first.
//! With no command it opens the app by running `icloud-findmy-app` (the
//! window, `src/bin/app.rs`) in its place: from the same directory as this
//! binary, else from `PATH`.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP: &str = "icloud-findmy-app";

fn main() {
    if std::env::args_os().len() <= 1 {
        std::process::exit(open_app());
    }
    std::process::exit(icloud_findmy::cli::run().into());
}

/// Replaces this process with the app; returns only if that failed.
fn open_app() -> i32 {
    let app = beside_this_binary().unwrap_or_else(|| PathBuf::from(APP));
    let err = Command::new(&app).exec();
    eprintln!(
        "icloud-findmy: cannot open the app ({}: {err}); run `icloud-findmy help` for the command line",
        app.display()
    );
    icloud_session::cli::EXIT_ERROR.into()
}

fn beside_this_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent().unwrap_or(Path::new("/")).join(APP);
    app.is_file().then_some(app)
}
