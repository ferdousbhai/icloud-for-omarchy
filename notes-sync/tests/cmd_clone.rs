//! `clone`. Originally derived from icloud-md's tests.

use std::path::{Path, PathBuf};

use icloud_notes_sync::cloudkit::Database;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cmd::clone::{CloneOptions, CloneSummary, run_clone_with};
use icloud_notes_sync::cmd::pull::run_pull_with;
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress, NoticeLevel};
use icloud_notes_sync::vault::state::{Account, CloneState, read_clone_state, write_clone_state};
use indexmap::IndexMap;
use serde_json::{Value, json};

#[test]
fn refuses_an_already_cloned_folder_without_authenticating() {
    let dir = tempfile::tempdir().unwrap();
    let state = CloneState {
        account: Some(Account {
            apple_id: "me@example.com".into(),
            dsid: "1234".into(),
        }),
        sync_token: Some("token".into()),
        ..Default::default()
    };
    write_clone_state(dir.path(), &state).unwrap();
    let connector = FnConnector(|| -> Result<Remote, Error> { panic!("must not authenticate") });
    let err = run_clone_with(&connector, dir.path(), &mut NoProgress, &CloneOptions::default()).unwrap_err();
    assert!(err.to_string().contains("is already a cloned notes directory"));
}

// docs/DESIGN.md §1, "New notes listed without their text": clone looks such notes up and, if they
// still have no text, saves no private sync token so the first pull sees them.

const FRESH: &str = "5f1d0c3a-7b2e-4c9a-9e61-2b8d4a6c0f17";
const PRIVATE_ZONE: &str = "/database/1/com.apple.notes/production/private/changes/zone";

fn cassette(name: &str) -> Cassette {
    Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/differential/cassettes")
            .join(name),
    )
    .unwrap()
}

/// A connector over `cassette`, logging requests to `log`.
fn replay(cassette: Cassette, log: PathBuf) -> impl Connector {
    FnConnector(move || -> Result<Remote, Error> {
        let account = Account {
            apple_id: cassette.account.apple_id.clone(),
            dsid: cassette.account.dsid.clone(),
        };
        let transport = ReplayTransport::from_cassette(cassette.clone(), Some(log.clone()))?;
        Ok(Remote {
            db: Database::new(Box::new(transport)),
            account,
        })
    })
}

fn clone_from(cassette: Cassette, vault: &Path, log: PathBuf) -> CloneSummary {
    run_clone_with(&replay(cassette, log), vault, &mut NoProgress, &CloneOptions::default()).unwrap()
}

fn logged(log: &Path) -> Vec<Value> {
    let log: Value = serde_json::from_str(&std::fs::read_to_string(log).unwrap()).unwrap();
    log["requests"].as_array().unwrap().clone()
}

fn lookups(requests: &[Value], scope: &str) -> Vec<Value> {
    let path = format!("/database/1/com.apple.notes/production/{scope}/records/lookup");
    requests
        .iter()
        .filter(|r| r["path"] == path.as_str())
        .cloned()
        .collect()
}

#[test]
fn clone_looks_up_a_note_listed_without_its_text() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let log = dir.path().join("requests.json");
    let summary = clone_from(cassette("bodyless-clone.json"), &vault, log.clone());
    assert_eq!((summary.written, summary.skipped_undecodable), (2, 0));
    assert!(summary.notices.is_empty(), "{:?}", summary.notices);
    let lookups = lookups(&logged(&log), "private");
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0]["body"]["records"], json!([{ "recordName": FRESH }]));
    assert!(vault.join("Notes/Fresh.md").exists());
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.sync_token.as_deref(), Some("AQAAAAAAAAAB"));
    assert_eq!(state.notes[FRESH].file, "Notes/Fresh.md");
}

