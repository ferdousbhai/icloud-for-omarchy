//! What's done to the vault's files is honoured as if done in Notes, even
//! when another device changed the note since the last pull
//! (docs/DESIGN.md §1): a deleted file
//! is trashed and never written back, a renamed file takes the remote edit
//! and still goes up as a move, and a pull never writes over a file it
//! doesn't track.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

const RECORD: &str = "03667d1d-eee8-4e98-82fb-8c5cd02fd9d1";
const FILE: &str = "Notes/Test Note.md";

fn differential() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The tiny clone with its one note's file deleted.
fn vault_with_note_deleted(out: &Path) -> PathBuf {
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    std::fs::remove_file(vault.join(FILE)).unwrap();
    vault
}

fn run(out: &Path, cassette: &Path, args: &[&str]) -> (i32, Value, String) {
    std::fs::create_dir_all(out.join("home")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(args)
        .env("HOME", out.join("home"))
        .env("XDG_RUNTIME_DIR", out.join("home"))
        .env("ICLOUD_NOTES_SYNC_CASSETTE", cassette)
        .env("ICLOUD_NOTES_SYNC_REQUEST_LOG", out.join("requests.json"))
        .env("ICLOUD_NOTES_SYNC_NOW", "1790000000000")
        .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout isn't JSON ({e}): {}\n{stderr}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output.status.code().unwrap_or(-1), stdout, stderr)
}

fn state(vault: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(vault.join(".icloud-notes/state.json")).unwrap()).unwrap()
}

fn modify_requests(out: &Path) -> usize {
    let log: Value = serde_json::from_str(&std::fs::read_to_string(out.join("requests.json")).unwrap()).unwrap();
    log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"].as_str().is_some_and(|p| p.ends_with("/records/modify")))
        .count()
}

/// tiny-push-delete's answers, with the live record at `tag` instead of the
/// tag the vault tracks (25q).
fn push_delete_cassette(out: &Path, tag: &str) -> PathBuf {
    let mut cassette: Value =
        serde_json::from_str(&std::fs::read_to_string(differential().join("cassettes/tiny-push-delete.json")).unwrap())
            .unwrap();
    let lookup = &mut cassette["interactions"][0];
    assert!(lookup["request"]["path"].as_str().unwrap().ends_with("/records/lookup"));
    lookup["response"]["body"]["records"][0]["recordChangeTag"] = Value::from(tag);
    let path = out.join("cassette.json");
    std::fs::write(&path, serde_json::to_vec(&cassette).unwrap()).unwrap();
    path
}

#[test]
fn push_trashes_a_deleted_note_that_changed_remotely() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = vault_with_note_deleted(&out);
    let cassette = push_delete_cassette(&out, "26a");

    let (code, stdout, stderr) = run(&out, &cassette, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let entries = stdout["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{stdout}");
    assert_eq!(entries[0]["kind"], "delete");
    assert_eq!(entries[0]["file"], FILE);
    assert_eq!(entries[0]["resolution"], "ready", "{stdout}");
    assert_eq!(modify_requests(&out), 1);
    let state = state(&vault);
    assert!(state["notes"].get(RECORD).is_none(), "no longer tracked: {state}");
    assert!(state["trashed"].get(RECORD).is_some(), "remembered as trashed: {state}");
    assert!(!vault.join(FILE).exists());
}

/// tiny-push-delete's answers, with the lookup answering `code` for the
/// note instead of the record.
fn push_delete_lookup_error_cassette(out: &Path, code: &str) -> PathBuf {
    let mut cassette: Value =
        serde_json::from_str(&std::fs::read_to_string(differential().join("cassettes/tiny-push-delete.json")).unwrap())
            .unwrap();
    let lookup = &mut cassette["interactions"][0];
    assert!(lookup["request"]["path"].as_str().unwrap().ends_with("/records/lookup"));
    lookup["response"]["body"]["records"][0] =
        serde_json::json!({ "recordName": RECORD, "serverErrorCode": code, "reason": "replayed" });
    let path = out.join("cassette.json");
    std::fs::write(&path, serde_json::to_vec(&cassette).unwrap()).unwrap();
    path
}

