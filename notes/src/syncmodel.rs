//! The pure text and JSON logic behind safe sync: a note's ID block, its
//! conflict blocks, the merge of unsaved edits with a note that changed on
//! disk, the characters a plain-text editor gives back changed, titles and
//! previews, folder order. Nothing here touches disk.
//!
//! It was the Qt app's C++ `SyncModel`; tests/fixtures/syncmodel.json holds
//! it to that one's answers, to the byte. The merge works on UTF-16 units,
//! as the C++ did, so that an offset found in a text's editor form holds in
//! the text itself.

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_collator::options::{CollatorOptions, Strength};
use icu_collator::preferences::CollationNumericOrdering;
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::Locale;
use serde_json::{Map, Value, json};

use crate::text::{is_digit, left_utf16, utf16_len};

// ---- the ID block --------------------------------------------------------------

/// A file split into its frontmatter envelope (fences included, byte-exact)
/// and the editable body. No envelope means `("", text)`; an unterminated
/// fence is content, not an envelope. The blank lines icloud-notes-sync
/// writes after the closing fence belong to the envelope, as they do in the
/// engine, so the body starts at the title.
pub fn split_envelope(text: &str) -> (&str, &str) {
    let mut pos = 0;
    let mut line = 0;
    loop {
        let newline = text[pos..].find('\n').map(|i| pos + i);
        let end = newline.map_or(text.len(), |n| n + 1);
        if text[pos..end].trim() != "---" {
            if line == 0 {
                return ("", text);
            }
        } else if line > 0 {
            let body = end + text[end..].bytes().take_while(|&b| b == b'\n').count();
            return text.split_at(body);
        }
        if newline.is_none() {
            return ("", text); // unterminated: all body
        }
        pos = end;
        line += 1;
    }
}

/// Lines of the envelope without the fences; empty when there is none.
fn frontmatter_lines(text: &str) -> Vec<&str> {
    let envelope = split_envelope(text).0;
    if envelope.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = envelope.split('\n').collect();
    lines.remove(0); // opening fence
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop(); // the newline after the closing fence
    }
    lines.pop(); // closing fence
    lines
}

/// The apple-note-id (the note's CloudKit recordName) recorded in the
/// envelope, or "". Only a top-level scalar counts: a nested key or a
/// literal/folded block is not the note's identity.
pub fn extract_note_id(text: &str) -> String {
    for raw in frontmatter_lines(text) {
        if raw.chars().next().is_some_and(char::is_whitespace) {
            continue; // nested key, or a line of a block scalar
        }
        let Some((key, value)) = raw.split_once(':') else {
            continue;
        };
        if key.trim() != "apple-note-id" {
            continue;
        }
        let value = value.trim();
        if value.ends_with('|') || value.ends_with('>') {
            return String::new();
        }
        let quoted = |q: char| value.chars().count() >= 2 && value.starts_with(q) && value.ends_with(q);
        return if quoted('"') || quoted('\'') {
            value[1..value.len() - 1].to_string()
        } else {
            value.to_string()
        };
    }
    String::new()
}

/// A copy Syncthing made of a file two devices changed at once:
/// `<name>.sync-conflict-<YYYYMMDD>-<HHMMSS>-<device>.<ext>`, the device 7
/// letters or digits, the extension optional. The sync engine never reads
/// one as a note, though a copy of a note carries the note's apple-note-id.
/// `path` is the file's name, or a path ending in it.
pub fn is_sync_conflict_name(path: &str) -> bool {
    const MARK: &[u8] = b".sync-conflict-";
    let name = path.rsplit('/').next().unwrap_or(path).as_bytes();
    let digits = |from: usize, n: usize| name.len() >= from + n && name[from..from + n].iter().all(u8::is_ascii_digit);
    (0..name.len()).any(|at| {
        let stamp = at + MARK.len();
        let device = stamp + 8 + 1 + 6 + 1;
        name[at..].starts_with(MARK)
            && digits(stamp, 8)
            && name.get(stamp + 8) == Some(&b'-')
            && digits(stamp + 9, 6)
            && name.get(stamp + 15) == Some(&b'-')
            && name.len() >= device + 7
            && name[device..device + 7].iter().all(u8::is_ascii_alphanumeric)
            // Then a dot or the name's end (a last newline aside).
            && matches!(&name[device + 7..], [] | [b'\n'] | [b'.', ..])
    })
}

