//! The frontmatter envelope and the keys this tool owns in it. Ports icloud-md
//! `src/notes/frontmatter.ts` and `noteIdFrontmatter.ts`. Owner: workstream C.
//!
//! Plan deviation: the plan's `with_note_id(body, id)` is icloud-md's
//! `composeNoteFile(frontmatter, body, recordName, unrepresentableTitle)`;
//! every vault's note files are exactly `---\napple-note-id: <ID>\n---\n\n`
//! + body when the file had no other frontmatter.
//!
//! icloud-md edits the YAML with the `yaml` package's Document API, which
//! re-serializes the whole block on any change. `super::yaml` reproduces
//! that byte for byte on the YAML subset it models; for YAML outside it
//! (comments, block scalars, anchors, ...) this falls back to editing just
//! the one `key: value` line, which is where the output can differ from
//! icloud-md's (never in what the keys read back as).

use super::js;
use super::yaml::{self, Document, Parsed, Scalar, Style, Value};

pub const FENCE: &str = "---";
pub const NOTE_ID_KEY: &str = "apple-note-id";
pub const NOTE_TITLE_KEY: &str = "apple-note-title";

/// `SplitMarkdown`. Invariant: `frontmatter + body` is the original text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// The `---` fences, YAML, and following blank lines, verbatim; empty
    /// when the file has none.
    pub frontmatter: String,
    pub body: String,
}

/// `SplitFrontmatterOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SplitOptions {
    /// Filename-as-title vault: the body may itself open with `---` or blank
    /// lines.
    pub filename_as_title: bool,
}

/// `splitFrontmatter`.
pub fn split_frontmatter(text: &str, options: SplitOptions) -> Envelope {
    let lines: Vec<&str> = text.split('\n').collect();
    let whole = || Envelope {
        frontmatter: String::new(),
        body: text.to_string(),
    };
    if lines[0] != FENCE {
        return whole();
    }
    if options.filename_as_title && matches!(lines.get(1), Some(&FENCE) | Some(&"") | None) {
        return whole();
    }
    let Some(closing) = lines.iter().skip(1).position(|l| *l == FENCE).map(|i| i + 1) else {
        return whole();
    };
    let mut envelope_end = closing;
    if options.filename_as_title {
        if lines.get(envelope_end + 1) == Some(&"") {
            envelope_end += 1;
        }
    } else {
        while envelope_end + 1 < lines.len() && lines[envelope_end + 1].is_empty() {
            envelope_end += 1;
        }
    }
    let body_lines = &lines[envelope_end + 1..];
    let envelope = lines[..=envelope_end].join("\n");
    let frontmatter = if body_lines.is_empty() {
        envelope
    } else {
        envelope + "\n"
    };
    Envelope {
        frontmatter,
        body: body_lines.join("\n"),
    }
}

/// `joinFrontmatter`.
pub fn join_frontmatter(frontmatter: &str, body: &str) -> String {
    format!("{frontmatter}{body}")
}

