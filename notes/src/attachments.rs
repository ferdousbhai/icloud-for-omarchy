//! The files a note links to under `attachments/`. The sync engine keeps one
//! attachments/ directory per folder, shared by every note in it and named
//! after the attachment, and always rewrites each attachment into the note
//! text as a link (an embed for images), so the links are the full list.
//! The links themselves must stay as written: each stands for its
//! attachment, and one gone from the text removes the attachment at the
//! next push.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::files::absolute_clean;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub path: PathBuf,
    /// A picture the window shows inline.
    pub image: bool,
}

/// A link to an attachment in a note's text: where it is (bytes) and its
/// target as written, `attachments/...`, percent-encoded.
struct Link<'a> {
    start: usize,
    end: usize,
    target: &'a str,
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// A parenthesised stretch of a link's target starting at `at` (one more
/// may sit inside it): where it ends.
fn group_end(text: &str, at: usize, nested: bool) -> Option<usize> {
    let mut i = at + 1;
    loop {
        match text[i..].chars().next()? {
            ')' => return Some(i + 1),
            '(' if !nested => i = group_end(text, i, true)?,
            c if c == '(' || is_space(c) => return None,
            c => i += c.len_utf8(),
        }
    }
}

/// The link starting at `at`, if one does: `[text](attachments/...)`, or an
/// embed with `!` before it. The text may hold escaped brackets; the target
/// holds no space and only parentheses that pair up.
fn link_at(text: &str, at: usize) -> Option<Link<'_>> {
    let mut i = at + usize::from(text[at..].starts_with('!'));
    if !text[i..].starts_with('[') {
        return None;
    }
    i += 1;
    let mut chars = text[i..].char_indices();
    let close = loop {
        let (offset, c) = chars.next()?;
        match c {
            ']' => break i + offset,
            '\n' => return None,
            // A backslash escapes what follows, on the same line.
            '\\' => {
                chars.next().filter(|&(_, escaped)| escaped != '\n')?;
            }
            _ => {}
        }
    };
    let target_start = close + text[close..].strip_prefix("](attachments/").map(|_| 2)?;
    let mut end = target_start + "attachments/".len();
    let first = end;
    loop {
        match text[end..].chars().next() {
            Some('(') => match group_end(text, end, false) {
                Some(after) => end = after,
                None => break,
            },
            Some(c) if c != ')' && !is_space(c) => end += c.len_utf8(),
            _ => break,
        }
    }
    (end > first && text[end..].starts_with(')')).then(|| Link {
        start: at,
        end: end + 1,
        target: &text[target_start..end],
    })
}

fn links(text: &str) -> Vec<Link<'_>> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        match link_at(text, at) {
            Some(link) => {
                at = link.end;
                found.push(link);
            }
            None => at += c.len_utf8(),
        }
    }
    found
}

