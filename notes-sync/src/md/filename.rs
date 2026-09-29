//! Note file names. Ports icloud-md `src/notes/filename.ts`. Owner:
//! workstream C.
//!
//! Plan deviation: the plan's `note_filename(title, taken)` is two calls in
//! icloud-md - `noteFileNameFor(titleLine, titleMode)` then
//! `uniqueFileName(name, usedNames)` (per directory) - kept separate here.
//! icloud-md does no Unicode normalization of names; neither does this.

use std::collections::HashSet;

use super::js;
use super::title::{carried_title_spelling, encode_title_stem, title_is_representable};
use crate::vault::state::TitleMode;

fn first_line(text: &str) -> &str {
    text.split('\n').next().unwrap_or("")
}

/// `noteFileNameFor`.
pub fn note_file_name_for(title_line: &str, title_mode: TitleMode) -> String {
    if title_mode != TitleMode::Filename {
        return note_file_name(title_line);
    }
    let first = first_line(title_line);
    if title_is_representable(first) {
        format!("{}.md", encode_title_stem(&carried_title_spelling(first)))
    } else {
        "Untitled.md".into()
    }
}

/// `titleNeedingFrontmatter`.
pub fn title_needing_frontmatter(title_line: &str, title_mode: TitleMode) -> Option<String> {
    if title_mode != TitleMode::Filename {
        return None;
    }
    let first = first_line(title_line);
    if js::trim(first).is_empty() {
        return None;
    }
    if title_is_representable(first) {
        None
    } else {
        Some(first.to_string())
    }
}

/// `fileNameCarriesTitle`.
pub fn file_name_carries_title(file_name: &str, title_line: &str) -> bool {
    let wanted = note_file_name_for(title_line, TitleMode::Filename);
    if file_name == wanted {
        return true;
    }
    let extension = js::extname(&wanted);
    let stem = &wanted[..wanted.len() - extension.len()];
    let Some(actual) = file_name.strip_suffix(extension.as_str()) else {
        return false;
    };
    let Some(suffix) = actual.strip_prefix(stem).and_then(|rest| rest.strip_prefix(' ')) else {
        return false;
    };
    // `/^[0-9]+$/.test(suffix) && Number(suffix) >= 2`
    let significant = suffix.trim_start_matches('0').as_bytes();
    !suffix.is_empty()
        && suffix.bytes().all(|b| b.is_ascii_digit())
        && match significant {
            [] => false,
            [digit] => *digit >= b'2',
            _ => true,
        }
}

/// `noteFileName`: the in-body vault's slug (≤ 80 UTF-16 units).
pub fn note_file_name(title: &str) -> String {
    let first = js::trim(first_line(title));
    let stripped: String = first
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    // `.replace(/\s+/g, " ")`
    let mut collapsed = String::with_capacity(stripped.len());
    let mut in_space = false;
    for c in stripped.chars() {
        if js::is_js_whitespace(c) {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    let slug = js::slice16(js::trim(&collapsed), 0, 80);
    let base = if slug.is_empty() { "Untitled" } else { slug.as_str() };
    format!("{base}.md")
}

/// `uniqueFileName`: `Foo.md`, `Foo 2.md`, `Foo 3.md`, ...
pub fn unique_file_name(file_name: &str, used_file_names: &HashSet<String>) -> String {
    if !used_file_names.contains(file_name) {
        return file_name.to_string();
    }
    let ext = js::extname(file_name);
    let stem = &file_name[..file_name.len() - ext.len()];
    let mut n = 2u64;
    loop {
        let candidate = format!("{stem} {n}{ext}");
        if !used_file_names.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
