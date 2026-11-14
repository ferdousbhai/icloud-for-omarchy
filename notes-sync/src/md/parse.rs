//! Markdown → format model. Originally derived from icloud-md (which uses
//! remark-parse + remark-gfm; here `markdown` (markdown-rs, a port of the
//! same micromark tokenizer) to mdast with GFM and positions).

use markdown::mdast;

use crate::doc::format::{
    FormatParagraph, InlineSpan, InlineStyle, MAX_NESTING, ParagraphKind, too_deeply_nested_reason,
    trim_trailing_whitespace,
};
use crate::js;

/// `{status: "ok", paragraphs, text}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNoteMarkdown {
    pub paragraphs: Vec<FormatParagraph>,
    /// The paragraphs' texts joined with `\n`.
    pub text: String,
}

impl ParsedNoteMarkdown {
    /// A note's paragraphs with every `start` recomputed, and their text.
    pub fn from_paragraphs(mut paragraphs: Vec<FormatParagraph>) -> Self {
        let mut offset = 0;
        for paragraph in &mut paragraphs {
            paragraph.start = offset;
            offset += js::len16(&paragraph.text) + 1;
        }
        let text = joined_text(&paragraphs);
        ParsedNoteMarkdown { paragraphs, text }
    }
}

/// The paragraphs' texts joined with `\n`: the note's text.
pub(crate) fn joined_text(paragraphs: &[FormatParagraph]) -> String {
    paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `{status: "unsupported", reason}`: markdown Apple Notes can't represent.
/// `reason` is shown verbatim in push refusals.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct ParseRefusal {
    pub reason: String,
}

fn refuse<T>(reason: impl Into<String>) -> Result<T, ParseRefusal> {
    Err(ParseRefusal { reason: reason.into() })
}

/// remark-parse + remark-gfm, as markdown-rs options.
fn parse_options() -> markdown::ParseOptions {
    markdown::ParseOptions::gfm()
}

/// Why `to_mdast` has no tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unparsed {
    /// markdown-rs 1.0.0 panicked (a few pathological list/fence nestings), or
    /// no thread could be started to run it.
    Panicked,
    /// The tree would be deeper than `MAX_TREE_DEPTH`.
    TooDeep,
}

/// How deep a parsed tree may be: `MAX_NESTING` quotes around `MAX_NESTING`
/// nested lists (two nodes a level) and a few inline styles, with room to
/// spare. The fix-ups, the parser and the tree's own drop all recurse per
/// level, so a deeper tree never leaves `parse_bounded`.
const MAX_TREE_DEPTH: usize = 128;

/// markdown-rs tokenizes without recursing per level (10000 levels parse on a
/// 64 KiB stack), but it runs on its own generous stack all the same: nothing
/// a vault file holds may abort the process.
const PARSER_STACK_BYTES: usize = 32 << 20;

/// markdown-rs's tree for `source`, refused where it panics or nests deeper
/// than `MAX_TREE_DEPTH`.
pub(crate) fn parse_bounded(source: &str) -> Result<mdast::Node, Unparsed> {
    let parsed = std::thread::scope(|scope| {
        let parser = std::thread::Builder::new()
            .stack_size(PARSER_STACK_BYTES)
            .spawn_scoped(scope, || {
                let root = markdown::to_mdast(source, &parse_options()).expect("markdown without MDX always parses");
                if exceeds_depth(&root, MAX_TREE_DEPTH) {
                    dismantle(root);
                    return Err(Unparsed::TooDeep);
                }
                Ok(root)
            })
            .map_err(|_| Unparsed::Panicked)?;
        parser.join().map_err(|_| Unparsed::Panicked)
    });
    parsed?
}

/// Whether `root` has a node more than `limit` levels down, without recursing.
fn exceeds_depth(root: &mdast::Node, limit: usize) -> bool {
    let mut pending = vec![(root, 1)];
    while let Some((node, depth)) = pending.pop() {
        if depth > limit {
            return true;
        }
        pending.extend(node.children().into_iter().flatten().map(|child| (child, depth + 1)));
    }
    false
}

