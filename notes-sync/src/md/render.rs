//! Format model → Markdown. Ports icloud-md `src/notes/renderNoteMarkdown.ts`
//! (remark-stringify + remark-gfm, `unsafe` escaping, `tablePipeAlign:false`;
//! the serializer itself is `to_markdown`).
//!
//! Offsets and slices are UTF-16 code units, as in the JS.

use super::parse::parse_note_markdown;
use super::to_markdown::{Node, Serializer, W, unw, w};
use crate::doc::format::{
    FormatParagraph, InlineSpan, ParagraphKind, formats_round_trip_equal, normalize_spans, trim_trailing_whitespace,
};
use crate::js;

/// `RawSpelling`: optional escaping relaxations tried nicest-first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RawSpelling {
    /// Obsidian's own notation (`[[Note]]` rather than `\[\[Note]]`).
    pub obsidian: bool,
    /// Punctuation escaped only against GFM autolink literals.
    pub autolink: bool,
}

/// `CONSERVATIVE_SPELLING`.
pub const CONSERVATIVE_SPELLING: RawSpelling = RawSpelling {
    obsidian: false,
    autolink: false,
};

/// `spellingCandidates`.
pub fn spelling_candidates<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<RawSpelling> {
    let mut obsidian = false;
    let mut autolink = false;
    for line in lines {
        let line = w(line);
        obsidian = obsidian || !find_obsidian_raw_ranges(&line).is_empty();
        autolink = autolink || !find_autolink_escape_ranges(&line).is_empty();
    }
    [
        RawSpelling {
            obsidian: true,
            autolink: true,
        },
        RawSpelling {
            obsidian: true,
            autolink: false,
        },
        RawSpelling {
            obsidian: false,
            autolink: true,
        },
    ]
    .into_iter()
    .filter(|c| (!c.obsidian || obsidian) && (!c.autolink || autolink) && (c.obsidian || c.autolink))
    .collect()
}

/// `renderNoteMarkdown`.
pub fn render_note_markdown(raw_paragraphs: &[FormatParagraph]) -> String {
    let paragraphs: Vec<FormatParagraph> = raw_paragraphs.iter().map(trim_trailing_whitespace).collect();
    for spelling in spelling_candidates(paragraphs.iter().map(|p| p.text.as_str())) {
        let rendered = render_lines(&paragraphs, spelling);
        if projection_survives(&paragraphs, &rendered) {
            return rendered;
        }
    }
    render_lines(&paragraphs, CONSERVATIVE_SPELLING)
}

/// `projectionSurvives`.
fn projection_survives(paragraphs: &[FormatParagraph], rendered: &str) -> bool {
    match parse_note_markdown(rendered) {
        Ok(back) => {
            back.text
                == paragraphs
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
                && formats_round_trip_equal(paragraphs, &back.paragraphs)
        }
        Err(_) => false,
    }
}

fn heading_depth(kind: ParagraphKind) -> Option<u8> {
    match kind {
        ParagraphKind::Title => Some(1),
        ParagraphKind::Heading => Some(2),
        ParagraphKind::Subheading => Some(3),
        _ => None,
    }
}

fn render_lines(paragraphs: &[FormatParagraph], spelling: RawSpelling) -> String {
    let mut lines: Vec<W> = Vec::new();
    let mut i = 0;
    while i < paragraphs.len() {
        let paragraph = &paragraphs[i];
        let bq = paragraph.block_quote_level;

        if paragraph.kind == ParagraphKind::Monospaced {
            let mut end = i;
            while end < paragraphs.len()
                && paragraphs[end].kind == ParagraphKind::Monospaced
                && paragraphs[end].block_quote_level == bq
            {
                end += 1;
            }
            let value = paragraphs[i..end]
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            push_block_lines(&mut lines, Node::Code(w(&value)), bq);
            i = end;
            continue;
        }

        if paragraph.kind.is_list() {
            let mut end = i;
            while end < paragraphs.len() && paragraphs[end].kind.is_list() && paragraphs[end].block_quote_level == bq {
                end += 1;
            }
            for list in build_list_nodes(&paragraphs[i..end], spelling) {
                push_block_lines(&mut lines, list, bq);
            }
            i = end;
            continue;
        }

        if let Some(depth) = heading_depth(paragraph.kind) {
            let heading = Node::Heading {
                depth,
                children: phrasing_from_paragraph(paragraph, spelling),
            };
            push_block_lines(&mut lines, heading, bq);
            i += 1;
            continue;
        }

        if paragraph.text.is_empty() {
            lines.push(vec![b'>' as u16; bq as usize]);
        } else {
            push_block_lines(
                &mut lines,
                Node::Paragraph(phrasing_from_paragraph(paragraph, spelling)),
                bq,
            );
        }
        i += 1;
    }
    unw(&lines.join(&10u16))
}

