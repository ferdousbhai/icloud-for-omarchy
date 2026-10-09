//! A reminder's `TitleDocument` and `NotesDocument`: Apple's "topotext"
//! mergeable string (the CRDT Notes uses too), as a protobuf
//! `versioned_document.Document` whose first `Version.data` is a
//! `topotext.String`, zlib-compressed and base64-encoded.
//!
//! The message layouts are timlaing/pyicloud's
//! `services/reminders/protobuf/{versioned_document,reminders}.proto`
//! (extracted from icloud.com's Reminders web app). Decoding takes the
//! plain text (`String.string`, field 2) and ignores the merge history;
//! encoding writes the one-replica document pyicloud's
//! `_encode_crdt_document` writes, which is what icloud.com accepts for a
//! whole-text replacement.

use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unreadable text document: {0}")]
pub struct DecodeError(&'static str);

/// The text of a `TitleDocument` / `NotesDocument` field value.
pub fn decode(b64: &str) -> Result<String, DecodeError> {
    // Apple's base64 sometimes arrives unpadded.
    let trimmed = b64.trim().trim_end_matches('=');
    let raw = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(trimmed)
        .map_err(|_| DecodeError("not base64"))?;
    let data = inflate(&raw).unwrap_or(raw);
    // Document { version: [Version { data: String }] }, then a bare
    // Version, then a bare String: pyicloud's three fallbacks, in order.
    if let Some(text) = field(&data, 2)
        .and_then(|version| field(version, 3))
        .and_then(text_of)
    {
        return Ok(text);
    }
    if let Some(text) = field(&data, 3).and_then(text_of) {
        return Ok(text);
    }
    text_of(&data).ok_or(DecodeError("no text in the document"))
}

/// The base64 field value for `text`.
pub fn encode(text: &str) -> String {
    // Lengths count UTF-16 code units, as Apple's do.
    let len = text.encode_utf16().count() as u64;
    let char_id = |replica: u64, clock: u64| {
        let mut m = Vec::new();
        put_varint_field(&mut m, 1, replica);
        put_varint_field(&mut m, 2, clock);
        m
    };
    let substring = |replica: u64, clock: u64, length: u64, child: Option<u64>| {
        let mut m = Vec::new();
        put_bytes_field(&mut m, 1, &char_id(replica, clock));
        put_varint_field(&mut m, 2, length);
        put_bytes_field(&mut m, 3, &char_id(replica, clock));
        if let Some(c) = child {
            put_varint_field(&mut m, 5, c);
        }
        m
    };
    const CLOCK_MAX: u64 = 0xFFFF_FFFF;
    // pyicloud's fixed replica UUID for documents it writes.
    const REPLICA: [u8; 16] = [
        0xd4, 0x6b, 0xca, 0xe4, 0x1b, 0x87, 0x66, 0xc1, 0x8d, 0x75, 0xef, 0xe3, 0x5c, 0x91, 0x45, 0xc3,
    ];

    let mut string = Vec::new();
    put_bytes_field(&mut string, 2, text.as_bytes());
    put_bytes_field(&mut string, 3, &substring(0, 0, 0, Some(1)));
    if len > 0 {
        put_bytes_field(&mut string, 3, &substring(1, 0, len, Some(2)));
    }
    put_bytes_field(&mut string, 3, &substring(0, CLOCK_MAX, 0, None));
    let mut clock = Vec::new();
    put_bytes_field(&mut clock, 1, &REPLICA);
    let replica_clock = |c: u64| {
        let mut m = Vec::new();
        put_varint_field(&mut m, 1, c);
        m
    };
    put_bytes_field(&mut clock, 2, &replica_clock(len));
    put_bytes_field(&mut clock, 2, &replica_clock(1));
    let mut timestamp = Vec::new();
    put_bytes_field(&mut timestamp, 1, &clock);
    put_bytes_field(&mut string, 4, &timestamp);
    if len > 0 {
        let mut run = Vec::new();
        put_varint_field(&mut run, 1, len);
        put_bytes_field(&mut string, 5, &run);
    }

    let mut version = Vec::new();
    put_varint_field(&mut version, 1, 0);
    put_varint_field(&mut version, 2, 0);
    put_bytes_field(&mut version, 3, &string);
    let mut document = Vec::new();
    put_varint_field(&mut document, 1, 0);
    put_bytes_field(&mut document, 2, &version);

    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&document).expect("writing to memory");
    STANDARD.encode(z.finish().expect("writing to memory"))
}

