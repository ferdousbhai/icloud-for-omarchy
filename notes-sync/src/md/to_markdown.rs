//! The subset of `mdast-util-to-markdown` 2.1.2 (+ `mdast-util-gfm` 3.1.0's
//! strikethrough, task-list, table, footnote and autolink-literal
//! extensions, `markdown-table` 3) that `remark-stringify` runs for
//! icloud-md: the node types `renderNoteMarkdown` and `markdownTable.ts`
//! build, the full `unsafe` escaping machinery, and the attention
//! encoding rules. MIT, Titus Wormer - see NOTICE.
//!
//! Strings are UTF-16 code-unit vectors throughout, because the JS code
//! slices, compares and classifies single code units (`charAt`,
//! `charCodeAt`, `slice(-1)`), and that is observable for astral
//! characters. Options are fixed to what icloud-md passes: `bullet` is
//! configurable (`-` for notes, the default `*` for tables), everything
//! else is the default, and GFM's `tablePipeAlign: false`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::js;

/// A UTF-16 string.
pub type W = Vec<u16>;

pub fn w(s: &str) -> W {
    s.encode_utf16().collect()
}

pub fn unw(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// The mdast nodes the renderers build (fields as mdast; `spread` is always
/// false and `lang`/`meta`/`title`/`align` always absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Root(Vec<Node>),
    Paragraph(Vec<Node>),
    Heading {
        depth: u8,
        children: Vec<Node>,
    },
    Code(W),
    Blockquote(Vec<Node>),
    List {
        ordered: bool,
        start: Option<u32>,
        children: Vec<Node>,
    },
    ListItem {
        checked: Option<bool>,
        children: Vec<Node>,
    },
    Text(W),
    Strong(Vec<Node>),
    Emphasis(Vec<Node>),
    Delete(Vec<Node>),
    Link {
        url: W,
        children: Vec<Node>,
    },
    Html(W),
    Table(Vec<Node>),
    TableRow(Vec<Node>),
    TableCell(Vec<Node>),
}

impl Node {
    fn children(&self) -> &[Node] {
        match self {
            Node::Root(c)
            | Node::Paragraph(c)
            | Node::Heading { children: c, .. }
            | Node::Blockquote(c)
            | Node::List { children: c, .. }
            | Node::ListItem { children: c, .. }
            | Node::Strong(c)
            | Node::Emphasis(c)
            | Node::Delete(c)
            | Node::Link { children: c, .. }
            | Node::Table(c)
            | Node::TableRow(c)
            | Node::TableCell(c) => c,
            Node::Code(_) | Node::Text(_) | Node::Html(_) => &[],
        }
    }

    fn value(&self) -> Option<&W> {
        match self {
            Node::Code(v) | Node::Text(v) | Node::Html(v) => Some(v),
            _ => None,
        }
    }
}

/// `mdast-util-to-string` (with `includeHtml`, the default).
fn to_string(node: &Node) -> W {
    if let Some(value) = node.value() {
        return value.clone();
    }
    node.children().iter().flat_map(to_string).collect()
}

// --- unsafe patterns -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// `[\r\n]`
    Eol,
    /// `\]`
    RBracket,
    /// `\d+`
    Digits,
    /// `[+\-.\w]`
    EmailAtext,
    /// `[Ww]`
    W,
    /// `[ps]`
    PS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// `[\r\n]`
    Eol,
    /// `\[`
    LBracket,
    /// `[#A-Za-z]`
    HashAlpha,
    /// `(?:[\r\n]|$)`
    EolOrEnd,
    /// `(?:[ \t\r\n*])`
    SpaceStar,
    /// `(?:[ \t\r\n])`
    Space,
    /// `(?:[ \t\r\n-])`
    SpaceDash,
    /// `(?:[ \t\r\n]|$)`
    SpaceOrEnd,
    /// `[!/?A-Za-z]`
    HtmlStart,
    /// `[:|-]`
    ColonPipeDash,
    /// `[\-.\w]`
    DashDotWord,
    /// `\/`
    Slash,
    /// `[\t :-]`
    TabSpaceColonDash,
    /// `-`
    Dash,
}

fn is_word(u: u16) -> bool {
    u == b'_' as u16 || (u < 128 && (u as u8).is_ascii_alphanumeric())
}

fn in_set(u: u16, set: &str) -> bool {
    u < 128 && set.as_bytes().contains(&(u as u8))
}

fn is_alpha(u: u16) -> bool {
    u < 128 && (u as u8).is_ascii_alphabetic()
}