/// iCloud failing to look the note up is not iCloud saying it is gone: the
/// delete is refused and the note stays tracked, for the next push.
#[test]
fn push_refuses_a_delete_whose_lookup_answered_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = vault_with_note_deleted(&out);
    let cassette = push_delete_lookup_error_cassette(&out, "TRY_AGAIN_LATER");

    let (code, stdout, stderr) = run(&out, &cassette, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let entries = stdout["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{stdout}");
    assert_eq!(entries[0]["kind"], "delete");
    assert_eq!(entries[0]["resolution"], "refused", "{stdout}");
    assert!(
        entries[0]["reason"].as_str().unwrap().contains("TRY_AGAIN_LATER"),
        "{stdout}"
    );
    assert_eq!(modify_requests(&out), 0);
    let state = state(&vault);
    assert!(state["notes"].get(RECORD).is_some(), "still tracked: {state}");
}

/// "Not found" is the one answer that does say it is gone.
#[test]
fn push_untracks_a_deleted_note_the_lookup_did_not_find() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = vault_with_note_deleted(&out);
    let cassette = push_delete_lookup_error_cassette(&out, "NOT_FOUND");

    let (code, stdout, stderr) = run(&out, &cassette, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert_eq!(modify_requests(&out), 0);
    let state = state(&vault);
    assert!(state["notes"].get(RECORD).is_none(), "no longer tracked: {state}");
}

#[test]
fn pull_leaves_a_deleted_note_deleted_and_the_next_push_trashes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = vault_with_note_deleted(&out);

    // Another device appended a line (tag 26a).
    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-pull-update.json"),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(!vault.join(FILE).exists(), "the file stays deleted");
    assert_eq!(stdout["updated"], 0, "{stdout}");
    let notices = stdout["notices"].to_string();
    assert!(
        notices.contains("the next push moves it to Recently Deleted"),
        "{stdout}"
    );
    let pulled = state(&vault);
    assert_eq!(pulled["notes"][RECORD]["file"], FILE);
    assert_eq!(
        pulled["notes"][RECORD]["recordChangeTag"], "26a",
        "tracking follows the remote record"
    );

    // The deletion is still pending, and goes up on the next push.
    let cassette = push_delete_cassette(&out, "26a");
    let (code, stdout, stderr) = run(&out, &cassette, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["kind"], "delete", "{stdout}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert!(state(&vault)["notes"].get(RECORD).is_none());
}

#[test]
fn pull_merges_a_remote_edit_into_a_renamed_file_and_the_move_still_pairs() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    let renamed = "Notes/Renamed.md";
    let original = std::fs::read_to_string(vault.join(FILE)).unwrap();
    std::fs::write(vault.join(renamed), original).unwrap();
    std::fs::remove_file(vault.join(FILE)).unwrap();

    // Another device appended a line (tag 26a) before the rename was pushed.
    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-pull-update.json"),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(!vault.join(FILE).exists(), "the old name isn't written back");
    let text = std::fs::read_to_string(vault.join(renamed)).unwrap();
    assert!(
        text.contains("A line added on the phone."),
        "remote edit merged in: {text}"
    );
    assert!(!text.contains("<<<<<<<"), "{text}");
    let pulled = state(&vault);
    assert_eq!(
        pulled["notes"][RECORD]["file"], FILE,
        "still tracked at the old name, so push pairs the move"
    );
    assert_eq!(pulled["notes"][RECORD]["recordChangeTag"], "26a");

    // The rename now goes up as a move instead of being refused.
    let cassette = push_delete_cassette(&out, "26a");
    let (code, stdout, stderr) = run(
        &out,
        &cassette,
        &["--quiet", "push", "--dry-run", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let entries = stdout["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["kind"] == "move" && e["resolution"] == "ready"),
        "{stdout}"
    );
    assert!(!entries.iter().any(|e| e["kind"] == "delete"), "{stdout}");
}

#[test]
fn pull_does_not_take_a_syncthing_conflict_copy_for_a_moved_note() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    // The note's file is gone, and a conflict copy of it (same id) is there.
    let copy = "Notes/Test Note.sync-conflict-20261114-103000-ABCDEFG.md";
    let original = std::fs::read_to_string(vault.join(FILE)).unwrap();
    assert!(original.contains(RECORD), "the file carries the note's id");
    std::fs::write(vault.join(copy), &original).unwrap();
    std::fs::remove_file(vault.join(FILE)).unwrap();

    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-pull-update.json"),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(
        std::fs::read_to_string(vault.join(copy)).unwrap(),
        original,
        "the remote edit is not merged into the conflict copy"
    );
    assert_eq!(state(&vault)["notes"][RECORD]["file"], FILE);
}

