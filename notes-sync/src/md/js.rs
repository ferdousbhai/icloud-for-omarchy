//! Small JavaScript-semantics helpers the ports lean on: JS's whitespace set
//! (`\s`, `trim`), UTF-16 lengths and slices, and Node's `path.posix`
//! `basename`/`extname`.

/// JS `\s` / `String.prototype.trim` whitespace (WhiteSpace + LineTerminator).
/// Differs from `char::is_whitespace`: includes U+FEFF, excludes U+0085.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `String.prototype.trimEnd`.
pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_whitespace)
}

/// `.length`: UTF-16 code units.
pub fn len16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.slice(start, end)` in UTF-16 units. A cut through a surrogate pair
/// yields U+FFFD for the lone half (what Node writes to disk for one).
pub fn slice16(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
}

/// Byte offset of UTF-16 offset `at` (clamped; rounds a cut inside a
/// surrogate pair up to the end of the pair).
pub fn byte_at16(s: &str, at: usize) -> usize {
    let mut units = 0;
    for (byte, c) in s.char_indices() {
        if units >= at {
            return byte;
        }
        units += c.len_utf16();
    }
    s.len()
}

/// Node `path.posix.basename(path, suffix)`.
pub fn basename(path: &str, suffix: &str) -> String {
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

/// Node `path.posix.extname(path)`.
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

#[cfg(test)]
mod tests {
    use super::*;

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
            assert_eq!(basename(path, ".md"), want, "{path}");
        }
        for (path, want) in [
            (".md", ""),
            ("x.md", ".md"),
            ("a/b/c.md/", ".md"),
            ("foo.MD", ".MD"),
            ("x", ""),
            ("..md", ".md"),
        ] {
            assert_eq!(extname(path), want, "{path}");
        }
    }
}
