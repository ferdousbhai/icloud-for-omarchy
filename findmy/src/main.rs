//! `icloud-findmy <command>` runs the command line (see
//! `icloud_findmy::cli`). This binary never calls into GTK, so the linker
//! leaves GTK, libadwaita and libshumate out of it and a command starts in
//! a few milliseconds instead of loading some 140 shared libraries first.
//! With no command it opens the app by running `icloud-findmy-app` (the
//! window, `src/bin/app/`) in its place: from the same directory as this
//! binary, else from `PATH`.

fn main() {
    icloud_app::cli::main("icloud-findmy", icloud_findmy::cli::run)
}
