//! `compress_note_document` must be byte-identical to Node's
//! `zlib.deflateSync` (Chromium zlib), which is what icloud-md uploads.

use std::io::Write;
use std::process::{Command, Stdio};

use icloud_notes_sync::doc::text::{compress_note_document, decompress_note_document};

/// Deterministic xorshift corpus: random bytes, low-entropy text, runs, and
/// sizes around the window (32 KiB), the slide point and the 16383-symbol
/// block limit.
fn corpus() -> Vec<Vec<u8>> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let words: [&[u8]; 9] = [
        b"hello",
        b"world",
        b"note",
        "\u{FFFC}".as_bytes(),
        b"\n",
        b"the",
        b"apple",
        b"table",
        b" ",
    ];
    let mut out = Vec::new();
    for n in [
        0usize, 1, 2, 3, 4, 5, 7, 10, 64, 100, 258, 259, 1000, 4096, 10_000, 32_768, 40_000, 65_536, 100_000, 300_000,
    ] {
        out.push((0..n).map(|_| next() as u8).collect());
        let mut text = Vec::new();
        while text.len() < n {
            text.extend_from_slice(words[(next() % words.len() as u64) as usize]);
        }
        out.push(text);
        out.push(vec![b'a'; n]);
        out.push((0..n).map(|_| if next() % 2 == 0 { b'a' } else { b'b' }).collect());
        out.push((0..n).map(|_| (next() % 4) as u8).collect());
    }
    out
}

fn fixture_payloads() -> Vec<Vec<u8>> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    match (k.as_str(), v) {
                        ("base64", serde_json::Value::String(s)) => out.push(s.clone()),
                        _ => walk(v, out),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real");
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let mut encoded = Vec::new();
    for path in names {
        if path.file_name().unwrap() == "index.json" {
            continue;
        }
        walk(
            &serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
            &mut encoded,
        );
    }
    use base64::Engine;
    encoded
        .iter()
        .map(|b| decompress_note_document(&base64::engine::general_purpose::STANDARD.decode(b).unwrap()).unwrap())
        .collect()
}

/// Runs every input through `node -e zlib.deflateSync`; `None` without node.
fn node_deflate(inputs: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let script = r#"
        const zlib = require("zlib");
        const lines = require("fs").readFileSync(0, "utf8").split("\n").filter((l, i, a) => i < a.length - 1);
        process.stdout.write(lines.map((l) => zlib.deflateSync(Buffer.from(l, "base64")).toString("base64")).join("\n") + "\n");
    "#;
    let mut child = Command::new("node")
        .args(["-e", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take().unwrap();
    let payload: String = inputs.iter().map(|i| b64.encode(i) + "\n").collect();
    let writer = std::thread::spawn(move || stdin.write_all(payload.as_bytes()).unwrap());
    let output = child.wait_with_output().ok()?;
    writer.join().unwrap();
    assert!(output.status.success(), "node failed");
    Some(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|l| b64.decode(l).unwrap())
            .collect(),
    )
}

#[test]
fn deflate_matches_node_on_fixtures_and_corpus() {
    let mut inputs = fixture_payloads();
    assert!(inputs.len() >= 50, "expected every fixture payload");
    inputs.extend(corpus());
    let Some(expected) = node_deflate(&inputs) else {
        eprintln!("node not available - skipping the live Node comparison");
        return;
    };
    assert_eq!(expected.len(), inputs.len());
    let mut mismatches = Vec::new();
    for (i, (input, want)) in inputs.iter().zip(&expected).enumerate() {
        let got = compress_note_document(input);
        if &got != want {
            mismatches.push(format!("#{i} ({} bytes)", input.len()));
        }
        assert_eq!(decompress_note_document(&got).unwrap(), *input);
    }
    assert!(mismatches.is_empty(), "differs from Node for {mismatches:?}");
}

#[test]
fn deflate_of_empty_input_matches_node() {
    assert_eq!(
        compress_note_document(b""),
        [0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]
    );
}
