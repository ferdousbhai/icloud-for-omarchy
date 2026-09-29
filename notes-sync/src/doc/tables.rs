//! Table attachments: the CRDT mergeable-data document and its grid. Ports
//! icloud-md `src/notes/decodeTableRecord.ts` and `mergeableDataPool.ts`.
//!
//! TS's `TableDocument` aliases the pool arrays (`objects`, `keyNames`,
//! `uuidTable`, `version`) into `document`; here the pool *is* `document`
//! and the accessors below read it.

use std::collections::HashMap;

use indexmap::IndexMap;

use super::proto::crdt::document::DocObject;
use super::proto::crdt::{self, ObjectID};
use super::proto::{Message, crdt::VectorTimestamp};
use super::text::{
    VersionedDocument, compress_note_document, decompress_note_document, encode_versioned_document,
    parse_versioned_document,
};
use super::{DocError, Result};

/// `TableDocument`: the outer wrapper, the inner `CRDT.Document`, and the
/// three top-level pool refs.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDocument {
    pub wrapper: VersionedDocument,
    pub document: crdt::Document,
    pub cr_rows_ref: u32,
    pub cr_columns_ref: u32,
    pub cell_columns_ref: u32,
}

impl TableDocument {
    /// `pool.objects`.
    pub fn objects(&self) -> &[DocObject] {
        &self.document.object
    }

    /// `pool.keyNames`.
    pub fn key_names(&self) -> &[String] {
        &self.document.key_item
    }

    /// `pool.uuidTable`.
    pub fn uuid_table(&self) -> &[Vec<u8>] {
        &self.document.uuid_item
    }

    /// `pool.version` (present: `parseTableDocument` requires it).
    pub fn version(&self) -> &VectorTimestamp {
        static EMPTY: std::sync::OnceLock<VectorTimestamp> = std::sync::OnceLock::new();
        self.document
            .version
            .as_ref()
            .unwrap_or_else(|| EMPTY.get_or_init(VectorTimestamp::default))
    }
}

/// `TableRowColumn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableRowColumn {
    pub identity_ref: usize,
    pub uuid_index: u64,
}

/// `TableCell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub text_ref: u32,
    pub text: String,
}

/// `ResolvedTable`; `cells` keyed by (row position, column position).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTable {
    pub rows: Vec<TableRowColumn>,
    pub columns: Vec<TableRowColumn>,
    pub cells: HashMap<(usize, usize), TableCell>,
}

/// `ParsedOrderedSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedOrderedSet {
    pub array_uuid_indexes: Vec<u64>,
    /// `(keyRef, valueRef)` redirects.
    pub contents: Vec<(u32, u32)>,
}

const LEFT_TO_RIGHT_DIRECTION: &str = "CRTableColumnDirectionLeftToRight";

pub(crate) fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(DocError::Invalid(message.into()))
}

/// `decodeTableMarkdown`: compressed `MergeableDataEncrypted` → rendered
/// markdown table (via `md::table::render_markdown_table`).
pub fn decode_table_markdown(compressed: &[u8]) -> Result<String> {
    let doc = parse_table_document(compressed)?;
    crate::md::table::render_markdown_table(&grid_from_table_document(&doc)?).map_err(DocError::Invalid)
}

/// `parseTableDocument`.
pub fn parse_table_document(compressed: &[u8]) -> Result<TableDocument> {
    let raw = decompress_note_document(compressed)?;
    let wrapper = parse_versioned_document(&raw)?;
    let document = crdt::Document::decode(&wrapper.data)?;
    require_version(&document)?;
    assert_left_to_right(&document)?;
    let (cr_rows_ref, cr_columns_ref, cell_columns_ref) = resolve_table_refs(&document)?;
    Ok(TableDocument {
        wrapper,
        document,
        cr_rows_ref,
        cr_columns_ref,
        cell_columns_ref,
    })
}

/// `encodeTableDocument` (compressed).
pub fn encode_table_document(doc: &TableDocument) -> Result<Vec<u8>> {
    let mut wrapper = doc.wrapper.clone();
    let raw = encode_versioned_document(&mut wrapper, &doc.document.encode()?)?;
    Ok(compress_note_document(&raw))
}