fn inflate(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(raw).read_to_end(&mut out).is_ok() {
        return Some(out);
    }
    out.clear();
    flate2::read::GzDecoder::new(raw).read_to_end(&mut out).ok().map(|_| out)
}

/// `topotext.String.string` (field 2) of a serialized String.
fn text_of(string: &[u8]) -> Option<String> {
    let bytes = field(string, 2)?;
    String::from_utf8(bytes.to_vec()).ok()
}

// --- the little protobuf this needs ---

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_varint_field(out: &mut Vec<u8>, number: u64, v: u64) {
    put_varint(out, number << 3);
    put_varint(out, v);
}

fn put_bytes_field(out: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    put_varint(out, (number << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return Some(v);
        }
    }
    None
}

/// The first length-delimited field `number` of a message, or `None` when
/// it is absent or the message does not parse.
fn field(buf: &[u8], number: u64) -> Option<&[u8]> {
    let mut pos = 0;
    while pos < buf.len() {
        let key = varint(buf, &mut pos)?;
        match key & 7 {
            0 => {
                varint(buf, &mut pos)?;
            }
            1 => pos = pos.checked_add(8)?,
            2 => {
                let len = usize::try_from(varint(buf, &mut pos)?).ok()?;
                let end = pos.checked_add(len).filter(|&e| e <= buf.len())?;
                if key >> 3 == number {
                    return Some(&buf[pos..end]);
                }
                pos = end;
            }
            5 => pos = pos.checked_add(4)?,
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_pyicloud_fixture_documents() {
        // timlaing/pyicloud tests/fixtures/reminders: a Version wrapping a
        // String, zlib-compressed.
        assert_eq!(
            decode("eJzjYBCS4GAQYJASEhJwy6woKS1KVQhKzc3MS0ktAgBBdwbW").unwrap(),
            "Fixture Reminder"
        );
        assert_eq!(
            decode("eJzjYBAS5WAQYJDiF+J1y6woKS1KVcjLL0ktBgAuaAXA").unwrap(),
            "Fixture notes"
        );
    }

    #[test]
    fn round_trips_any_text() {
        for text in ["", "Milk", "Call Ånne 📞 at 5", "two\nlines", &"x".repeat(5000)] {
            assert_eq!(decode(&encode(text)).unwrap(), text, "{text:?}");
        }
    }

    #[test]
    fn writes_pyicloud_s_document_byte_for_byte() {
        // The message pyicloud's _encode_crdt_document("Milk") builds,
        // serialized by protoc (`--encode=topotext.String` on its
        // reminders.proto) and wrapped in Version and Document.
        let encoded = STANDARD.decode(encode("Milk")).unwrap();
        let doc = inflate(&encoded).unwrap();
        let hex: String = doc.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            concat!(
                "0800126a080010001a64",
                "12044d696c6b",
                "1a100a040800100010001a04080010002801",
                "1a100a040801100010041a04080110002802",
                "1a160a08080010ffffffff0f10001a08080010ffffffff0f",
                "221c0a1a0a10d46bcae41b8766c18d75efe35c9145c31202080412020801",
                "2a020804",
            )
        );
    }

    #[test]
    fn unpadded_and_gzip_documents_decode() {
        let padded = encode("Eggs");
        assert_eq!(decode(padded.trim_end_matches('=')).unwrap(), "Eggs");
        let doc = inflate(&STANDARD.decode(&padded).unwrap()).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&doc).unwrap();
        assert_eq!(decode(&STANDARD.encode(gz.finish().unwrap())).unwrap(), "Eggs");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode("!!!").is_err());
        assert!(decode(&STANDARD.encode([0xff, 0xff, 0xff])).is_err());
        assert!(decode(&STANDARD.encode([0x12, 0x7f, 0x01])).is_err());
    }
}