/// `pushBlockLines`.
fn push_block_lines(lines: &mut Vec<W>, node: Node, block_quote_level: u32) {
    let mut wrapped = node;
    for _ in 0..block_quote_level {
        wrapped = Node::Blockquote(vec![wrapped]);
    }
    let mut rendered = Serializer::to_markdown(&Node::Root(vec![wrapped]), '-');
    if rendered.last() == Some(&10) {
        rendered.pop();
    }
    lines.extend(rendered.split(|&c| c == 10).map(<[u16]>::to_vec));
}

// --- lists -------------------------------------------------------------------

/// A list under construction: its fields, and its items (each a paragraph
/// plus the nested lists appended to it).
struct ListBuild {
    ordered: bool,
    start: Option<u32>,
    items: Vec<ItemBuild>,
}

struct ItemBuild {
    checked: Option<bool>,
    paragraph: Node,
    lists: Vec<usize>,
}

/// `buildListNodes`.
fn build_list_nodes(paragraphs: &[FormatParagraph], spelling: RawSpelling) -> Vec<Node> {
    // Arena of lists; `roots` are top-level list ids.
    let mut arena: Vec<ListBuild> = Vec::new();
    let mut roots: Vec<usize> = Vec::new();
    let mut stack: Vec<(usize, i32)> = Vec::new();

    for paragraph in paragraphs {
        let ordered = paragraph.kind == ParagraphKind::NumberedList;
        while stack.last().is_some_and(|&(_, indent)| indent > paragraph.indent) {
            stack.pop();
        }
        let mut top = stack.last().copied();
        if let Some((list, indent)) = top
            && indent == paragraph.indent
            && arena[list].ordered != ordered
        {
            stack.pop();
            top = stack.last().copied();
        }
        let needs_new = match top {
            None => true,
            Some((_, indent)) => indent < paragraph.indent,
        };
        if needs_new {
            let start = ordered.then_some(if paragraph.start_number == 0 {
                1
            } else {
                paragraph.start_number
            });
            arena.push(ListBuild {
                ordered,
                start,
                items: Vec::new(),
            });
            let id = arena.len() - 1;
            let parent_item = top.and_then(|(list, _)| arena[list].items.len().checked_sub(1).map(|i| (list, i)));
            match parent_item {
                Some((list, item)) => arena[list].items[item].lists.push(id),
                None => roots.push(id),
            }
            stack.push((id, paragraph.indent));
        }
        let (list, _) = *stack.last().unwrap();

        let empty_todo = paragraph.kind == ParagraphKind::TodoList && paragraph.text.is_empty();
        let checked =
            (paragraph.kind == ParagraphKind::TodoList && !empty_todo).then(|| paragraph.done.unwrap_or(false));
        let children = if empty_todo {
            vec![Node::Html(w(if paragraph.done == Some(true) { "[x]" } else { "[ ]" }))]
        } else {
            phrasing_from_paragraph(paragraph, spelling)
        };
        arena[list].items.push(ItemBuild {
            checked,
            paragraph: Node::Paragraph(children),
            lists: Vec::new(),
        });
    }

    fn assemble(arena: &mut Vec<ListBuild>, id: usize) -> Node {
        let items = std::mem::take(&mut arena[id].items);
        let children = items
            .into_iter()
            .map(|item| {
                let mut children = vec![item.paragraph];
                for list in item.lists {
                    children.push(assemble(arena, list));
                }
                Node::ListItem {
                    checked: item.checked,
                    children,
                }
            })
            .collect();
        Node::List {
            ordered: arena[id].ordered,
            start: arena[id].start,
            children,
        }
    }
    roots.into_iter().map(|id| assemble(&mut arena, id)).collect()
}

// --- inline content ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawRange {
    start: usize,
    end: usize,
}

