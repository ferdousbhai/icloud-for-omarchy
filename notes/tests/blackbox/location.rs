//! Where the vault is when nothing names it: `<Documents>/Notes`, to which
//! the folder of earlier releases, `<Documents>/icloud-notes`, moves once.
//! The documents folder here is the scratch home's.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::world::{Run, World};

const STATE: &str = r#"{"layoutVersion":4,"titleMode":"in-body","notes":{}}"#;

struct Documents {
    w: World,
    old: PathBuf,
    new: PathBuf,
}

impl Documents {
    fn new() -> Option<Documents> {
        let w = World::new()?;
        let documents = w.scratch().join("home/Documents");
        fs::create_dir(&documents).unwrap();
        Some(Documents {
            old: documents.join("icloud-notes"),
            new: documents.join("Notes"),
            w,
        })
    }

    /// A vault at `dir` holding one note.
    fn vault(&self, dir: &Path, note: &str) {
        fs::create_dir_all(dir.join(".icloud-notes")).unwrap();
        fs::write(dir.join(".icloud-notes/state.json"), STATE).unwrap();
        fs::write(dir.join(note), "# A note\n").unwrap();
    }

    fn run(&self, args: &[&str]) -> Run {
        self.w.cmd(args).default_vault().run()
    }

    /// The vault the binary uses, and the notes it lists there.
    fn uses(&self) -> (PathBuf, String) {
        let status = self.run(&["--json", "status"]);
        assert_eq!(status.code, 0, "{status:#?}");
        let list = self.run(&["--ids-only", "list"]);
        (PathBuf::from(status.data()["vault"].as_str().unwrap()), list.out)
    }
}

#[test]
fn the_vault_is_notes_in_the_documents_folder() {
    let Some(d) = Documents::new() else { return };
    let status = d.run(&["--json", "status"]);
    assert_eq!(
        (status.code, &status.data()["vault"], &status.data()["cloned"]),
        (0, &json!(d.new), &json!(false))
    );
    assert!(!d.new.exists(), "looking makes no folder");
    // The documents folder is the one the desktop names.
    let papers = d.w.scratch().join("home/Papers");
    fs::write(
        d.w.scratch().join("config/user-dirs.dirs"),
        "XDG_DOCUMENTS_DIR=\"$HOME/Papers\"\n",
    )
    .unwrap();
    assert_eq!(d.uses().0, papers.join("Notes"));
    // ICLOUD_NOTES_VAULT, when set, is the vault whatever is in Documents.
    d.vault(&d.old, "Old.md");
    assert_eq!(d.w.json(["status"]).ok()["vault"], json!(d.w.vault));
    assert!(d.old.is_dir());
}

#[test]
fn the_documents_folder_is_read_from_user_dirs_as_the_window_reads_it() {
    // The window and the command line must open the same vault, so a file
    // the desktop's own tools would read differently is still read one way.
    let Some(d) = Documents::new() else { return };
    let home = d.w.scratch().join("home");
    let sibling = |name: &str| PathBuf::from(format!("{}{name}", home.display()));
    for (file, documents) in [
        ("XDG_DOCUMENTS_DIR=\"$HOME/Papers\"\n", home.join("Papers")),
        ("XDG_DOCUMENTS_DIR=\"$HOME/Papers/\"\n", home.join("Papers")),
        ("XDG_DOCUMENTS_DIR=$HOME/Bare\r\n", home.join("Bare")),
        ("XDG_DOCUMENTS_DIR=\"$HOME\"\n", home.clone()),
        ("XDG_DOCUMENTS_DIR=\"/somewhere/else\"\n", "/somewhere/else".into()),
        // The last line that names it.
        (
            "XDG_DOCUMENTS_DIR=\"$HOME/First\"\nXDG_DESKTOP_DIR=\"$HOME/Desk\"\nXDG_DOCUMENTS_DIR=\"$HOME/Second\"\n",
            home.join("Second"),
        ),
        // Only a line that starts with the name.
        ("  XDG_DOCUMENTS_DIR=\"$HOME/Indented\"\n", home.join("Documents")),
        ("#XDG_DOCUMENTS_DIR=\"$HOME/Off\"\n", home.join("Documents")),
        ("XDG_DESKTOP_DIR=\"$HOME/Desk\"\n", home.join("Documents")),
        // $HOME is replaced as text, whatever follows it.
        ("XDG_DOCUMENTS_DIR=\"$HOMEX/Odd\"\n", sibling("X/Odd")),
    ] {
        fs::write(d.w.scratch().join("config/user-dirs.dirs"), file).unwrap();
        let status = d.run(&["--json", "status"]);
        assert_eq!(
            (status.code, &status.data()["vault"]),
            (0, &json!(documents.join("Notes"))),
            "{file:?}"
        );
    }
}

