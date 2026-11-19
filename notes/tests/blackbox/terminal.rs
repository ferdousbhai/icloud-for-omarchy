//! What changes on a terminal: text instead of the envelope, and the
//! questions destructive commands ask. The terminal is a pseudo-terminal
//! util-linux's `script` gives the binary, as stdin and stdout both.

use std::process::{Command, Stdio};

use crate::world::World;

/// Runs `icloud-notes <args>` on a terminal, typing `typed`. Gives the exit
/// code and what the terminal showed (stdout and stderr together, its line
/// ends as plain newlines).
fn on_a_terminal(w: &World, args: &[&str], typed: &str) -> (i32, String) {
    let mut script = Command::new("script");
    script.args(["--quiet", "--return", "--echo", "never", "--command"]);
    script.arg(w.shell_line(args)).arg("/dev/null");
    script.env_clear().env("PATH", "/usr/bin:/bin").env("SHELL", "/bin/sh");
    let mut child = script
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("script runs");
    use std::io::Write;
    child.stdin.take().unwrap().write_all(typed.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
    )
}

#[test]
fn a_terminal_gets_text_without_being_asked() {
    let Some(w) = World::seeded() else { return };
    assert_eq!(
        on_a_terminal(&w, &["folders"], ""),
        (0, "7\t(All Notes)\n2\tNotes\n4\tWork\n".into())
    );
    // A failure is the error: and hint: lines.
    assert_eq!(
        on_a_terminal(&w, &["list", "--folder", "Nope"], ""),
        (4, "error: no folder \"Nope\"\nhint: `icloud-notes folders` lists them; `icloud-notes new-folder` makes one.\n".into())
    );
    // The flags still pick a machine mode.
    let (code, out) = on_a_terminal(&w, &["folders", "--count"], "");
    assert_eq!((code, out.as_str()), (0, "2\n"));
    assert!(
        on_a_terminal(&w, &["--json", "folders"], "")
            .1
            .starts_with("{\n    \"ok\": true,")
    );
    // The engine's own commands are asked for text too.
    assert_eq!(
        on_a_terminal(&w, &["history", "alpha"], ""),
        (0, "e9  2026-09-01T00:00:00Z  changed: Notes/Alpha.md\n".into())
    );
}

#[test]
fn delete_asks_and_takes_only_a_yes() {
    let Some(w) = World::seeded() else { return };
    let question = "Move \"Notes/Alpha.md\" to the trash? The next push moves it to Recently Deleted in iCloud. [y/N] ";
    for answer in ["n\n", "\n", "maybe\n"] {
        assert_eq!(
            on_a_terminal(&w, &["delete", "alpha"], answer),
            (1, format!("{question}error: cancelled\n")),
            "{answer:?}"
        );
        assert!(w.exists("Notes/Alpha.md"));
    }
    assert_eq!(
        on_a_terminal(&w, &["delete", "alpha"], "y\n"),
        (0, format!("{question}Moved Notes/Alpha.md to the trash\n"))
    );
    assert!(!w.exists("Notes/Alpha.md"));
    let (code, out) = on_a_terminal(&w, &["delete", "Twin.md"], " YES \n");
    assert_eq!(
        (code, out.ends_with("Moved Twin.md to the trash\n")),
        (0, true),
        "{out}"
    );
    // --yes asks nothing; a machine mode cannot ask, terminal or not.
    assert_eq!(
        on_a_terminal(&w, &["delete", "Notes/Twin.md", "--yes"], ""),
        (0, "Moved Notes/Twin.md to the trash\n".into())
    );
    let (code, out) = on_a_terminal(&w, &["--json", "delete", "beta"], "y\n");
    assert_eq!(code, 2);
    assert!(out.contains("\"code\": \"usage\"") && w.exists("Work/Beta.md"), "{out}");
}

#[test]
fn the_other_destructive_commands_ask_in_their_own_words() {
    let Some(w) = World::seeded() else { return };
    let asked = |args: &[&str], question: &str| {
        let (code, out) = on_a_terminal(&w, args, "n\n");
        assert_eq!(
            (code, out),
            (1, format!("{question} [y/N] error: cancelled\n")),
            "{args:?}"
        );
    };
    asked(
        &["write", "alpha", "--body", ""],
        "Empty Notes/Alpha.md? The next push moves it to Recently Deleted in iCloud.",
    );
    asked(
        &["write", "pics", "--body", "# Pics\n"],
        "Remove the attachment pic.png from Work/Pics.md? The next push removes it on all your devices.",
    );
    asked(
        &["detach", "pics", "pic.png"],
        "Remove the attachment pic.png from Work/Pics.md? The next push removes it on all your devices.",
    );
    asked(
        &["delete-folder", "Work"],
        "Move \"Work\" and its 4 note(s) to the trash? The next push moves the notes to Recently Deleted in iCloud.",
    );
    asked(
        &["restore", "alpha"],
        "Throw away the local edits to \"Notes/Alpha.md\" and go back to the last synced copy?",
    );
    assert!(w.exists("Work/Pics.md") && w.read("Work/Pics.md").contains("pic.png"));
    // A yes goes ahead.
    let (code, out) = on_a_terminal(&w, &["detach", "pics", "pic.png"], "y\n");
    assert_eq!(
        (
            code,
            out.ends_with("Removed the attachment pic.png from Work/Pics.md\n")
        ),
        (0, true),
        "{out}"
    );
    let (code, _) = on_a_terminal(&w, &["restore", "alpha"], "yes\n");
    assert_eq!(code, 0);
    assert!(w.read("Notes/Alpha.md").ends_with("restored\n"));
}