#[test]
fn pull_never_overwrites_an_untracked_file_with_the_same_name() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    let mine = "# Fresh\nMy own note, not pushed yet.";
    std::fs::write(vault.join("Notes/Fresh.md"), mine).unwrap();

    // The phone made a note called "Fresh" too.
    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/bodyless-pull.json"),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["added"], 1, "{stdout}");
    assert_eq!(std::fs::read_to_string(vault.join("Notes/Fresh.md")).unwrap(), mine);
    let theirs = std::fs::read_to_string(vault.join("Notes/Fresh 2.md")).unwrap();
    assert!(theirs.contains("A note made locally."), "{theirs}");
    assert_eq!(
        state(&vault)["notes"]["5f1d0c3a-7b2e-4c9a-9e61-2b8d4a6c0f17"]["file"],
        "Notes/Fresh 2.md"
    );
}

#[test]
fn a_note_at_the_top_level_is_created_in_the_default_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    std::fs::write(vault.join("Fresh.md"), "# Fresh\nA note made locally.").unwrap();

    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-push-create.json"),
        &["--quiet", "push", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["kind"], "create", "{stdout}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    let log: Value = serde_json::from_str(&std::fs::read_to_string(out.join("requests.json")).unwrap()).unwrap();
    let modify = log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with("/records/modify")))
        .unwrap();
    assert!(
        modify["body"].to_string().contains("DefaultFolder-CloudKit"),
        "{}",
        modify["body"]
    );
    let state = state(&vault);
    let created = state["notes"]
        .as_object()
        .unwrap()
        .values()
        .find(|e| e["file"] == "Fresh.md")
        .expect("tracked");
    assert_eq!(created["folderRecordName"], "DefaultFolder-CloudKit");
}

#[test]
fn emptying_a_note_moves_it_to_recently_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    std::fs::write(vault.join(FILE), format!("---\napple-note-id: {RECORD}\n---\n")).unwrap();

    let cassette = push_delete_cassette(&out, "25q");
    let (code, stdout, stderr) = run(&out, &cassette, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["kind"], "delete", "{stdout}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert!(state(&vault)["notes"].get(RECORD).is_none());
    assert!(!vault.join(FILE).exists(), "the empty file goes with the note");
}

/// tiny-push's answers, with the lookup returning the note as another
/// device left it (tiny-pull-update's record: a line appended, tag 26a).
fn push_over_remote_edit_cassette(out: &Path) -> PathBuf {
    let read = |name: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(differential().join("cassettes").join(name)).unwrap()).unwrap()
    };
    let remote = read("tiny-pull-update.json")["interactions"][0]["response"]["body"]["zones"][0]["records"][0].clone();
    let mut cassette = read("tiny-push.json");
    cassette["interactions"][0]["response"]["body"]["records"][0] = remote;
    let path = out.join("cassette.json");
    std::fs::write(&path, serde_json::to_vec(&cassette).unwrap()).unwrap();
    path
}

