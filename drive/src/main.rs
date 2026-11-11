//! `icloud-drive`: see `icloud_drive::cli`.

fn main() {
    std::process::exit(icloud_drive::cli::run().into());
}