impl Before {
    fn one(self, u: u16) -> bool {
        match self {
            Before::Eol => in_set(u, "\r\n"),
            Before::RBracket => u == b']' as u16,
            Before::Digits => u < 128 && (u as u8).is_ascii_digit(),
            Before::EmailAtext => in_set(u, "+-.") || is_word(u),
            Before::W => in_set(u, "Ww"),
            Before::PS => in_set(u, "ps"),
        }
    }
}

impl After {
    /// Matches at `at`: `Some(consumed)` or `None`.
    fn matches(self, value: &[u16], at: usize) -> Option<usize> {
        let end = at >= value.len();
        let u = value.get(at).copied().unwrap_or(0xFFFF);
        let one = |ok: bool| if ok && !end { Some(1) } else { None };
        match self {
            After::Eol => one(in_set(u, "\r\n")),
            After::LBracket => one(u == b'[' as u16),
            After::HashAlpha => one(u == b'#' as u16 || is_alpha(u)),
            After::EolOrEnd => {
                if !end && in_set(u, "\r\n") {
                    Some(1)
                } else if end {
                    Some(0)
                } else {
                    None
                }
            }
            After::SpaceStar => one(in_set(u, " \t\r\n*")),
            After::Space => one(in_set(u, " \t\r\n")),
            After::SpaceDash => one(in_set(u, " \t\r\n-")),
            After::SpaceOrEnd => {
                if !end && in_set(u, " \t\r\n") {
                    Some(1)
                } else if end {
                    Some(0)
                } else {
                    None
                }
            }
            After::HtmlStart => one(in_set(u, "!/?") || is_alpha(u)),
            After::ColonPipeDash => one(in_set(u, ":|-")),
            After::DashDotWord => one(in_set(u, "-.") || is_word(u)),
            After::Slash => one(u == b'/' as u16),
            After::TabSpaceColonDash => one(in_set(u, "\t :-")),
            After::Dash => one(u == b'-' as u16),
        }
    }
}

#[derive(Debug, Clone)]
struct Unsafe {
    character: u16,
    at_break: bool,
    before: Option<Before>,
    after: Option<After>,
    in_construct: &'static [&'static str],
    not_in_construct: &'static [&'static str],
}

const FULL_PHRASING_SPANS: &[&str] = &[
    "autolink",
    "destinationLiteral",
    "destinationRaw",
    "reference",
    "titleQuote",
    "titleApostrophe",
];
const AUTOLINK_NOT_IN: &[&str] = &["autolink", "link", "image", "label"];

fn u(character: char) -> Unsafe {
    Unsafe {
        character: character as u16,
        at_break: false,
        before: None,
        after: None,
        in_construct: &[],
        not_in_construct: &[],
    }
}

impl Unsafe {
    fn brk(mut self) -> Self {
        self.at_break = true;
        self
    }
    fn before(mut self, before: Before) -> Self {
        self.before = Some(before);
        self
    }
    fn after(mut self, after: After) -> Self {
        self.after = Some(after);
        self
    }
    fn inside(mut self, constructs: &'static [&'static str]) -> Self {
        self.in_construct = constructs;
        self
    }
    fn not_inside(mut self, constructs: &'static [&'static str]) -> Self {
        self.not_in_construct = constructs;
        self
    }

    /// `patternInScope`.
    fn in_scope(&self, stack: &[&'static str]) -> bool {
        let any = |list: &[&str]| list.iter().any(|c| stack.contains(c));
        (self.in_construct.is_empty() || any(self.in_construct))
            && !(!self.not_in_construct.is_empty() && any(self.not_in_construct))
    }

    /// One attempt of the compiled pattern at `start`:
    /// `Some((position of the character, match end))`.
    fn match_at(&self, value: &[u16], start: usize) -> Option<(usize, usize)> {
        let mut j = start;
        if self.at_break {
            if !value.get(j).is_some_and(|&c| in_set(c, "\r\n")) {
                return None;
            }
            j += 1;
            while value.get(j).is_some_and(|&c| in_set(c, "\t ")) {
                j += 1;
            }
        }
        if let Some(before) = self.before {
            if !value.get(j).is_some_and(|&c| before.one(c)) {
                return None;
            }
            j += 1;
            if before == Before::Digits {
                while value.get(j).is_some_and(|&c| before.one(c)) {
                    j += 1;
                }
            }
        }
        if value.get(j) != Some(&self.character) {
            return None;
        }
        let position = j;
        j += 1;
        if let Some(after) = self.after {
            j += after.matches(value, j)?;
        }
        Some((position, j))
    }
}

