//! The Qt app's string rules, where its behaviour shows them: lengths and
//! cuts count UTF-16 units, and a digit is any decimal digit of the Basic
//! Multilingual Plane.

use icu_properties::CodePointMapData;
use icu_properties::props::GeneralCategory;

pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The first `n` UTF-16 units of `s`. A cut through a character outside the
/// Basic Multilingual Plane leaves half of it, which prints as U+FFFD.
pub fn left_utf16(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in s.chars() {
        if units + c.len_utf16() > n {
            if units < n {
                out.push(char::REPLACEMENT_CHARACTER);
            }
            break;
        }
        units += c.len_utf16();
        out.push(c);
    }
    out
}

/// A decimal digit, in any script of the Basic Multilingual Plane.
pub fn is_digit(c: char) -> bool {
    (c as u32) <= 0xffff && CodePointMapData::<GeneralCategory>::new().get(c) == GeneralCategory::DecimalNumber
}

/// The path's last component and what is before it ("" for a bare name).
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(slash) => (&path[..slash], &path[slash + 1..]),
        None => ("", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_counts_utf16_units() {
        assert_eq!(left_utf16("abc", 2), "ab");
        assert_eq!(left_utf16("abc", 9), "abc");
        assert_eq!(left_utf16("a😀b", 3), "a😀");
        assert_eq!(left_utf16("a😀b", 2), "a\u{fffd}");
        assert_eq!(utf16_len("a😀"), 3);
    }

    #[test]
    fn a_digit_is_any_decimal_digit() {
        assert!(is_digit('7') && is_digit('٣') && !is_digit('x') && !is_digit('½'));
    }

    #[test]
    fn paths_split_at_the_last_slash() {
        assert_eq!(split_path("a/b/c.md"), ("a/b", "c.md"));
        assert_eq!(split_path("c.md"), ("", "c.md"));
    }
}
