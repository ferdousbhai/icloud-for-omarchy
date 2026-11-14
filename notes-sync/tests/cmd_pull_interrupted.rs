//! A pull that fails part way: the notes it already wrote stay tracked, so
//! the next push doesn't send them to iCloud as new notes, and the next pull
//! finishes what is left.

use std::path::{Path, PathBuf};

use icloud_notes_sync::cloudkit::Database;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::pull::{PullSummary, run_pull_with};
use icloud_notes_sync::cmd::push::build_push_plan;
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::vault::state::{Account, read_clone_state};
use serde_json::json;

/// The attach cassette's notes: one with an audio attachment, then one with
/// an image (listed in that order).
const AUDIO_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000a0d1";
const IMAGE_NOTE: &str = "a1b2c3d4-0000-4000-8000-00000000196e";
const IMAGE_ASSET: &str = "https://cvws.icloud-content.com/B/image-asset";

fn cassette(name: &str) -> Cassette {
    Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/differential/cassettes")
            .join(name),
    )
    .unwrap()
}

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

fn pull(vault: &Path, cassette: Cassette, log: PathBuf) -> Result<PullSummary, Error> {
    run_pull_with(&replay(cassette, log), vault, &mut NoProgress, &mut |_| {})
}

/// Every Markdown file under `dir`, relative to it, sorted.
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

/// The image's download fails after the audio note is written: that note
/// stays tracked, so the push plan has nothing to create, and the next pull
/// (from the same sync token) adds the image note and its attachment without
/// a second copy of either note.
#[test]
fn a_note_written_before_the_pull_failed_stays_tracked() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    run_clone_with(
        &replay(cassette("tiny-clone.json"), dir.path().join("clone.json")),
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    let token = read_clone_state(&vault).unwrap().unwrap().sync_token;

    let mut failing = cassette("attach-clone.json");
    failing
        .interactions
        .retain(|i| i.request.url.as_deref() != Some(IMAGE_ASSET));
    assert!(pull(&vault, failing, dir.path().join("failing.json")).is_err());

    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.sync_token, token, "the rest is listed again");
    let audio = state.notes.get(AUDIO_NOTE).expect("the note written is tracked");
    assert!(vault.join(&audio.file).exists());
    assert!(!state.notes.contains_key(IMAGE_NOTE));
    // Its download may not have run (downloads stop at the first failure):
    // the next pull applies it again and fetches its attachment.
    assert_eq!(audio.record_change_tag, "");
    assert!(
        state
            .attachments
            .unwrap_or_default()
            .values()
            .all(|a| a.note_record_name != AUDIO_NOTE)
    );
    let no_network = FnConnector(|| -> Result<Remote, Error> { panic!("network: the plan found a change") });
    let plan = build_push_plan(&no_network, &vault, &mut |_| {}).unwrap();
    let entries: Vec<_> = plan.entries.iter().map(|e| (&e.entry.file, e.entry.kind)).collect();
    assert!(entries.is_empty(), "{entries:?}");

    let summary = pull(&vault, cassette("attach-clone.json"), dir.path().join("pull.json")).unwrap();
    assert_eq!(summary.added, 1);
    let state = read_clone_state(&vault).unwrap().unwrap();
    let attachments = state.attachments.unwrap();
    for note in [AUDIO_NOTE, IMAGE_NOTE] {
        let attachment = attachments
            .values()
            .find(|a| a.note_record_name == note)
            .unwrap_or_else(|| panic!("{note} has its attachment"));
        assert!(vault.join(&attachment.file).exists(), "{}", attachment.file);
    }
    let tracked: Vec<String> = {
        let mut files: Vec<String> = state.notes.values().map(|e| e.file.clone()).collect();
        files.sort();
        files
    };
    assert_eq!(notes_under(&vault), tracked);
}

/// The image note's Attachment record.
const IMAGE_ATTACHMENT: &str = "7ED80274-4400-4C02-87EA-F542F056FF02";

/// attach-clone's answers as a later pull's: the listing ends at the sync
/// token "NEXT" and has the image note at `image_tag`; with `failing`, the
/// lookup answers an error for that note's Attachment record.
fn image_note_listed(image_tag: &str, failing: bool) -> Cassette {
    let mut cassette = cassette("attach-clone.json");
    for interaction in &mut cassette.interactions {
        let path = interaction.request.path.clone().unwrap_or_default();
        let Some(body) = interaction.response.body.as_mut() else {
            continue;
        };
        if path.ends_with("/private/changes/zone") {
            body["zones"][0]["syncToken"] = json!("NEXT");
            for record in body["zones"][0]["records"].as_array_mut().unwrap() {
                if record["recordName"] == IMAGE_NOTE {
                    record["recordChangeTag"] = json!(image_tag);
                }
            }
        } else if path.ends_with("/records/lookup") && failing {
            for record in body["records"].as_array_mut().unwrap() {
                if record["recordName"] == IMAGE_ATTACHMENT {
                    *record = json!({
                        "recordName": IMAGE_ATTACHMENT, "reason": "try again", "serverErrorCode": "INTERNAL_ERROR",
                    });
                }
            }
        }
    }
    cassette
}

