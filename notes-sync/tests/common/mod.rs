//! Shared helpers for the `doc_*` tests: the real fixtures
//! (`tests/fixtures/real/`, exported from icloud-md's `realFixtures.ts`).
#![allow(dead_code)]

use serde_json::Value;

use icloud_notes_sync::doc::js::base64_decode;

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