#[test]
fn clone_saves_no_private_sync_token_past_a_note_still_without_text() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let summary = clone_from(
        cassette("bodyless-clone-unfilled.json"),
        &vault,
        dir.path().join("clone.json"),
    );
    assert_eq!((summary.written, summary.skipped_undecodable), (1, 1));
    assert_eq!(summary.notices.len(), 1);
    assert_eq!(summary.notices[0].level, NoticeLevel::Warn);
    assert!(
        summary.notices[0].message.contains("1 note(s)") && summary.notices[0].message.contains("first pull"),
        "{}",
        summary.notices[0].message
    );
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.sync_token, None);
    assert!(!state.notes.contains_key(FRESH));
    assert!(!vault.join("Notes/Fresh.md").exists());

    // The first pull walks the private zone from scratch, finds the note,
    // and (its lookup filling it this time) adds it and saves the token.
    let log = dir.path().join("pull.json");
    let pulled = run_pull_with(
        &replay(cassette("bodyless-clone.json"), log.clone()),
        &vault,
        &mut NoProgress,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(pulled.added, 1);
    assert_eq!(pulled.skipped_new_unsyncable, 0);
    let requests = logged(&log);
    let walk = requests.iter().find(|r| r["path"] == PRIVATE_ZONE).unwrap();
    let zone = walk["body"]["zones"][0].as_object().expect("a zone entry");
    assert!(!zone.contains_key("syncToken"), "{walk}");
    assert_eq!(lookups(&requests, "private").len(), 1);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.sync_token.as_deref(), Some("AQAAAAAAAAAB"));
    assert_eq!(state.notes[FRESH].file, "Notes/Fresh.md");
    assert!(vault.join("Notes/Fresh.md").exists());
}

/// Shared zones already behaved: the fetch looks such notes up, and a zone
/// still missing text is skipped with a warning and no token, so the first
/// pull fetches it from scratch. The private token is saved as usual.
#[test]
fn clone_skips_a_shared_zone_still_without_text_and_saves_no_token_for_it() {
    let mut c = cassette("bodyless-clone-unfilled.json");
    let private = &mut c.interactions[0].response.body.as_mut().unwrap()["zones"][0]["records"];
    let fresh = private.as_array_mut().unwrap().pop().unwrap();
    assert_eq!(fresh["recordName"], FRESH);
    let shared_zone = json!({ "zoneName": "Notes", "ownerRecordName": "_ownerA" });
    c.interactions[1].response.body = Some(json!({
        "zones": [{ "zoneID": shared_zone }], "moreComing": false, "syncToken": "db-1",
    }));
    let mut lookup = c.interactions.pop().unwrap();
    lookup.request.path = Some("/database/1/com.apple.notes/production/shared/records/lookup".into());
    let mut walk = c.interactions[0].clone();
    walk.request.path = Some("/database/1/com.apple.notes/production/shared/changes/zone".into());
    walk.response.body = Some(json!({
        "zones": [{ "zoneID": shared_zone, "moreComing": false, "syncToken": "shared-1", "records": [fresh] }],
    }));
    c.interactions.push(walk);
    c.interactions.push(lookup);

    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let log = dir.path().join("requests.json");
    let summary = clone_from(c, &vault, log.clone());
    assert_eq!((summary.written, summary.written_shared), (1, 0));
    assert_eq!(summary.notices.len(), 1, "{:?}", summary.notices);
    assert!(
        summary.notices[0].message.contains("_ownerA"),
        "{}",
        summary.notices[0].message
    );
    let requests = logged(&log);
    assert!(lookups(&requests, "private").is_empty());
    assert_eq!(lookups(&requests, "shared").len(), 1);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.sync_token.as_deref(), Some("AQAAAAAAAAAB"));
    assert_eq!(state.shared_zone_sync_tokens, Some(IndexMap::new()));
}

/// `name` (tiny-clone or one of its pulls) with its one note's text replaced
/// by `text`.
fn tiny_with_text(name: &str, text: &str) -> Cassette {
    use icloud_notes_sync::doc::document::{build_initial_note_document, encode_note_document};
    use icloud_notes_sync::doc::text::compress_note_document;
    let doc = build_initial_note_document(text, &[7; 16]).unwrap();
    let data = icloud_notes_sync::js::base64_encode(&compress_note_document(&encode_note_document(&doc).unwrap()));
    let mut c = cassette(name);
    let records = &mut c.interactions[0].response.body.as_mut().unwrap()["zones"][0]["records"];
    let note = records
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["recordType"] == "Note")
        .unwrap();
    note["fields"]["TextDataEncrypted"]["value"] = json!(data);
    c
}