fn unsafe_patterns() -> &'static [Unsafe] {
    static PATTERNS: OnceLock<Vec<Unsafe>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        const PHRASING: &[&str] = &["phrasing"];
        vec![
            // mdast-util-to-markdown lib/unsafe.js
            u('\t').after(After::Eol).inside(PHRASING),
            u('\t').before(Before::Eol).inside(PHRASING),
            u('\t').inside(&["codeFencedLangGraveAccent", "codeFencedLangTilde"]),
            u('\r').inside(&[
                "codeFencedLangGraveAccent",
                "codeFencedLangTilde",
                "codeFencedMetaGraveAccent",
                "codeFencedMetaTilde",
                "destinationLiteral",
                "headingAtx",
            ]),
            u('\n').inside(&[
                "codeFencedLangGraveAccent",
                "codeFencedLangTilde",
                "codeFencedMetaGraveAccent",
                "codeFencedMetaTilde",
                "destinationLiteral",
                "headingAtx",
            ]),
            u(' ').after(After::Eol).inside(PHRASING),
            u(' ').before(Before::Eol).inside(PHRASING),
            u(' ').inside(&["codeFencedLangGraveAccent", "codeFencedLangTilde"]),
            u('!')
                .after(After::LBracket)
                .inside(PHRASING)
                .not_inside(FULL_PHRASING_SPANS),
            u('"').inside(&["titleQuote"]),
            u('#').brk(),
            u('#').inside(&["headingAtx"]).after(After::EolOrEnd),
            u('&').after(After::HashAlpha).inside(PHRASING),
            u('\'').inside(&["titleApostrophe"]),
            u('(').inside(&["destinationRaw"]),
            u('(')
                .before(Before::RBracket)
                .inside(PHRASING)
                .not_inside(FULL_PHRASING_SPANS),
            u(')').brk().before(Before::Digits),
            u(')').inside(&["destinationRaw"]),
            u('*').brk().after(After::SpaceStar),
            u('*').inside(PHRASING).not_inside(FULL_PHRASING_SPANS),
            u('+').brk().after(After::Space),
            u('-').brk().after(After::SpaceDash),
            u('.').brk().before(Before::Digits).after(After::SpaceOrEnd),
            u('<').brk().after(After::HtmlStart),
            u('<')
                .after(After::HtmlStart)
                .inside(PHRASING)
                .not_inside(FULL_PHRASING_SPANS),
            u('<').inside(&["destinationLiteral"]),
            u('=').brk(),
            u('>').brk(),
            u('>').inside(&["destinationLiteral"]),
            u('[').brk(),
            u('[').inside(PHRASING).not_inside(FULL_PHRASING_SPANS),
            u('[').inside(&["label", "reference"]),
            u('\\').after(After::Eol).inside(PHRASING),
            u(']').inside(&["label", "reference"]),
            u('_').brk(),
            u('_').inside(PHRASING).not_inside(FULL_PHRASING_SPANS),
            u('`').brk(),
            u('`').inside(&["codeFencedLangGraveAccent", "codeFencedMetaGraveAccent"]),
            u('`').inside(PHRASING).not_inside(FULL_PHRASING_SPANS),
            u('~').brk(),
            // mdast-util-gfm-autolink-literal
            u('@')
                .before(Before::EmailAtext)
                .after(After::DashDotWord)
                .inside(PHRASING)
                .not_inside(AUTOLINK_NOT_IN),
            u('.')
                .before(Before::W)
                .after(After::DashDotWord)
                .inside(PHRASING)
                .not_inside(AUTOLINK_NOT_IN),
            u(':')
                .before(Before::PS)
                .after(After::Slash)
                .inside(PHRASING)
                .not_inside(AUTOLINK_NOT_IN),
            // mdast-util-gfm-footnote
            u('[').inside(&["label", "phrasing", "reference"]),
            // mdast-util-gfm-strikethrough
            u('~').inside(PHRASING).not_inside(FULL_PHRASING_SPANS),
            // mdast-util-gfm-table
            u('\r').inside(&["tableCell"]),
            u('\n').inside(&["tableCell"]),
            u('|').brk().after(After::TabSpaceColonDash),
            u('|').inside(&["tableCell"]),
            u(':').brk().after(After::Dash),
            u('-').brk().after(After::ColonPipeDash),
            // mdast-util-gfm-task-list-item
            u('-').brk().after(After::ColonPipeDash),
        ]
    })
}

/// `encodeCharacterReference`; `None` is JS's `NaN` (`"&#xNAN;"`).
fn char_ref(code: Option<u16>) -> W {
    match code {
        Some(code) => w(&format!("&#x{:X};", code)),
        None => w("&#xNAN;"),
    }
}

fn is_ascii_punct(u: u16) -> bool {
    u < 128 && (u as u8).is_ascii_punctuation()
}

fn slice(value: &[u16], start: usize, end: usize) -> &[u16] {
    let end = end.min(value.len());
    &value[start.min(end)..end]
}