/// Drop `root` a node at a time: dropping it whole recurses per level.
fn dismantle(root: mdast::Node) {
    let mut pending = vec![root];
    while let Some(mut node) = pending.pop() {
        if let Some(children) = node.children_mut() {
            pending.append(children);
        }
    }
}

/// The mdast tree remark would build for `source`, refused where `source`
/// nests too deeply to parse safely or markdown-rs panics.
pub(crate) fn to_mdast(source: &str) -> Result<mdast::Node, Unparsed> {
    if too_deeply_nested(source) {
        return Err(Unparsed::TooDeep);
    }
    let mut root = parse_bounded(source)?;
    super::mdast_fix::fix_tree(&mut root, source);
    Ok(root)
}

/// The mdast `type` of a markdown-rs node.
fn node_type(node: &mdast::Node) -> &'static str {
    use mdast::Node as N;
    match node {
        N::Root(_) => "root",
        N::Blockquote(_) => "blockquote",
        N::FootnoteDefinition(_) => "footnoteDefinition",
        N::MdxJsxFlowElement(_) => "mdxJsxFlowElement",
        N::List(_) => "list",
        N::MdxjsEsm(_) => "mdxjsEsm",
        N::Toml(_) => "toml",
        N::Yaml(_) => "yaml",
        N::Break(_) => "break",
        N::InlineCode(_) => "inlineCode",
        N::InlineMath(_) => "inlineMath",
        N::Delete(_) => "delete",
        N::Emphasis(_) => "emphasis",
        N::MdxTextExpression(_) => "mdxTextExpression",
        N::FootnoteReference(_) => "footnoteReference",
        N::Html(_) => "html",
        N::Image(_) => "image",
        N::ImageReference(_) => "imageReference",
        N::MdxJsxTextElement(_) => "mdxJsxTextElement",
        N::Link(_) => "link",
        N::LinkReference(_) => "linkReference",
        N::Strong(_) => "strong",
        N::Text(_) => "text",
        N::Code(_) => "code",
        N::Math(_) => "math",
        N::MdxFlowExpression(_) => "mdxFlowExpression",
        N::Heading(_) => "heading",
        N::Table(_) => "table",
        N::ThematicBreak(_) => "thematicBreak",
        N::TableRow(_) => "tableRow",
        N::TableCell(_) => "tableCell",
        N::ListItem(_) => "listItem",
        N::Definition(_) => "definition",
        N::Paragraph(_) => "paragraph",
    }
}

/// `nodeLines`: 1-based inclusive source lines.
fn node_lines(node: &mdast::Node) -> (usize, usize) {
    let position = node
        .position()
        .unwrap_or_else(|| panic!("markdown {} node is missing its source position", node_type(node)));
    (position.start.line, position.end.line)
}

/// `markdown`'s lines as markdown-rs numbers them: `\r\n`, a lone `\r` and
/// `\n` each end one.
fn source_lines(markdown: &str) -> Vec<&str> {
    let bytes = markdown.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\n' => {
                lines.push(&markdown[start..at]);
                at += 1;
            }
            b'\r' => {
                lines.push(&markdown[start..at]);
                at += if bytes.get(at + 1) == Some(&b'\n') { 2 } else { 1 };
            }
            _ => {
                at += 1;
                continue;
            }
        }
        start = at;
    }
    lines.push(&markdown[start..]);
    lines
}

/// How deep a line's quotes and lists may nest. markdown-rs recurses once per
/// level (thousands overflow the stack) and slows quadratically with depth;
/// rendered notes stay well below it (`MAX_NESTING` of each, plus indentation).
const MAX_MARKDOWN_NESTING: usize = 64;

