//! The `icloud-notes` binary itself: what it links, and what it hands to
//! the app's binary. Its commands are held by tests/blackbox/.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use icloud_app::testing::{assert_cli_links_no_gtk, assert_no_command_runs_the_app, write_script};

const CLI: &str = env!("CARGO_BIN_EXE_icloud-notes");

/// `icloud-notes <args>` from a directory holding a copy of the binary and
/// an `icloud-notes-app` that records its arguments and exits 7.
fn beside_an_app(args: &[&str]) -> (Output, String) {
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("icloud-notes");
    fs::copy(CLI, &copy).unwrap();
    let ran = tmp.path().join("ran");
    write_script(
        &tmp.path().join("icloud-notes-app"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 7\n", ran.display()),
    );
    let out = run(&copy, args, tmp.path());
    (out, fs::read_to_string(ran).unwrap_or_default())
}

/// A binary's run with nothing of the owner's in reach: a scratch home, no
/// session bus, no engine, and a vault that is not there.
fn scratch(binary: &Path, args: &[&str], scratch: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .env("PATH", scratch)
        .env("HOME", scratch)
        .env("XDG_RUNTIME_DIR", scratch)
        .env("XDG_DATA_HOME", scratch)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .env("ICLOUD_NOTES_VAULT", scratch.join("no-vault"))
        .env("ICLOUD_NOTES_SYNC_BIN", scratch.join("no-engine"))
        .stdin(Stdio::null());
    command
}

/// Runs it ([`scratch`]).
fn run(binary: &Path, args: &[&str], scratch_dir: &Path) -> Output {
    // A test thread forking while a copy was open for writing holds its fd
    // until that child execs: "Text file busy" (ETXTBSY) for a moment.
    for _ in 0..50 {
        let out = scratch(binary, args, scratch_dir).output();
        match out {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            out => return out.expect("the binary runs"),
        }
    }
    panic!("the binary stayed busy");
}

#[test]
fn the_command_line_binary_does_not_link_gtk() {
    // Nor what draws the window or a page: GTK's libraries, the Qt the
    // window was once drawn with, WebKit.
    assert_cli_links_no_gtk(
        CLI,
        None,
        &[
            "libgtk-",
            "libadwaita-",
            "libQt",
            "libwebkit",
            "libjavascriptcore",
            "libgdk",
            "libpango",
            "libcairo",
        ],
        &[],
    );
}

#[test]
fn with_no_command_the_binary_runs_the_app() {
    assert_no_command_runs_the_app(CLI, "icloud-notes");
}

#[test]
fn a_first_argument_that_is_no_command_goes_to_the_app_with_the_rest() {
    for args in [&["bogus", "--json"][..], &["-x"], &["--folder=Notes", "list"]] {
        let (out, ran) = beside_an_app(args);
        assert_eq!(out.status.code(), Some(7), "{args:?}: {out:?}");
        assert_eq!(ran.lines().collect::<Vec<_>>(), args, "{out:?}");
    }
}

#[test]
fn a_command_stays_here_and_the_app_is_not_run() {
    // With no display either: the command line never needs one.
    for args in [
        &["--version"][..],
        &["--help"],
        &["help"],
        &["--json", "status"],
        &["list", "--bogus"],
        &["--sync"],
    ] {
        let (out, ran) = beside_an_app(args);
        assert_ne!(out.status.code(), Some(7), "{args:?}: {out:?}");
        assert_eq!(ran, "", "{args:?}");
    }
    let (out, _) = beside_an_app(&["--version"]);
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        concat!("icloud-notes ", env!("CARGO_PKG_VERSION"), "\n")
    );
}

#[test]
fn open_runs_the_app_with_nothing_after_it() {
    for args in [&["open"][..], &["open", "--json", "list"]] {
        let (out, ran) = beside_an_app(args);
        assert_eq!(out.status.code(), Some(7), "{args:?}: {out:?}");
        assert_eq!(
            ran.trim_end_matches('\n'),
            "",
            "{args:?}: the app is given no arguments"
        );
    }
}