/// `RAW_TOKEN_CLASS`: `[A-Za-z0-9./?=&%#+:,;@!$'()-]`.
fn is_raw_token(u: u16) -> bool {
    u < 128 && ((u as u8).is_ascii_alphanumeric() || b"./?=&%#+:,;@!$'()-".contains(&(u as u8)))
}

fn is_ws_unit(u: u16) -> bool {
    js::is_whitespace16(u)
}

/// `ENTITY_SHAPED`: `/&(?:#|[A-Za-z][A-Za-z0-9]*;)/`.
fn entity_shaped(token: &[u16]) -> bool {
    let alpha = |u: u16| u < 128 && (u as u8).is_ascii_alphabetic();
    let alnum = |u: u16| u < 128 && (u as u8).is_ascii_alphanumeric();
    token.iter().enumerate().any(|(i, &c)| {
        if c != b'&' as u16 {
            return false;
        }
        match token.get(i + 1) {
            Some(&n) if n == b'#' as u16 => true,
            Some(&n) if alpha(n) => {
                let run = token[i + 2..].iter().take_while(|&&c| alnum(c)).count();
                token.get(i + 2 + run) == Some(&(b';' as u16))
            }
            _ => false,
        }
    })
}

/// `rawRangesFor(text, pattern)`, given the pattern's matches.
fn raw_ranges_for(text: &[u16], matches: Vec<RawRange>) -> Vec<RawRange> {
    matches
        .into_iter()
        .filter(|m| {
            let before = if m.start == 0 { None } else { Some(text[m.start - 1]) };
            let after = text.get(m.end).copied();
            let before_ok = before.is_none_or(|b| b == b'(' as u16 || is_ws_unit(b));
            let after_ok = after.is_none_or(is_ws_unit);
            before_ok && after_ok && !entity_shaped(&text[m.start..m.end])
        })
        .collect()
}

fn starts_with_at(text: &[u16], at: usize, needle: &str) -> bool {
    let needle = w(needle);
    text.len() >= at + needle.len() && text[at..at + needle.len()] == needle[..]
}