/// Whether some line opens more than `MAX_MARKDOWN_NESTING` containers: its
/// leading `>` and list markers, plus its indentation counted as a list level
/// per two columns. Lines of a fenced code block opened outside any container
/// are code, however they start: such a fence runs to its closing fence or
/// the end of the note, so skipping them hides no nesting.
fn too_deeply_nested(markdown: &str) -> bool {
    let mut fence: Option<(u8, usize)> = None;
    source_lines(markdown).iter().any(|line| {
        if let Some((marker, length)) = fence {
            if top_level_fence(line).is_some_and(|(m, l, info)| m == marker && l >= length && info.trim().is_empty()) {
                fence = None;
            }
            return false;
        }
        if let Some((marker, length, info)) = top_level_fence(line)
            && !(marker == b'`' && info.contains('`'))
        {
            fence = Some((marker, length));
            return false;
        }
        let bytes = line.as_bytes();
        let (mut at, mut columns, mut markers) = (0, 0, 0);
        loop {
            let marker_end = match bytes.get(at) {
                Some(b' ') => {
                    columns += 1;
                    at += 1;
                    continue;
                }
                Some(b'\t') => {
                    columns += 4;
                    at += 1;
                    continue;
                }
                Some(b'>') => at + 1,
                Some(b'-' | b'*' | b'+') => at + 1,
                Some(b'0'..=b'9') => {
                    let digits = bytes[at..].iter().take_while(|b| b.is_ascii_digit()).count();
                    match bytes.get(at + digits) {
                        Some(b'.' | b')') => at + digits + 1,
                        _ => break,
                    }
                }
                _ => break,
            };
            if bytes[at] != b'>' && !matches!(bytes.get(marker_end), None | Some(b' ' | b'\t')) {
                break;
            }
            markers += 1;
            at = marker_end;
        }
        markers > 0 && markers + columns / 2 > MAX_MARKDOWN_NESTING
    })
}

/// `line` as a code fence outside any container: up to three spaces, then
/// three or more backticks or tildes. Its marker, length and the rest.
fn top_level_fence(line: &str) -> Option<(u8, usize, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let marker = *rest.as_bytes().first().filter(|b| matches!(b, b'`' | b'~'))?;
    let length = rest.bytes().take_while(|&b| b == marker).count();
    (length >= 3).then(|| (marker, length, &rest[length..]))
}

/// `parseNoteMarkdown`.
pub fn parse_note_markdown(markdown: &str) -> Result<ParsedNoteMarkdown, ParseRefusal> {
    if too_deeply_nested(markdown) {
        return refuse(too_deeply_nested_reason());
    }
    let lines = source_lines(markdown);
    let root = match to_mdast(markdown) {
        Ok(root) => root,
        Err(Unparsed::TooDeep) => {
            return refuse("the note nests its Markdown (quotes, lists, emphasis or links) too deeply to read");
        }
        Err(Unparsed::Panicked) => return refuse("the markdown parser couldn't read this note's list structure"),
    };
    let mut parser = Parser {
        source: markdown,
        lines: &lines,
        line_output: vec![LineOutput::Untouched; lines.len()],
        line_missed: None,
    };
    for child in root.children().map(Vec::as_slice).unwrap_or(&[]) {
        parser.walk_block(child, 0)?;
    }
    Ok(ParsedNoteMarkdown::from_paragraphs(
        parser.sweep()?.iter().map(trim_trailing_whitespace).collect(),
    ))
}

#[derive(Debug, Clone)]
enum Piece {
    Text(String, InlineStyle),
    LineBreak,
}

#[derive(Debug, Clone)]
struct Seed {
    kind: ParagraphKind,
    indent: i32,
    done: Option<bool>,
    start_number: u32,
}

const BODY_SEED: Seed = Seed {
    kind: ParagraphKind::Body,
    indent: 0,
    done: None,
    start_number: 0,
};

const MONOSPACED_SEED: Seed = Seed {
    kind: ParagraphKind::Monospaced,
    ..BODY_SEED
};

#[derive(Debug, Clone)]
enum LineOutput {
    Untouched,
    Syntax,
    Paragraphs(Vec<FormatParagraph>),
}

struct Parser<'a> {
    source: &'a str,
    lines: &'a [&'a str],
    line_output: Vec<LineOutput>,
    /// A node line past `lines` (the two disagreeing on a line ending):
    /// `sweep` refuses rather than guess.
    line_missed: Option<usize>,
}

