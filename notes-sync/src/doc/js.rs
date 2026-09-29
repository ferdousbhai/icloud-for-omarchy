//! Small JavaScript-semantics helpers: icloud-md works on JS strings, whose
//! indexes and lengths are UTF-16 code units, and on Node's `Buffer` base64.

/// `s` as UTF-16 code units.
pub fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `s.length`.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 units back to a `String`; a lone surrogate (which a JS slice can
/// produce) becomes U+FFFD, exactly what `Buffer.from(str, "utf-8")` and
/// `TextEncoder` write for it.
pub fn from_utf16(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `str.slice(start, end)` in UTF-16 units (clamped like JS).
pub fn slice16(s: &str, start: usize, end: usize) -> String {
    let units = utf16(s);
    let end = end.min(units.len());
    let start = start.min(end);
    from_utf16(&units[start..end])
}

/// ECMAScript `\s` for one UTF-16 code unit (WhiteSpace + LineTerminator).
pub fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x09 | 0x0a | 0x0b | 0x0c | 0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `Buffer.from(bytes).toString("base64")`.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `Buffer.from(s, "base64")`: lenient like Node - both the standard and the
/// URL-safe alphabet, characters outside them skipped, decoding stops at the
/// first `=`, and a dangling single character is dropped.
pub fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in s.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// `Buffer.from(bytes).toString("utf-8")`: lossy, BOM kept.
pub fn buffer_to_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
