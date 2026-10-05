//! `icloud-findmy-app` opens the Find My window (GTK4, libadwaita,
//! libshumate); the desktop entry runs it, and so does `icloud-findmy` with
//! no command. Given a command it runs the command line, like
//! `icloud-findmy`.

fn main() {
    if std::env::args_os().len() > 1 {
        std::process::exit(icloud_findmy::cli::run().into());
    }
    std::process::exit(i32::from(icloud_findmy::ui::run().get()));
}