/// The package puts the window in /usr/lib/icloud-notes/, beside nothing on
/// `PATH`: the command finds it there, from its own directory.
#[test]
fn open_finds_the_app_where_the_package_puts_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, lib) = (tmp.path().join("bin"), tmp.path().join("lib/icloud-notes"));
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&lib).unwrap();
    let copy = bin.join("icloud-notes");
    fs::copy(CLI, &copy).unwrap();
    let ran = tmp.path().join("ran");
    write_script(
        &lib.join("icloud-notes-app"),
        &format!("#!/bin/sh\necho opened > '{}'\nexit 7\n", ran.display()),
    );
    let out = run(&copy, &["open"], tmp.path());
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    assert_eq!(fs::read_to_string(ran).unwrap(), "opened\n");
}

#[test]
fn export_pdf_is_laid_out_by_the_app_once_its_arguments_are_right() {
    // The whole command goes over, as it was given.
    let args = ["--json", "--vault", "/nowhere", "export-pdf", "Some note"];
    let (out, ran) = beside_an_app(&args);
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    assert_eq!(ran.lines().collect::<Vec<_>>(), args);
    // A usage error is this binary's own, and so is the help.
    let (out, ran) = beside_an_app(&["--json", "export-pdf"]);
    assert_eq!((out.status.code(), ran.as_str()), (Some(1), ""), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: icloud-notes export-pdf NOTE"));
    let (out, ran) = beside_an_app(&["export-pdf", "--help"]);
    assert_eq!((out.status.code(), ran.as_str()), (Some(0), ""), "{out:?}");
}

#[test]
fn a_failure_keeps_its_exit_code_when_nobody_reads_stderr() {
    // `icloud-notes … 2>&1 | head -1`, once head has gone.
    let tmp = tempfile::tempdir().unwrap();
    for (args, code) in [
        (&["--styled", "list", "--nope"][..], 1),
        (&["--styled", "read", "no such note"], 1),
        (&["--ids-only", "status"], 0),
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut command = scratch(Path::new(CLI), args, tmp.path());
        let status = command.stdout(Stdio::null()).stderr(writer).status().unwrap();
        assert_eq!(status.code(), Some(code), "{args:?}");
    }
}

#[test]
fn without_an_app_the_binary_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("icloud-notes");
    fs::copy(CLI, &copy).unwrap();
    // The window: a line on stderr, exit 1.
    let out = run(&copy, &[], tmp.path());
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let said = String::from_utf8(out.stderr).unwrap();
    assert!(
        said.starts_with("icloud-notes: cannot open the app (icloud-notes-app: "),
        "{said}"
    );
    assert!(
        said.ends_with("run `icloud-notes help` for the command line\n"),
        "{said}"
    );
    // open says the same.
    let out = run(&copy, &["open"], tmp.path());
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let said = String::from_utf8(out.stderr).unwrap();
    assert!(
        said.starts_with("icloud-notes: cannot open the app (icloud-notes-app: "),
        "{said}"
    );
    assert!(
        said.ends_with("run `icloud-notes help` for the command line\n"),
        "{said}"
    );
    // export-pdf: the contract's failure, in the mode asked for.
    let out = run(&copy, &["--json", "export-pdf", "Some note"], tmp.path());
    assert_eq!(
        (out.status.code(), out.stdout.as_slice()),
        (Some(1), &b""[..]),
        "{out:?}"
    );
    let said = String::from_utf8(out.stderr).unwrap();
    assert!(
        said.starts_with("{\"ok\":false,\"error\":{\"code\":\"error\","),
        "{said}"
    );
    assert!(
        said.contains("the Notes app is needed to lay out a PDF, and icloud-notes-app did not run: "),
        "{said}"
    );
    let out = run(&copy, &["--styled", "export-pdf", "Some note"], tmp.path());
    assert_eq!(
        (out.status.code(), out.stdout.as_slice()),
        (Some(1), &b""[..]),
        "{out:?}"
    );
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .starts_with("error: the Notes app is needed")
    );
}
