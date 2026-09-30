//! Ports icloud-md `src/commands/clone.test.ts`.

use std::path::{Path, PathBuf};

use icloud_notes_sync::cloudkit::Database;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cmd::clone::{CloneOptions, CloneSummary, run_clone_with};
use icloud_notes_sync::cmd::pull::{PullOptions, run_pull_with};
use icloud_notes_sync::cmd::remote::{AnyTransport, Connector, FnConnector, Remote};
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
    let err = run_clone_with(
        &connector,
        dir.path(),
        &mut NoProgress,
        &mut |_| {},
        &CloneOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("is already a cloned notes directory"));
}

// Deliberate difference from icloud-md 0.6.2 (docs/PORT_PLAN.md §1, "New
// notes listed without their text"): clone looks such notes up and, if they
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
            db: Database::new(AnyTransport::Replay(Box::new(transport))),
            account,
        })
    })
}

fn clone_from(cassette: Cassette, vault: &Path, log: PathBuf) -> CloneSummary {
    run_clone_with(
        &replay(cassette, log),
        vault,
        &mut NoProgress,
        &mut |_| {},
        &CloneOptions::default(),
    )
    .unwrap()
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
        &PullOptions::default(),
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
