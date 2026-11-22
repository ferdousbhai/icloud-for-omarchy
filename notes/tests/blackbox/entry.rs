//! What the first arguments make of a run: the version, the help, the
//! parsing of options and the usage errors.

use crate::world::{USAGE_HINT, World, fixture};

/// Every command, in the order the usage text lists them.
pub const COMMANDS: [&str; 27] = [
    "status",
    "folders",
    "list",
    "read",
    "search",
    "new",
    "write",
    "rename",
    "move",
    "delete",
    "toggle",
    "attach",
    "detach",
    "resolve",
    "recover",
    "export-pdf",
    "new-folder",
    "rename-folder",
    "delete-folder",
    "sync",
    "pull",
    "push",
    "clone",
    "history",
    "diff",
    "restore",
    "leave",
];

#[test]
fn version_is_one_plain_line_in_every_mode() {
    let Some(w) = World::new() else { return };
    let plain = w.run(["--version"]);
    assert_eq!((plain.code, plain.err.as_str()), (0, ""));
    let version = plain
        .out
        .strip_prefix("icloud-notes ")
        .and_then(|v| v.strip_suffix('\n'))
        .expect(&plain.out);
    assert!(
        !version.is_empty() && !version.contains(char::is_whitespace),
        "{version:?}"
    );
    for args in [
        vec!["-V"],
        vec!["--json", "--version"],
        vec!["--quiet", "-V"],
        vec!["--styled", "--version"],
        vec!["--count", "--version"],
        vec!["--ids-only", "-V"],
        vec!["--vault", "/nowhere", "--version"],
    ] {
        let r = w.run(&args);
        assert_eq!(
            (r.code, r.out.as_str(), r.err.as_str()),
            (0, plain.out.as_str(), ""),
            "{args:?}"
        );
    }
}

#[test]
fn help_is_the_usage_text_in_every_mode() {
    let Some(w) = World::new() else { return };
    let help = w.run(["help"]);
    assert_eq!((help.code, help.err.as_str()), (0, ""));
    fixture("help/usage.txt", &help.out);
    assert!(
        help.out
            .starts_with("Usage: icloud-notes [open]          open the app\n")
    );
    for command in COMMANDS {
        assert!(help.out.contains(&format!("\n  {command}")), "help lists {command}");
    }
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["--json", "help"],
        vec!["--quiet", "--help"],
        vec!["--count", "-h"],
    ] {
        let r = w.run(&args);
        assert_eq!(
            (r.code, r.out.as_str(), r.err.as_str()),
            (0, help.out.as_str(), ""),
            "{args:?}"
        );
    }
}

#[test]
fn every_command_has_its_own_help() {
    let Some(w) = World::new() else { return };
    let usage = w.run(["help"]).out;
    // The footer every help ends with: the output modes and the exit codes.
    let footer = &usage[usage.find("Output: text on a terminal").unwrap()..];
    for command in COMMANDS {
        let help = w.run(["help", command]);
        assert_eq!((help.code, help.err.as_str()), (0, ""), "{command}");
        fixture(&format!("help/{command}.txt"), &help.out);
        assert!(
            help.out.starts_with(&format!("Usage: icloud-notes {command}")),
            "{command}"
        );
        assert!(help.out.ends_with(footer), "{command}");
        for args in [
            vec![command, "--help"],
            vec![command, "-h"],
            vec!["--json", command, "--help"],
        ] {
            let r = w.run(&args);
            assert_eq!(
                (r.code, r.out.as_str(), r.err.as_str()),
                (0, help.out.as_str(), ""),
                "{args:?}"
            );
        }
    }
    // Help needs no arguments of the command, nor a vault.
    assert_eq!(w.run(["read", "--help"]).code, 0);
    let (message, hint) = w.run(["help", "bogus"]).fails("usage");
    assert_eq!(
        (message.as_str(), hint.as_str()),
        ("unknown command \"bogus\"", USAGE_HINT)
    );
}