/// `escapeBackslashes(value, after)`.
fn escape_backslashes(value: &[u16], after: &[u16]) -> W {
    let whole: W = value.iter().chain(after).copied().collect();
    let positions: Vec<usize> = (0..whole.len())
        .filter(|&i| whole[i] == b'\\' as u16 && whole.get(i + 1).is_some_and(|&n| is_ascii_punct(n)))
        .collect();
    let mut results = W::new();
    let mut start = 0;
    for position in positions {
        if start != position {
            results.extend_from_slice(slice(value, start, position));
        }
        results.push(b'\\' as u16);
        start = position;
    }
    results.extend_from_slice(slice(value, start, value.len()));
    results
}

/// `classifyCharacter` on a `charCodeAt` result (`None` = `NaN`, which
/// classifies as a letter).
fn classify(code: Option<u16>) -> u8 {
    let Some(code) = code else { return 0 };
    let Some(c) = char::from_u32(code as u32) else { return 0 };
    if code == 32 || js::is_whitespace(c) {
        return 1;
    }
    if is_unicode_punctuation(c) {
        return 2;
    }
    0
}

/// `/\p{P}|\p{S}/u`.
pub(crate) fn is_unicode_punctuation(c: char) -> bool {
    use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
    let gc = icu_properties::CodePointMapData::<GeneralCategory>::new().get(c);
    GeneralCategoryGroup::Punctuation.contains(gc) || GeneralCategoryGroup::Symbol.contains(gc)
}

/// `encodeInfo(outside, inside, marker)` → `(inside, outside)`, for `*`.
fn encode_info(outside: Option<u16>, inside: Option<u16>) -> (bool, bool) {
    let outside_kind = classify(outside);
    let inside_kind = classify(inside);
    match (outside_kind, inside_kind) {
        (0, 0) => (false, false),
        (0, 1) => (true, true),
        (0, _) => (false, true),
        (1, 0) => (false, false),
        (1, 1) => (true, true),
        (1, _) => (false, false),
        (_, 0) => (false, false),
        (_, 1) => (true, false),
        (_, _) => (false, false),
    }
}

struct Info<'a> {
    before: &'a [u16],
    after: &'a [u16],
}

/// Serializer state (`State`).
pub struct Serializer {
    stack: Vec<&'static str>,
    index_stack: Vec<usize>,
    bullet: u16,
    bullet_current: Option<u16>,
    bullet_last_used: Option<u16>,
    /// `attentionEncodeSurroundingInfo` as `(before, after)`.
    attention: Option<(bool, bool)>,
}

impl Serializer {
    /// `toMarkdown(tree, {bullet})` with remark-gfm's extensions.
    pub fn to_markdown(tree: &Node, bullet: char) -> W {
        let mut state = Serializer {
            stack: Vec::new(),
            index_stack: Vec::new(),
            bullet: bullet as u16,
            bullet_current: None,
            bullet_last_used: None,
            attention: None,
        };
        let newline = [b'\n' as u16];
        let mut result = state.handle(
            tree,
            None,
            &Info {
                before: &newline,
                after: &newline,
            },
        );
        if let Some(&last) = result.last()
            && last != 10
            && last != 13
        {
            result.push(10);
        }
        result
    }

    fn enter(&mut self, name: &'static str) {
        self.stack.push(name);
    }

    fn exit(&mut self) {
        self.stack.pop();
    }

    /// `state.safe(value, {before, after, encode})`.
    fn safe(&self, input: &[u16], before: &[u16], after: &[u16], encode: &[u16]) -> W {
        let value: W = before.iter().chain(input).chain(after).copied().collect();
        let mut infos: BTreeMap<usize, (bool, bool)> = BTreeMap::new();
        for pattern in unsafe_patterns() {
            if !pattern.in_scope(&self.stack) {
                continue;
            }
            let mut last_index = 0;
            while last_index < value.len() {
                let found = (last_index..value.len()).find_map(|i| pattern.match_at(&value, i));
                let Some((position, end)) = found else { break };
                let has_before = pattern.before.is_some() || pattern.at_break;
                let has_after = pattern.after.is_some();
                infos
                    .entry(position)
                    .and_modify(|info| {
                        if info.0 && !has_before {
                            info.0 = false;
                        }
                        if info.1 && !has_after {
                            info.1 = false;
                        }
                    })
                    .or_insert((has_before, has_after));
                last_index = end;
            }
        }
        let positions: Vec<usize> = infos.keys().copied().collect();

        let mut result = W::new();
        let mut start = before.len();
        let end = value.len() - after.len();
        for (index, &position) in positions.iter().enumerate() {
            if position < start || position >= end {
                continue;
            }
            let info = infos[&position];
            let next_plain = position + 1 < end
                && positions.get(index + 1) == Some(&(position + 1))
                && info.1
                && !infos[&(position + 1)].0
                && !infos[&(position + 1)].1;
            let previous_plain = index > 0
                && positions[index - 1] + 1 == position
                && info.0
                && !infos[&(position - 1)].0
                && !infos[&(position - 1)].1;
            if next_plain || previous_plain {
                continue;
            }
            if start != position {
                result.extend(escape_backslashes(slice(&value, start, position), &[b'\\' as u16]));
            }
            start = position;
            let character = value[position];
            if is_ascii_punct(character) && !encode.contains(&character) {
                result.push(b'\\' as u16);
            } else {
                result.extend(char_ref(Some(character)));
                start += 1;
            }
        }
        result.extend(escape_backslashes(slice(&value, start, end), after));
        result
    }