// ---- conflict blocks -------------------------------------------------------------

/// A line of what a merge writes on overlapping edits (diff3 shape). Text
/// carrying these must never be uploaded; both push and pull gate on it.
pub fn is_conflict_marker(line: &str) -> bool {
    let s = line.trim();
    s.starts_with("<<<<<<<") || s.starts_with("|||||||") || s.starts_with(">>>>>>>") || s == "======="
}

pub fn has_conflict_markers(text: &str) -> bool {
    text.split('\n').any(is_conflict_marker)
}

/// One block of conflict markers: the lines it spans (marker lines
/// included) and its three sides. "local" is this computer's edit, "remote"
/// the one from iCloud, "base" what both started from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictHunk {
    pub first: usize,
    pub last: usize,
    pub local: Vec<String>,
    pub base: Vec<String>,
    pub remote: Vec<String>,
}

/// The conflict blocks of a text, in order, over its '\n'-split lines.
/// Markers out of order or left open mean the text is not a clean merge
/// result, so none are returned and it stays editable only as text.
pub fn parse_conflicts(text: &str) -> Vec<ConflictHunk> {
    #[derive(PartialEq)]
    enum Part {
        Outside,
        Local,
        Base,
        Remote,
    }
    let mut hunks = Vec::new();
    let mut hunk = ConflictHunk::default();
    let mut part = Part::Outside;
    for (i, line) in text.split('\n').enumerate() {
        let s = line.trim();
        if s.starts_with("<<<<<<<") {
            if part != Part::Outside {
                return Vec::new();
            }
            hunk = ConflictHunk {
                first: i,
                last: i,
                ..Default::default()
            };
            part = Part::Local;
        } else if s.starts_with("|||||||") {
            if part != Part::Local {
                return Vec::new();
            }
            part = Part::Base;
        } else if s == "=======" {
            if part != Part::Local && part != Part::Base {
                return Vec::new();
            }
            part = Part::Remote;
        } else if s.starts_with(">>>>>>>") {
            if part != Part::Remote {
                return Vec::new();
            }
            hunk.last = i;
            hunks.push(std::mem::take(&mut hunk));
            part = Part::Outside;
        } else {
            match part {
                Part::Local => hunk.local.push(line.to_string()),
                Part::Base => hunk.base.push(line.to_string()),
                Part::Remote => hunk.remote.push(line.to_string()),
                Part::Outside => {}
            }
        }
    }
    if part == Part::Outside { hunks } else { Vec::new() }
}

/// Conflict markers [`parse_conflicts`] cannot read: blocks nested in one
/// another, closed out of order or left open (a merge run over a note that
/// still had a conflict). Only an opening, base or closing marker counts: a
/// lone "=======" is also a setext underline or a rule in plain prose.
pub fn has_unreadable_conflicts(text: &str) -> bool {
    let marked = text.split('\n').any(|line| {
        let s = line.trim();
        s.starts_with("<<<<<<<") || s.starts_with("|||||||") || s.starts_with(">>>>>>>")
    });
    marked && parse_conflicts(text).is_empty()
}

/// The text with every conflict marker line dropped and every other line
/// kept, whichever side it came from: nothing is lost, and what is left is
/// plain text to read over (repeated lines included).
pub fn strip_conflict_markers(text: &str) -> String {
    let kept: Vec<&str> = text.split('\n').filter(|line| !is_conflict_marker(line)).collect();
    kept.join("\n")
}

