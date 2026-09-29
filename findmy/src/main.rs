//! `icloud-findmy` opens the app; `icloud-findmy <command>` runs the command
//! line (see `icloud_findmy::cli`) without initializing GTK.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        std::process::exit(gui());
    }
    std::process::exit(icloud_findmy::cli::run(&args));
}

#[cfg(feature = "ui")]
fn gui() -> i32 {
    i32::from(icloud_findmy::ui::run().get())
}

#[cfg(not(feature = "ui"))]
fn gui() -> i32 {
    eprintln!("icloud-findmy: built without the app (the `ui` feature); run `icloud-findmy help`");
    icloud_findmy::cli::EXIT_USAGE
}