    fn handle(&mut self, node: &Node, parent: Option<&Node>, info: &Info) -> W {
        match node {
            Node::Root(children) => self.container_flow(node, children),
            Node::Paragraph(_) => {
                self.enter("paragraph");
                self.enter("phrasing");
                let value = self.container_phrasing(node, info);
                self.exit();
                self.exit();
                value
            }
            Node::Heading { depth, .. } => self.heading(node, *depth),
            Node::Code(raw) => {
                let streak = longest_streak(raw, b'`' as u16);
                let sequence: W = vec![b'`' as u16; (streak + 1).max(3)];
                self.enter("codeFenced");
                let mut value = sequence.clone();
                value.push(10);
                if !raw.is_empty() {
                    value.extend(raw);
                    value.push(10);
                }
                value.extend(&sequence);
                self.exit();
                value
            }
            Node::Blockquote(children) => {
                self.enter("blockquote");
                let inner = self.container_flow(node, children);
                let value = indent_lines(&inner, |line, _, blank| {
                    let mut out = w(if blank { ">" } else { "> " });
                    out.extend(line);
                    out
                });
                self.exit();
                value
            }
            Node::List { ordered, children, .. } => self.list(node, *ordered, children, parent),
            Node::ListItem { checked, children } => self.list_item(node, *checked, children, parent),
            Node::Text(value) => self.safe(value, info.before, info.after, &[]),
            Node::Strong(_) => self.attention(node, info, &w("**")),
            Node::Emphasis(_) => self.attention(node, info, &w("*")),
            Node::Delete(_) => {
                self.enter("strikethrough");
                let before = w("~~");
                let mut value = before.clone();
                value.extend(self.container_phrasing(
                    node,
                    &Info {
                        before: &before,
                        after: &w("~"),
                    },
                ));
                value.extend(w("~~"));
                self.exit();
                value
            }
            Node::Link { url, children } => self.link(node, url, children),
            Node::Html(value) => value.clone(),
            Node::Table(rows) => {
                self.enter("table");
                let matrix: Vec<Vec<W>> = rows.iter().map(|row| self.table_row(row, info)).collect();
                self.exit();
                markdown_table(&matrix)
            }
            Node::TableRow(_) => {
                let row = self.table_row(node, info);
                let value = markdown_table(&[row]);
                let cut = value.iter().position(|&c| c == 10).unwrap_or(value.len());
                value[..cut].to_vec()
            }
            Node::TableCell(_) => self.table_cell(node, info),
        }
    }

    fn table_row(&mut self, row: &Node, info: &Info) -> Vec<W> {
        self.enter("tableRow");
        let cells = row.children().iter().map(|cell| self.table_cell(cell, info)).collect();
        self.exit();
        cells
    }

    fn table_cell(&mut self, cell: &Node, _info: &Info) -> W {
        self.enter("tableCell");
        self.enter("phrasing");
        let around = w("|");
        let value = self.container_phrasing(
            cell,
            &Info {
                before: &around,
                after: &around,
            },
        );
        self.exit();
        self.exit();
        value
    }

    /// The `peek` a phrasing handler exposes (or the handler itself).
    fn peek(&mut self, node: &Node) -> W {
        match node {
            Node::Strong(_) | Node::Emphasis(_) => w("*"),
            Node::Delete(_) => w("~"),
            Node::Html(_) => w("<"),
            Node::Link { url, children } => w(if format_link_as_autolink(node, url, children) {
                "<"
            } else {
                "["
            }),
            other => self.handle(
                other,
                None,
                &Info {
                    before: &[],
                    after: &[],
                },
            ),
        }
    }

