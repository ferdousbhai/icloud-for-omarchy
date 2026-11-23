//! A vault that left iCloud (the engine's `leave` handed it to Omacloud):
//! its notes are plain files that still read and change, and nothing that
//! would reach iCloud runs.

use std::fs;

use serde_json::json;

use crate::world::{World, column, strings};

/// The seeded vault after `leave`: the engine's state moved aside, and the
/// record that names the vault.
fn left() -> Option<World> {
    let w = World::seeded()?;
    let data = w.scratch().join("data/icloud-notes");
    fs::create_dir_all(&data).unwrap();
    fs::rename(w.path(".icloud-notes"), data.join("left-icloud-state")).unwrap();
    fs::write(
        data.join("left-icloud.json"),
        json!({"version": 1, "at": 1_790_000_000, "folder": w.vault}).to_string(),
    )
    .unwrap();
    Some(w)
}

#[test]
fn its_notes_still_read_as_plain_files() {
    let Some(w) = left() else { return };
    assert_eq!(column(&w.json(["list"]).ok(), "path"), World::SEEDED);
    assert_eq!(w.json(["read", "alpha"]).ok()["id"], "id-a");
    assert_eq!(column(&w.json(["search", "eggs"]).ok(), "path"), ["Notes/Alpha.md"]);
    // Nothing tracks them any more.
    let status = w.run(["status"]);
    let data = status.ok();
    assert_eq!(
        (&data["left_icloud"], &data["cloned"], &data["notes"], &data["folders"]),
        (&json!(true), &json!(false), &json!(7), &json!(2))
    );
    assert_eq!(strings(&data["flagged"]["foreign-id"]).len(), 5);
    assert_eq!(status.summary(), "7 notes, left iCloud");
    let text = w.text(["status"]).out;
    assert!(
        text.starts_with(&format!(
            "Vault:       {} (left iCloud: Omacloud syncs it)\n",
            w.vault.display()
        )),
        "{text}"
    );
}

#[test]
fn status_needs_no_sign_in() {
    let Some(w) = left() else { return };
    w.session.set_signed_in(false);
    let status = w.run(["status"]);
    assert_eq!(status.ok()["signed_in"], false);
}

#[test]
fn its_notes_still_change() {
    let Some(w) = left() else { return };
    // No engine says which folder is the default: a new note goes to the root.
    assert_eq!(w.json(["new", "Gamma", "--body", "hello"]).ok()["path"], "Gamma.md");
    assert_eq!(w.read("Gamma.md"), "# Gamma\nhello\n");
    w.json(["write", "alpha", "--append", "--body", "more"]).ok();
    w.json(["toggle", "alpha", "2"]).ok();
    w.json(["move", "gamma", "Work"]).ok();
    w.json(["delete", "Work/Gamma.md", "--yes"]).ok();
    w.json(["new-folder", "Plain"]).ok();
    // The notes iCloud held read-only are plain files too.
    w.json(["write", "grid", "--body", "# Grid\nmine now\n"]).ok();
    assert_eq!(w.stub_log(), [""; 0]);
}

#[test]
fn nothing_reaches_icloud() {
    let Some(w) = left() else { return };
    let vault = w.vault.display();
    let message = format!("Notes left iCloud on this computer; Omacloud syncs {vault} now");
    let hint = format!("change the files in {vault}");
    for args in [
        vec!["sync"],
        vec!["pull"],
        vec!["push"],
        vec!["clone"],
        vec!["push", "--dry-run"],
        vec!["history", "alpha"],
        vec!["diff", "alpha", "e9"],
        vec!["restore", "alpha", "--yes"],
    ] {
        let said = w.run(&args).fails("left_icloud");
        assert_eq!(
            (said.0.as_str(), said.1.as_str()),
            (message.as_str(), hint.as_str()),
            "{args:?}"
        );
    }
    w.text(["sync"]).fails_in_text(&message, &hint);
    // A change's --push: the change is made, and its sync says why it was not.
    let pushed = w.run(["write", "alpha", "--append", "--body", "again", "--push"]);
    assert_eq!((pushed.code, pushed.err.as_str()), (1, ""));
    let data = pushed.data();
    assert_eq!(data["changed"], true);
    assert_eq!(
        data["sync"],
        json!({"ok": false, "error": {"code": "left_icloud", "message": format!("write done here, but not synced: {message}"), "hint": hint}})
    );
    assert_eq!(pushed.summary(), "Wrote Notes/Alpha.md; not synced");
    assert!(w.read("Notes/Alpha.md").ends_with("again\n"));
    assert_eq!(w.stub_log(), [""; 0]);
    // The timer's sync has nothing to do: the vault is not cloned.
    let background = w.run(["--sync"]);
    assert_eq!(
        (background.code, background.out.as_str(), background.err.as_str()),
        (0, "", "")
    );
}

#[test]
fn the_record_names_one_vault() {
    let Some(w) = left() else { return };
    // Another vault is not the one that left.
    let other = w.scratch().join("other");
    w.run(["--vault".as_ref(), other.as_os_str(), "list".as_ref()])
        .fails("not_cloned");
    // The same vault by another spelling of its path is.
    let record = w.scratch().join("data/icloud-notes/left-icloud.json");
    fs::write(
        &record,
        json!({"folder": format!("{}/../vault/", w.vault.display())}).to_string(),
    )
    .unwrap();
    assert_eq!(w.json(["status"]).ok()["left_icloud"], true);
    // A record that cannot be read, or names nothing, is no record.
    for broken in ["not json", "{}", r#"{"folder":""}"#] {
        fs::write(&record, broken).unwrap();
        w.run(["list"]).fails("not_cloned");
    }
}
