//! What the two binaries link. The sign-in window is GTK and WebKitGTK with
//! omui's look; the daemon, which is also the `icloud-session` command line,
//! is built from the same crate with the same dependencies and must load
//! none of it: it is started for every call an app makes.

use icloud_app::testing::assert_cli_links_no_gtk;

/// `icloud-sessiond` needs neither GTK nor WebKitGTK; the sign-in window
/// (absent from a build without the `signin` feature) needs both, and
/// neither needs libadwaita, which omui's look cannot share a process with.
#[test]
fn the_daemon_links_no_toolkit_and_the_sign_in_window_no_libadwaita() {
    assert_cli_links_no_gtk(
        env!("CARGO_BIN_EXE_icloud-sessiond"),
        option_env!("CARGO_BIN_EXE_icloud-session-signin"),
        &["libgtk-4.", "libwebkitgtk-6.0."],
        &["libadwaita-1."],
    );
}
