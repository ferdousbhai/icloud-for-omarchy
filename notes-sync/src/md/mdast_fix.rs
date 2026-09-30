//! Where markdown-rs 1.0.0's tree differs from remark-parse's (micromark's)
//! on the same input, observed in the golden corpora, fixed up after the
//! fact so the parser sees what icloud-md's does:
//!
//! - ATX heading content that starts with a `#` run loses it (`# #tag`).
//! - A line that can't interrupt a paragraph or an indented code block
//!   (an empty list item; an ordered one not starting at 1) sometimes
//!   does anyway, when a container closed earlier in the document: remark
//!   reads the `-` as a setext underline and the rest as paragraph
//!   continuation text.

use markdown::mdast::{self, Node};
use markdown::unist::{Point, Position};

pub(crate) fn fix_tree(root: &mut Node, source: &str) {
    fix_missed_tables(root, source);
    fix_atx_heading_content(root, source);
    fix_interrupts(root, source);
    fix_links_in_labels(root, source, false);
    fix_astral_flanking(root, source);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Whitespace,
    Punctuation,
    Other,
}

/// micromark classifies UTF-16 code units: both halves of an astral
/// character (emoji...) are "other"; markdown-rs classifies the character.
fn classify(c: Option<char>, utf16: bool) -> Class {
    match c {
        None => Class::Whitespace,
        Some(c) if c.is_whitespace() || crate::js::is_whitespace(c) => Class::Whitespace,
        Some(c) if utf16 && (c as u32) > 0xFFFF => Class::Other,
        Some(c) if super::to_markdown::is_unicode_punctuation(c) => Class::Punctuation,
        Some(_) => Class::Other,
    }
}

/// Whether a delimiter run at `start..end` (all `marker`) can open / close.
fn flanking(source: &str, start: usize, end: usize, marker: u8, utf16: bool) -> (bool, bool) {
    let before = classify(source[..start].chars().next_back(), utf16);
    let after = classify(source[end..].chars().next(), utf16);
    use Class::*;
    let left = after != Whitespace && (after != Punctuation || before == Whitespace || before == Punctuation);
    let right = before != Whitespace && (before != Punctuation || after == Whitespace || after == Punctuation);
    if marker == b'_' {
        (
            left && (!right || before == Punctuation),
            right && (!left || after == Punctuation),
        )
    } else {
        (left, right)
    }
}

fn run_around(source: &str, at: usize, marker: u8) -> (usize, usize) {
    let b = source.as_bytes();
    let (mut start, mut end) = (at, at);
    while start > 0 && b[start - 1] == marker {
        start -= 1;
    }
    while end < b.len() && b[end] == marker {
        end += 1;
    }
    (start, end)
}

/// Emphasis/strong that forms only because markdown-rs reads an astral
/// symbol next to its delimiters as punctuation: back to literal text.
fn fix_astral_flanking(node: &mut Node, source: &str) {
    let Some(children) = node.children_mut() else { return };
    let mut i = 0;
    while i < children.len() {
        fix_astral_flanking(&mut children[i], source);
        let (Node::Emphasis(_) | Node::Strong(_), Some(p)) = (&children[i], children[i].position().cloned()) else {
            i += 1;
            continue;
        };
        let marker = source.as_bytes()[p.start.offset];
        let size = if matches!(children[i], Node::Strong(_)) { 2 } else { 1 };
        let (os, oe) = run_around(source, p.start.offset, marker);
        let (cs, ce) = run_around(source, p.end.offset - 1, marker);
        let astral = |at: usize, forward: bool| {
            let c = if forward {
                source[at..].chars().next()
            } else {
                source[..at].chars().next_back()
            };
            c.is_some_and(|c| (c as u32) > 0xFFFF)
        };
        if !(astral(os, false) || astral(oe, true) || astral(cs, false) || astral(ce, true)) {
            i += 1;
            continue;
        }
        let (can_open, _) = flanking(source, os, oe, marker, true);
        let (_, can_close) = flanking(source, cs, ce, marker, true);
        if can_open && can_close {
            i += 1;
            continue;
        }
        let delimiter = String::from_utf8(vec![marker; size]).unwrap();
        let inner = std::mem::take(children[i].children_mut().unwrap());
        let mut replacement = vec![text(&delimiter)];
        replacement.extend(inner);
        replacement.push(text(&delimiter));
        let count = replacement.len();
        children.splice(i..=i, replacement);
        i += count;
    }
}