/// `tableDocumentRoundTrips`.
pub fn table_document_round_trips(compressed: &[u8]) -> bool {
    let Ok(raw) = decompress_note_document(compressed) else {
        return false;
    };
    let reencoded = (|| -> Result<Vec<u8>> {
        let mut wrapper = parse_versioned_document(&raw)?;
        let document = crdt::Document::decode(&wrapper.data)?;
        require_version(&document)?;
        assert_left_to_right(&document)?;
        resolve_table_refs(&document)?;
        encode_versioned_document(&mut wrapper, &document.encode()?)
    })();
    reencoded.is_ok_and(|bytes| bytes == raw)
}

fn require_version(document: &crdt::Document) -> Result<()> {
    if document.version.is_none() {
        return fail("Table document is missing its version vector (CRDT.Document field 1)");
    }
    Ok(())
}

fn resolve_table_refs(document: &crdt::Document) -> Result<(u32, u32, u32)> {
    let Some(table_object) = document.object.first() else {
        return fail("Table object pool is empty");
    };
    let table = parse_dict_by_name(document, table_object, "table object")?;
    Ok((
        resolve_ref(require_entry(&table, "crRows")?, "crRows")?,
        resolve_ref(require_entry(&table, "crColumns")?, "crColumns")?,
        resolve_ref(require_entry(&table, "cellColumns")?, "cellColumns")?,
    ))
}

/// `gridFromTableDocument`: rows × columns of cell text, visual order.
pub fn grid_from_table_document(doc: &TableDocument) -> Result<Vec<Vec<String>>> {
    let resolved = resolve_table(doc)?;
    let num_rows = resolved.rows.len();
    let num_cols = resolved.columns.len();
    if num_cols == 0 {
        return fail("Table has no columns - refusing to guess at its structure");
    }
    let mut grid = vec![vec![String::new(); num_cols]; num_rows];
    for (row, cells) in grid.iter_mut().enumerate() {
        for (col, cell) in cells.iter_mut().enumerate() {
            if let Some(found) = resolved.cells.get(&(row, col)) {
                *cell = found.text.clone();
            }
        }
    }
    Ok(grid)
}

/// `resolveTable`.
pub fn resolve_table(doc: &TableDocument) -> Result<ResolvedTable> {
    let pool = &doc.document;
    let row_set = parse_ordered_set(pool, doc.cr_rows_ref)?;
    let column_set = parse_ordered_set(pool, doc.cr_columns_ref)?;
    let row_positions = compute_positions(pool, &row_set)?;
    let column_positions = compute_positions(pool, &column_set)?;
    let rows = row_set
        .array_uuid_indexes
        .iter()
        .map(|&i| identity_of(pool, i))
        .collect::<Result<Vec<_>>>()?;
    let columns = column_set
        .array_uuid_indexes
        .iter()
        .map(|&i| identity_of(pool, i))
        .collect::<Result<Vec<_>>>()?;

    let mut cells = HashMap::new();
    for (column_ref, row_map_ref) in parse_ref_pair_list(pool, doc.cell_columns_ref, "cellColumns")? {
        let column_uuid = uuid_index_of_ref(pool, resolve_ref(&column_ref, "cellColumns entry column ref")?)?;
        let Some(&column_position) = column_positions.get(&column_uuid) else {
            return fail("Table column reference does not resolve to a known column position");
        };
        let row_map = resolve_ref(&row_map_ref, "cellColumns entry row-map ref")?;
        for (row_ref, cell_text_ref) in parse_ref_pair_list(pool, row_map, "column row-map")? {
            let row_uuid = uuid_index_of_ref(pool, resolve_ref(&row_ref, "row-map entry row ref")?)?;
            let Some(&row_position) = row_positions.get(&row_uuid) else {
                return fail("Table row reference does not resolve to a known row position");
            };
            let text_ref = resolve_ref(&cell_text_ref, "row-map entry cell-text ref")?;
            let text = resolve_cell_text(pool, text_ref)?;
            cells.insert((row_position, column_position), TableCell { text_ref, text });
        }
    }
    Ok(ResolvedTable { rows, columns, cells })
}