#[test]
fn a_local_edit_over_a_remote_edit_goes_up_merged_in_one_push() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    let original = std::fs::read_to_string(vault.join(FILE)).unwrap();
    std::fs::write(
        vault.join(FILE),
        original.replace("# Test Note", "# Test Note, edited here"),
    )
    .unwrap();

    let cassette = push_over_remote_edit_cassette(&out);
    let (code, stdout, stderr) = run(
        &out,
        &cassette,
        &["--quiet", "push", "--dry-run", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert_eq!(
        std::fs::read_to_string(vault.join(FILE)).unwrap(),
        original.replace("# Test Note", "# Test Note, edited here"),
        "a preview writes nothing"
    );

    let (code, stdout, stderr) = run(&out, &cassette, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["kind"], "update", "{stdout}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert_eq!(modify_requests(&out), 1);
    let log: Value = serde_json::from_str(&std::fs::read_to_string(out.join("requests.json")).unwrap()).unwrap();
    let modify = log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with("/records/modify")))
        .unwrap();
    assert_eq!(
        modify["body"]["operations"][0]["record"]["recordChangeTag"], "26a",
        "written against the live record, not the tracked 25q"
    );
    let text = std::fs::read_to_string(vault.join(FILE)).unwrap();
    assert!(text.contains("# Test Note, edited here"), "{text}");
    assert!(text.contains("A line added on the phone."), "{text}");
    assert_eq!(state(&vault)["notes"][RECORD]["recordChangeTag"], "26b");
    let base = std::fs::read_to_string(vault.join(format!(".icloud-notes/base/{RECORD}.md"))).unwrap();
    assert!(base.contains("edited here") && base.contains("on the phone"), "{base}");
}

#[test]
fn a_rename_with_an_edit_goes_up_in_one_push() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    let original = std::fs::read_to_string(vault.join(FILE)).unwrap();
    let renamed = "Notes/Renamed.md";
    std::fs::write(vault.join(renamed), format!("{original}\n\nA line added here.")).unwrap();
    std::fs::remove_file(vault.join(FILE)).unwrap();

    // A rename within its folder changes nothing in iCloud: the one write is
    // the edit (tiny-push's answers, tag 26b).
    let path = differential().join("cassettes/tiny-push.json");
    let (code, stdout, stderr) = run(&out, &path, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"].as_array().unwrap().len(), 1, "{stdout}");
    assert_eq!(stdout["entries"][0]["kind"], "move", "{stdout}");
    assert!(
        stdout["entries"][0]["outcome"]["message"]
            .as_str()
            .unwrap()
            .ends_with("with its edits"),
        "{stdout}"
    );
    assert_eq!(modify_requests(&out), 1);
    let state = state(&vault);
    assert_eq!(state["notes"][RECORD]["file"], renamed);
    assert_eq!(state["notes"][RECORD]["recordChangeTag"], "26b");
    let base = std::fs::read_to_string(vault.join(format!(".icloud-notes/base/{RECORD}.md"))).unwrap();
    assert!(base.ends_with("A line added here."), "{base}");
}