#[test]
fn the_folder_of_earlier_releases_moves_to_notes_once() {
    let Some(d) = Documents::new() else { return };
    d.vault(&d.old, "Old.md");
    assert_eq!(d.uses(), (d.new.clone(), "Old.md\n".into()));
    assert!(!d.old.exists());
    assert_eq!(fs::read_to_string(d.new.join("Old.md")).unwrap(), "# A note\n");
    assert_eq!(d.uses(), (d.new.clone(), "Old.md\n".into()));
    // A change goes to the new place.
    assert_eq!(d.run(&["new", "Fresh"]).ok()["path"], "Fresh.md");
    assert!(d.new.join("Fresh.md").is_file() && !d.old.exists());
}

#[test]
fn an_empty_notes_folder_gives_way_to_the_old_vault() {
    let Some(d) = Documents::new() else { return };
    d.vault(&d.old, "Old.md");
    fs::create_dir(&d.new).unwrap();
    assert_eq!(d.uses(), (d.new.clone(), "Old.md\n".into()));
    assert!(!d.old.exists());
}

#[test]
fn a_notes_folder_of_the_owners_own_stops_the_move() {
    let Some(d) = Documents::new() else { return };
    d.vault(&d.old, "Old.md");
    fs::create_dir(&d.new).unwrap();
    fs::write(d.new.join("shopping.txt"), "mine").unwrap();
    // The old vault stays the vault, and nothing in either folder is touched.
    assert_eq!(d.uses(), (d.old.clone(), "Old.md\n".into()));
    assert_eq!(fs::read_dir(&d.new).unwrap().count(), 1);
    assert!(d.old.join("Old.md").is_file());
}

#[test]
fn with_a_vault_in_both_places_notes_wins_and_the_old_one_is_left() {
    let Some(d) = Documents::new() else { return };
    d.vault(&d.old, "Old.md");
    d.vault(&d.new, "New.md");
    assert_eq!(d.uses(), (d.new.clone(), "New.md\n".into()));
    assert!(d.old.join("Old.md").is_file() && !d.new.join("Old.md").exists());
}

#[test]
fn an_old_vault_in_use_is_not_pulled_away() {
    let Some(d) = Documents::new() else { return };
    d.vault(&d.old, "Old.md");
    // A window of the earlier release still has it open.
    let held = d.w.hold_lock_of(&d.old, "Notes (pid 4242)");
    let status = d.run(&["--json", "status"]);
    assert_eq!((status.code, &status.data()["vault"]), (0, &json!(d.old)));
    // It waited a little for the old vault to be let go, then kept to it.
    assert!(status.elapsed >= Duration::from_millis(1900), "{status:#?}");
    assert!(d.old.is_dir() && !d.new.exists());
    assert_eq!(d.run(&["--ids-only", "list"]).out, "Old.md\n");
    // The next start, with the vault free, moves it.
    drop(held);
    assert_eq!(d.uses(), (d.new.clone(), "Old.md\n".into()));
    assert!(!d.old.exists());
}