impl Parser<'_> {
    fn sweep(&self) -> Result<Vec<FormatParagraph>, ParseRefusal> {
        if let Some(line) = self.line_missed {
            return refuse(format!("line {line} couldn't be matched to the note's source lines"));
        }
        let mut out = Vec::new();
        for (index, recorded) in self.line_output.iter().enumerate() {
            match recorded {
                LineOutput::Syntax => {}
                LineOutput::Paragraphs(paragraphs) => out.extend(paragraphs.iter().cloned()),
                LineOutput::Untouched => {
                    let raw = self.lines[index];
                    if !js::trim(strip_quote_markers(raw)).is_empty() {
                        return refuse(format!(
                            "line {} wasn't recognized as any markdown construct this tool can represent",
                            index + 1
                        ));
                    }
                    out.push(make_paragraph(
                        &BODY_SEED,
                        count_quote_markers(raw) as u32,
                        String::new(),
                        Vec::new(),
                    ));
                }
            }
        }
        Ok(out)
    }

    fn slot(&mut self, line_number: usize) -> Option<&mut LineOutput> {
        let slot = line_number
            .checked_sub(1)
            .and_then(|index| self.line_output.get_mut(index));
        if slot.is_none() {
            self.line_missed.get_or_insert(line_number);
        }
        slot
    }

    fn record(&mut self, line_number: usize, paragraph: FormatParagraph) {
        let Some(slot) = self.slot(line_number) else { return };
        match slot {
            LineOutput::Paragraphs(existing) => existing.push(paragraph),
            _ => *slot = LineOutput::Paragraphs(vec![paragraph]),
        }
    }

    fn mark_syntax(&mut self, line_number: usize) {
        let Some(slot) = self.slot(line_number) else { return };
        if matches!(slot, LineOutput::Untouched) {
            *slot = LineOutput::Syntax;
        }
    }

    fn walk_block(&mut self, node: &mdast::Node, block_quote_level: u32) -> Result<(), ParseRefusal> {
        use mdast::Node as N;
        match node {
            N::Paragraph(p) => self.record_paragraph_lines(node, &p.children, block_quote_level, &BODY_SEED, None),
            N::Heading(h) => self.record_heading(node, h, block_quote_level),
            N::Code(code) => {
                let lines = node_lines(node);
                self.record_code(&code.value, lines, block_quote_level);
                Ok(())
            }
            N::Blockquote(_) if block_quote_level >= MAX_NESTING => refuse(too_deeply_nested_reason()),
            N::Blockquote(quote) => {
                for child in &quote.children {
                    self.walk_block(child, block_quote_level + 1)?;
                }
                Ok(())
            }
            N::List(list) => self.walk_list(list, block_quote_level, 0),
            N::ThematicBreak(_) => refuse("a thematic break (---/***) has no Apple Notes equivalent"),
            N::Html(_) => refuse(
                "raw HTML blocks can't be represented in Apple Notes (underline's <u> tags are supported inline, within a line of text)",
            ),
            N::Table(_) => refuse("a markdown table doesn't correspond to any table in this note"),
            other => refuse(format!(
                "markdown construct \"{}\" has no Apple Notes equivalent",
                node_type(other)
            )),
        }
    }

    fn record_heading(
        &mut self,
        node: &mdast::Node,
        heading: &mdast::Heading,
        block_quote_level: u32,
    ) -> Result<(), ParseRefusal> {
        let kind = match heading.depth {
            1 => ParagraphKind::Title,
            2 => ParagraphKind::Heading,
            3 => ParagraphKind::Subheading,
            depth => {
                return refuse(format!(
                    "a depth-{depth} heading has no Apple Notes equivalent (only # through ### map to Title/Heading/Subheading)"
                ));
            }
        };
        let mut underline = 0;
        let pieces = self.flatten_inline(&heading.children, &InlineStyle::PLAIN, &mut underline)?;
        let joined: Vec<Piece> = pieces
            .into_iter()
            .map(|piece| match piece {
                Piece::LineBreak => Piece::Text(" ".into(), InlineStyle::PLAIN),
                other => other,
            })
            .collect();
        let (text, spans) = line_from_pieces(&joined);
        let (start, end) = node_lines(node);
        let seed = Seed { kind, ..BODY_SEED };
        self.record(start, make_paragraph(&seed, block_quote_level, text, spans));
        for line in start + 1..=end {
            self.mark_syntax(line);
        }
        Ok(())
    }

    fn record_code(&mut self, value: &str, (start, end): (usize, usize), block_quote_level: u32) {
        let content_lines: Vec<&str> = value.split('\n').collect();
        let source_span = end - start + 1;
        let fenced = source_span > content_lines.len();
        let content_start = if fenced { start + 1 } else { start };
        for line in start..=end {
            self.mark_syntax(line);
        }
        for (i, line) in content_lines.iter().enumerate() {
            self.record(
                content_start + i,
                make_paragraph(&MONOSPACED_SEED, block_quote_level, line.to_string(), Vec::new()),
            );
        }
    }

    fn walk_list(&mut self, list: &mdast::List, block_quote_level: u32, indent: i32) -> Result<(), ParseRefusal> {
        for (item_index, item_node) in list.children.iter().enumerate() {
            let mdast::Node::ListItem(item) = item_node else {
                continue;
            };
            let checked = item.checked;
            let (item_start, _) = node_lines(item_node);
            let item_end = content_end_line(item_node);

            if checked.is_none() && !list.ordered {
                let line = self.lines.get(item_start - 1).copied().unwrap_or("");
                if let Some(done) = empty_todo_line(line)
                    && item_end == item_start
                {
                    let seed = Seed {
                        kind: ParagraphKind::TodoList,
                        indent,
                        done: Some(done),
                        start_number: 0,
                    };
                    self.record(
                        item_start,
                        make_paragraph(&seed, block_quote_level, String::new(), Vec::new()),
                    );
                    continue;
                }
            }

            let kind = if checked.is_some() {
                ParagraphKind::TodoList
            } else if list.ordered {
                ParagraphKind::NumberedList
            } else {
                ParagraphKind::BulletList
            };
            let list_start = list.start.unwrap_or(1);
            let seed = Seed {
                kind,
                indent,
                done: checked,
                start_number: if item_index == 0 && list.ordered && list_start != 1 {
                    list_start
                } else {
                    0
                },
            };

            if item.children.is_empty() {
                self.record(
                    item_start,
                    make_paragraph(&seed, block_quote_level, String::new(), Vec::new()),
                );
                continue;
            }
            let mut first_paragraph_seen = false;
            for child in &item.children {
                match child {
                    mdast::Node::Paragraph(p) => {
                        // remark starts a task item's paragraph at its check
                        // (`[x]` then a line ending puts the text on the next
                        // line); markdown-rs at the text.
                        let start_line =
                            (checked.is_some() && !first_paragraph_seen && item.children.first() == Some(child))
                                .then(|| task_check_line(self.source, child))
                                .flatten();
                        let seed = if first_paragraph_seen { &BODY_SEED } else { &seed };
                        self.record_paragraph_lines(child, &p.children, block_quote_level, seed, start_line)?;
                        first_paragraph_seen = true;
                    }
                    mdast::Node::List(_) if indent >= MAX_NESTING as i32 => return refuse(too_deeply_nested_reason()),
                    mdast::Node::List(nested) => self.walk_list(nested, block_quote_level, indent + 1)?,
                    other => {
                        return refuse(format!(
                            "markdown construct \"{}\" inside a list item has no Apple Notes equivalent",
                            node_type(other)
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn record_paragraph_lines(
        &mut self,
        node: &mdast::Node,
        children: &[mdast::Node],
        block_quote_level: u32,
        seed: &Seed,
        start_line: Option<usize>,
    ) -> Result<(), ParseRefusal> {
        let mut underline = 0;
        let pieces = self.flatten_inline(children, &InlineStyle::PLAIN, &mut underline)?;
        let mut piece_lines: Vec<Vec<Piece>> = vec![Vec::new()];
        for piece in pieces {
            match piece {
                Piece::LineBreak => piece_lines.push(Vec::new()),
                other => piece_lines.last_mut().unwrap().push(other),
            }
        }
        let (start, end) = node_lines(node);
        let start = start_line.unwrap_or(start);
        let source_line_count = end - start + 1;
        let positions_reliable = piece_lines.len() == source_line_count;

        for (line_index, line_pieces) in piece_lines.iter_mut().enumerate() {
            // markdown-rs keeps a continuation line's leading whitespace
            // when it holds a tab; micromark strips it.
            if line_index > 0
                && positions_reliable
                && let Some(raw) = self.lines.get(start + line_index - 1)
                && strip_quote_markers(raw).trim_start_matches(' ').starts_with('\t')
                && let Some(Piece::Text(text, _)) = line_pieces.first_mut()
            {
                *text = text.trim_start_matches([' ', '\t']).to_string();
            }
            let (text, spans) = line_from_pieces(line_pieces);
            let source_line = start + line_index.min(source_line_count - 1);
            if line_index == 0 {
                self.record(
                    source_line,
                    make_paragraph(seed, block_quote_level, text, spans),
                );
            } else {
                let raw_line = if positions_reliable {
                    self.lines.get(source_line - 1).copied()
                } else {
                    None
                };
                let level = raw_line.map_or(block_quote_level, |raw| count_quote_markers(raw) as u32);
                self.record(
                    source_line,
                    make_paragraph(&BODY_SEED, level, text, spans),
                );
            }
        }
        // A lone `\r` ends a source line but stays inside the text, so the
        // lines it opens are already held by the paragraphs above.
        let lone_crs = node.position().map_or(0, |p| {
            let bytes = self
                .source
                .as_bytes()
                .get(p.start.offset..p.end.offset)
                .unwrap_or_default();
            (0..bytes.len())
                .filter(|&i| bytes[i] == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
                .count()
        });
        let covered = start + piece_lines.len();
        for line in covered..(covered + lone_crs).min(end + 1) {
            self.mark_syntax(line);
        }
        Ok(())
    }

    fn flatten_inline(
        &self,
        children: &[mdast::Node],
        style: &InlineStyle,
        underline: &mut usize,
    ) -> Result<Vec<Piece>, ParseRefusal> {
        use mdast::Node as N;
        let with_underline = |style: &InlineStyle, underline: usize| InlineStyle {
            underline: underline > 0,
            ..style.clone()
        };
        let mut out = Vec::new();
        for child in children {
            match child {
                N::Text(text) => {
                    for (i, part) in text.value.split('\n').enumerate() {
                        if i > 0 {
                            out.push(Piece::LineBreak);
                        }
                        if !part.is_empty() {
                            out.push(Piece::Text(part.to_string(), with_underline(style, *underline)));
                        }
                    }
                }
                N::Strong(node) => out.extend(self.flatten_inline(
                    &node.children,
                    &InlineStyle {
                        bold: true,
                        ..style.clone()
                    },
                    underline,
                )?),
                N::Emphasis(node) => out.extend(self.flatten_inline(
                    &node.children,
                    &InlineStyle {
                        italic: true,
                        ..style.clone()
                    },
                    underline,
                )?),
                N::Delete(node) => out.extend(self.flatten_inline(
                    &node.children,
                    &InlineStyle {
                        strikethrough: true,
                        ..style.clone()
                    },
                    underline,
                )?),
                N::Link(link) => {
                    let syntax = child
                        .position()
                        .and_then(|p| self.source.as_bytes().get(p.start.offset).copied());
                    let explicit = syntax == Some(b'[') || syntax == Some(b'<');
                    let inner = if explicit {
                        InlineStyle {
                            link: link.url.clone(),
                            ..style.clone()
                        }
                    } else {
                        style.clone()
                    };
                    out.extend(self.flatten_inline(&link.children, &inner, underline)?);
                }
                N::Html(html) => {
                    if html.value.eq_ignore_ascii_case("<u>") {
                        *underline += 1;
                    } else if html.value.eq_ignore_ascii_case("</u>") {
                        *underline = underline.saturating_sub(1);
                    } else {
                        out.push(Piece::Text(html.value.clone(), with_underline(style, *underline)));
                    }
                }
                N::Break(_) => out.push(Piece::LineBreak),
                other => {
                    let Some(position) = other.position() else {
                        return refuse(format!(
                            "inline markdown construct \"{}\" has no Apple Notes equivalent",
                            node_type(other)
                        ));
                    };
                    let raw = self
                        .source
                        .get(position.start.offset..position.end.offset)
                        .unwrap_or_default();
                    out.push(Piece::Text(raw.to_string(), with_underline(style, *underline)));
                }
            }
        }
        Ok(out)
    }
}

/// The last source line of a list item's content: remark ends items at
/// their last non-blank line, markdown-rs runs them over trailing blank
/// lines.
fn content_end_line(node: &mdast::Node) -> usize {
    match node.children().and_then(|c| c.last()) {
        Some(last) if matches!(node, mdast::Node::ListItem(_) | mdast::Node::List(_)) => content_end_line(last),
        _ if matches!(node, mdast::Node::ListItem(_)) => node_lines(node).0,
        _ => node_lines(node).1,
    }
}

/// For a task item's first paragraph whose text starts on the line after
/// the check (`- [x]` + line ending): the check's line.
fn task_check_line(source: &str, paragraph: &mdast::Node) -> Option<usize> {
    let position = paragraph.position()?;
    let before = source[..position.start.offset].trim_end_matches([' ', '\t']);
    let before = before
        .strip_suffix('\n')
        .map(|b| b.strip_suffix('\r').unwrap_or(b))
        .or_else(|| before.strip_suffix('\r'))?;
    let line = before.rsplit(['\n', '\r']).next().unwrap_or(before);
    let trimmed = line.trim_end_matches([' ', '\t']);
    (trimmed.ends_with("[x]") || trimmed.ends_with("[X]") || trimmed.ends_with("[ ]")).then(|| position.start.line - 1)
}

/// `/^[ \t>]*[-*+][ \t]+\[([ xX])\][ \t]*$/` → `done`.
fn empty_todo_line(line: &str) -> Option<bool> {
    let rest = line.trim_start_matches([' ', '\t', '>']);
    let rest = rest.strip_prefix(['-', '*', '+'])?;
    let after_space = rest.trim_start_matches([' ', '\t']);
    if after_space.len() == rest.len() {
        return None;
    }
    let inner = after_space.strip_prefix('[')?;
    let mark = inner.chars().next()?;
    if !matches!(mark, ' ' | 'x' | 'X') {
        return None;
    }
    let tail = inner[1..].strip_prefix(']')?;
    if !tail.trim_start_matches([' ', '\t']).is_empty() {
        return None;
    }
    Some(mark != ' ')
}

fn make_paragraph(seed: &Seed, block_quote_level: u32, text: String, spans: Vec<InlineSpan>) -> FormatParagraph {
    FormatParagraph {
        kind: seed.kind,
        indent: seed.indent,
        block_quote_level,
        done: seed.done,
        start_number: seed.start_number,
        text,
        spans,
        start: 0,
    }
}

fn line_from_pieces(pieces: &[Piece]) -> (String, Vec<InlineSpan>) {
    let mut text = String::new();
    let mut spans: Vec<InlineSpan> = Vec::new();
    for piece in pieces {
        let Piece::Text(value, style) = piece else { continue };
        text.push_str(value);
        let length = js::len16(value);
        match spans.last_mut() {
            Some(previous) if previous.style == *style => previous.length += length,
            _ => spans.push(InlineSpan {
                style: style.clone(),
                length,
            }),
        }
    }
    (text, spans)
}

/// `countQuoteMarkers`.
pub fn count_quote_markers(raw_line: &str) -> usize {
    split_quote_markers(raw_line).0
}

fn strip_quote_markers(raw_line: &str) -> &str {
    split_quote_markers(raw_line).1
}

/// A line's leading quote markers (each up to three spaces, `>`, and one
/// optional space): how many, and what follows them.
fn split_quote_markers(raw_line: &str) -> (usize, &str) {
    let bytes = raw_line.as_bytes();
    let mut count = 0;
    let mut at = 0;
    loop {
        let mut spaces = 0;
        while spaces < 3 && bytes.get(at + spaces) == Some(&b' ') {
            spaces += 1;
        }
        if bytes.get(at + spaces) != Some(&b'>') {
            return (count, &raw_line[at..]);
        }
        at += spaces + 1;
        count += 1;
        if bytes.get(at) == Some(&b' ') {
            at += 1;
        }
    }
}