fn clone_of(name: &str, vault: &Path, log: PathBuf) {
    run_clone_with(
        &replay(cassette(name), log),
        vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
}

/// iCloud answers an error (not "not found") for a tracked note's Attachment
/// record: that says nothing of the attachment, so the note, its link and
/// its file stay as they are, the pull goes on, and the zone's sync token
/// stays put so the next pull lists the note again.
#[test]
fn an_attachment_lookup_error_leaves_the_note_and_its_file_for_the_next_pull() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    clone_of("attach-clone.json", &vault, dir.path().join("clone.json"));
    let before = read_clone_state(&vault).unwrap().unwrap();
    let note_file = vault.join(&before.notes[IMAGE_NOTE].file);
    let text = std::fs::read_to_string(&note_file).unwrap();
    let attachment = before.attachments.clone().unwrap()[IMAGE_ATTACHMENT].clone();
    assert!(vault.join(&attachment.file).exists());

    let summary = pull(&vault, image_note_listed("31a", true), dir.path().join("failing.json")).unwrap();
    assert!(
        summary
            .notices
            .iter()
            .any(|n| n.message.contains("INTERNAL_ERROR") && n.message.contains(&before.notes[IMAGE_NOTE].file)),
        "{:?}",
        summary.notices
    );
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&note_file).unwrap(), text, "no embed marker");
    assert!(vault.join(&attachment.file).exists(), "the file is kept");
    assert_eq!(state.attachments.clone().unwrap()[IMAGE_ATTACHMENT], attachment);
    assert_eq!(state.notes[IMAGE_NOTE], before.notes[IMAGE_NOTE]);
    assert_eq!(state.sync_token, before.sync_token, "listed again by the next pull");

    pull(&vault, image_note_listed("31a", false), dir.path().join("pull.json")).unwrap();
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes[IMAGE_NOTE].record_change_tag, "31a");
    assert_eq!(state.sync_token.as_deref(), Some("NEXT"));
    assert_eq!(std::fs::read_to_string(&note_file).unwrap(), text);
    assert!(vault.join(&attachment.file).exists());
}

/// The same for a note new to the vault: it is not written this run (with a
/// marker for an attachment it has), the other notes are, and the next pull
/// adds it.
#[test]
fn an_attachment_lookup_error_holds_a_new_note_back_without_stopping_the_pull() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    clone_of("tiny-clone.json", &vault, dir.path().join("clone.json"));
    let token = read_clone_state(&vault).unwrap().unwrap().sync_token;

    let summary = pull(&vault, image_note_listed("30b", true), dir.path().join("failing.json")).unwrap();
    assert_eq!(summary.added, 1, "the audio note");
    assert!(summary.notices.iter().any(|n| n.message.contains("INTERNAL_ERROR")));
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(state.notes.contains_key(AUDIO_NOTE) && !state.notes.contains_key(IMAGE_NOTE));
    assert_eq!(state.sync_token, token);

    let summary = pull(&vault, image_note_listed("30b", false), dir.path().join("pull.json")).unwrap();
    assert_eq!(summary.added, 1);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(vault.join(&state.attachments.unwrap()[IMAGE_ATTACHMENT].file).exists());
    assert_eq!(notes_under(&vault).len(), state.notes.len());
}

/// A clone leaves such a note out and saves no sync token for its zone, so
/// the first pull adds it.
#[test]
fn an_attachment_lookup_error_in_a_clone_leaves_the_note_to_the_first_pull() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let failing = replay(image_note_listed("30b", true), dir.path().join("clone.json"));
    let summary = run_clone_with(&failing, &vault, &mut NoProgress, &CloneOptions::default()).unwrap();
    assert!(summary.notices.iter().any(|n| n.message.contains("INTERNAL_ERROR")));
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(state.notes.contains_key(AUDIO_NOTE) && !state.notes.contains_key(IMAGE_NOTE));
    assert_eq!(state.sync_token, None);

    let summary = pull(&vault, image_note_listed("30b", false), dir.path().join("pull.json")).unwrap();
    assert_eq!(summary.added, 1);
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert!(state.notes.contains_key(IMAGE_NOTE));
    assert_eq!(notes_under(&vault).len(), state.notes.len());
}

/// tiny-pull-update's answers, listing `count` new notes (copies of its one
/// note under other record names) in place of the update.
fn many_new_notes(count: usize) -> Cassette {
    let mut cassette = cassette("tiny-pull-update.json");
    let listing = cassette.interactions[0].response.body.as_mut().unwrap();
    let records = listing["zones"][0]["records"].as_array_mut().unwrap();
    let note = records.iter().find(|r| r["recordType"] == "Note").unwrap().clone();
    *records = (0..count)
        .map(|n| {
            let mut copy = note.clone();
            copy["recordName"] = format!("a1b2c3d4-0000-4000-8000-{n:012x}").into();
            copy
        })
        .collect();
    cassette
}

/// Stops the pull, as a kill would, once `after` records were processed:
/// nothing of the run's own bookkeeping gets to run.
struct KilledAfter {
    after: usize,
    processed: usize,
}