/// markdown-rs sometimes doesn't start a GFM table at the top level after
/// an earlier table-looking paragraph and a container (its table-head
/// state goes stale); micromark does. A root paragraph that isn't a
/// continuation of the one before it and parses as a table on its own is
/// that table.
fn fix_missed_tables(root: &mut Node, source: &str) {
    let Some(children) = root.children_mut() else { return };
    let mut i = 0;
    while i < children.len() {
        let Node::Paragraph(para) = &children[i] else {
            i += 1;
            continue;
        };
        let Some(p) = para.position.clone() else {
            i += 1;
            continue;
        };
        let continuation = i > 0
            && matches!(&children[i - 1], Node::Paragraph(prev) if prev.position.as_ref().is_some_and(|pp| pp.end.line + 1 == p.start.line));
        let slice = &source[p.start.offset..p.end.offset];
        if continuation || p.start.line == p.end.line || !slice.contains('|') {
            i += 1;
            continue;
        }
        let Ok(Ok(alone)) = std::panic::catch_unwind(|| markdown::to_mdast(slice, &super::parse::parse_options()))
        else {
            i += 1;
            continue;
        };
        let mut nodes = alone.children().cloned().unwrap_or_default();
        if !matches!(nodes.first(), Some(Node::Table(_))) {
            i += 1;
            continue;
        }
        for node in &mut nodes {
            shift(node, p.start.line - 1, p.start.offset);
        }
        let count = nodes.len();
        children.splice(i..=i, nodes);
        i += count;
    }
}

fn shift(node: &mut Node, lines: usize, offset: usize) {
    if let Some(p) = node.position_mut() {
        p.start.line += lines;
        p.end.line += lines;
        p.start.offset += offset;
        p.end.offset += offset;
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            shift(child, lines, offset);
        }
    }
}

/// markdown-rs forms GFM autolink literals inside link labels (where
/// micromark doesn't), leaving their escapes and references undecoded:
/// turn those back into the plain text micromark would have produced.
fn fix_links_in_labels(node: &mut Node, source: &str, in_label: bool) {
    let label = in_label || matches!(node, Node::Link(_) | Node::LinkReference(_));
    if let Some(children) = node.children_mut() {
        for child in children.iter_mut() {
            if label
                && let Node::Link(link) = child
                && let Some(p) = &link.position
                && !matches!(source.as_bytes().get(p.start.offset), Some(b'[' | b'<'))
            {
                let value = decode_text(&source[p.start.offset..p.end.offset]);
                *child = Node::Text(mdast::Text {
                    value,
                    position: link.position.clone(),
                });
                continue;
            }
            fix_links_in_labels(child, source, label);
        }
    }
}

/// Backslash escapes and character references, as micromark decodes text.
fn decode_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(c) = rest.chars().next() {
        if c == '\\' && rest[1..].starts_with(|n: char| n.is_ascii_punctuation()) {
            out.push_str(&rest[1..2]);
            rest = &rest[2..];
            continue;
        }
        if c == '&'
            && let Some(end) = rest.find(';')
            && let Some(decoded) = decode_reference(&rest[1..end])
        {
            out.push(decoded);
            rest = &rest[end + 1..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn decode_reference(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) if (1..=6).contains(&hex.len()) => u32::from_str_radix(hex, 16).ok()?,
            None if (1..=7).contains(&num.len()) => num.parse().ok()?,
            _ => return None,
        };
        return Some(char::from_u32(code).filter(|&c| c != '\0').unwrap_or('\u{FFFD}'));
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{A0}',
        "copy" => '\u{A9}',
        _ => return None,
    })
}

fn fix_atx_heading_content(node: &mut Node, source: &str) {
    if let Node::Heading(heading) = node
        && let Some(position) = &heading.position
    {
        let bytes = source.as_bytes();
        let mut at = position.start.offset;
        while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
            at += 1;
        }
        let atx = bytes.get(at) == Some(&b'#');
        while at < bytes.len() && bytes[at] == b'#' {
            at += 1;
        }
        while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
            at += 1;
        }
        let first = heading
            .children
            .first()
            .and_then(|c| c.position().map(|p| p.start.offset));
        if atx
            && let Some(first) = first
            && first > at
            && first <= position.end.offset
            && let Some(gap) = dropped_heading_sequence(source, at, first)
        {
            heading.children.insert(
                0,
                Node::Text(mdast::Text {
                    value: gap.to_string(),
                    position: None,
                }),
            );
        }
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            fix_atx_heading_content(child, source);
        }
    }
}

/// `source[at..first]` when it is what markdown-rs's ATX resolver drops: a
/// `#` run (and whitespace/further runs), maybe followed by the backslash of
/// an escape markdown-rs positions its text after.
fn dropped_heading_sequence(source: &str, at: usize, first: usize) -> Option<&str> {
    let mut gap = &source[at..first];
    if gap.ends_with('\\') && source.as_bytes().get(first).is_some_and(u8::is_ascii_punctuation) {
        gap = &gap[..gap.len() - 1];
    }
    (gap.starts_with('#') && gap.bytes().all(|b| matches!(b, b'#' | b' ' | b'\t'))).then_some(gap)
}

fn position(node: &Node) -> Option<&Position> {
    node.position()
}

fn text(value: &str) -> Node {
    Node::Text(mdast::Text {
        value: value.to_string(),
        position: None,
    })
}

fn is_indented_code(node: &Node, source: &str) -> bool {
    let (Node::Code(_), Some(p)) = (node, node.position()) else {
        return false;
    };
    let rest = source[p.start.offset..].trim_start_matches([' ', '\t']);
    !(rest.starts_with("```") || rest.starts_with("~~~"))
}

