//! A file whose last line is a list item, heading or quote with nothing in
//! it. A paragraph's style is carried by its characters, and the last
//! paragraph of a note, when empty, has none: such a line is an empty line
//! in the note, and the push goes up as that.

use std::path::{Path, PathBuf};

use icloud_notes_sync::cloudkit::Database;
use icloud_notes_sync::cloudkit::transport::{Cassette, ReplayTransport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::push::{PushOptions, run_push_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::doc::format::{ParagraphKind, decode_note_format};
use icloud_notes_sync::doc::text::decode_note_string;
use icloud_notes_sync::js::base64_decode;
use icloud_notes_sync::vault::state::Account;
use serde_json::Value;

const NOTE_FILE: &str = "Notes/Test Note.md";

fn replay(name: &str, log: PathBuf) -> impl Connector {
    let cassette = Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/differential/cassettes")
            .join(name),
    )
    .unwrap();
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

#[test]
fn a_last_line_that_is_an_empty_item_heading_or_quote_goes_up_as_an_empty_line() {
    use ParagraphKind::*;
    // What is added to the note's file, and what its lines are in the note.
    for (added, kinds) in [
        ("\n- one\n- ", vec![BulletList, Body]),
        ("\n- [x] done\n- [ ] ", vec![TodoList, Body]),
        ("\n1. one\n2. ", vec![NumberedList, Body]),
        ("\n## ", vec![Body]),
        ("\n> quoted\n> ", vec![Body, Body]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        let clone = replay("tiny-clone.json", dir.path().join("clone.json"));
        run_clone_with(&clone, &vault, &mut NoProgress, &CloneOptions::default()).unwrap();
        let file = vault.join(NOTE_FILE);
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}{added}")).unwrap();

        let log = dir.path().join("push.json");
        let push = replay("tiny-push.json", log.clone());
        let result = run_push_with(&push, &vault, &mut |_| {}, &PushOptions::default()).unwrap();
        let reasons: Vec<_> = result.entries.iter().map(|e| e.entry.reason.clone()).collect();
        assert_eq!(result.pushed, Some(1), "{added:?}: {reasons:?}");

        let log: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
        let modify = log["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|request| request["path"].as_str().unwrap().ends_with("/records/modify"))
            .unwrap();
        let sent = modify["body"]["operations"][0]["record"]["fields"]["TextDataEncrypted"]["value"]
            .as_str()
            .unwrap();
        let note = decode_note_string(&base64_decode(sent)).unwrap();
        let format = decode_note_format(note.string(), &note.attribute_run).unwrap();
        let added_lines = &format[format.len() - kinds.len()..];
        assert_eq!(
            added_lines.iter().map(|p| p.kind).collect::<Vec<_>>(),
            kinds,
            "{added:?}"
        );
        let last = format.last().unwrap();
        assert_eq!((last.text.as_str(), last.block_quote_level), ("", 0), "{added:?}");
    }
}