    /// `containerPhrasing`.
    fn container_phrasing(&mut self, parent: &Node, info: &Info) -> W {
        let children = parent.children();
        let mut results: Vec<W> = Vec::new();
        let mut before: W = info.before.to_vec();
        let mut encode_after: Option<W> = None;
        self.index_stack.push(0);
        for (index, child) in children.iter().enumerate() {
            *self.index_stack.last_mut().unwrap() = index;
            let after: W = if index + 1 < children.len() {
                self.peek(&children[index + 1]).into_iter().take(1).collect()
            } else {
                info.after.to_vec()
            };

            if !results.is_empty() && (before == [13] || before == [10]) && matches!(child, Node::Html(_)) {
                let last = results.last_mut().unwrap();
                if last.ends_with(&[13, 10]) {
                    last.truncate(last.len() - 2);
                    last.push(32);
                } else if last.ends_with(&[10]) || last.ends_with(&[13]) {
                    last.truncate(last.len() - 1);
                    last.push(32);
                }
                before = vec![32];
            }

            let mut value = self.handle(
                child,
                Some(parent),
                &Info {
                    before: &before,
                    after: &after,
                },
            );

            if let Some(encode) = &encode_after
                && !encode.is_empty()
                && value.first() == encode.first()
            {
                let mut encoded = char_ref(Some(encode[0]));
                encoded.extend(&value[1..]);
                value = encoded;
            }

            let encoding_info = self.attention.take();
            encode_after = None;
            if let Some((encode_before, encode_after_flag)) = encoding_info {
                if let Some(last) = results.last_mut()
                    && encode_before
                    && before.as_slice() == last_unit(last)
                {
                    let code = before.first().copied();
                    last.pop();
                    last.extend(char_ref(code));
                }
                if encode_after_flag {
                    encode_after = Some(after.clone());
                }
            }

            before = last_unit(&value).to_vec();
            results.push(value);
        }
        self.index_stack.pop();
        results.concat()
    }

    /// `containerFlow` (with `joinDefaults`, the only join in play).
    fn container_flow(&mut self, parent: &Node, children: &[Node]) -> W {
        let mut results = W::new();
        self.index_stack.push(0);
        let newline = [10u16];
        for (index, child) in children.iter().enumerate() {
            *self.index_stack.last_mut().unwrap() = index;
            results.extend(self.handle(
                child,
                Some(parent),
                &Info {
                    before: &newline,
                    after: &newline,
                },
            ));
            if !matches!(child, Node::List { .. }) {
                self.bullet_last_used = None;
            }
            if index + 1 < children.len() {
                let spread_parent = matches!(parent, Node::List { .. } | Node::ListItem { .. });
                let paragraphs = matches!(child, Node::Paragraph(_))
                    && (matches!(children[index + 1], Node::Paragraph(_))
                        || matches!(&children[index + 1], Node::Heading { .. } if self.heading_as_setext(&children[index + 1])));
                results.extend(w(if spread_parent && !paragraphs { "\n" } else { "\n\n" }));
            }
        }
        self.index_stack.pop();
        results
    }

    /// `formatHeadingAsSetext` (the `setext` option is off).
    fn heading_as_setext(&self, node: &Node) -> bool {
        fn literal_with_break(node: &Node) -> bool {
            node.value().is_some_and(|v| v.iter().any(|&c| c == 10 || c == 13))
                || node.children().iter().any(literal_with_break)
        }
        let Node::Heading { depth, .. } = node else {
            return false;
        };
        *depth < 3 && !to_string(node).is_empty() && literal_with_break(node)
    }

    fn heading(&mut self, node: &Node, depth: u8) -> W {
        let rank = depth.clamp(1, 6) as usize;
        if self.heading_as_setext(node) {
            self.enter("headingSetext");
            self.enter("phrasing");
            let newline = [10u16];
            let mut value = self.container_phrasing(
                node,
                &Info {
                    before: &newline,
                    after: &newline,
                },
            );
            self.exit();
            self.exit();
            let last_eol = value.iter().rposition(|&c| c == 10 || c == 13).map_or(0, |i| i + 1);
            let underline = value.len() - last_eol;
            value.push(10);
            value.extend(std::iter::repeat_n(
                if rank == 1 { b'=' } else { b'-' } as u16,
                underline,
            ));
            return value;
        }
        let sequence: W = vec![b'#' as u16; rank];
        self.enter("headingAtx");
        self.enter("phrasing");
        let mut value = self.container_phrasing(
            node,
            &Info {
                before: &w("# "),
                after: &[10],
            },
        );
        if value.first().is_some_and(|&c| c == 9 || c == 32) {
            let mut encoded = char_ref(Some(value[0]));
            encoded.extend(&value[1..]);
            value = encoded;
        }
        let value = if value.is_empty() {
            sequence
        } else {
            let mut out = sequence;
            out.push(32);
            out.extend(value);
            out
        };
        self.exit();
        self.exit();
        value
    }