/// Percent-decodes `text`; bytes that decode to no UTF-8 stay encoded.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = String::new();
    let mut run = Vec::new(); // the bytes of consecutive %XX
    let flush = |run: &mut Vec<u8>, out: &mut String| {
        let mut rest = run.as_slice();
        while !rest.is_empty() {
            let (valid, bad) = match std::str::from_utf8(rest) {
                Ok(valid) => (valid, &rest[rest.len()..]),
                Err(e) => (
                    std::str::from_utf8(&rest[..e.valid_up_to()]).expect("valid"),
                    &rest[e.valid_up_to()..],
                ),
            };
            out.push_str(valid);
            if let Some(byte) = bad.first() {
                out.push_str(&format!("%{byte:02X}"));
            }
            rest = bad.get(1..).unwrap_or_default();
        }
        run.clear();
    };
    let mut i = 0;
    while i < bytes.len() {
        let pair = (bytes[i] == b'%')
            .then(|| Some((hex(*bytes.get(i + 1)?)?, hex(*bytes.get(i + 2)?)?)))
            .flatten();
        match pair {
            Some((high, low)) => {
                run.push(high * 16 + low);
                i += 3;
            }
            None => {
                flush(&mut run, &mut out);
                let c = text[i..].chars().next().expect("a character boundary");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The path a link's target names: without a query or fragment, decoded.
fn target_path(target: &str) -> String {
    let path = target.split(['#', '?']).next().unwrap_or_default();
    percent_decoded(path)
}

/// The attachments `text` links, each once, for the note at `note_path`.
/// Only files really inside the folder's attachments/ directory: a link's
/// `..` or a symlink there could otherwise point the preview (and `read`)
/// anywhere.
pub fn attachments_for(note_path: &Path, text: &str) -> Vec<Attachment> {
    const IMAGES: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "svg"];
    let Some(dir) = note_path.parent() else {
        return Vec::new();
    };
    let Ok(inside) = fs::canonicalize(dir.join("attachments")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for link in links(text) {
        let path = absolute_clean(&dir.join(target_path(link.target)));
        let real = fs::canonicalize(&path).ok();
        // Inside it, not the directory itself.
        if !real.is_some_and(|real| real != inside && real.starts_with(&inside)) || !seen.insert(path.clone()) {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let image = name
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| IMAGES.contains(&suffix.to_lowercase().as_str()));
        found.push(Attachment { name, path, image });
    }
    found
}

/// `body` without the link(s) to the attachment `name`: with the whole line
/// when nothing else is on it.
pub fn without_attachment_link(body: &str, name: &str) -> String {
    let target = format!("attachments/{name}");
    let mut out = String::new();
    let mut from = 0;
    for link in links(body) {
        // Links are URL-encoded.
        let plain = String::from_utf8_lossy(&percent_bytes(link.target)).into_owned();
        if target_path(link.target) != target && plain != target {
            continue;
        }
        let (mut start, mut end) = (link.start, link.end);
        // Alone on its line: the line goes too.
        let line_start = body[..start].rfind('\n').map_or(0, |newline| newline + 1);
        let line_end = body[end..].find('\n').map_or(body.len(), |newline| end + newline);
        if body[line_start..start].trim().is_empty() && body[end..line_end].trim().is_empty() {
            start = line_start;
            end = (line_end + 1).min(body.len());
        }
        if start < from {
            continue;
        }
        out += &body[from..start];
        from = end;
    }
    if from == 0 {
        body.to_string()
    } else {
        out + &body[from..]
    }
}

/// `text` with every `%XX` as the byte it names.
fn percent_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let pair = (bytes[i] == b'%')
            .then(|| Some((hex(*bytes.get(i + 1)?)?, hex(*bytes.get(i + 2)?)?)))
            .flatten();
        match pair {
            Some((high, low)) => {
                out.push(high * 16 + low);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    out
}

/// The link the sync engine writes for an attachment file, and so the one
/// it takes for a file to add (notes-sync `format_attachment_markdown`): an
/// embed for the image types it knows, the file's name as the link text with
/// its brackets and backslashes escaped, and the path percent-encoded as
/// JavaScript's encodeURIComponent does.
pub fn attachment_link(name: &str) -> String {
    const EMBEDDED: [&str; 8] = ["png", "jpg", "jpeg", "heic", "tiff", "tif", "gif", "webp"];
    let mut text = String::new();
    for c in name.chars() {
        if matches!(c, '[' | ']' | '\\') {
            text.push('\\');
        }
        text.push(c);
    }
    let image = name
        .rfind('.')
        .is_some_and(|dot| dot > 0 && EMBEDDED.contains(&name[dot + 1..].to_lowercase().as_str()));
    // Parentheses that pair up stay as they are ("photo (1).png"); ones
    // that don't would end the link early, so they are encoded.
    let mut depth = 0i32;
    let mut paired = true;
    for c in name.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                paired &= depth >= 0;
            }
            _ => {}
        }
    }
    paired &= depth == 0;
    let mut encoded = String::new();
    for &byte in name.as_bytes() {
        let kept = byte.is_ascii_alphanumeric() || b"-._~!*'".contains(&byte) || (paired && b"()".contains(&byte));
        if kept {
            encoded.push(byte as char);
        } else {
            encoded += &format!("%{byte:02X}");
        }
    }
    format!("{}[{text}](attachments/{encoded})", if image { "!" } else { "" })
}

/// Whether a file's name can be an attachment's: one plain name, as the
/// engine wants it (no control characters, at most 190 bytes).
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 190 && name.chars().all(|c| (c as u32) >= 0x20 && c != '\u{7f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(text: &str) -> Vec<&str> {
        links(text).into_iter().map(|link| link.target).collect()
    }

    #[test]
    fn links_are_found_as_the_engine_writes_them() {
        assert_eq!(
            targets("a [one](attachments/a.pdf) ![two](attachments/b%20c.png)\n[x](other/a.pdf) [y](attachments/)"),
            ["attachments/a.pdf", "attachments/b%20c.png"]
        );
        // Escaped brackets in the text, paired parentheses in the target.
        assert_eq!(
            targets(r"[a\[b\].pdf](attachments/a%5Bb%5D.pdf) ![r (1).png](attachments/r%20(1).png)"),
            ["attachments/a%5Bb%5D.pdf", "attachments/r%20(1).png"]
        );
        assert_eq!(targets("[n](attachments/a((b)c).x)"), ["attachments/a((b)c).x"]);
        // Not links: a space or an unpaired parenthesis in the target, a line
        // end in the text, three levels of parentheses.
        assert_eq!(targets("[n](attachments/a b)"), [] as [&str; 0]);
        assert_eq!(targets("[n\n](attachments/a)"), [] as [&str; 0]);
        assert_eq!(targets("[n](attachments/a(b)"), [] as [&str; 0]);
        assert_eq!(targets("[n](attachments/a(((b))))"), [] as [&str; 0]);
        // The first bracket that can open a link does.
        let both = links("[a [b](attachments/x)");
        assert_eq!((both[0].start, both[0].target), (0, "attachments/x"));
    }

    #[test]
    fn a_target_names_a_decoded_path() {
        assert_eq!(target_path("attachments/r%20(1).png"), "attachments/r (1).png");
        assert_eq!(target_path("attachments/d%C3%A9.png?x#y"), "attachments/dé.png");
        assert_eq!(target_path("attachments/100%.png"), "attachments/100%.png");
        assert_eq!(target_path("attachments/%FF%41"), "attachments/%FFA");
    }

    #[test]
    fn the_link_for_a_file_is_the_engines() {
        assert_eq!(
            attachment_link("receipt (1).png"),
            "![receipt (1).png](attachments/receipt%20(1).png)"
        );
        assert_eq!(attachment_link("a[b].pdf"), r"[a\[b\].pdf](attachments/a%5Bb%5D.pdf)");
        assert_eq!(attachment_link("odd).pdf"), "[odd).pdf](attachments/odd%29.pdf)");
        assert_eq!(
            attachment_link("é it's.HEIC"),
            "![é it's.HEIC](attachments/%C3%A9%20it's.HEIC)"
        );
        assert_eq!(attachment_link(".png"), "[.png](attachments/.png)");
    }

    #[test]
    fn a_link_is_cut_with_its_line_when_alone_on_it() {
        let body = "# T\n![p](attachments/p.png)\ntext [d](attachments/d%20e.pdf) more\n";
        assert_eq!(
            without_attachment_link(body, "p.png"),
            "# T\ntext [d](attachments/d%20e.pdf) more\n"
        );
        assert_eq!(
            without_attachment_link(body, "d e.pdf"),
            "# T\n![p](attachments/p.png)\ntext  more\n"
        );
        assert_eq!(without_attachment_link(body, "none.pdf"), body);
        assert_eq!(without_attachment_link("[p](attachments/p.png)", "p.png"), "");
    }

    #[test]
    fn only_files_inside_the_attachments_directory_count() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Work");
        fs::create_dir_all(folder.join("attachments")).unwrap();
        fs::write(folder.join("attachments/pic.PNG"), "x").unwrap();
        fs::write(folder.join("attachments/doc.pdf"), "x").unwrap();
        fs::write(folder.join("secret.txt"), "x").unwrap();
        let text = "![a](attachments/pic.PNG) [b](attachments/doc.pdf) [again](attachments/doc.pdf) \
                    [gone](attachments/gone.pdf) [out](attachments/../secret.txt)";
        let found = attachments_for(&folder.join("Note.md"), text);
        let names: Vec<(&str, bool)> = found.iter().map(|a| (a.name.as_str(), a.image)).collect();
        assert_eq!(names, [("pic.PNG", true), ("doc.pdf", false)]);
        assert_eq!(found[0].path, folder.join("attachments/pic.PNG"));
    }
}