/// The text with each conflict block replaced by the side chosen for it:
/// "local", "remote" or "both" (this computer's lines, then iCloud's).
/// Anything but one valid choice per block leaves the text unchanged.
pub fn resolve_conflicts(text: &str, choices: &[String]) -> String {
    let hunks = parse_conflicts(text);
    if hunks.is_empty() || hunks.len() != choices.len() {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<&str> = Vec::new();
    let mut next = 0;
    for (hunk, choice) in hunks.iter().zip(choices) {
        if !matches!(choice.as_str(), "local" | "remote" | "both") {
            return text.to_string();
        }
        out.extend(&lines[next..hunk.first]);
        if choice != "remote" {
            out.extend(hunk.local.iter().map(String::as_str));
        }
        if choice != "local" {
            out.extend(hunk.remote.iter().map(String::as_str));
        }
        next = hunk.last + 1;
    }
    out.extend(&lines[next..]);
    out.join("\n")
}

/// For each line of `a`, whether `b` lacks it (a line diff by longest
/// common subsequence). Past a size cap every line counts as changed.
pub fn lines_missing_from(a: &[String], b: &[String]) -> Vec<bool> {
    let (n, m) = (a.len(), b.len());
    let mut missing = vec![true; n];
    if n * m > 4_000_000 {
        return missing;
    }
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if a[i] == b[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            missing[i] = false;
            i += 1;
            j += 1;
        } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    missing
}

// ---- the editor's characters -------------------------------------------------------

const NBSP: u16 = 0x00a0;
const LINE_SEPARATOR: u16 = 0x2028;
const PARAGRAPH_SEPARATOR: u16 = 0x2029;
const NEWLINE: u16 = b'\n' as u16;

/// A plain-text editor gives back Apple's no-break spaces as spaces and its
/// line and paragraph separators (U+2028/U+2029) as newlines. Writing that
/// back would reformat the note in Notes (soft breaks become paragraphs), so
/// every stretch an edit left alone is taken from the original instead: the
/// mapping is one unit for one, so positions line up.
fn editor_unit(c: u16) -> u16 {
    match c {
        NBSP => b' ' as u16,
        LINE_SEPARATOR | PARAGRAPH_SEPARATOR => NEWLINE,
        other => other,
    }
}

fn editor_units(text: &[u16]) -> Vec<u16> {
    text.iter().map(|&c| editor_unit(c)).collect()
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// The text as a plain-text editor holds it.
pub fn editor_form(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{a0}' => ' ',
            '\u{2028}' | '\u{2029}' => '\n',
            other => other,
        })
        .collect()
}

fn split_lines(text: &[u16]) -> Vec<&[u16]> {
    text.split(|&c| c == NEWLINE).collect()
}

/// Lines split after each newline, the newline kept, so they rejoin
/// exactly: where each starts, and its length.
fn lines_keeping_newlines(text: &[u16]) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, &c) in text.iter().enumerate() {
        if c == NEWLINE {
            lines.push((start, i + 1 - start));
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push((start, text.len() - start));
    }
    lines
}

fn views<'a>(text: &'a [u16], lines: &[(usize, usize)]) -> Vec<&'a [u16]> {
    lines.iter().map(|&(start, len)| &text[start..start + len]).collect()
}

/// Index pairs of the lines `a` and `b` share, in order (Myers' diff).
/// `None` when the two differ in more lines than is worth aligning.
fn match_lines(a: &[&[u16]], b: &[&[u16]]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m).min(2000);
    let at = |k: isize| (k + max) as usize;
    let mut v = vec![0isize; (2 * max + 2) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut ok = false;
    let mut d = 0;
    while d <= max && !ok {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                ok = true;
                break;
            }
            k += 2;
        }
        d += 1;
    }
    if !ok {
        return None;
    }
    let mut pairs = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let tv = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && tv[at(k - 1)] < tv[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = tv[at(prev_k)];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        if d > 0 {
            x = prev_x;
            y = prev_y;
        }
    }
    pairs.reverse();
    Some(pairs)
}