/// `UUIDIndex` key position (`keyNames.indexOf`), -1 when absent.
pub(crate) fn key_index(pool: &crdt::Document, name: &str) -> i64 {
    pool.key_item.iter().position(|k| k == name).map_or(-1, |p| p as i64)
}

/// The UUID-table index an identity object carries, when `entry` is one
/// (a one-entry custom object keyed `key` with an inline unsigned value).
pub(crate) fn identity_uuid_index(entry: &DocObject, key: i64) -> Option<u64> {
    let custom = entry.custom.as_ref()?;
    if custom.map_entry.len() != 1 {
        return None;
    }
    let map_entry = &custom.map_entry[0];
    if i64::from(map_entry.key.unwrap_or(0)) != key {
        return None;
    }
    map_entry.value.as_ref()?.unsigned_integer_value
}

/// `identityOf`.
pub fn identity_of(pool: &crdt::Document, uuid_index: u64) -> Result<TableRowColumn> {
    let key = key_index(pool, "UUIDIndex");
    for (identity_ref, entry) in pool.object.iter().enumerate() {
        if identity_uuid_index(entry, key) == Some(uuid_index) {
            return Ok(TableRowColumn {
                identity_ref,
                uuid_index,
            });
        }
    }
    fail(format!(
        "Table array entry's UUID-table index {uuid_index} has no matching identity object in the pool"
    ))
}

fn assert_left_to_right(pool: &crdt::Document) -> Result<()> {
    let direction_key_index = (key_index(pool, "crTableColumnDirection") + 1) as usize;
    let Some(direction_key_name) = pool.key_item.get(direction_key_index) else {
        return fail("Table's key-name table is missing the column-direction marker's key");
    };
    for object in &pool.object {
        let Ok(dict) = parse_dict_by_name(pool, object, "direction candidate") else {
            continue;
        };
        if dict.len() != 1 {
            continue;
        }
        let Some(direction) = dict
            .get(direction_key_name.as_str())
            .and_then(|v| v.string_value.as_ref())
        else {
            continue;
        };
        if direction != LEFT_TO_RIGHT_DIRECTION {
            return fail(format!(
                "Table has unsupported column direction \"{direction}\" - refusing to guess at column order"
            ));
        }
        return Ok(());
    }
    fail("Table is missing its column-direction marker")
}

/// `parseDictByName`: a custom object's key-name → ObjectID pairs.
pub fn parse_dict_by_name<'a>(
    pool: &'a crdt::Document,
    entry: &'a DocObject,
    label: &str,
) -> Result<IndexMap<&'a str, &'a ObjectID>> {
    let Some(custom) = &entry.custom else {
        return fail(format!("Expected {label} to be a custom object (field 13)"));
    };
    let mut result = IndexMap::new();
    for pair in &custom.map_entry {
        let key = pair.key.unwrap_or(0);
        let name = usize::try_from(key).ok().and_then(|k| pool.key_item.get(k));
        if let (Some(name), Some(value)) = (name, &pair.value) {
            result.insert(name.as_str(), value);
        }
    }
    Ok(result)
}

/// `requireEntry`.
pub fn require_entry<'a>(dict: &IndexMap<&str, &'a ObjectID>, key: &str) -> Result<&'a ObjectID> {
    match dict.get(key) {
        Some(value) => Ok(value),
        None => fail(format!("Table object is missing expected key \"{key}\"")),
    }
}

/// `resolveRef`.
pub fn resolve_ref(object_id: &ObjectID, label: &str) -> Result<u32> {
    match object_id.object_index {
        Some(index) => Ok(index),
        None => fail(format!("Expected {label} to be a pool reference")),
    }
}

fn resolve_number(object_id: &ObjectID, label: &str) -> Result<u64> {
    match object_id.unsigned_integer_value {
        Some(value) => Ok(value),
        None => fail(format!("Expected {label} to be an inline number")),
    }
}

fn object_at(pool: &crdt::Document, pool_ref: u32) -> Result<&DocObject> {
    match pool.object.get(pool_ref as usize) {
        Some(entry) => Ok(entry),
        None => fail(format!("Table pool reference {pool_ref} is out of range")),
    }
}

