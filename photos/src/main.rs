//! `icloud-photos`: the command-line interface (`src/cli.rs`), with no GTK
//! linked in, so a command starts in milliseconds instead of loading the
//! toolkit's hundred-odd shared libraries. With no arguments it opens the
//! app, as `icloud-photos open` does, by running `icloud-photos-app`
//! (`src/app.rs`) in its place: the package installs that under
//! `/usr/lib/icloud-photos/`, so that `icloud-photos` is the one command
//! (`icloud_app::cli::app_binary` says where it is looked for, and
//! `$ICLOUD_PHOTOS_APP` names another).

mod cli;

use std::process::ExitCode;

/// The app's one name: the command, the package, the desktop entry, the
/// icon, and the window's Wayland app id.
const NAME: &str = "icloud-photos";

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        return cli::main();
    }
    // Only returns on failure; on success the app replaces this process.
    let err = icloud_app::cli::open_app(NAME);
    eprintln!("{NAME}: {}; run `{NAME} help` for the command line", err.message);
    ExitCode::FAILURE
}