/// Within one changed stretch: the original around the edited run.
fn restore_around_edit(original: &[u16], edited: &[u16]) -> Vec<u16> {
    let limit = original.len().min(edited.len());
    let mut prefix = 0;
    while prefix < limit && editor_unit(original[prefix]) == edited[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < limit - prefix
        && editor_unit(original[original.len() - 1 - suffix]) == edited[edited.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mut out = original[..prefix].to_vec();
    out.extend(&edited[prefix..edited.len() - suffix]);
    out.extend(&original[original.len() - suffix..]);
    out
}

fn restore_editor_units(original: &[u16], edited: &[u16]) -> Vec<u16> {
    if original.iter().all(|&c| editor_unit(c) == c) {
        return edited.to_vec();
    }
    let normalized = editor_units(original);
    // Lines untouched by the edit keep the original's characters; each run
    // of changed lines keeps them around its own edited stretch.
    let orig_lines = lines_keeping_newlines(&normalized);
    let edit_lines = lines_keeping_newlines(edited);
    let (orig_views, edit_views) = (views(&normalized, &orig_lines), views(edited, &edit_lines));
    let Some(pairs) = match_lines(&orig_views, &edit_views) else {
        return restore_around_edit(original, edited);
    };
    let offset_of = |line: usize| orig_lines.get(line).map_or(normalized.len(), |l| l.0);
    let edit_offset_of = |line: usize| edit_lines.get(line).map_or(edited.len(), |l| l.0);
    let mut result = Vec::with_capacity(edited.len());
    let (mut oi, mut ei) = (0, 0);
    let hunk = |oi: usize, ei: usize, o_end: usize, e_end: usize| {
        restore_around_edit(
            &original[offset_of(oi)..offset_of(o_end)],
            &edited[edit_offset_of(ei)..edit_offset_of(e_end)],
        )
    };
    for (o, e) in pairs {
        if o > oi || e > ei {
            result.extend(hunk(oi, ei, o, e));
        }
        let (start, len) = orig_lines[o];
        result.extend(&original[start..start + len]);
        oi = o + 1;
        ei = e + 1;
    }
    if oi < orig_lines.len() || ei < edit_lines.len() {
        result.extend(hunk(oi, ei, orig_lines.len(), edit_lines.len()));
    }
    result
}

/// Editor text with the original's special characters put back.
pub fn restore_editor_chars(original: &str, edited: &str) -> String {
    string(&restore_editor_units(&units(original), &units(edited)))
}

// ---- the merge of unsaved edits ------------------------------------------------------

fn append(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}

fn two_way_units(theirs: &[u16], base: &[u16], mine: &[u16]) -> Vec<u16> {
    let theirs_e = editor_units(theirs);
    let (t, m, b) = (split_lines(&theirs_e), split_lines(mine), split_lines(base));
    let most = t.len().min(m.len());
    let mut head = 0;
    while head < most && t[head] == m[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < most - head && t[t.len() - 1 - tail] == m[m.len() - 1 - tail] {
        tail += 1;
    }
    // Where line i starts in a text split into lines, capped at its end.
    let offset =
        |lines: &[&[u16]], i: usize, size: usize| lines[..i].iter().map(|l| l.len() + 1).sum::<usize>().min(size);
    let middle = |text: &[u16], lines: &[&[u16]]| {
        let from = offset(lines, head, text.len());
        let mut mid = text[from..offset(lines, lines.len() - tail, text.len())].to_vec();
        if mid.last().is_some_and(|&c| c != NEWLINE) {
            mid.push(NEWLINE);
        }
        mid
    };
    // The base section only when base shares the lines kept around the block.
    let base_fits = b.len() >= head + tail && b[..head] == t[..head] && b[b.len() - tail..] == t[t.len() - tail..];
    let tail_at = offset(&t, t.len() - tail, theirs.len());
    // A block that ends theirs without a final newline ends without one
    // too, so picking theirs gives it back byte for byte.
    let open_end = tail_at >= theirs.len() && theirs.last() != Some(&NEWLINE);
    let mut out = theirs[..offset(&t, head, theirs.len())].to_vec();
    append(&mut out, "<<<<<<< local\n");
    out.extend(middle(mine, &m));
    if base_fits {
        append(&mut out, "||||||| base\n");
        out.extend(middle(base, &b));
    }
    append(&mut out, "=======\n");
    out.extend(middle(theirs, &t));
    append(&mut out, if open_end { ">>>>>>> remote" } else { ">>>>>>> remote\n" });
    out.extend(&theirs[tail_at..]);
    out
}

/// The two-way fallback of [`conflict_body`]: one conflict block around the
/// lines where mine and theirs part ways, with the base section only when
/// base shares the lines kept around the block. Theirs keeps its own
/// characters.
pub fn two_way_conflict_body(theirs: &str, base: &str, mine: &str) -> String {
    if has_conflict_markers(theirs) || has_conflict_markers(base) || has_conflict_markers(mine) {
        return theirs.to_string(); // never nests blocks: see conflict_body
    }
    string(&two_way_units(&units(theirs), &units(base), &units(mine)))
}

/// Unsaved editor text (mine) whose note changed on disk (theirs) since the
/// editor loaded it (base; mine and base in editor form), merged line by
/// line the way diff3 does: a stretch only one side changed takes that
/// side, a stretch both changed alike takes it once, and only a stretch
/// both changed differently becomes a conflict block in the shape
/// [`parse_conflicts`] reads (local, base, remote), to pick from instead of
/// being overwritten. No block means a clean merge, free of markers.
///
/// Theirs keeps its own characters (Apple's no-break spaces and soft
/// breaks) wherever its lines are used, and a clean merge also around the
/// lines taken from mine, as a save would. A base that shares no line with
/// both sides, or notes too large or too far apart to align, fall back to
/// one block around where mine and theirs part ways. Any side already
/// carrying conflict markers is never merged: blocks nested in blocks are
/// unreadable, so theirs comes back unchanged and the caller keeps mine
/// apart.
pub fn conflict_body(theirs: &str, base: &str, mine: &str) -> String {
    if has_conflict_markers(theirs) || has_conflict_markers(base) || has_conflict_markers(mine) {
        return theirs.to_string();
    }
    let (theirs, base, mine) = (units(theirs), units(base), units(mine));
    let two_way = || string(&two_way_units(&theirs, &base, &mine));
    let theirs_e = editor_units(&theirs);
    let (t, m, b) = (split_lines(&theirs_e), split_lines(&mine), split_lines(&base));
    if t.len() + m.len() + b.len() > 30000 {
        return two_way();
    }
    let (Some(pairs_mine), Some(pairs_theirs)) = (match_lines(&b, &m), match_lines(&b, &t)) else {
        return two_way();
    };
    // For each base line, the line of mine (of theirs) it stayed as.
    let mut in_mine = vec![None; b.len()];
    let mut in_theirs = vec![None; b.len()];
    for (bi, mi) in pairs_mine {
        in_mine[bi] = Some(mi);
    }
    for (bi, ti) in pairs_theirs {
        in_theirs[bi] = Some(ti);
    }
    let anchor = |bi: usize| {
        in_mine
            .get(bi)
            .copied()
            .flatten()
            .zip(in_theirs.get(bi).copied().flatten())
    };
    let blank = |line: &[u16]| string(line).trim().is_empty();
    if !(0..b.len()).any(|bi| anchor(bi).is_some() && !blank(b[bi])) {
        return two_way();
    }

    // Theirs line by line in its own characters, with the character that
    // ended each line there ('\n', or a separator the editor form split at).
    let mut theirs_lines: Vec<&[u16]> = Vec::with_capacity(t.len());
    let mut theirs_ends = Vec::with_capacity(t.len());
    let mut at = 0;
    for line in &t {
        theirs_lines.push(&theirs[at..at + line.len()]);
        theirs_ends.push(theirs.get(at + line.len()).copied().unwrap_or(NEWLINE));
        at += line.len() + 1;
    }
    // A line of the result, and when it is a line of theirs, which: it is
    // joined to the next one as theirs joins them.
    let mut out: Vec<(Vec<u16>, Option<usize>)> = Vec::new();
    let marker = |text: &str| (units(text), None);
    let mut conflicted = false;
    let (mut ib, mut im, mut it) = (0, 0, 0);
    loop {
        let next = (ib..b.len()).find(|&bi| anchor(bi).is_some()).unwrap_or(b.len());
        let (m_end, t_end) = anchor(next).unwrap_or((m.len(), t.len()));
        let take_theirs = |out: &mut Vec<(Vec<u16>, Option<usize>)>, from: usize, to: usize| {
            out.extend((from..to).map(|k| (theirs_lines[k].to_vec(), Some(k))));
        };
        if next == ib && m_end == im && t_end == it {
            // a line nobody changed
            if next == b.len() {
                break;
            }
            take_theirs(&mut out, it, it + 1);
            ib += 1;
            im += 1;
            it += 1;
            continue;
        }
        let (bc, mc, tc) = (&b[ib..next], &m[im..m_end], &t[it..t_end]);
        let plain = |lines: &[&[u16]]| lines.iter().map(|l| (l.to_vec(), None)).collect::<Vec<_>>();
        if mc == bc || mc == tc {
            take_theirs(&mut out, it, t_end);
        } else if tc == bc {
            out.extend(plain(mc));
        } else {
            conflicted = true;
            out.push(marker("<<<<<<< local"));
            out.extend(plain(mc));
            out.push(marker("||||||| base"));
            out.extend(plain(bc));
            out.push(marker("======="));
            take_theirs(&mut out, it, t_end);
            out.push(marker(">>>>>>> remote"));
        }
        (ib, im, it) = (next, m_end, t_end);
    }
    let mut merged = Vec::new();
    for (i, (line, from_theirs)) in out.iter().enumerate() {
        merged.extend(line);
        if let Some((_, next)) = out.get(i + 1) {
            merged.push(match (from_theirs, next) {
                (Some(k), Some(n)) if *n == k + 1 => theirs_ends[*k],
                _ => NEWLINE,
            });
        }
    }
    // Lines taken from mine are in editor form: a soft break or no-break
    // space of theirs next to or inside them would become a paragraph or a
    // plain space. A clean merge is one text, so restore it like a save.
    if conflicted {
        string(&merged)
    } else {
        string(&restore_editor_units(&theirs, &editor_units(&merged)))
    }
}

// ---- folders ---------------------------------------------------------------------

/// The collator of the process's locale, numbers by value and case
/// ignored. The locale is the Qt app's reading of the environment: `LC_ALL`,
/// else `LC_MESSAGES`, else `LANG`; when that names one, the first language
/// of `LANGUAGE` instead, if there is one. The C locale has no order worth
/// the name, so it sorts as English.
fn collator() -> &'static CollatorBorrowed<'static> {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR.get_or_init(|| {
        let set = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
        let named = ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().find_map(set);
        let named = named.filter(|name| name != "C" && name != "POSIX").map(|name| {
            let preferred = set("LANGUAGE").and_then(|list| list.split(':').next().map(str::to_string));
            preferred.filter(|language| !language.is_empty()).unwrap_or(name)
        });
        // "sv_SE.UTF-8@euro" is the locale sv-SE.
        let tag = named
            .as_deref()
            .and_then(|name| name.split(['.', '@']).next())
            .map(|name| name.replace('_', "-"));
        let english = Locale::try_from_str("en").expect("a valid tag");
        let locale = tag.and_then(|tag| Locale::try_from_str(&tag).ok()).unwrap_or(english);
        let mut preferences = CollatorPreferences::from(&locale);
        preferences.numeric_ordering = Some(CollationNumericOrdering::True);
        let mut options = CollatorOptions::default();
        options.strength = Some(Strength::Secondary);
        Collator::try_new(preferences, options).expect("collation data is compiled in")
    })
}

/// Folder paths in Apple Notes order: iCloud keeps no folder positions, so
/// Notes lists the default folder first and the rest by name, "2" before
/// "10", each folder directly followed by its own subfolders.
pub fn sort_folders(folders: &mut [String], default_dir: &str) {
    let parts = |path: &str| {
        path.split('/')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    folders.sort_by(|a, b| {
        let (a, b) = (parts(a), parts(b));
        if a.is_empty() || b.is_empty() {
            return a.len().cmp(&b.len()); // the vault root heads the list
        }
        let (a_default, b_default) = (a[0] == default_dir, b[0] == default_dir);
        if a_default != b_default {
            return b_default.cmp(&a_default);
        }
        for (x, y) in a.iter().zip(&b) {
            // "work" and "Work" stay in a stable order
            let order = collator()
                .compare(x, y)
                .then_with(|| x.encode_utf16().cmp(y.encode_utf16()));
            if order != Ordering::Equal {
                return order;
            }
        }
        a.len().cmp(&b.len())
    });
}

// ---- titles and previews -----------------------------------------------------------

/// Retitle for in-body vaults: replace the first body line, keeping its
/// heading marks ("# ") when it had them and leaving a bare line bare.
pub fn retitle_in_body(text: &str, new_title: &str) -> String {
    let (envelope, body) = split_envelope(text);
    if body.trim().is_empty() {
        return format!("{envelope}# {new_title}\n");
    }
    let (first, rest) = body.split_once('\n').map_or((body, None), |(f, r)| (f, Some(r)));
    let hashes = first.bytes().take_while(|&b| b == b'#').count();
    let mut out = envelope.to_string();
    if hashes > 0 {
        out += &"#".repeat(hashes);
        out.push(' ');
    }
    out += new_title;
    if let Some(rest) = rest {
        out.push('\n');
        out += rest;
    }
    out
}

/// Length of a list marker ("- ", "* ", "+ ", "12. ", "3) ") at the start
/// of `s`, in characters, or 0 when the line is not a list item.
fn list_marker_length(s: &[char]) -> usize {
    if s.len() >= 2 && matches!(s[0], '-' | '*' | '+') && s[1].is_whitespace() {
        return 2;
    }
    let digits = s.iter().take_while(|&&c| is_digit(c)).count();
    if digits > 0 && digits + 1 < s.len() && matches!(s[digits], '.' | ')') && s[digits + 1].is_whitespace() {
        return digits + 2;
    }
    0
}

/// Length of a "[ ]"/"[x]" checkbox at the start of `s`, or 0.
fn checkbox_length(s: &[char]) -> usize {
    let boxed = s.len() >= 3 && s[0] == '[' && s[2] == ']' && matches!(s[1], ' ' | 'x' | 'X');
    if boxed && (s.len() == 3 || s[3].is_whitespace()) {
        3
    } else {
        0
    }
}

/// `s` with each Markdown link or image reduced to its text.
fn without_links(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let open = if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            i + 1
        } else {
            i
        };
        let link = (chars[open] == '[')
            .then(|| {
                let close = open + chars[open..].iter().position(|&c| c == ']')?;
                (chars.get(close + 1) == Some(&'(')).then_some(())?;
                let end = close + chars[close..].iter().position(|&c| c == ')')?;
                Some((close, end))
            })
            .flatten();
        match link {
            Some((close, end)) => {
                out.extend(&chars[open + 1..close]);
                i = end + 1;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// The prose of a line for titles and snippets: heading/quote marks, list
/// markers and checkboxes stripped, and inline markup (emphasis, code,
/// links, images) reduced to its text.
pub fn strip_markdown_lead(line: &str) -> String {
    let mut s = line.trim();
    while s.starts_with('#') || s.starts_with('>') {
        s = s[1..].trim();
    }
    let chars: Vec<char> = s.chars().collect();
    let after_marker: String = chars[list_marker_length(&chars)..].iter().collect();
    let chars: Vec<char> = after_marker.trim().chars().collect();
    let after_box: String = chars[checkbox_length(&chars)..].iter().collect();
    let text = without_links(after_box.trim());
    // Emphasis and code marks: "**", "__", "~~", "*", "_" and "`".
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' => {}
            '~' if chars.peek() == Some(&'~') => {
                chars.next();
            }
            other => out.push(other),
        }
    }
    out.trim().to_string()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotePreview {
    pub title: String,
    pub snippet: String,
}

/// Title and snippet for the note list. In in-body vaults the first content
/// line is the title and the next one the snippet; in filename vaults the
/// title is the file name (`fallback_title`) and the first line the snippet.
pub fn preview_note(text: &str, fallback_title: &str, mode: &str) -> NotePreview {
    let mut lines = split_envelope(text).1.split('\n');
    let mut next_prose = || {
        lines
            .by_ref()
            .filter(|line| !is_conflict_marker(line))
            .map(strip_markdown_lead)
            .find(|prose| !prose.is_empty())
            .unwrap_or_default()
    };
    let mut title = fallback_title.to_string();
    if mode != "filename" {
        let first = next_prose();
        if !first.is_empty() {
            title = first;
        }
    }
    NotePreview {
        title,
        snippet: left_utf16(&next_prose(), 140),
    }
}

/// Two or more consecutive lines carrying '|' read as a Markdown table.
/// Single-pipe prose lines do not count.
pub fn has_table(text: &str) -> bool {
    let mut run = 0;
    for line in text.split('\n') {
        let s = line.trim();
        run = if utf16_len(s) > 2 && s.contains('|') {
            run + 1
        } else {
            0
        };
        if run >= 2 {
            return true;
        }
    }
    false
}

/// Flip (or plant) the checkbox on one 0-based line. Bare list items gain
/// "[ ]"; anything else is returned unchanged.
pub fn toggle_checkbox(text: &str, line_index: i64) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let Some((line_index, line)) = usize::try_from(line_index).ok().and_then(|i| Some((i, lines.get(i)?))) else {
        return text.to_string();
    };
    let chars: Vec<char> = line.chars().collect();
    let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
    let marker = list_marker_length(&chars[lead..]);
    if marker == 0 {
        return text.to_string();
    }
    let (start, rest) = chars.split_at(lead + marker);
    let had_box = checkbox_length(rest) == 3;
    let mut toggled: String = start.iter().collect();
    toggled += if had_box && rest[1] == ' ' { "[x]" } else { "[ ]" };
    if had_box {
        toggled.extend(&rest[3..]);
    } else {
        toggled.push(' ');
        toggled.extend(rest);
    }
    lines[line_index] = toggled;
    lines.join("\n")
}

// ---- the engine's answers the window shows ---------------------------------------------

/// The objects of the array `key` in a parsed document, or an "error"
/// entry. A document without the array is an error, never an empty plan.
fn array_items(doc: Option<&Value>, key: &str, what: &str) -> Map<String, Value> {
    let error = |message: String| Map::from_iter([("error".to_string(), Value::String(message))]);
    let Some(object) = doc.and_then(Value::as_object) else {
        return error(format!("{what} output is not JSON"));
    };
    let Some(array) = object.get(key).and_then(Value::as_array) else {
        return error(format!("{what} output has no {key}"));
    };
    let items: Vec<Value> = array.iter().filter(|v| v.is_object()).cloned().collect();
    Map::from_iter([(key.to_string(), Value::Array(items))])
}

/// A JSON number as the whole number it is, else 0.
fn whole(value: Option<&Value>) -> i64 {
    let Some(value) = value else { return 0 };
    value
        .as_i64()
        .or_else(|| value.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
        .filter(|n| i32::try_from(*n).is_ok())
        .unwrap_or(0)
}

/// `icloud-notes-sync --quiet status`: `{entries: [{kind, file, resolution,
/// reason?, ...}], unchanged, notices: [message]}`, or `{error}`.
pub fn parse_status_json(bytes: &[u8]) -> Map<String, Value> {
    let doc: Option<Value> = serde_json::from_slice(bytes).ok();
    let mut result = array_items(doc.as_ref(), "entries", "status");
    if result.contains_key("error") {
        return result;
    }
    let object = doc.as_ref().and_then(Value::as_object);
    let notices: Vec<Value> = object
        .and_then(|o| o.get("notices"))
        .and_then(Value::as_array)
        .map(|notices| {
            notices
                .iter()
                .map(|n| json!(n.get("message").and_then(Value::as_str).unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    result.insert(
        "unchanged".into(),
        json!(whole(object.and_then(|o| o.get("unchanged")))),
    );
    result.insert("notices".into(), Value::Array(notices));
    result
}

/// `icloud-notes-sync --quiet history`: `{epochs: [{id, timestamp,
/// changed}]}`, or `{error}`.
pub fn parse_history_json(bytes: &[u8]) -> Map<String, Value> {
    let doc: Option<Value> = serde_json::from_slice(bytes).ok();
    array_items(doc.as_ref(), "epochs", "history")
}
