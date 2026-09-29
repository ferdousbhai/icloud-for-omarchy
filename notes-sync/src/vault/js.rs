//! The handful of JavaScript semantics icloud-md's vault code leans on, so
//! the ports below can say `js::trim(..)` where the TypeScript says
//! `.trim()` and mean exactly that: UTF-16 string lengths and slices, the
//! ECMAScript whitespace set, `String.prototype.localeCompare` (ICU root
//! collation, what Node uses), `Date.prototype.toISOString`,
//! `encodeURIComponent`, and `path.posix`. Owner: workstream D.

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_collator::CollatorBorrowed;
use icu_collator::options::CollatorOptions;

/// ECMAScript `WhiteSpace` + `LineTerminator` (what `\s`, `trim()` and
/// friends match): TAB, VT, FF, ZWNBSP, every `Zs`, LF, CR, LS, PS.
pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_whitespace)
}

/// `String.prototype.trimEnd`.
pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_whitespace)
}

/// `.length`: UTF-16 code units.
pub fn len16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `.slice(start, end)` in UTF-16 units (non-negative indexes, clamped). A
/// cut through a surrogate pair yields U+FFFD for the orphaned half - what
/// Node writes for a lone surrogate when the string reaches a UTF-8 file.
pub fn slice16(s: &str, start: usize, end: Option<usize>) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.unwrap_or(units.len()).min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
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
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let (h, m, s, milli) = (rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{milli:03}Z")
}

/// Howard Hinnant's days-since-epoch → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
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
        assert_eq!(slice16("a😀b", 0, Some(2)), "a\u{FFFD}");
        assert_eq!(slice16("abcdef", 2, Some(4)), "cd");
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
}
