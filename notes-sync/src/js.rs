//! JavaScript semantics the code derived from icloud-md leans on, so it can
//! say `js::trim(..)` where the TypeScript said `.trim()` and mean exactly that: the
//! ECMAScript whitespace set, UTF-16 lengths and slices, Node's `Buffer`
//! base64 and UTF-8, `String.prototype.localeCompare` (ICU root collation,
//! what Node uses), `Date.prototype.toISOString`, `encodeURIComponent`, `typeof`
//! object checks, and `path.posix`.

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_collator::CollatorBorrowed;
use icu_collator::options::CollatorOptions;
use serde_json::Value;

/// ECMAScript `WhiteSpace` + `LineTerminator` (what `\s`, `trim()` and
/// friends match): TAB, VT, FF, ZWNBSP, every `Zs`, LF, CR, LS, PS. Differs
/// from `char::is_whitespace`: includes U+FEFF, excludes U+0085.
pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `is_whitespace` for one UTF-16 code unit (a lone surrogate is not).
pub fn is_whitespace16(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(is_whitespace)
}

/// `isRecord`: `typeof v === "object" && v !== null` (arrays included).
pub(crate) fn is_record(v: &Value) -> bool {
    v.is_object() || v.is_array()
}

/// `String.prototype.trim`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_whitespace)
}

/// `String.prototype.trimEnd`.
pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_whitespace)
}

/// `s` as UTF-16 code units.
pub fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Whether `needle` (as UTF-16) occurs in `haystack` at unit `at`.
pub(crate) fn starts_with_at(haystack: &[u16], at: usize, needle: &str) -> bool {
    let needle = utf16(needle);
    haystack.len() >= at + needle.len() && haystack[at..at + needle.len()] == needle[..]
}