/// `isNoteId`: UUID-shaped, either case.
pub fn is_note_id(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The envelope's YAML, parsed: `None` when there is no envelope or yaml
/// would reject it (or it isn't a mapping).
enum Frontmatter {
    Doc(Document),
    /// Outside `yaml`'s modelled subset: the envelope's YAML lines.
    Lines(Vec<String>),
}

/// `envelopeBody`.
fn envelope_body(frontmatter: &str) -> Option<String> {
    let lines: Vec<&str> = frontmatter.split('\n').collect();
    if lines[0] != FENCE {
        return None;
    }
    let closing = lines.iter().skip(1).position(|l| *l == FENCE)? + 1;
    Some(lines[1..closing].join("\n"))
}

/// `parseFrontmatterDocument`.
fn parse_frontmatter(frontmatter: &str) -> Option<Frontmatter> {
    let body = envelope_body(frontmatter)?;
    match yaml::parse_document(&body) {
        Parsed::Ok(doc) => Some(Frontmatter::Doc(doc)),
        Parsed::Invalid => None,
        Parsed::Unsupported => {
            if fallback_is_mapping(&body) {
                Some(Frontmatter::Lines(body.split('\n').map(str::to_string).collect()))
            } else {
                None
            }
        }
    }
}

/// `reassemble`.
fn reassemble(original: &str, yaml_text: &str) -> String {
    let lines: Vec<&str> = original.split('\n').collect();
    let closing = lines.iter().skip(1).position(|l| *l == FENCE).map(|i| i + 1);
    let trailer = closing.map_or(String::new(), |c| lines[c + 1..].join("\n"));
    let yaml_text = yaml_text.strip_suffix('\n').unwrap_or(yaml_text);
    format!("{FENCE}\n{yaml_text}\n{FENCE}\n{trailer}")
}

fn read_string(frontmatter: &str, key: &str) -> Option<String> {
    match parse_frontmatter(frontmatter)? {
        Frontmatter::Doc(doc) => doc.get_str(key).map(str::to_string),
        Frontmatter::Lines(lines) => fallback_read(&lines, key),
    }
}

/// `readNoteId`: total - broken YAML, missing key, non-UUID all read `None`.
pub fn read_note_id(frontmatter: &str) -> Option<String> {
    read_string(frontmatter, NOTE_ID_KEY).filter(|v| is_note_id(v))
}

/// `setNoteId`.
pub fn set_note_id(frontmatter: &str, id: &str) -> String {
    if !is_note_id(id) || read_note_id(frontmatter).as_deref() == Some(id) {
        return frontmatter.to_string();
    }
    if js::trim(frontmatter).is_empty() {
        return format!("---\n{NOTE_ID_KEY}: {id}\n---\n\n");
    }
    set_key(frontmatter, NOTE_ID_KEY, id)
}

fn set_key(frontmatter: &str, key: &str, value: &str) -> String {
    match parse_frontmatter(frontmatter) {
        None => frontmatter.to_string(),
        Some(Frontmatter::Doc(mut doc)) => {
            doc.set_str(key, value);
            reassemble(frontmatter, &yaml::stringify_document(&doc))
        }
        Some(Frontmatter::Lines(lines)) => reassemble(frontmatter, &fallback_set(lines, key, value)),
    }
}

fn clear_key(frontmatter: &str, key: &str) -> String {
    match parse_frontmatter(frontmatter) {
        None => frontmatter.to_string(),
        Some(Frontmatter::Doc(mut doc)) => {
            if !doc.has(key) {
                return frontmatter.to_string();
            }
            doc.delete(key);
            let text = yaml::stringify_document(&doc);
            let trimmed = js::trim(&text);
            if trimmed == "{}" || trimmed.is_empty() {
                return String::new();
            }
            reassemble(frontmatter, &text)
        }
        Some(Frontmatter::Lines(lines)) => {
            if fallback_key_line(&lines, key).is_none() {
                return frontmatter.to_string();
            }
            let remaining = fallback_clear(lines, key);
            if remaining
                .iter()
                .all(|l| js::trim(l).is_empty() || js::trim(l).starts_with('#'))
            {
                return String::new();
            }
            reassemble(frontmatter, &remaining.join("\n"))
        }
    }
}

/// `clearNoteId`.
pub fn clear_note_id(frontmatter: &str) -> String {
    clear_key(frontmatter, NOTE_ID_KEY)
}

/// `readNoteTitle`.
pub fn read_note_title(frontmatter: &str) -> Option<String> {
    read_string(frontmatter, NOTE_TITLE_KEY).filter(|v| !v.is_empty())
}

/// `setNoteTitle`.
pub fn set_note_title(frontmatter: &str, title: &str) -> String {
    if read_note_title(frontmatter).as_deref() == Some(title) {
        return frontmatter.to_string();
    }
    if js::trim(frontmatter).is_empty() {
        return format!(
            "---\n{NOTE_TITLE_KEY}: {}\n---\n\n",
            serde_json::to_string(title).unwrap()
        );
    }
    set_key(frontmatter, NOTE_TITLE_KEY, title)
}

/// `clearNoteTitle`.
pub fn clear_note_title(frontmatter: &str) -> String {
    clear_key(frontmatter, NOTE_TITLE_KEY)
}

/// `composeNoteFile`: stamp the id, set or clear `apple-note-title`, join.
pub fn compose_note_file(
    frontmatter: &str,
    body: &str,
    record_name: &str,
    unrepresentable_title: Option<&str>,
) -> String {
    let stamped = set_note_id(frontmatter, record_name);
    let envelope = match unrepresentable_title {
        None => clear_note_title(&stamped),
        Some(title) => set_note_title(&stamped, title),
    };
    join_frontmatter(&envelope, body)
}

// --- line-level fallback for YAML outside the modelled subset --------------

/// A top-level `key:` line (not indented, not a comment).
fn top_level_key(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '#', '-']) {
        return None;
    }
    let colon = line.find(": ").or_else(|| line.strip_suffix(':').map(|l| l.len()))?;
    Some(line[..colon].trim_end())
}

