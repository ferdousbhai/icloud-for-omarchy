//! `icloud-contacts <command>` runs the command line (see
//! `icloud_contacts::cli`). This binary never calls into GTK, so the
//! linker leaves GTK and libadwaita out of it, and a command starts in
//! milliseconds. With no command it opens the app by running
//! `icloud-contacts-app` (the window, `src/bin/app/`) in its place: from
//! the same directory as this binary, else from `PATH`.

fn main() {
    icloud_app::cli::main("icloud-contacts", icloud_contacts::cli::run)
}