/// `uuidIndexOfRef`.
pub fn uuid_index_of_ref(pool: &crdt::Document, pool_ref: u32) -> Result<u64> {
    let entry = object_at(pool, pool_ref)?;
    let dict = parse_dict_by_name(pool, entry, &format!("pool[{pool_ref}] identity object"))?;
    resolve_number(
        require_entry(&dict, "UUIDIndex")?,
        &format!("pool[{pool_ref}] UUIDIndex"),
    )
}

fn find_uuid_table_index(pool: &crdt::Document, uuid: &[u8]) -> Result<u64> {
    match pool.uuid_item.iter().position(|candidate| candidate == uuid) {
        Some(index) => Ok(index as u64),
        None => fail("Table array entry's UUID was not found in the document UUID table"),
    }
}

/// `parseOrderedSet`.
pub fn parse_ordered_set(pool: &crdt::Document, pool_ref: u32) -> Result<ParsedOrderedSet> {
    let entry = object_at(pool, pool_ref)?;
    let Some(ordered_set) = &entry.ts_ordered_set else {
        return fail(format!("Expected pool[{pool_ref}] to be an OrderedSet (field 16)"));
    };
    let Some(ordering) = &ordered_set.array else {
        return fail("OrderedSet is missing its array (ordering) field");
    };
    let Some(string_array) = &ordering.array else {
        return fail("OrderedSet ordering is missing its string-array field");
    };
    let array_uuid_indexes = string_array
        .attachments
        .iter()
        .map(|a| find_uuid_table_index(pool, a.contents.as_deref().unwrap_or(&[])))
        .collect::<Result<Vec<_>>>()?;
    let mut contents = Vec::new();
    if let Some(dictionary) = &ordering.dictionary {
        for pair in &dictionary.element {
            let (Some(key), Some(value)) = (&pair.key, &pair.value) else {
                return fail("OrderedSet redirect pair is missing its key or value");
            };
            contents.push((resolve_ref(key, "redirect key")?, resolve_ref(value, "redirect value")?));
        }
    }
    Ok(ParsedOrderedSet {
        array_uuid_indexes,
        contents,
    })
}

/// `computePositions`: UUID-table index → visual position, redirects followed.
pub fn compute_positions(pool: &crdt::Document, ordered_set: &ParsedOrderedSet) -> Result<HashMap<u64, usize>> {
    let mut positions = HashMap::new();
    for (position, &uuid_index) in ordered_set.array_uuid_indexes.iter().enumerate() {
        positions.insert(uuid_index, position);
    }
    for &(key_ref, value_ref) in &ordered_set.contents {
        if let Some(&position) = positions.get(&uuid_index_of_ref(pool, key_ref)?) {
            positions.insert(uuid_index_of_ref(pool, value_ref)?, position);
        }
    }
    Ok(positions)
}

/// `parseRefPairList`: a dictionary object's `(key, value)` pairs.
pub fn parse_ref_pair_list(pool: &crdt::Document, pool_ref: u32, label: &str) -> Result<Vec<(ObjectID, ObjectID)>> {
    let entry = object_at(pool, pool_ref)?;
    let Some(dictionary) = &entry.dictionary else {
        return fail(format!(
            "Expected {label} (pool[{pool_ref}]) to be a dictionary (field 6)"
        ));
    };
    dictionary
        .element
        .iter()
        .map(|element| match (&element.key, &element.value) {
            (Some(a), Some(b)) => Ok((a.clone(), b.clone())),
            _ => fail(format!("{label} entry is missing one of its two references")),
        })
        .collect()
}

/// `resolveCellText`.
pub fn resolve_cell_text(pool: &crdt::Document, pool_ref: u32) -> Result<String> {
    let entry = object_at(pool, pool_ref)?;
    match &entry.string {
        Some(s) => Ok(s.string.clone().unwrap_or_default()),
        None => fail(format!("Expected pool[{pool_ref}] to be a cell-text object (field 10)")),
    }
}

/// `pool.keyNames.indexOf(name)` (-1 when absent).
pub fn key_index_of(doc: &TableDocument, name: &str) -> i64 {
    key_index(&doc.document, name)
}
