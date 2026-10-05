//! Shared helpers for the tests: the real fixtures (`tests/fixtures/real/`,
//! payloads captured from iCloud), the golden corpora (`tests/golden/`), a
//! scratch vault's files, and small values several tests build.
#![allow(dead_code)]

use std::path::Path;

use serde_json::Value;

use icloud_notes_sync::cloudkit::ZoneId;
use icloud_notes_sync::cmd::Error;
use icloud_notes_sync::cmd::remote::{FnConnector, Remote};
use icloud_notes_sync::doc::embeds::AttachmentReference;
use icloud_notes_sync::js::base64_decode;
use icloud_notes_sync::vault::state::{CloneState, NoteEntry};

pub fn fixture(file: &str) -> Value {
    let path = format!("{}/tests/fixtures/real/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Every fixture file except the index.
pub fn fixture_files() -> Vec<String> {
    let index = fixture("index.json");
    index
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["file"].as_str().unwrap().to_string())
        .collect()
}

/// The compressed payload (bytes) of a single-payload fixture.
pub fn payload(file: &str) -> Vec<u8> {
    base64_decode(fixture(file)["base64"].as_str().unwrap())
}

/// The base64 payload string of a single-payload fixture.
pub fn payload_base64(file: &str) -> String {
    fixture(file)["base64"].as_str().unwrap().to_string()
}

/// `(label, compressed payload, golden)` for every payload in every fixture,
/// revisions included.
pub fn all_payloads() -> Vec<(String, String, Vec<u8>, Value)> {
    let mut out = Vec::new();
    for file in fixture_files() {
        let json = fixture(&file);
        let kind = json["kind"].as_str().unwrap().to_string();
        if let Some(b64) = json["base64"].as_str() {
            out.push((file.clone(), kind.clone(), base64_decode(b64), json["golden"].clone()));
        }
        if let Some(revisions) = json["revisions"].as_array() {
            for (i, revision) in revisions.iter().enumerate() {
                out.push((
                    format!("{file}[{i}]"),
                    "table".to_string(),
                    base64_decode(revision["base64"].as_str().unwrap()),
                    revision["golden"].clone(),
                ));
            }
        }
    }
    out
}

/// `ICLOUD_NOTES_SYNC_REGEN=1`: tests with recorded expectations rewrite
/// them from what this crate does now instead of comparing.
pub fn regen() -> bool {
    std::env::var_os("ICLOUD_NOTES_SYNC_REGEN").is_some_and(|v| v != "0")
}

fn golden_path(name: &str) -> String {
    format!("{}/tests/golden/{name}.gz", env!("CARGO_MANIFEST_DIR"))
}

fn golden_text(name: &str) -> String {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(golden_path(name)).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// A gzipped golden corpus, `tests/golden/<name>.gz`.
pub fn golden(name: &str) -> Value {
    serde_json::from_str(&golden_text(name)).unwrap()
}

/// Checks this crate against a golden corpus section: `tests/golden/<name>.gz`
/// (`section` names a key of its top-level object, or `None` for a top-level
/// array) holds cases whose inputs are frozen and whose outputs are what the
/// crate gave when last recorded. `outputs` computes, for one case, each
/// output field's value now; every field must equal the recorded one.
///
/// With [`regen`], the differing fields are rewritten instead (the inputs
/// stay as they are) and the count printed - review the diff before
/// committing it.
pub fn check_golden(name: &str, section: Option<&str>, outputs: impl Fn(&Value) -> Vec<(&'static str, Value)>) {
    // Several tests may rewrite sections of the same file.
    static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let label = format!("{name}{}", section.map(|s| format!(" {s}")).unwrap_or_default());
    let data = golden(name);
    let mut cases = match section {
        Some(key) => data[key].clone(),
        None => data,
    };
    let list = cases.as_array_mut().unwrap();
    let total = list.len();
    let mut failures = Vec::new();
    for case in list.iter_mut() {
        let got = outputs(case);
        let mut differ = Vec::new();
        for (field, value) in got {
            if case[field] != value {
                differ.push(format!("{field}: recorded {} now {value}", case[field]));
                case[field] = value;
            }
        }
        if !differ.is_empty() {
            failures.push(format!("{case}\n  {}", differ.join("\n  ")));
        }
    }
    if regen() {
        eprintln!("{label}: {} of {total} cases re-recorded", failures.len());
        if failures.is_empty() {
            return;
        }
        let _guard = WRITE.lock().unwrap();
        let mut data = golden(name);
        match section {
            Some(key) => data[key] = cases,
            None => data = cases,
        }
        let text = serde_json::to_string(&data).unwrap() + "\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut gz, text.as_bytes()).unwrap();
        std::fs::write(golden_path(name), gz.finish().unwrap()).unwrap();
        return;
    }
    eprintln!("{label}: {}/{total} match", total - failures.len());
    for failure in failures.iter().take(8) {
        eprintln!("--- {failure}");
    }
    assert!(
        failures.is_empty(),
        "{label}: {} of {total} cases differ from the recording (if intended: ICLOUD_NOTES_SYNC_REGEN=1, then review the diff)",
        failures.len()
    );
}

pub fn grid(value: &Value) -> Vec<Vec<String>> {
    serde_json::from_value(value.clone()).unwrap()
}

pub fn strings(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(|s| s.to_string()).collect())
        .collect()
}

pub const PARSED_MARKDOWN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/parsed_markdown.json");

/// `parse_note_markdown` of `markdown` as recorded in
/// `tests/fixtures/parsed_markdown.json` (kept in line with the parser by
/// `doc_reconcile`'s `recorded_parses_match_the_parser`): `(text, paragraphs)`.
pub fn parsed_markdown(markdown: &str) -> (String, Vec<icloud_notes_sync::doc::format::FormatParagraph>) {
    let all: Value = serde_json::from_str(&std::fs::read_to_string(PARSED_MARKDOWN).unwrap()).unwrap();
    let parsed = &all[markdown];
    assert_eq!(parsed["status"], "ok", "no recorded parse for {markdown:?}");
    (
        parsed["text"].as_str().unwrap().to_string(),
        serde_json::from_value(parsed["paragraphs"].clone()).unwrap(),
    )
}

/// A deterministic, never-repeating 16-byte source (a process-wide counter,
/// like `randomBytes` never handing out the same identity twice).
pub fn counting_uuids() -> impl FnMut() -> [u8; 16] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    move || {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let mut bytes = [0x5a; 16];
        bytes[8..].copy_from_slice(&n.to_be_bytes());
        bytes[6] = 0x40 | (bytes[6] & 0x0f);
        bytes
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The shared `Notes` zone `owner` holds.
pub fn zone(owner: &str) -> ZoneId {
    ZoneId {
        zone_name: "Notes".into(),
        owner_record_name: Some(owner.into()),
    }
}

/// A connector for a command that must not reach iCloud.
pub fn no_network() -> FnConnector<impl Fn() -> Result<Remote, Error>> {
    FnConnector(|| -> Result<Remote, Error> { panic!("network") })
}

/// A clone state tracking one note, `REC1` at `Test Note.md`.
pub fn state() -> CloneState {
    CloneState {
        sync_token: Some("token".into()),
        notes: [("REC1".to_owned(), NoteEntry::new("Test Note.md", "1a", 100))]
            .into_iter()
            .collect(),
        ..Default::default()
    }
}

pub fn write_vault_file(dir: &Path, file: &str, content: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

pub fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap()
}

pub fn reference(id: &str, uti: &str) -> AttachmentReference {
    AttachmentReference {
        attachment_identifier: id.into(),
        type_uti: uti.into(),
    }
}