/// Matches of `RAW_URL_PATTERN` (`/https?:\/\/[CLASS]+/g`).
fn raw_url_matches(text: &[u16]) -> Vec<RawRange> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let prefix = if starts_with_at(text, i, "https://") {
            Some(8)
        } else if starts_with_at(text, i, "http://") {
            Some(7)
        } else {
            None
        };
        if let Some(prefix) = prefix {
            let run = text[i + prefix..].iter().take_while(|&&c| is_raw_token(c)).count();
            if run > 0 {
                let end = i + prefix + run;
                out.push(RawRange { start: i, end });
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn find_raw_url_ranges(text: &[u16]) -> Vec<RawRange> {
    raw_ranges_for(text, raw_url_matches(text))
}

/// Matches of `AUTOLINK_ESCAPE_PATTERN`: every maximal raw-token run
/// holding one of the triggers (`[Ww]\.[A-Za-z0-9.-]`,
/// `[A-Za-z0-9.+-]@[A-Za-z0-9.-]`, `[ps]:\/`) is one match.
fn autolink_escape_matches(text: &[u16]) -> Vec<RawRange> {
    let alnum = |u: u16| u < 128 && (u as u8).is_ascii_alphanumeric();
    let in_set = |u: u16, set: &[u8]| u < 128 && set.contains(&(u as u8));
    let trigger_at = |run: &[u16], k: usize| -> bool {
        let get = |o: usize| run.get(k + o).copied();
        let domain = |u: Option<u16>| u.is_some_and(|u| alnum(u) || in_set(u, b".-"));
        (get(0).is_some_and(|u| in_set(u, b"Ww")) && get(1) == Some(b'.' as u16) && domain(get(2)))
            || (get(0).is_some_and(|u| alnum(u) || in_set(u, b".+-")) && get(1) == Some(b'@' as u16) && domain(get(2)))
            || (get(0).is_some_and(|u| in_set(u, b"ps")) && get(1) == Some(b':' as u16) && get(2) == Some(b'/' as u16))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if !is_raw_token(text[i]) {
            i += 1;
            continue;
        }
        let end = i + text[i..].iter().take_while(|&&c| is_raw_token(c)).count();
        let run = &text[i..end];
        if (0..run.len()).any(|k| trigger_at(run, k)) {
            out.push(RawRange { start: i, end });
        }
        i = end;
    }
    out
}

fn find_autolink_escape_ranges(text: &[u16]) -> Vec<RawRange> {
    raw_ranges_for(text, autolink_escape_matches(text))
}

/// `[^[\]\n\\<>&*_~`]`
fn is_bracket_inner(u: u16) -> bool {
    !(u < 128 && b"[]\n\\<>&*_~`".contains(&(u as u8)))
}

/// A `\[\[X*\]\]|\[X*\]` match at `at`: its end.
fn bracket_at(text: &[u16], at: usize) -> Option<usize> {
    let open = b'[' as u16;
    let close = b']' as u16;
    if text.get(at) == Some(&open) && text.get(at + 1) == Some(&open) {
        let run = text[at + 2..].iter().take_while(|&&c| is_bracket_inner(c)).count();
        let end = at + 2 + run;
        if text.get(end) == Some(&close) && text.get(end + 1) == Some(&close) {
            return Some(end + 2);
        }
    }
    if text.get(at) == Some(&open) {
        let run = text[at + 1..].iter().take_while(|&&c| is_bracket_inner(c)).count();
        let end = at + 1 + run;
        if text.get(end) == Some(&close) {
            return Some(end + 1);
        }
    }
    None
}

/// Matches of `RAW_BRACKET_PATTERN` (`/!?(?:\[\[X*\]\]|\[X*\])/g`).
fn raw_bracket_matches(text: &[u16]) -> Vec<RawRange> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let with_bang = if text[i] == b'!' as u16 {
            bracket_at(text, i + 1)
        } else {
            None
        };
        let found = with_bang.or_else(|| bracket_at(text, i));
        if let Some(end) = found {
            out.push(RawRange { start: i, end });
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// `findObsidianRawRanges`.
fn find_obsidian_raw_ranges(text: &[u16]) -> Vec<RawRange> {
    let mut out = Vec::new();
    let hash = b'#' as u16;
    let equals = b'=' as u16;
    // `LEADING_TAG_PATTERN`: /^#+(?![ \t]|$)/, with backtracking.
    let hashes = text.iter().take_while(|&&c| c == hash).count();
    let blocked = |n: usize| match text.get(n) {
        None => true,
        Some(&c) => c == b' ' as u16 || c == b'\t' as u16,
    };
    let tag = if hashes == 0 {
        None
    } else if !blocked(hashes) {
        Some(hashes)
    } else if hashes >= 2 {
        Some(hashes - 1)
    } else {
        None
    };
    let leading = tag.or_else(|| {
        let equals_run = text.iter().take_while(|&&c| c == equals).count();
        let only_equals = !text.is_empty() && equals_run == text.len();
        if only_equals || equals_run == 0 {
            None
        } else {
            Some(equals_run)
        }
    });
    if let Some(end) = leading {
        out.push(RawRange { start: 0, end });
    }
    for m in raw_bracket_matches(text) {
        if text
            .get(m.end)
            .is_some_and(|&c| c == b'(' as u16 || c == b'[' as u16 || c == b':' as u16)
        {
            continue;
        }
        out.push(m);
    }
    out
}

/// `findRawRanges`.
fn find_raw_ranges(text: &[u16], spelling: RawSpelling) -> Vec<RawRange> {
    let mut all = find_raw_url_ranges(text);
    if spelling.obsidian {
        all.extend(find_obsidian_raw_ranges(text));
    }
    if spelling.autolink {
        all.extend(find_autolink_escape_ranges(text));
    }
    // Stable sort: start ascending, then end descending.
    all.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<RawRange> = Vec::new();
    for range in all {
        if out.last().is_none_or(|previous| range.start >= previous.end) {
            out.push(range);
        }
    }
    out
}

/// `textPieces`.
fn text_pieces(value: &[u16], absolute_start: usize, raw_ranges: &[RawRange], escape_pipes: bool) -> Vec<Node> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for range in raw_ranges {
        if range.start < absolute_start || range.end < absolute_start {
            continue;
        }
        let start = range.start - absolute_start;
        let end = range.end - absolute_start;
        if start < at || end > value.len() {
            continue;
        }
        if start > at {
            out.push(Node::Text(value[at..start].to_vec()));
        }
        let raw = &value[start..end];
        let pipe = b'|' as u16;
        if escape_pipes && raw.contains(&pipe) {
            for (index, part) in raw.split(|&c| c == pipe).enumerate() {
                if index > 0 {
                    out.push(Node::Text(vec![pipe]));
                }
                if !part.is_empty() {
                    out.push(Node::Html(part.to_vec()));
                }
            }
        } else {
            out.push(Node::Html(raw.to_vec()));
        }
        at = end;
    }
    if at == 0 {
        return vec![Node::Text(value.to_vec())];
    }
    if at < value.len() {
        out.push(Node::Text(value[at..].to_vec()));
    }
    out
}

/// `textPhrasing`: single-line plain text (table cells).
pub(crate) fn text_phrasing(value: &[u16], spelling: RawSpelling) -> Vec<Node> {
    text_pieces(value, 0, &find_raw_ranges(value, spelling), true)
}

struct StyledText<'a> {
    text: &'a [u16],
    span: &'a InlineSpan,
    start: usize,
}

fn phrasing_from_paragraph(paragraph: &FormatParagraph, spelling: RawSpelling) -> Vec<Node> {
    let text = w(&paragraph.text);
    let spans = normalize_spans(paragraph);
    let mut pieces = Vec::new();
    let mut at = 0usize;
    for span in &spans {
        if span.length > 0 {
            let start = at.min(text.len());
            let end = (at + span.length).min(text.len());
            pieces.push(StyledText {
                text: &text[start..end],
                span,
                start: at,
            });
        }
        at += span.length;
    }
    let ranges = find_raw_ranges(&text, spelling);
    build_phrasing(
        &pieces,
        &[Dim::Link, Dim::Bold, Dim::Italic, Dim::Strikethrough, Dim::Underline],
        &ranges,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dim {
    Link,
    Bold,
    Italic,
    Strikethrough,
    Underline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DimValue<'a> {
    Flag(bool),
    Link(&'a str),
}

impl DimValue<'_> {
    fn styled(&self) -> bool {
        match self {
            DimValue::Flag(flag) => *flag,
            DimValue::Link(link) => !link.is_empty(),
        }
    }
}

fn dimension_value(span: &InlineSpan, dimension: Dim) -> DimValue<'_> {
    match dimension {
        Dim::Link => DimValue::Link(&span.style.link),
        Dim::Bold => DimValue::Flag(span.style.bold),
        Dim::Italic => DimValue::Flag(span.style.italic),
        Dim::Strikethrough => DimValue::Flag(span.style.strikethrough),
        Dim::Underline => DimValue::Flag(span.style.underline),
    }
}

/// `buildPhrasing`.
fn build_phrasing(pieces: &[StyledText], dimensions: &[Dim], raw_ranges: &[RawRange]) -> Vec<Node> {
    let mut dimension = None;
    let mut fewest = usize::MAX;
    for &candidate in dimensions {
        let mut groups = 0;
        let mut any_styled = false;
        let mut previous: Option<DimValue> = None;
        for piece in pieces {
            let value = dimension_value(piece.span, candidate);
            if previous.as_ref() != Some(&value) {
                groups += 1;
            }
            if value.styled() {
                any_styled = true;
            }
            previous = Some(value);
        }
        if any_styled && groups < fewest {
            dimension = Some(candidate);
            fewest = groups;
        }
    }
    let Some(dimension) = dimension else {
        return pieces
            .iter()
            .flat_map(|piece| text_pieces(piece.text, piece.start, raw_ranges, false))
            .collect();
    };
    let rest: Vec<Dim> = dimensions.iter().copied().filter(|&d| d != dimension).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < pieces.len() {
        let value = dimension_value(pieces[i].span, dimension);
        let mut end = i;
        while end < pieces.len() && dimension_value(pieces[end].span, dimension) == value {
            end += 1;
        }
        let inner = build_phrasing(&pieces[i..end], &rest, raw_ranges);
        if !value.styled() {
            out.extend(inner);
        } else {
            match (dimension, &value) {
                (Dim::Link, DimValue::Link(url)) => out.push(Node::Link {
                    url: w(url),
                    children: inner,
                }),
                (Dim::Bold, _) => out.push(Node::Strong(inner)),
                (Dim::Italic, _) => out.push(Node::Emphasis(inner)),
                (Dim::Strikethrough, _) => out.push(Node::Delete(inner)),
                _ => {
                    out.push(Node::Html(w("<u>")));
                    out.extend(inner);
                    out.push(Node::Html(w("</u>")));
                }
            }
        }
        i = end;
    }
    out
}
