//! `icloud-reminders <command>` runs the command line (see
//! `icloud_reminders::cli`). This binary never calls into GTK, so the
//! linker leaves GTK and libadwaita out of it: the background timer runs it
//! every minute, and a command starts in milliseconds. With no command it
//! opens the app by running `icloud-reminders-app` (the window,
//! `src/bin/app/`) in its place: from the same directory as this binary,
//! else from `PATH`.

fn main() {
    icloud_app::cli::main("icloud-reminders", icloud_reminders::cli::run)
}