/// GitHub issue #21: a note whose body opens with blank lines reads back
/// without them (the envelope takes them), so the base copy must be what the
/// file reads back as or a fresh clone has a change to push.
#[test]
fn a_fresh_clone_of_a_note_opening_with_blank_lines_has_nothing_to_push() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let summary = clone_from(
        tiny_with_text("tiny-clone.json", "\n\nTest Note\nSome text"),
        &vault,
        dir.path().join("requests.json"),
    );
    assert_eq!(summary.written, 1);
    assert_nothing_to_push(&vault);
}

/// The same for a pull that updates a clean note to such a text.
#[test]
fn a_pull_to_a_note_opening_with_blank_lines_leaves_nothing_to_push() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    clone_from(cassette("tiny-clone.json"), &vault, dir.path().join("clone.json"));
    let pulled = run_pull_with(
        &replay(
            tiny_with_text("tiny-pull-update.json", "\n\nTest Note\nSome text"),
            dir.path().join("pull.json"),
        ),
        &vault,
        &mut NoProgress,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(pulled.updated, 1);
    assert_nothing_to_push(&vault);
}

fn assert_nothing_to_push(vault: &Path) {
    use icloud_notes_sync::cmd::push::build_push_plan;
    // Anything to push would look the note up first.
    let no_network = FnConnector(|| -> Result<Remote, Error> { panic!("network: the plan found a change") });
    let plan = build_push_plan(&no_network, vault, &mut |_| {}).unwrap();
    let entries: Vec<_> = plan.entries.iter().map(|e| (&e.entry.file, e.entry.kind)).collect();
    assert!(entries.is_empty(), "{entries:?}");
}

/// Clone into a directory that already holds a file where a note would go
/// (a Markdown folder of the user's own) writes the note beside it, never
/// over it.
#[test]
fn a_clone_never_writes_over_a_file_already_in_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let text = "Groceries\nMilk";
    let probe = dir.path().join("probe");
    clone_from(
        tiny_with_text("tiny-clone.json", text),
        &probe,
        dir.path().join("probe.json"),
    );
    let probed = read_clone_state(&probe).unwrap().unwrap();
    let file = probed.notes.values().next().unwrap().file.clone();

    let vault = dir.path().join("vault");
    let own = vault.join(&file);
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    std::fs::write(&own, "my own list, not from iCloud").unwrap();
    let summary = clone_from(
        tiny_with_text("tiny-clone.json", text),
        &vault,
        dir.path().join("requests.json"),
    );
    assert_eq!(summary.written, 1);
    assert_eq!(std::fs::read_to_string(&own).unwrap(), "my own list, not from iCloud");
    let state = read_clone_state(&vault).unwrap().unwrap();
    let cloned = &state.notes.values().next().unwrap().file;
    assert_ne!(cloned, &file);
    assert!(std::fs::read_to_string(vault.join(cloned)).unwrap().contains("Milk"));
}

/// Every Markdown file under `dir` outside the state directory, relative to
/// it, sorted.
fn notes_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.ends_with(".icloud-notes") {
                    walk(root, &path, out);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn tracked_files(vault: &Path) -> Vec<String> {
    let state = read_clone_state(vault).unwrap().unwrap();
    let mut files: Vec<String> = state.notes.values().map(|e| e.file.clone()).collect();
    files.sort();
    files
}

/// A clone killed before it wrote its state leaves its note files, each
/// carrying its id. The clone after it takes those files for the notes they
/// are: no `X 2.md` twin beside each, and so nothing for push to create.
#[test]
fn a_second_clone_over_an_unfinished_one_adopts_the_files_it_left() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    clone_from(cassette("attach-clone.json"), &vault, dir.path().join("first.json"));
    let written = notes_under(&vault);
    assert_eq!(written.len(), 3, "{written:?}");
    // As if the run had been killed before the state write.
    std::fs::remove_dir_all(vault.join(".icloud-notes")).unwrap();

    let summary = clone_from(cassette("attach-clone.json"), &vault, dir.path().join("second.json"));
    assert_eq!(summary.written, 3);
    assert_eq!(notes_under(&vault), written, "no twins");
    assert_eq!(tracked_files(&vault), written);
    assert_nothing_to_push(&vault);
}