/// `.length`: UTF-16 code units.
pub fn len16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// UTF-16 units back to a `String`; a lone surrogate (which a JS slice can
/// produce) becomes U+FFFD, exactly what `Buffer.from(str, "utf-8")` and
/// `TextEncoder` write for it.
pub fn from_utf16(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `s.slice(start, end)` in UTF-16 units (non-negative indexes, clamped like
/// JS). A cut through a surrogate pair yields U+FFFD for the orphaned half.
pub fn slice16(s: &str, start: usize, end: usize) -> String {
    let units = utf16(s);
    let end = end.min(units.len());
    let start = start.min(end);
    from_utf16(&units[start..end])
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

fn collator() -> &'static CollatorBorrowed<'static> {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR.get_or_init(|| {
        CollatorBorrowed::try_new(Default::default(), CollatorOptions::default())
            .expect("root collation data is compiled in")
    })
}

/// `a.localeCompare(b)` with Node's default (ICU root, tertiary strength).
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    collator().compare(a, b)
}

/// `new Date(ms).toISOString()`: `YYYY-MM-DDTHH:mm:ss.sssZ` (years 0-9999).
pub fn iso_string(ms: i64) -> String {
    icloud_session::time::rfc3339_millis(ms)
}

/// `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `path.posix` - the subset icloud-md uses on vault-relative paths.
pub mod posix {
    /// `path.posix.basename(p)`.
    pub fn basename(p: &str) -> &str {
        let trimmed = p.trim_end_matches('/');
        if trimmed.is_empty() {
            return "";
        }
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }

    /// `path.posix.dirname(p)`.
    pub fn dirname(p: &str) -> String {
        if p.is_empty() {
            return ".".into();
        }
        let absolute = p.starts_with('/');
        let trimmed = p.trim_end_matches('/');
        if trimmed.is_empty() {
            return "/".into();
        }
        match trimmed.rfind('/') {
            None => ".".into(),
            Some(i) => {
                let head = trimmed[..i].trim_end_matches('/');
                if head.is_empty() {
                    if absolute { "/".into() } else { ".".into() }
                } else {
                    head.into()
                }
            }
        }
    }

    /// `path.posix.normalize(p)`.
    pub fn normalize(p: &str) -> String {
        if p.is_empty() {
            return ".".into();
        }
        let absolute = p.starts_with('/');
        let trailing = p.ends_with('/');
        let mut parts: Vec<&str> = Vec::new();
        for seg in p.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    if matches!(parts.last(), Some(last) if *last != "..") {
                        parts.pop();
                    } else if !absolute {
                        parts.push("..");
                    }
                }
                other => parts.push(other),
            }
        }
        let mut out = parts.join("/");
        if absolute {
            out.insert(0, '/');
        }
        if out.is_empty() {
            return if absolute { "/".into() } else { ".".into() };
        }
        if trailing && !out.ends_with('/') {
            out.push('/');
        }
        out
    }

    /// `path.posix.join(a, b, ...)`.
    pub fn join(parts: &[&str]) -> String {
        let joined: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
        if joined.is_empty() {
            return ".".into();
        }
        normalize(&joined.join("/"))
    }

    /// `path.posix.relative(from, to)` for relative (vault-rooted) paths.
    pub fn relative(from: &str, to: &str) -> String {
        let from = normalize(from);
        let to = normalize(to);
        let split = |p: &str| -> Vec<String> {
            if p == "." {
                Vec::new()
            } else {
                p.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect()
            }
        };
        let (f, t) = (split(&from), split(&to));
        let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
        let mut out: Vec<String> = std::iter::repeat_n("..".to_string(), f.len() - common).collect();
        out.extend(t[common..].iter().cloned());
        out.join("/")
    }

    /// `path.posix.basename(path, suffix)`, byte for byte Node's algorithm.
    pub fn basename_suffix(path: &str, suffix: &str) -> String {
        let p = path.as_bytes();
        let x = suffix.as_bytes();
        let mut start = 0usize;
        let mut end: isize = -1;
        let mut matched_slash = true;
        if !x.is_empty() && x.len() <= p.len() {
            if suffix == path {
                return String::new();
            }
            let mut ext_idx = x.len() as isize - 1;
            let mut first_non_slash_end: isize = -1;
            let mut i = p.len() as isize - 1;
            while i >= 0 {
                let code = p[i as usize];
                if code == b'/' {
                    if !matched_slash {
                        start = i as usize + 1;
                        break;
                    }
                } else {
                    if first_non_slash_end == -1 {
                        matched_slash = false;
                        first_non_slash_end = i + 1;
                    }
                    if ext_idx >= 0 {
                        if code == x[ext_idx as usize] {
                            ext_idx -= 1;
                            if ext_idx == -1 {
                                end = i;
                            }
                        } else {
                            ext_idx = -1;
                            end = first_non_slash_end;
                        }
                    }
                }
                i -= 1;
            }
            if start as isize == end {
                end = first_non_slash_end;
            } else if end == -1 {
                end = p.len() as isize;
            }
            return lossy_bytes(&p[start..(end.max(start as isize) as usize)]);
        }
        let mut i = p.len() as isize - 1;
        while i >= 0 {
            if p[i as usize] == b'/' {
                if !matched_slash {
                    start = i as usize + 1;
                    break;
                }
            } else if end == -1 {
                matched_slash = false;
                end = i + 1;
            }
            i -= 1;
        }
        if end == -1 {
            return String::new();
        }
        lossy_bytes(&p[start..end as usize])
    }

    fn lossy_bytes(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// `path.posix.extname(path)`.
    pub fn extname(path: &str) -> String {
        let p = path.as_bytes();
        let mut start_dot: isize = -1;
        let mut start_part: isize = 0;
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut pre_dot_state = 0;
        let mut i = p.len() as isize - 1;
        while i >= 0 {
            let code = p[i as usize];
            if code == b'/' {
                if !matched_slash {
                    start_part = i + 1;
                    break;
                }
                i -= 1;
                continue;
            }
            if end == -1 {
                matched_slash = false;
                end = i + 1;
            }
            if code == b'.' {
                if start_dot == -1 {
                    start_dot = i;
                } else if pre_dot_state != 1 {
                    pre_dot_state = 1;
                }
            } else if start_dot != -1 {
                pre_dot_state = -1;
            }
            i -= 1;
        }
        if start_dot == -1
            || end == -1
            || pre_dot_state == 0
            || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
        {
            return String::new();
        }
        lossy_bytes(&p[start_dot as usize..end as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_strings() {
        assert_eq!(iso_string(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_string(1_790_000_000_000), "2026-09-21T14:13:20.000Z");
        assert_eq!(iso_string(1_752_564_000_123), "2025-07-15T07:20:00.123Z");
    }

    #[test]
    fn utf16() {
        assert_eq!(len16("a😀"), 3);
        assert_eq!(slice16("a😀b", 0, 2), "a\u{FFFD}");
        assert_eq!(slice16("abcdef", 2, 4), "cd");
    }

    #[test]
    fn whitespace() {
        assert_eq!(trim("\u{FEFF} x \u{3000}"), "x");
        assert_eq!(trim("\u{85}x"), "\u{85}x");
    }

    #[test]
    fn collation_matches_node() {
        use std::cmp::Ordering::*;
        assert_eq!(locale_compare("apple", "Banana"), Less);
        assert_eq!(locale_compare("a", "A"), Less);
        assert_eq!(locale_compare("Recipes", "recipes"), Greater);
        assert_eq!(locale_compare("same", "same"), Equal);
        assert_eq!(locale_compare("_a", "a"), Less);
    }

    #[test]
    fn uri_component() {
        assert_eq!(encode_uri_component("a b/ç.jpeg"), "a%20b%2F%C3%A7.jpeg");
    }

    #[test]
    fn posix_paths() {
        use posix::*;
        assert_eq!(basename("Notes/a.md"), "a.md");
        assert_eq!(dirname("Notes/a.md"), "Notes");
        assert_eq!(dirname("a.md"), ".");
        assert_eq!(join(&["", "a.md"]), "a.md");
        assert_eq!(join(&["Notes", "attachments", "x"]), "Notes/attachments/x");
        assert_eq!(relative("Notes", "Notes/attachments/x"), "attachments/x");
        assert_eq!(relative("A/B", "C/x"), "../../C/x");
        assert_eq!(relative("", "attachments/x"), "attachments/x");
    }

    #[test]
    fn basename_matches_node() {
        for (path, want) in [
            (".md", ""),
            ("a/.md", ".md"),
            ("x.md", "x"),
            ("a/b/c.md/", "c"),
            ("foo.MD", "foo.MD"),
            ("/", ""),
            ("", ""),
            ("a.md.md", "a.md"),
            ("dir/", "dir"),
            ("x", "x"),
        ] {
            assert_eq!(posix::basename_suffix(path, ".md"), want, "{path}");
        }
        for (path, want) in [
            (".md", ""),
            ("x.md", ".md"),
            ("a/b/c.md/", ".md"),
            ("foo.MD", ".MD"),
            ("x", ""),
            ("..md", ".md"),
        ] {
            assert_eq!(posix::extname(path), want, "{path}");
        }
    }
}