/// Does this YAML look like a block mapping (its first content line a key)?
fn fallback_is_mapping(body: &str) -> bool {
    body.split('\n')
        .map(str::trim_end)
        .find(|l| !l.is_empty() && !l.trim_start().starts_with('#'))
        .is_none_or(|l| top_level_key(l).is_some())
}

fn fallback_key_line(lines: &[String], key: &str) -> Option<usize> {
    lines.iter().position(|l| top_level_key(l) == Some(key))
}

/// The key's line plus its (more-indented or blank-then-indented)
/// continuation lines.
fn fallback_span(lines: &[String], at: usize) -> std::ops::Range<usize> {
    let mut end = at + 1;
    while end < lines.len() && (lines[end].starts_with([' ', '\t']) || lines[end].starts_with("- ")) {
        end += 1;
    }
    at..end
}

fn fallback_read(lines: &[String], key: &str) -> Option<String> {
    let at = fallback_key_line(lines, key)?;
    let line = &lines[at];
    let rest = line[line.find(':')? + 1..].trim_start_matches(' ');
    if rest.starts_with(['[', '{', '&', '*', '!', '|', '>']) {
        return None;
    }
    if rest.starts_with(['\'', '"']) {
        let scalar = yaml::parse_document(&format!("k: {rest}"));
        if let Parsed::Ok(doc) = scalar {
            return doc.get_str("k").map(str::to_string);
        }
        return None;
    }
    let plain = match rest.find(" #") {
        Some(i) => &rest[..i],
        None => rest,
    }
    .trim_end();
    match yaml::resolve_plain(plain) {
        Scalar::Str { value, .. } if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn fallback_set(mut lines: Vec<String>, key: &str, value: &str) -> String {
    let doc = Document {
        contents: Some(vec![yaml::Pair {
            key: key.to_string(),
            value: Value::Scalar(Scalar::Str {
                value: value.to_string(),
                style: Style::Plain,
            }),
            space_before: false,
            raw: true,
        }]),
    };
    let rendered = yaml::stringify_document(&doc);
    let rendered = rendered.strip_suffix('\n').unwrap_or(&rendered).to_string();
    match fallback_key_line(&lines, key) {
        Some(at) => {
            let span = fallback_span(&lines, at);
            lines.splice(span, [rendered]);
        }
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(rendered);
        }
    }
    lines.join("\n")
}

fn fallback_clear(mut lines: Vec<String>, key: &str) -> Vec<String> {
    if let Some(at) = fallback_key_line(&lines, key) {
        let span = fallback_span(&lines, at);
        lines.drain(span);
    }
    lines
}