/// A file with the note's id that no longer reads as the note does is not
/// the clone's to take over: it stays as it is, and the note is written
/// beside it.
#[test]
fn a_second_clone_leaves_a_left_file_that_was_edited_alone() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    clone_from(cassette("tiny-clone.json"), &vault, dir.path().join("first.json"));
    let written = notes_under(&vault);
    assert_eq!(written.len(), 1);
    std::fs::remove_dir_all(vault.join(".icloud-notes")).unwrap();
    let edited = std::fs::read_to_string(vault.join(&written[0])).unwrap() + "\nedited since";
    std::fs::write(vault.join(&written[0]), &edited).unwrap();

    clone_from(cassette("tiny-clone.json"), &vault, dir.path().join("second.json"));
    assert_eq!(std::fs::read_to_string(vault.join(&written[0])).unwrap(), edited);
    assert_eq!(notes_under(&vault).len(), 2);
    assert_ne!(tracked_files(&vault), written);
}

/// Other files may carry the left note's id by the time of the second clone
/// (copies edited since, Syncthing conflict copies). The file that still
/// reads as the note is the one taken, whichever of them is found first.
#[test]
fn a_second_clone_adopts_the_left_file_among_copies_that_carry_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    clone_from(cassette("tiny-clone.json"), &vault, dir.path().join("first.json"));
    let written = notes_under(&vault);
    assert_eq!(written.len(), 1);
    std::fs::remove_dir_all(vault.join(".icloud-notes")).unwrap();
    let edited = std::fs::read_to_string(vault.join(&written[0])).unwrap() + "\nedited since";
    let stem = written[0].strip_suffix(".md").unwrap();
    for n in 0..4 {
        std::fs::write(vault.join(format!("{stem} copy {n}.md")), &edited).unwrap();
        let conflict = format!("{stem}.sync-conflict-20261114-10000{n}-ABCDEFG.md");
        std::fs::write(vault.join(conflict), &edited).unwrap();
    }

    clone_from(cassette("tiny-clone.json"), &vault, dir.path().join("second.json"));
    assert_eq!(tracked_files(&vault), written, "the note's own file is taken");
    assert_eq!(notes_under(&vault).len(), 1 + 8, "nothing written beside it");
}

/// A clone that fails part way (here an attachment download) records the
/// notes it wrote, as a failed pull does: push has nothing to create, and
/// the first pull brings the rest without a second copy of any note.
#[test]
fn a_clone_that_fails_part_way_tracks_what_it_wrote() {
    const IMAGE_ASSET: &str = "https://cvws.icloud-content.com/B/image-asset";
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let mut failing = cassette("attach-clone.json");
    failing
        .interactions
        .retain(|i| i.request.url.as_deref() != Some(IMAGE_ASSET));
    let failed = run_clone_with(
        &replay(failing, dir.path().join("failing.json")),
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    );
    assert!(failed.is_err());

    let state = read_clone_state(&vault).unwrap().expect("what was written is recorded");
    assert!(!state.notes.is_empty());
    assert_eq!(state.sync_token, None, "the first pull lists everything");
    assert_eq!(notes_under(&vault), tracked_files(&vault));
    assert_nothing_to_push(&vault);

    run_pull_with(
        &replay(cassette("attach-clone.json"), dir.path().join("pull.json")),
        &vault,
        &mut NoProgress,
        &mut |_| {},
    )
    .unwrap();
    let files = tracked_files(&vault);
    assert_eq!(files.len(), 3, "{files:?}");
    assert_eq!(notes_under(&vault), files);
    let state = read_clone_state(&vault).unwrap().unwrap();
    for attachment in state.attachments.unwrap().values() {
        assert!(vault.join(&attachment.file).exists(), "{}", attachment.file);
    }
    assert_nothing_to_push(&vault);
}
