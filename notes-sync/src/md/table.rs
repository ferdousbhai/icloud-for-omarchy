//! Markdown tables for table attachments. Ports icloud-md
//! `src/notes/markdownTable.ts`. Owner: workstream C.
#![allow(unused_variables)]

/// `MarkdownTableBlock` (line indexes are 0-based, `end_line` exclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownTableBlock {
    pub start_line: usize,
    pub end_line: usize,
    pub grid: Vec<Vec<String>>,
}

/// `renderMarkdownTable`; `Err` for a grid with no rows.
pub fn render_markdown_table(grid: &[Vec<String>]) -> Result<String, String> {
    todo!()
}

/// `parseMarkdownTable`: exactly one table and nothing else, or `Err`.
pub fn parse_markdown_table(markdown: &str) -> Result<Vec<Vec<String>>, String> {
    todo!()
}

/// `findMarkdownTableBlocks`.
pub fn find_markdown_table_blocks(text: &str) -> Vec<MarkdownTableBlock> {
    todo!()
}