    fn list(&mut self, node: &Node, ordered: bool, children: &[Node], parent: Option<&Node>) -> W {
        self.enter("list");
        let bullet_current = self.bullet_current;
        let mut bullet = if ordered { b'.' as u16 } else { self.bullet };
        let bullet_other = if ordered {
            b')' as u16
        } else if self.bullet == b'*' as u16 {
            b'-' as u16
        } else {
            b'*' as u16
        };
        let mut use_different_marker =
            parent.is_some() && self.bullet_last_used.is_some() && Some(bullet) == self.bullet_last_used;
        if !ordered {
            let first_empty = children.first().is_some_and(|item| item.children().is_empty());
            let s = &self.stack;
            let n = s.len();
            let i = &self.index_stack;
            let m = i.len();
            if (bullet == b'*' as u16 || bullet == b'-' as u16)
                && first_empty
                && n >= 4
                && s[n - 1] == "list"
                && s[n - 2] == "listItem"
                && s[n - 3] == "list"
                && s[n - 4] == "listItem"
                && m >= 3
                && i[m - 1] == 0
                && i[m - 2] == 0
                && i[m - 3] == 0
            {
                use_different_marker = true;
            }
            // `checkRule(state) === bullet` needs a thematic break as a
            // list item's first child, which these trees never hold.
        }
        if use_different_marker {
            bullet = bullet_other;
        }
        self.bullet_current = Some(bullet);
        let value = self.container_flow(node, children);
        self.bullet_last_used = Some(bullet);
        self.bullet_current = bullet_current;
        self.exit();
        value
    }

    fn list_item(&mut self, node: &Node, checked: Option<bool>, children: &[Node], parent: Option<&Node>) -> W {
        let checkable = checked.is_some() && matches!(children.first(), Some(Node::Paragraph(_)));
        let checkbox = w(if checked == Some(true) { "[x] " } else { "[ ] " });

        let mut bullet: W = vec![self.bullet_current.unwrap_or(self.bullet)];
        if let Some(Node::List {
            ordered: true, start, ..
        }) = parent
        {
            let index = *self.index_stack.last().unwrap_or(&0);
            let number = start.map_or(1, |s| s as usize) + index;
            let mut numbered = w(&number.to_string());
            numbered.extend(bullet);
            bullet = numbered;
        }
        let size = bullet.len() + 1;
        self.enter("listItem");
        let inner = self.container_flow(node, children);
        let mut value = indent_lines(&inner, |line, index, blank| {
            let mut out = if index > 0 {
                if blank { W::new() } else { vec![32; size] }
            } else if blank {
                bullet.clone()
            } else {
                let mut b = bullet.clone();
                b.extend(std::iter::repeat_n(32u16, size - bullet.len()));
                b
            };
            out.extend(line);
            out
        });
        self.exit();

        if checkable {
            // `/^(?:[*+-]|\d+\.)([\r\n]| {1,3})/`
            let marker_end = if value.first().is_some_and(|&c| in_set(c, "*+-")) {
                Some(1)
            } else {
                let digits = value
                    .iter()
                    .take_while(|&&c| c < 128 && (c as u8).is_ascii_digit())
                    .count();
                (digits > 0 && value.get(digits) == Some(&(b'.' as u16))).then_some(digits + 1)
            };
            if let Some(at) = marker_end {
                let gap = if value.get(at).is_some_and(|&c| c == 10 || c == 13) {
                    1
                } else {
                    value[at..].iter().take(3).take_while(|&&c| c == 32).count()
                };
                if gap > 0 {
                    let cut = at + gap;
                    let mut out = value[..cut].to_vec();
                    out.extend(&checkbox);
                    out.extend(&value[cut..]);
                    value = out;
                }
            }
        }
        value
    }

    fn attention(&mut self, node: &Node, info: &Info, marker: &[u16]) -> W {
        self.enter(if marker.len() == 2 { "strong" } else { "emphasis" });
        let star = [b'*' as u16];
        let mut between = self.container_phrasing(
            node,
            &Info {
                before: marker,
                after: &star,
            },
        );
        let (open_inside, open_outside) = encode_info(info.before.last().copied(), between.first().copied());
        if open_inside {
            let mut encoded = char_ref(between.first().copied());
            encoded.extend(between.iter().skip(1));
            between = encoded;
        }
        let (close_inside, close_outside) = encode_info(info.after.first().copied(), between.last().copied());
        if close_inside {
            let tail = between.pop();
            between.extend(char_ref(tail));
        }
        self.exit();
        self.attention = Some((open_outside, close_outside));
        let mut value = marker.to_vec();
        value.extend(between);
        value.extend(marker);
        value
    }

