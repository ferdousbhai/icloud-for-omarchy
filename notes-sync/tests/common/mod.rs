//! Shared helpers for the `doc_*` tests: the real fixtures
//! (`tests/fixtures/real/`, exported from icloud-md's `realFixtures.ts`).
#![allow(dead_code)]

use serde_json::Value;

use icloud_notes_sync::js::base64_decode;

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

pub fn grid(value: &Value) -> Vec<Vec<String>> {
    serde_json::from_value(value.clone()).unwrap()
}

pub fn strings(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(|s| s.to_string()).collect())
        .collect()
}

/// icloud-md's `parseNoteMarkdown` of `markdown`, as recorded in
/// `tests/doc_node/parsed_markdown.json` (regenerate with the oracle):
/// `(text, paragraphs)`.
pub fn parsed_markdown(markdown: &str) -> (String, Vec<icloud_notes_sync::doc::format::FormatParagraph>) {
    let path = format!("{}/tests/doc_node/parsed_markdown.json", env!("CARGO_MANIFEST_DIR"));
    let all: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let parsed = &all[markdown];
    assert_eq!(parsed["status"], "ok", "no recorded parse for {markdown:?}");
    (
        parsed["text"].as_str().unwrap().to_string(),
        serde_json::from_value(parsed["paragraphs"].clone()).unwrap(),
    )
}

/// Runs requests through `tests/doc_node/oracle.mts` (icloud-md itself);
/// `None` when node/tsx or the icloud-md clone isn't available.
pub fn oracle(requests: &Value) -> Option<Value> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let root = env!("CARGO_MANIFEST_DIR");
    let icloud_md = std::env::var("ICLOUD_MD").unwrap_or_else(|_| format!("{root}/../../../coddingtonbear/icloud-md"));
    let tsx = format!("{icloud_md}/node_modules/.bin/tsx");
    if !std::path::Path::new(&tsx).exists() {
        eprintln!("icloud-md oracle unavailable ({tsx} missing) - skipping the Node comparison");
        return None;
    }
    let mut child = Command::new(tsx)
        .arg(format!("{root}/tests/doc_node/oracle.mts"))
        .env("ICLOUD_MD", &icloud_md)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take().unwrap();
    let payload = serde_json::to_vec(requests).unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&payload).unwrap());
    let output = child.wait_with_output().ok()?;
    writer.join().unwrap();
    assert!(output.status.success(), "oracle failed");
    Some(serde_json::from_slice(&output.stdout).unwrap())
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

/// `bytes` as a canonical UUID string (what `randomUUID` returns).
pub fn uuid_string(bytes: &[u8; 16]) -> String {
    let h = hex(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}
