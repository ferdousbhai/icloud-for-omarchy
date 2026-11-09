//! `compress_note_document` / `decompress_note_document` (the deflate
//! codec, flate2): everything compressed must inflate back unchanged, and
//! the container must stay zlib (what Notes writes and icloud-md sent).

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
    encoded
        .iter()
        .map(|b| decompress_note_document(&icloud_notes_sync::js::base64_decode(b)).unwrap())
        .collect()
}

#[test]
fn deflate_round_trips_on_fixtures_and_corpus() {
    let mut inputs = fixture_payloads();
    assert!(inputs.len() >= 50, "expected every fixture payload");
    inputs.extend(corpus());
    for (i, input) in inputs.iter().enumerate() {
        let compressed = compress_note_document(input);
        assert_eq!(
            decompress_note_document(&compressed).unwrap(),
            *input,
            "#{i} ({} bytes)",
            input.len()
        );
    }
}

/// A small payload inflating past the cap (gzip members concatenated, or
/// one zlib stream of zeros) is an error, not 1 GiB of memory.
#[test]
fn a_document_inflating_past_the_cap_is_refused() {
    use std::io::Write;
    let mut member = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    member.write_all(&[0u8; 1 << 20]).unwrap();
    let member = member.finish().unwrap();
    let bomb = member.repeat(65);
    assert!(bomb.len() < 1 << 20);
    let err = decompress_note_document(&bomb)
        .map(|out| out.len())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "the note's document inflates past 64 MiB");

    let zlib_bomb = compress_note_document(&vec![0u8; (64 << 20) + 1]);
    assert!(decompress_note_document(&zlib_bomb).is_err());
    let under = compress_note_document(&vec![0u8; 1 << 20]);
    assert_eq!(decompress_note_document(&under).unwrap().len(), 1 << 20);
}

/// A zlib stream (RFC 1950 header, not gzip's 1f 8b), at the default level.
#[test]
fn compressed_documents_are_zlib_streams() {
    for input in [&b""[..], b"hello note", &[7u8; 5000][..]] {
        let out = compress_note_document(input);
        assert_eq!(out[0], 0x78, "deflate, 32 KiB window");
        assert_eq!(u16::from_be_bytes([out[0], out[1]]) % 31, 0, "header check bits");
        assert_eq!(out[1] & 0x20, 0, "no preset dictionary");
    }
}