    fn link(&mut self, node: &Node, url: &[u16], children: &[Node]) -> W {
        if format_link_as_autolink(node, url, children) {
            let stack = std::mem::take(&mut self.stack);
            self.enter("autolink");
            let mut value = w("<");
            let inner = self.container_phrasing(
                node,
                &Info {
                    before: &value.clone(),
                    after: &w(">"),
                },
            );
            value.extend(inner);
            value.push(b'>' as u16);
            self.stack = stack;
            return value;
        }
        self.enter("link");
        self.enter("label");
        let mut value = w("[");
        let inner = self.container_phrasing(
            node,
            &Info {
                before: &value.clone(),
                after: &w("]("),
            },
        );
        value.extend(inner);
        value.extend(w("]("));
        self.exit();
        if url.iter().any(|&c| c <= 32 || c == 0x7F) {
            self.enter("destinationLiteral");
            value.push(b'<' as u16);
            let safe = self.safe(url, &value, &w(">"), &[]);
            value.extend(safe);
            value.push(b'>' as u16);
        } else {
            self.enter("destinationRaw");
            let safe = self.safe(url, &value, &w(")"), &[]);
            value.extend(safe);
        }
        self.exit();
        value.push(b')' as u16);
        self.exit();
        value
    }
}

fn last_unit(value: &[u16]) -> &[u16] {
    if value.is_empty() {
        &[]
    } else {
        &value[value.len() - 1..]
    }
}

/// `formatLinkAsAutolink` (no `resourceLink`, no titles).
fn format_link_as_autolink(node: &Node, url: &[u16], children: &[Node]) -> bool {
    let raw = to_string(node);
    let mut mailto = w("mailto:");
    mailto.extend(&raw);
    let protocol = {
        // `/^[a-z][a-z+.-]+:/i`
        let alpha = |c: u16| c < 128 && (c as u8).is_ascii_alphabetic();
        let scheme = |c: u16| alpha(c) || in_set(c, "+.-");
        url.first().is_some_and(|&c| alpha(c)) && {
            let run = url[1..].iter().take_while(|&&c| scheme(c)).count();
            run >= 1 && url.get(1 + run) == Some(&(b':' as u16))
        }
    };
    !url.is_empty()
        && children.len() == 1
        && matches!(children[0], Node::Text(_))
        && (raw == url || mailto == url)
        && protocol
        && !url
            .iter()
            .any(|&c| c <= 32 || c == b'<' as u16 || c == b'>' as u16 || c == 0x7F)
}

/// `longest-streak`.
fn longest_streak(value: &[u16], unit: u16) -> usize {
    let mut max = 0;
    let mut count = 0;
    for &c in value {
        if c == unit {
            count += 1;
            max = max.max(count);
        } else {
            count = 0;
        }
    }
    max
}

/// `indentLines(value, map)`: lines split on `\r?\n|\r`, separators kept.
fn indent_lines(value: &[u16], mut map: impl FnMut(&[u16], usize, bool) -> W) -> W {
    let mut result = W::new();
    let mut start = 0;
    let mut line = 0;
    let mut i = 0;
    while i < value.len() {
        let eol_len = match value[i] {
            13 if value.get(i + 1) == Some(&10) => 2,
            13 | 10 => 1,
            _ => 0,
        };
        if eol_len > 0 {
            let piece = &value[start..i];
            result.extend(map(piece, line, piece.is_empty()));
            result.extend(&value[i..i + eol_len]);
            start = i + eol_len;
            line += 1;
            i += eol_len;
        } else {
            i += 1;
        }
    }
    let piece = &value[start..];
    result.extend(map(piece, line, piece.is_empty()));
    result
}

/// `markdown-table` with `alignDelimiters: false` (and default padding and
/// delimiters, no alignment).
fn markdown_table(rows: &[Vec<W>]) -> W {
    let most = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut matrix: Vec<Vec<W>> = rows.to_vec();
    let delimiter: Vec<W> = (0..most).map(|_| w("-")).collect();
    matrix.insert(1.min(matrix.len()), delimiter);
    let mut lines: Vec<W> = Vec::new();
    for row in &matrix {
        let mut line = W::new();
        for column in 0..most {
            let cell = row.get(column).cloned().unwrap_or_default();
            if column == 0 {
                line.push(b'|' as u16);
            }
            if !cell.is_empty() {
                line.push(32);
            }
            line.extend(&cell);
            line.push(32);
            line.push(b'|' as u16);
        }
        lines.push(line);
    }
    lines.join(&10u16)
}
