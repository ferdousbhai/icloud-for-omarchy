//! What's done to the vault's files is honoured as if done in Notes, even
//! when another device changed the note since the last pull (deliberate
//! differences from icloud-md 0.6.2, docs/PORT_PLAN.md §1): a deleted file
//! is trashed and never written back, a renamed file takes the remote edit
//! and still goes up as a move, and a pull never writes over a file it
//! doesn't track.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

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
        .env("ICLOUD_NOTES_SYNC_ASSET_BODIES", "0")
        .env("HOME", out.join("home"))
        .env("XDG_RUNTIME_DIR", out.join("home"))
        .env("ICLOUD_NOTES_SYNC_CASSETTE", cassette)
        .env("ICLOUD_NOTES_SYNC_REQUEST_LOG", out.join("requests.json"))
        .env("ICLOUD_NOTES_SYNC_NOW", "1790000000000")
        .env("ICLOUD_NOTES_SYNC_DETERMINISTIC", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("stdout isn't JSON ({e}): {}\n{stderr}", String::from_utf8_lossy(&output.stdout)));
    (output.status.code().unwrap_or(-1), stdout, stderr)
}

fn state(vault: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(vault.join(".icloud-md/state.json")).unwrap()).unwrap()
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

    let (code, stdout, stderr) = run(&out, &cassette, &["--json", "push", vault.to_str().unwrap()]);
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

#[test]
fn pull_leaves_a_deleted_note_deleted_and_the_next_push_trashes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().canonicalize().unwrap();
    let vault = vault_with_note_deleted(&out);

    // Another device appended a line (tag 26a).
    let (code, stdout, stderr) = run(
        &out,
        &differential().join("cassettes/tiny-pull-update.json"),
        &["--json", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(!vault.join(FILE).exists(), "the file stays deleted");
    assert_eq!(stdout["updated"], 0, "{stdout}");
    let notices = stdout["notices"].to_string();
    assert!(notices.contains("the next push moves it to Recently Deleted"), "{stdout}");
    let pulled = state(&vault);
    assert_eq!(pulled["notes"][RECORD]["file"], FILE);
    assert_eq!(pulled["notes"][RECORD]["recordChangeTag"], "26a", "tracking follows the remote record");

    // The deletion is still pending, and goes up on the next push.
    let cassette = push_delete_cassette(&out, "26a");
    let (code, stdout, stderr) = run(&out, &cassette, &["--json", "push", vault.to_str().unwrap()]);
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
        &["--json", "pull", vault.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(!vault.join(FILE).exists(), "the old name isn't written back");
    let text = std::fs::read_to_string(vault.join(renamed)).unwrap();
    assert!(text.contains("A line added on the phone."), "remote edit merged in: {text}");
    assert!(!text.contains("<<<<<<<"), "{text}");
    let pulled = state(&vault);
    assert_eq!(pulled["notes"][RECORD]["file"], FILE, "still tracked at the old name, so push pairs the move");
    assert_eq!(pulled["notes"][RECORD]["recordChangeTag"], "26a");

    // The rename now goes up as a move instead of being refused.
    let cassette = push_delete_cassette(&out, "26a");
    let (code, stdout, stderr) = run(&out, &cassette, &["--json", "push", "--dry-run", vault.to_str().unwrap()]);
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    let entries = stdout["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["kind"] == "move" && e["resolution"] == "ready"),
        "{stdout}"
    );
    assert!(!entries.iter().any(|e| e["kind"] == "delete"), "{stdout}");
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
        &["--json", "pull", vault.to_str().unwrap()],
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
        &["--json", "push", vault.to_str().unwrap()],
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
    std::fs::write(
        vault.join(FILE),
        format!("---\napple-note-id: {RECORD}\n---\n"),
    )
    .unwrap();

    let cassette = push_delete_cassette(&out, "25q");
    let (code, stdout, stderr) = run(&out, &cassette, &["--json", "push", vault.to_str().unwrap()]);
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
    std::fs::write(vault.join(FILE), original.replace("# Test Note", "# Test Note, edited here")).unwrap();

    let cassette = push_over_remote_edit_cassette(&out);
    let (code, stdout, stderr) = run(&out, &cassette, &["--json", "push", "--dry-run", vault.to_str().unwrap()]);
    assert_eq!(code, 3, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert_eq!(
        std::fs::read_to_string(vault.join(FILE)).unwrap(),
        original.replace("# Test Note", "# Test Note, edited here"),
        "a preview writes nothing"
    );

    let (code, stdout, stderr) = run(&out, &cassette, &["--json", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"][0]["kind"], "update", "{stdout}");
    assert_eq!(stdout["entries"][0]["resolution"], "ready", "{stdout}");
    assert_eq!(modify_requests(&out), 1);
    let text = std::fs::read_to_string(vault.join(FILE)).unwrap();
    assert!(text.contains("# Test Note, edited here"), "{text}");
    assert!(text.contains("A line added on the phone."), "{text}");
    assert_eq!(state(&vault)["notes"][RECORD]["recordChangeTag"], "26b");
    let base = std::fs::read_to_string(vault.join(format!(".icloud-md/base/{RECORD}.md"))).unwrap();
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

    // tiny-push's lookup, then two writes: the move (tag 26a), the edit (26b).
    let mut cassette: Value =
        serde_json::from_str(&std::fs::read_to_string(differential().join("cassettes/tiny-push.json")).unwrap())
            .unwrap();
    let mut first = cassette["interactions"][1].clone();
    first["response"]["body"]["records"][0]["recordChangeTag"] = Value::from("26a");
    cassette["interactions"].as_array_mut().unwrap().insert(1, first);
    let path = out.join("cassette.json");
    std::fs::write(&path, serde_json::to_vec(&cassette).unwrap()).unwrap();

    let (code, stdout, stderr) = run(&out, &path, &["--json", "push", vault.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(stdout["entries"].as_array().unwrap().len(), 1, "{stdout}");
    assert_eq!(stdout["entries"][0]["kind"], "move", "{stdout}");
    assert!(
        stdout["entries"][0]["outcome"]["message"].as_str().unwrap().ends_with("with its edits"),
        "{stdout}"
    );
    assert_eq!(modify_requests(&out), 2);
    let state = state(&vault);
    assert_eq!(state["notes"][RECORD]["file"], renamed);
    assert_eq!(state["notes"][RECORD]["recordChangeTag"], "26b");
    let base = std::fs::read_to_string(vault.join(format!(".icloud-md/base/{RECORD}.md"))).unwrap();
    assert!(base.ends_with("A line added here."), "{base}");
}