impl icloud_notes_sync::cmd::SyncProgress for KilledAfter {
    fn on_record_processed(&mut self) {
        self.processed += 1;
        if self.processed > self.after {
            panic!("killed");
        }
    }
}

/// A pull killed part way never reaches the code that records what it
/// applied. The notes it had written are tracked all the same, from the
/// state saved along the way, so the next push does not create them in
/// iCloud a second time, and the next pull adds only the rest.
#[test]
fn notes_written_before_the_pull_was_killed_are_not_pushed_as_new() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    run_clone_with(
        &replay(cassette("tiny-clone.json"), dir.path().join("clone.json")),
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    let token = read_clone_state(&vault).unwrap().unwrap().sync_token;

    // Killed while applying the 51st of 60 new notes: 50 are written.
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_pull_with(
            &replay(many_new_notes(60), dir.path().join("killed.json")),
            &vault,
            &mut KilledAfter {
                after: 50,
                processed: 0,
            },
            &mut |_| {},
        )
    }));
    assert!(killed.is_err(), "the pull was cut off");

    let written = notes_under(&vault);
    assert_eq!(written.len(), 1 + 51, "{written:?}");
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.sync_token, token, "everything is listed again");
    let tracked: Vec<&str> = state.notes.values().map(|e| e.file.as_str()).collect();
    assert_eq!(tracked.len(), 1 + 50, "what the last save knew of");
    assert!(tracked.iter().all(|file| written.iter().any(|w| w == file)));

    // Only the note written after the last save is untracked (the state is
    // saved every 25 notes); set it aside, and a push has nothing to create.
    let untracked: Vec<&String> = written.iter().filter(|w| !tracked.contains(&w.as_str())).collect();
    assert_eq!(untracked.len(), 1, "{untracked:?}");
    std::fs::remove_file(vault.join(untracked[0])).unwrap();
    let no_network = FnConnector(|| -> Result<Remote, Error> { panic!("network: the plan found a change") });
    let plan = build_push_plan(&no_network, &vault, &mut |_| {}).unwrap();
    let entries: Vec<_> = plan.entries.iter().map(|e| (&e.entry.file, e.entry.kind)).collect();
    assert!(entries.is_empty(), "{entries:?}");

    // The next pull finishes the job without a second copy of any note.
    pull(&vault, many_new_notes(60), dir.path().join("pull.json")).unwrap();
    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes.len(), 1 + 60);
    let mut tracked: Vec<String> = state.notes.values().map(|e| e.file.clone()).collect();
    tracked.sort();
    assert_eq!(notes_under(&vault), tracked);
}

/// tiny-pull-update's answers, with the note's text replaced by bytes that
/// can't be decoded (a format this build doesn't read).
fn undecodable_update() -> Cassette {
    let mut cassette = cassette("tiny-pull-update.json");
    let listing = cassette.interactions[0].response.body.as_mut().unwrap();
    let records = listing["zones"][0]["records"].as_array_mut().unwrap();
    let note = records.iter_mut().find(|r| r["recordType"] == "Note").unwrap();
    note["fields"]["TextDataEncrypted"]["value"] = "bm90IGEgbm90ZSBkb2N1bWVudA==".into();
    cassette
}

/// A tracked note whose text this build can no longer decode still exists
/// in iCloud. It stays tracked, with its file as it was: untracked, the file
/// (which carries the note's id) would be sent to iCloud as a new note by
/// the next push, a second copy of a note that is still there.
#[test]
fn a_tracked_note_that_became_undecodable_stays_tracked_and_is_not_pushed_as_new() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    run_clone_with(
        &replay(cassette("tiny-clone.json"), dir.path().join("clone.json")),
        &vault,
        &mut NoProgress,
        &CloneOptions::default(),
    )
    .unwrap();
    let before = read_clone_state(&vault).unwrap().unwrap();
    let (record_name, entry) = before.notes.first().unwrap();
    let text = std::fs::read_to_string(vault.join(&entry.file)).unwrap();

    let summary = pull(&vault, undecodable_update(), dir.path().join("pull.json")).unwrap();
    assert_eq!(summary.dropped_unsyncable, 0);
    assert!(summary.changes.is_empty(), "{:?}", summary.changes);
    assert!(
        summary.notices.iter().any(|n| n.message.contains(&entry.file)),
        "{:?}",
        summary.notices
    );

    let state = read_clone_state(&vault).unwrap().unwrap();
    assert_eq!(state.notes.get(record_name).map(|e| &e.file), Some(&entry.file));
    assert_eq!(std::fs::read_to_string(vault.join(&entry.file)).unwrap(), text);
    let no_network = FnConnector(|| -> Result<Remote, Error> { panic!("network: the plan found a change") });
    let plan = build_push_plan(&no_network, &vault, &mut |_| {}).unwrap();
    let entries: Vec<_> = plan.entries.iter().map(|e| (&e.entry.file, e.entry.kind)).collect();
    assert!(entries.is_empty(), "{entries:?}");
}
