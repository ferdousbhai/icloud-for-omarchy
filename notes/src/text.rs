//! The Qt app's string rules, where its behaviour shows them: lengths and
//! cuts count UTF-16 units, comparing "ignoring case" folds each character,
//! and a digit is any decimal digit of the Basic Multilingual Plane.

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

/// `c` with its case folded away, one character for one (Unicode's simple
/// case folding, from the standard library's mappings: through upper case,
/// so that "ſ" meets "s" and "ς" meets "σ").
fn fold(c: char) -> char {
    // The dotless i has an upper case but folds only in Turkic languages.
    if c == 'ı' {
        return c;
    }
    let single = |mut mapped: std::char::ToUppercase| match (mapped.next(), mapped.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    };
    let upper = single(c.to_uppercase()).unwrap_or(c);
    let mut lower = upper.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => upper,
    }
}

pub fn folded(s: &str) -> Vec<char> {
    s.chars().map(fold).collect()
}

pub fn eq_ignoring_case(a: &str, b: &str) -> bool {
    a.chars().map(fold).eq(b.chars().map(fold))
}

/// Whether `haystack` (already [`folded`]) holds `needle` (also folded).
pub fn folded_contains(haystack: &[char], needle: &[char]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
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

/// `folder/name`, or `name` in the vault root.
pub fn join_path(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
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
    fn case_is_folded_a_character_at_a_time() {
        assert!(eq_ignoring_case("Straße", "STRAßE"));
        assert!(!eq_ignoring_case("Straße", "STRASSE"));
        assert!(eq_ignoring_case("ſ", "S"));
        assert!(eq_ignoring_case("ὈΔΥΣΣΕΎΣ", "ὀδυσσεύς"));
        assert!(!eq_ignoring_case("ı", "I"));
        assert!(eq_ignoring_case("İ", "İ"));
        assert!(folded_contains(&folded("Hello Wörld"), &folded("WÖR")));
        assert!(!folded_contains(&folded("Hello"), &folded("xyz")));
    }

    #[test]
    fn a_digit_is_any_decimal_digit() {
        assert!(is_digit('7') && is_digit('٣') && !is_digit('x') && !is_digit('½'));
    }

    #[test]
    fn paths_split_at_the_last_slash() {
        assert_eq!(split_path("a/b/c.md"), ("a/b", "c.md"));
        assert_eq!(split_path("c.md"), ("", "c.md"));
        assert_eq!(join_path("", "c.md"), "c.md");
        assert_eq!(join_path("a", "c.md"), "a/c.md");
    }
}
