//! Markdown → format model. Originally derived from icloud-md (which uses
//! remark-parse + remark-gfm; here `markdown` (markdown-rs, a port of the
//! same micromark tokenizer) to mdast with GFM and positions).

use markdown::mdast;

use crate::doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind, trim_trailing_whitespace};
use crate::js;

/// `{status: "ok", paragraphs, text}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNoteMarkdown {
    pub paragraphs: Vec<FormatParagraph>,
    /// The paragraphs' texts joined with `\n`.
    pub text: String,
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
pub(crate) fn parse_options() -> markdown::ParseOptions {
    markdown::ParseOptions::gfm()
}

/// The mdast tree remark would build for `source`, or `None` where
/// markdown-rs 1.0.0 panics (a few pathological list/fence nestings).
pub(crate) fn to_mdast(source: &str) -> Option<mdast::Node> {
    let parsed = std::panic::catch_unwind(|| markdown::to_mdast(source, &parse_options())).ok()?;
    let mut root = parsed.expect("markdown without MDX always parses");
    super::mdast_fix::fix_tree(&mut root, source);
    Some(root)
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

/// `parseNoteMarkdown`.
pub fn parse_note_markdown(markdown: &str) -> Result<ParsedNoteMarkdown, ParseRefusal> {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let Some(root) = to_mdast(markdown) else {
        return refuse("the markdown parser couldn't read this note's list structure");
    };
    let mut parser = Parser {
        source: markdown,
        lines: &lines,
        line_output: vec![LineOutput::Untouched; lines.len()],
    };
    for child in root.children().map(Vec::as_slice).unwrap_or(&[]) {
        parser.walk_block(child, 0)?;
    }
    let mut paragraphs: Vec<FormatParagraph> = parser.sweep()?.iter().map(trim_trailing_whitespace).collect();
    let mut offset = 0;
    for paragraph in &mut paragraphs {
        paragraph.start = offset;
        offset += js::len16(&paragraph.text) + 1;
    }
    let text = paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ParsedNoteMarkdown { paragraphs, text })
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
}

impl Parser<'_> {
    fn sweep(&self) -> Result<Vec<FormatParagraph>, ParseRefusal> {
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
                        ParagraphKind::Body,
                        count_quote_markers(raw) as u32,
                        String::new(),
                        Vec::new(),
                        &BODY_SEED,
                    ));
                }
            }
        }
        Ok(out)
    }

    fn record(&mut self, line_number: usize, paragraph: FormatParagraph) {
        let slot = &mut self.line_output[line_number - 1];
        match slot {
            LineOutput::Paragraphs(existing) => existing.push(paragraph),
            _ => *slot = LineOutput::Paragraphs(vec![paragraph]),
        }
    }

    fn mark_syntax(&mut self, line_number: usize) {
        let slot = &mut self.line_output[line_number - 1];
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
        self.record(start, make_paragraph(kind, block_quote_level, text, spans, &BODY_SEED));
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
                make_paragraph(
                    ParagraphKind::Monospaced,
                    block_quote_level,
                    line.to_string(),
                    Vec::new(),
                    &BODY_SEED,
                ),
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
                        make_paragraph(
                            ParagraphKind::TodoList,
                            block_quote_level,
                            String::new(),
                            Vec::new(),
                            &seed,
                        ),
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
                    make_paragraph(seed.kind, block_quote_level, String::new(), Vec::new(), &seed),
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
                    make_paragraph(seed.kind, block_quote_level, text, spans, seed),
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
                    make_paragraph(ParagraphKind::Body, level, text, spans, &BODY_SEED),
                );
            }
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

fn make_paragraph(
    kind: ParagraphKind,
    block_quote_level: u32,
    text: String,
    spans: Vec<InlineSpan>,
    seed: &Seed,
) -> FormatParagraph {
    FormatParagraph {
        kind,
        indent: if kind == seed.kind { seed.indent } else { 0 },
        block_quote_level,
        done: if kind == ParagraphKind::TodoList {
            seed.done
        } else {
            None
        },
        start_number: if kind == seed.kind { seed.start_number } else { 0 },
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
    let bytes = raw_line.as_bytes();
    let mut count = 0;
    let mut at = 0;
    loop {
        let mut spaces = 0;
        while spaces < 3 && bytes.get(at + spaces) == Some(&b' ') {
            spaces += 1;
        }
        if bytes.get(at + spaces) != Some(&b'>') {
            return count;
        }
        at += spaces + 1;
        count += 1;
        if bytes.get(at) == Some(&b' ') {
            at += 1;
        }
    }
}

fn strip_quote_markers(raw_line: &str) -> &str {
    let bytes = raw_line.as_bytes();
    let mut at = 0;
    loop {
        let mut spaces = 0;
        while spaces < 3 && bytes.get(at + spaces) == Some(&b' ') {
            spaces += 1;
        }
        if bytes.get(at + spaces) != Some(&b'>') {
            return &raw_line[at..];
        }
        at += spaces + 1;
        if bytes.get(at) == Some(&b' ') {
            at += 1;
        }
    }
}