/// The list item's marker text as remark would read that line as
/// paragraph text: from the marker to the item's content (or line end).
fn item_line_prefix<'a>(source: &'a str, item: &Node) -> &'a str {
    let p = item.position().unwrap();
    let marker = marker_offset(source, item);
    let content_start = item
        .children()
        .and_then(|c| c.first())
        .and_then(|c| c.position())
        .map(|c| c.start.offset);
    let line_end = source[p.start.offset..]
        .find(['\n', '\r'])
        .map_or(source.len(), |i| p.start.offset + i);
    match content_start {
        Some(start) if start <= line_end => &source[marker..start.max(marker)],
        _ => source[marker..p.end.offset.min(line_end).max(marker)].trim_end_matches([' ', '\t']),
    }
}

/// An item that can't interrupt a paragraph / indented code: empty, or
/// ordered and not starting at 1.
fn cannot_interrupt(source: &str, list: &mdast::List, item: &Node) -> bool {
    let empty = item.children().is_none_or(Vec::is_empty);
    empty || (list.ordered && item_number(source, item) != Some(1))
}

fn item_number(source: &str, item: &Node) -> Option<u32> {
    let digits: String = source[marker_offset(source, item)..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Where an item's marker starts (its position includes indentation).
fn marker_offset(source: &str, item: &Node) -> usize {
    let start = item.position().map_or(0, |p| p.start.offset);
    start + source[start..].len() - source[start..].trim_start_matches([' ', '\t']).len()
}

/// Can this item be folded into a paragraph as one continuation line?
fn single_line_item(item: &Node) -> bool {
    let Some(p) = item.position() else { return false };
    match item.children().map(Vec::as_slice) {
        None | Some([]) => true,
        Some([Node::Paragraph(para)]) => para
            .position
            .as_ref()
            .is_some_and(|pp| pp.start.line == p.start.line && pp.end.line == p.start.line),
        _ => false,
    }
}

fn fix_interrupts(node: &mut Node, source: &str) {
    let Some(children) = node.children_mut() else { return };
    for child in children.iter_mut() {
        fix_interrupts(child, source);
    }
    let mut i = 0;
    while i + 1 < children.len() {
        let (left, right) = children.split_at_mut(i + 1);
        let (left, right) = (&mut left[i], &mut right[0]);
        let Node::List(list) = right else {
            i += 1;
            continue;
        };
        let (Some(lp), Some(first)) = (position(left).cloned(), list.children.first()) else {
            i += 1;
            continue;
        };
        let adjacent = first.position().is_some_and(|fp| fp.start.line == lp.end.line + 1);
        if !adjacent || !cannot_interrupt(source, list, first) || !single_line_item(first) {
            i += 1;
            continue;
        }
        if is_indented_code(left, source) {
            // The item's line is a paragraph of its own.
            let item = list.children.remove(0);
            let ip = item.position().unwrap().clone();
            let mut para_children = vec![text(item_line_prefix(source, &item))];
            if let Some([Node::Paragraph(para)]) = item.children().map(Vec::as_slice) {
                para_children.extend(para.children.iter().cloned());
            }
            let paragraph = Node::Paragraph(mdast::Paragraph {
                children: para_children,
                position: Some(ip),
            });
            renumber(source, list);
            let list_empty = list.children.is_empty();
            if list_empty {
                children[i + 1] = paragraph;
            } else {
                children.insert(i + 1, paragraph);
            }
            i += 1;
            continue;
        }
        let Node::Paragraph(para) = left else {
            i += 1;
            continue;
        };
        let item = list.children.remove(0);
        let ip = item.position().unwrap().clone();
        let prefix = item_line_prefix(source, &item);
        let empty = item.children().is_none_or(Vec::is_empty);
        if empty && !list.ordered && prefix.starts_with('-') {
            // A setext underline: the paragraph becomes a level-2 heading.
            let heading = Node::Heading(mdast::Heading {
                children: std::mem::take(&mut para.children),
                position: Some(Position {
                    start: lp.start.clone(),
                    end: ip.end.clone(),
                }),
                depth: 2,
            });
            *left = heading;
        } else {
            para.children.push(text("\n"));
            para.children.push(text(prefix));
            if let Some([Node::Paragraph(inner)]) = item.children().map(Vec::as_slice) {
                para.children.extend(inner.children.iter().cloned());
            }
            para.position = Some(Position {
                start: lp.start.clone(),
                end: Point { ..ip.end.clone() },
            });
        }
        renumber(source, list);
        if list.children.is_empty() {
            children.remove(i + 1);
        }
        // Stay on `i`: the next item may not interrupt either.
    }
}

/// After dropping a list's first item, its `start` is the new first item's.
fn renumber(source: &str, list: &mut mdast::List) {
    if list.ordered
        && let Some(first) = list.children.first()
    {
        list.start = item_number(source, first).or(list.start);
    }
}
