//! Markdown tables for table attachments. Originally derived from icloud-md.

use markdown::mdast;

use super::parse::to_mdast;
use super::render::{CONSERVATIVE_SPELLING, RawSpelling, spelling_candidates, text_phrasing};
use super::to_markdown::{Node, Serializer, unw, w};
use crate::js;

/// `MarkdownTableBlock` (line indexes are 0-based, `end_line` exclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownTableBlock {
    pub start_line: usize,
    pub end_line: usize,
    pub grid: Vec<Vec<String>>,
}

/// `renderMarkdownTable`; `Err` for a grid with no rows.
pub fn render_markdown_table(grid: &[Vec<String>]) -> Result<String, String> {
    if grid.is_empty() {
        return Err("Table has no rows - refusing to guess at its structure".into());
    }
    let conservative = stringify_grid(grid, CONSERVATIVE_SPELLING);
    for spelling in spelling_candidates(grid.iter().flatten().map(String::as_str)) {
        let candidate = stringify_grid(grid, spelling);
        if let (Ok(a), Ok(b)) = (parse_markdown_table(&candidate), parse_markdown_table(&conservative))
            && a == b
        {
            return Ok(candidate);
        }
    }
    Ok(conservative)
}

fn stringify_grid(grid: &[Vec<String>], spelling: RawSpelling) -> String {
    let rows = grid
        .iter()
        .map(|row| {
            Node::TableRow(
                row.iter()
                    .map(|cell| Node::TableCell(cell_children(cell, spelling)))
                    .collect(),
            )
        })
        .collect();
    let mut out = Serializer::to_markdown(&Node::Root(vec![Node::Table(rows)]), '*');
    if out.last() == Some(&10) {
        out.pop();
    }
    unw(&out)
}

/// `cellChildren`: `text.split(/\r?\n/)` joined by raw `<br>` nodes.
fn cell_children(text: &str, spelling: RawSpelling) -> Vec<Node> {
    let units = w(text);
    let mut parts: Vec<&[u16]> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < units.len() {
        if units[i] == 10 {
            let end = if i > start && units[i - 1] == 13 { i - 1 } else { i };
            parts.push(&units[start..end]);
            start = i + 1;
        }
        i += 1;
    }
    parts.push(&units[start..]);
    let mut children = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            children.push(Node::Html(w("<br>")));
        }
        if !part.is_empty() {
            children.extend(text_phrasing(part, spelling));
        }
    }
    children
}

/// `parseMarkdownTable`: exactly one table and nothing else, or `Err`.
pub fn parse_markdown_table(markdown: &str) -> Result<Vec<Vec<String>>, String> {
    let blocks = find_markdown_table_blocks(markdown);
    if blocks.len() != 1 {
        return Err("Markdown table is missing its header or separator row, or contains more than one table".into());
    }
    let block = &blocks[0];
    for (index, line) in markdown.split('\n').enumerate() {
        if (index < block.start_line || index >= block.end_line) && !js::trim(line).is_empty() {
            return Err(
                "Markdown table has a row with a different column count than its header, or trailing content - refusing to guess"
                    .into(),
            );
        }
    }
    Ok(block.grid.clone())
}

/// `findMarkdownTableBlocks`.
pub fn find_markdown_table_blocks(text: &str) -> Vec<MarkdownTableBlock> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks = Vec::new();
    let mut from_line = 0;
    while from_line < lines.len() {
        let remainder = lines[from_line..].join("\n");
        let Some(root) = to_mdast(&remainder) else {
            break;
        };
        let Some(table) = root
            .children()
            .and_then(|c| c.iter().find(|n| matches!(n, mdast::Node::Table(_))))
        else {
            break;
        };
        let start_line = node_start_line(table);
        let rows = table.children().map(Vec::as_slice).unwrap_or(&[]);
        let header_row = rows.first().expect("Markdown table node has no header row");
        let header_line = lines.get(from_line + start_line);
        let separator_line = lines.get(from_line + start_line + 1);
        match (header_line, separator_line) {
            (Some(h), Some(s)) if is_strict_pipe_line(h) && is_strict_pipe_line(s) => {}
            _ => {
                from_line += start_line + 1;
                continue;
            }
        }
        let header: Vec<String> = cells(header_row)
            .iter()
            .map(|cell| cell_text(cell, &remainder))
            .collect();
        let mut grid = vec![header.clone()];
        let mut end_line = start_line + 2;
        for row in &rows[1..] {
            let row_start = node_start_line(row);
            let row_line = lines.get(from_line + row_start);
            let row_cells = cells(row);
            if row_cells.len() != header.len() || !row_line.is_some_and(|l| is_strict_pipe_line(l)) {
                break;
            }
            grid.push(row_cells.iter().map(|cell| cell_text(cell, &remainder)).collect());
            end_line = row_start + 1;
        }
        blocks.push(MarkdownTableBlock {
            start_line: from_line + start_line,
            end_line: from_line + end_line,
            grid,
        });
        from_line += end_line;
    }
    blocks
}

fn cells(row: &mdast::Node) -> &[mdast::Node] {
    row.children().map(Vec::as_slice).unwrap_or(&[])
}

fn node_start_line(node: &mdast::Node) -> usize {
    node.position()
        .expect("Markdown table node is missing its source position")
        .start
        .line
        - 1
}

/// `isStrictPipeLine`.
fn is_strict_pipe_line(line: &str) -> bool {
    let trimmed = js::trim(line);
    js::len16(trimmed) >= 2 && trimmed.starts_with('|') && trimmed.ends_with('|')
}

/// `/^<br\s*\/?>$/i`
fn is_br_tag(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("<br") else {
        return false;
    };
    let rest = rest.trim_start_matches(js::is_whitespace);
    rest == ">" || rest == "/>"
}

/// `mdast-util-to-string`.
fn mdast_to_string(node: &mdast::Node) -> String {
    match node {
        mdast::Node::Text(t) => t.value.clone(),
        mdast::Node::InlineCode(c) => c.value.clone(),
        mdast::Node::Html(h) => h.value.clone(),
        mdast::Node::Image(i) => i.alt.clone(),
        mdast::Node::ImageReference(i) => i.alt.clone(),
        other => other
            .children()
            .map(|c| c.iter().map(mdast_to_string).collect())
            .unwrap_or_default(),
    }
}

/// `cellText`.
fn cell_text(cell: &mdast::Node, source: &str) -> String {
    let mut text = String::new();
    for child in cells(cell) {
        match child {
            mdast::Node::Text(t) => text.push_str(&t.value),
            mdast::Node::Html(h) if is_br_tag(&h.value) => text.push('\n'),
            other => match other.position() {
                Some(p) => text.push_str(source.get(p.start.offset..p.end.offset).unwrap_or_default()),
                None => text.push_str(&mdast_to_string(other)),
            },
        }
    }
    text
}