#[test]
fn several_deletes_go_up_in_one_request() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    // A second tracked note, a copy of the first under another record name.
    let second = "Notes/Second.md";
    let other = "11111111-2222-3333-4444-555555555555";
    let mut state_json = state(&vault);
    let mut entry = state_json["notes"][RECORD].clone();
    entry["file"] = Value::from(second);
    state_json["notes"][other] = entry;
    std::fs::write(
        vault.join(".icloud-notes/state.json"),
        serde_json::to_vec_pretty(&state_json).unwrap(),
    )
    .unwrap();
    std::fs::copy(
        vault.join(format!(".icloud-notes/base/{RECORD}.md")),
        vault.join(format!(".icloud-notes/base/{other}.md")),
    )
    .unwrap();
    std::fs::remove_file(vault.join(FILE)).unwrap();

    // Lookup and modify answer for both records.
    let mut cassette: Value =
        serde_json::from_str(&std::fs::read_to_string(differential().join("cassettes/tiny-push-delete.json")).unwrap())
            .unwrap();
    for i in 0..2 {
        let records = cassette["interactions"][i]["response"]["body"]["records"]
            .as_array_mut()
            .unwrap();
        let mut copy = records[0].clone();
        copy["recordName"] = Value::from(other);
        records.push(copy);
    }
    let path = out.join("cassette.json");
    std::fs::write(&path, serde_json::to_vec(&cassette).unwrap()).unwrap();

    let (code, stdout, stderr) = run(&out, &path, &["--quiet", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let entries = stdout["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{stdout}");
    assert!(
        entries
            .iter()
            .all(|e| e["kind"] == "delete" && e["outcome"]["succeeded"] == true),
        "{stdout}"
    );
    assert_eq!(modify_requests(&out), 1, "one request for both");
    let log: Value = serde_json::from_str(&std::fs::read_to_string(out.join("requests.json")).unwrap()).unwrap();
    let modify = log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"].as_str().is_some_and(|p| p.ends_with("/records/modify")))
        .unwrap();
    assert_eq!(modify["body"]["atomic"], false);
    assert_eq!(modify["body"]["operations"].as_array().unwrap().len(), 2);
    let after = state(&vault);
    assert!(after["notes"].get(RECORD).is_none() && after["notes"].get(other).is_none());
}

/// The push report (text mode) names folder changes apart from notes.
#[test]
fn the_push_summary_counts_folder_changes_apart_from_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    std::fs::create_dir(vault.join("Ideas")).unwrap();
    std::fs::create_dir_all(out.join("home")).unwrap();
    // The folder create, answered for the name it asks for (the first
    // deterministic uuid).
    let cassette = out.join("folder-create.json");
    let answer = json!({ "records": [{
        "recordName": "00000000-0000-4000-8000-000000000001", "recordType": "Folder", "recordChangeTag": "1", "fields": {},
    }] });
    let interaction = json!({
        "request": { "method": "POST", "path": "/database/1/com.apple.notes/production/private/records/modify" },
        "response": { "status": 200, "body": answer },
    });
    let account = json!({ "dsid": "10000000001", "appleId": "harness@example.com" });
    std::fs::write(
        &cassette,
        json!({ "version": 1, "account": account, "interactions": [interaction] }).to_string(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_icloud-notes-sync"))
        .args(["--styled", "push", vault.to_str().unwrap()])
        .env("HOME", out.join("home"))
        .env("XDG_RUNTIME_DIR", out.join("home"))
        .env("ICLOUD_NOTES_SYNC_CASSETTE", &cassette)
        .env("ICLOUD_NOTES_SYNC_REQUEST_LOG", out.join("requests.json"))
        .env("ICLOUD_NOTES_SYNC_NOW", "1790000000000")
        .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Pushed 1 folder change(s) from"), "{stdout}");
}

// --- a note that is gone from iCloud ---------------------------------------------------

/// tiny-pull-update's answers, with the note moved to Recently Deleted on
/// another device instead of edited.
fn pull_deleted_cassette(out: &Path) -> PathBuf {
    let mut cassette: Value =
        serde_json::from_str(&std::fs::read_to_string(differential().join("cassettes/tiny-pull-update.json")).unwrap())
            .unwrap();
    let note = &mut cassette["interactions"][0]["response"]["body"]["zones"][0]["records"][0];
    assert_eq!(note["recordName"], RECORD);
    note["fields"]["Folder"] = serde_json::json!({ "value": { "recordName": "TrashFolder-CloudKit", "action": "VALIDATE" }, "type": "REFERENCE" });
    let path = out.join("deleted.json");
    std::fs::write(&path, serde_json::to_vec(&cassette).unwrap()).unwrap();
    path
}

/// A note renamed here (not pushed yet) and deleted on another device is
/// deleted: its file, untouched since the last sync, goes with it. Left in
/// place it would carry an id nothing tracks, and the next push would send
/// the deleted note back to iCloud as a new one.
#[test]
fn a_note_deleted_remotely_and_only_renamed_here_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    let renamed = "Notes/Renamed.md";
    std::fs::rename(vault.join(FILE), vault.join(renamed)).unwrap();

    let (code, stdout, stderr) = run(
        &out,
        &pull_deleted_cassette(&out),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["removed"], 1, "{stdout}");
    assert!(!vault.join(renamed).exists(), "the renamed file went with the note");
    assert!(state(&vault)["notes"].get(RECORD).is_none());

    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-lookup.json"),
        &["--quiet", "push", "--dry-run", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"], serde_json::json!([]), "nothing to create");
}

/// Renamed and edited here, deleted elsewhere: the edited text is kept as a
/// new note, as an edited file under its old name is.
#[test]
fn a_note_deleted_remotely_and_renamed_and_edited_here_is_kept_as_a_new_note() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/tiny-clone/vault"), &vault);
    let renamed = "Notes/Renamed.md";
    let edited = std::fs::read_to_string(vault.join(FILE)).unwrap() + "\nA line added here.\n";
    std::fs::write(vault.join(renamed), edited).unwrap();
    std::fs::remove_file(vault.join(FILE)).unwrap();

    let (code, stdout, stderr) = run(
        &out,
        &pull_deleted_cassette(&out),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["changes"][0]["kind"], "untrack", "{stdout}");
    assert_eq!(stdout["changes"][0]["file"], renamed);
    let text = std::fs::read_to_string(vault.join(renamed)).unwrap();
    assert!(text.contains("A line added here."), "{text}");
    assert!(!text.contains(RECORD), "it lost the deleted note's id: {text}");
}

/// The `asset-clone` vault (it has a note someone shares) pulled with the
/// share revoked, after `prepare` changed the vault.
fn pull_revoked(out: &Path, prepare: impl FnOnce(&Path)) -> (PathBuf, Value) {
    let vault = out.join("vault");
    copy_dir(&differential().join("expected/asset-clone/vault"), &vault);
    prepare(&vault);
    let (code, stdout, stderr) = run(
        out,
        &differential().join("cassettes/shared-pull-revoked.json"),
        &["--quiet", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    (vault, stdout)
}

const SHARED_FILE: &str = "_0123456789abcdef0123456789abcdef/Shared Big Note.md";

/// A note someone stopped sharing is gone for this account. Its file,
/// unchanged here, is removed like a deleted note's; left in place, the next
/// push would upload it (and a folder named after the sharer) into this
/// account's own notes.
#[test]
fn a_note_no_longer_shared_is_removed_and_nothing_is_uploaded() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let (vault, stdout) = pull_revoked(&out, |_| {});
    assert_eq!(stdout["unsharedUntracked"], 1, "{stdout}");
    assert!(!vault.join(SHARED_FILE).exists());

    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-lookup.json"),
        &["--quiet", "push", "--dry-run", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"], serde_json::json!([]), "{stdout}");
}

/// With edits made here, the file is kept, with its id, as a file that is
/// not synced: push says so and creates neither a note nor a folder for it.
/// Without the id line it is a note of the user's own.
#[test]
fn an_edited_copy_of_a_note_no_longer_shared_is_kept_and_not_uploaded() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let (vault, stdout) = pull_revoked(&out, |vault| {
        let path = vault.join(SHARED_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, text + "\nMy own remarks.\n").unwrap();
    });
    assert_eq!(stdout["unsharedUntracked"], 1, "{stdout}");
    assert_eq!(stdout["changes"][0]["kind"], "untrack", "{stdout}");
    let text = std::fs::read_to_string(vault.join(SHARED_FILE)).unwrap();
    assert!(text.contains("My own remarks."), "{text}");
    let pulled = state(&vault);
    let unshared = pulled["unshared"].as_array().expect("the kept copy is listed");
    assert_eq!(unshared.len(), 1, "{pulled}");
    assert!(text.contains(unshared[0].as_str().unwrap()), "it keeps its id: {text}");

    let preview = |out: &Path| {
        let (code, stdout, stderr) = run(
            out,
            &differential().join("cassettes/tiny-lookup.json"),
            &["--quiet", "push", "--dry-run", vault.to_str().unwrap()],
        );
        assert_eq!(code, 0, "{stdout}\n{stderr}");
        stdout
    };
    let stdout = preview(&out);
    assert_eq!(stdout["entries"], serde_json::json!([]), "{stdout}");
    let notices = stdout["notices"].to_string();
    assert!(
        notices.contains("no longer shared with you, so it is not synced"),
        "{notices}"
    );

    // The user makes it their own: now it is a new note (in a new folder).
    let own: String = text
        .lines()
        .filter(|l| !l.starts_with("apple-note-id:"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(vault.join(SHARED_FILE), own).unwrap();
    let stdout = preview(&out);
    let kinds: Vec<&str> = stdout["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["createFolder", "create"], "{stdout}");
}
