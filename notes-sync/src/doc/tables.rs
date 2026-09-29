//! Table attachments: the CRDT mergeable-data document and its grid. Ports
//! icloud-md `src/notes/decodeTableRecord.ts` and `mergeableDataPool.ts`.
//! Owner: workstream B.
#![allow(unused_variables)]

use super::Result;
use super::proto::crdt;
use super::text::VersionedDocument;

/// `MergeableDataPool` / `TablePool`.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeableDataPool {
    pub objects: Vec<crdt::document::DocObject>,
    pub key_names: Vec<String>,
    pub uuid_table: Vec<Vec<u8>>,
    pub version: crdt::VectorTimestamp,
}

/// `TableDocument`.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDocument {
    pub pool: MergeableDataPool,
    pub wrapper: VersionedDocument,
    pub document: crdt::Document,
    pub cr_rows_ref: u32,
    pub cr_columns_ref: u32,
    pub cell_columns_ref: u32,
}

/// `decodeTableMarkdown`: compressed `MergeableDataEncrypted` → rendered
/// markdown table (via `md::table::render_markdown_table`).
pub fn decode_table_markdown(compressed: &[u8]) -> Result<String> {
    todo!()
}

/// `parseTableDocument`.
pub fn parse_table_document(compressed: &[u8]) -> Result<TableDocument> {
    todo!()
}

/// `encodeTableDocument` (compressed).
pub fn encode_table_document(doc: &TableDocument) -> Result<Vec<u8>> {
    todo!()
}

/// `tableDocumentRoundTrips`.
pub fn table_document_round_trips(compressed: &[u8]) -> bool {
    todo!()
}

/// `gridFromTableDocument`: rows × columns of cell text, visual order.
pub fn grid_from_table_document(doc: &TableDocument) -> Result<Vec<Vec<String>>> {
    todo!()
}