#[test]
fn usage_errors_name_what_is_wrong() {
    let Some(w) = World::seeded() else { return };
    let cases: &[(&[&str], &str)] = &[
        (&["--json"], "no command given"),
        (&["--json", "bogus"], "unknown command \"bogus\""),
        (&["list", "--nope"], "unknown option --nope"),
        (&["list", "-x"], "unknown option -x"),
        (&["list", "--folder"], "--folder needs a value"),
        (&["new", "T", "--body"], "--body needs a value"),
        (&["--vault"], "--vault needs a value"),
        (&["--wait"], "--wait needs a value"),
        (&["list", "--json=1"], "--json takes no value"),
        (&["read", "alpha", "--raw=yes"], "--raw takes no value"),
        (&["delete", "alpha", "--yes=1"], "--yes takes no value"),
        (&["read", "alpha", "--yes"], "read does not take --yes"),
        (&["list", "--push"], "list does not take --push"),
        (&["pull", "--yes"], "pull does not take --yes"),
        (&["status", "--folder", "Notes"], "status does not take --folder"),
        (
            &["list", "extra"],
            "usage: icloud-notes list [--folder F] [--flag FLAG]",
        ),
        (&["read"], "usage: icloud-notes read NOTE [--raw]"),
        (&["attach", "alpha"], "usage: icloud-notes attach NOTE FILE [--push]"),
        (
            &["toggle", "alpha", "1", "2"],
            "usage: icloud-notes toggle NOTE LINE [--push]",
        ),
        (&["--vault", "", "list"], "--vault needs a directory"),
        (&["--wait", "x", "new", "T"], "--wait wants whole seconds, not \"x\""),
        (&["--wait", "-1", "new", "T"], "--wait wants whole seconds, not \"-1\""),
        (
            &["--wait", "1.5", "new", "T"],
            "--wait wants whole seconds, not \"1.5\"",
        ),
        // What follows `--` is arguments, never the command.
        (&["--json", "--", "list", "--folder"], "no command given"),
        (&["list", "--version"], "list does not take --version"),
    ];
    for (args, message) in cases {
        let (said, hint) = w.run(*args).fails("usage");
        assert_eq!((said.as_str(), hint.as_str()), (*message, USAGE_HINT), "{args:?}");
    }
    assert!(!w.exists("Notes/T.md"), "a usage error changes nothing");
}

#[test]
fn a_usage_error_is_printed_in_the_mode_the_flags_ask_for() {
    let Some(w) = World::seeded() else { return };
    // The flags count wherever they stand, even after what was wrong.
    w.run(["--styled", "bogus"])
        .fails_in_text("unknown command \"bogus\"", USAGE_HINT);
    w.run(["list", "--nope", "--styled"])
        .fails_in_text("unknown option --nope", USAGE_HINT);
    w.run(["--styled", "list", "--nope", "--quiet"]).fails("usage");
    // After `--` nothing is a flag.
    w.run(["list", "--nope", "--", "--styled"]).fails("usage");
}

#[test]
fn options_go_before_or_after_the_command() {
    let Some(w) = World::seeded() else { return };
    let after = w.run(["list", "--folder", "Notes", "--ids-only"]);
    assert_eq!(after.out, "Notes/Twin.md\nNotes/Alpha.md\n");
    for args in [
        vec!["--ids-only", "--folder", "Notes", "list"],
        vec!["--ids-only", "--folder=Notes", "list"],
        vec!["list", "--ids-only", "--folder=Notes"],
        // A repeated option keeps its last value.
        vec!["list", "--folder", "Work", "--ids-only", "--folder", "Notes"],
    ] {
        let r = w.run(&args);
        assert_eq!((r.code, r.out.as_str()), (0, after.out.as_str()), "{args:?}");
    }
    // An empty value is a value.
    assert_eq!(w.run(["list", "--folder=", "--count"]).out, "7\n");
}

#[test]
fn a_double_dash_ends_the_options() {
    let Some(w) = World::seeded() else { return };
    let made = w.json(["new", "--", "--dashed"]).ok();
    assert_eq!(made["path"], "Notes/--dashed.md");
    assert_eq!(w.read("Notes/--dashed.md"), "# --dashed\n");
    assert_eq!(
        w.run(["read", "--ids-only", "--", "--dashed"]).out,
        "Notes/--dashed.md\n"
    );
}

#[test]
fn the_vault_option_names_another_vault() {
    let Some(w) = World::seeded() else { return };
    let none = w.scratch().join("none");
    let (message, hint) = w
        .run(["--vault".as_ref(), none.as_os_str(), "list".as_ref()])
        .fails("not_cloned");
    assert_eq!(message, format!("no notes cloned at {}", none.display()));
    assert_eq!(hint, "Run `icloud-notes clone` (signed in to icloud-session) first.");
    // A relative directory is taken from where the command runs.
    let status = w
        .json([
            format!("--vault={}/./vault/../vault", w.scratch().display()).as_str(),
            "status",
        ])
        .ok();
    assert_eq!(status["notes"], 7);
}
