//! Table writes: grid diff → CRDT operations. Ports icloud-md
//! `src/notes/tableEdit.ts`, `tableCellEdit.ts` and `tablePushEdit.ts`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde_json::Value;

use super::Result;
use super::document::{
    RunCoord, TextRun, adjust_attribute_runs, compare_bytes, compute_splice16, encode_text_run, find_insert_index,
    for_each_visible_piece, initial_runs, insert_run_at, parse_text_run, validate_child_edges,
};
use super::proto::crdt::document::custom_object::MapEntry;
use super::proto::crdt::document::{CustomObject, DocObject};
use super::proto::crdt::string_array::ArrayAttachment;
use super::proto::crdt::vector_timestamp::Element;
use super::proto::crdt::{self, Dictionary, ObjectID, OrderedSet, VectorTimestamp};
use super::proto::topotext::vector_timestamp::{Clock, clock::ReplicaClock};
use super::proto::topotext::{self, AttributeRun};
use super::tables::{
    TableDocument, compute_positions, encode_table_document, fail, grid_from_table_document, identity_uuid_index,
    key_index, parse_ordered_set, parse_ref_pair_list, parse_table_document, resolve_ref, resolve_table,
    table_document_round_trips, uuid_index_of_ref,
};
use crate::cloudkit::CloudKitRecord;
use crate::js::{base64_decode, base64_encode, len16, utf16};

const IDENTITY_TYPE_NAME: &str = "com.apple.CRDT.NSUUID";
const TOMBSTONE_STYLE_CLOCK_BIAS: u64 = 8;
const ORC: &str = "\u{FFFC}";

/// A 16-byte random source (`randomBytes(16)`), injectable for tests.
pub type RandomSource<'a> = &'a mut dyn FnMut() -> [u8; 16];

/// `randomBytes(16)`, through `vault::rt` (the differential harness's
/// deterministic sequence).
fn random_uuid_bytes() -> [u8; 16] {
    crate::vault::rt::random_16()
}

// --- tablePushEdit ------------------------------------------------------------

/// `TableAttachmentUpdateResult` when `ok`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAttachmentUpdate {
    pub changed: bool,
    pub mergeable_data_base64: String,
}

/// `prepareTableAttachmentUpdate`; `Err(reason)` is `{ok: false, reason}`.
pub fn prepare_table_attachment_update(
    record: &CloudKitRecord,
    desired_grid: &[Vec<String>],
    replica_id: &[u8; 16],
) -> std::result::Result<TableAttachmentUpdate, String> {
    prepare_table_attachment_update_with(record, desired_grid, replica_id, &mut random_uuid_bytes)
}

/// `prepareTableAttachmentUpdate` with an injectable source for the identity
/// UUIDs row/column inserts mint.
pub fn prepare_table_attachment_update_with(
    record: &CloudKitRecord,
    desired_grid: &[Vec<String>],
    replica_id: &[u8; 16],
    random: RandomSource,
) -> std::result::Result<TableAttachmentUpdate, String> {
    let Some(Value::String(value)) = record.fields.get("MergeableDataEncrypted").map(|f| &f.value) else {
        return Err("table attachment has no readable data".into());
    };
    let compressed = base64_decode(value);
    if !table_document_round_trips(&compressed) {
        return Err(
            "the table's document doesn't round-trip byte-for-byte through our model - refusing to edit".into(),
        );
    }
    let mut attempt = || -> Result<std::result::Result<TableAttachmentUpdate, String>> {
        let mut doc = parse_table_document(&compressed)?;
        if !apply_table_edit_with(&mut doc, desired_grid, replica_id, random)? {
            return Ok(Ok(TableAttachmentUpdate {
                changed: false,
                mergeable_data_base64: value.clone(),
            }));
        }
        let encoded = encode_table_document(&doc)?;
        if !table_document_round_trips(&encoded) {
            return Ok(Err(
                "the edited table failed its own round-trip gate - refusing to write it".into(),
            ));
        }
        Ok(Ok(TableAttachmentUpdate {
            changed: true,
            mergeable_data_base64: base64_encode(&encoded),
        }))
    };
    attempt().unwrap_or_else(|cause| Err(cause.to_string()))
}

// --- grid diffing --------------------------------------------------------------

/// `CellEdit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEdit {
    pub row: usize,
    pub column: usize,
    pub text: String,
}

/// `TableEditPlan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableEditPlan {
    Noop,
    CellEdits(Vec<CellEdit>),
    InsertRows {
        position: usize,
        rows: Vec<Vec<String>>,
    },
    DeleteRows {
        position: usize,
        count: usize,
    },
    /// Each entry is one new column's cells, top to bottom.
    InsertColumns {
        position: usize,
        columns: Vec<Vec<String>>,
    },
    DeleteColumns {
        position: usize,
        count: usize,
    },
    Unsupported(String),
}

/// `diffTableGrid`.
pub fn diff_table_grid(current: &[Vec<String>], desired: &[Vec<String>]) -> TableEditPlan {
    if current == desired {
        return TableEditPlan::Noop;
    }
    let current_cols = current.first().map_or(0, Vec::len);
    let desired_cols = desired.first().map_or(0, Vec::len);
    let row_count_changed = current.len() != desired.len();
    let col_count_changed = current_cols != desired_cols;

    if row_count_changed && col_count_changed {
        return TableEditPlan::Unsupported(
            "both row and column counts changed in the same edit - can't safely resolve this as one structural operation".into(),
        );
    }
    if !row_count_changed && !col_count_changed {
        if is_pure_reorder(current, desired) {
            return TableEditPlan::Unsupported(
                "rows or columns were reordered without anything added or removed - not supported in one edit (split it into a delete push and an insert push)".into(),
            );
        }
        let mut edits = Vec::new();
        for (r, row) in current.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if let Some(desired_cell) = desired.get(r).and_then(|d| d.get(c))
                    && desired_cell != cell
                {
                    edits.push(CellEdit {
                        row: r,
                        column: c,
                        text: desired_cell.clone(),
                    });
                }
            }
        }
        return if edits.is_empty() {
            TableEditPlan::Noop
        } else {
            TableEditPlan::CellEdits(edits)
        };
    }
    if row_count_changed {
        let Some((start, delete_count, inserted)) = compute_axis_splice(current, desired) else {
            return TableEditPlan::Unsupported(
                "row insertion/deletion couldn't be resolved to a single contiguous change".into(),
            );
        };
        return if delete_count > 0 {
            TableEditPlan::DeleteRows {
                position: start,
                count: delete_count,
            }
        } else {
            TableEditPlan::InsertRows {
                position: start,
                rows: inserted,
            }
        };
    }
    let Some((start, delete_count, inserted)) = compute_axis_splice(&transpose(current), &transpose(desired)) else {
        return TableEditPlan::Unsupported(
            "column insertion/deletion couldn't be resolved to a single contiguous change".into(),
        );
    };
    if delete_count > 0 {
        TableEditPlan::DeleteColumns {
            position: start,
            count: delete_count,
        }
    } else {
        TableEditPlan::InsertColumns {
            position: start,
            columns: inserted,
        }
    }
}

fn compute_axis_splice(current: &[Vec<String>], desired: &[Vec<String>]) -> Option<(usize, usize, Vec<Vec<String>>)> {
    let min_len = current.len().min(desired.len());
    let mut prefix = 0;
    while prefix < min_len && current[prefix] == desired[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    let max_suffix = min_len - prefix;
    while suffix < max_suffix && current[current.len() - 1 - suffix] == desired[desired.len() - 1 - suffix] {
        suffix += 1;
    }
    if prefix + suffix != min_len {
        return None;
    }
    let delete_count = current.len() - prefix - suffix;
    let insert_count = desired.len() - prefix - suffix;
    Some((prefix, delete_count, desired[prefix..prefix + insert_count].to_vec()))
}

fn transpose(grid: &[Vec<String>]) -> Vec<Vec<String>> {
    let cols = grid.first().map_or(0, Vec::len);
    (0..cols)
        .map(|c| grid.iter().map(|row| row.get(c).cloned().unwrap_or_default()).collect())
        .collect()
}

fn is_pure_reorder(current: &[Vec<String>], desired: &[Vec<String>]) -> bool {
    rows_multiset_equal(current, desired) || rows_multiset_equal(&transpose(current), &transpose(desired))
}

fn rows_multiset_equal(a: &[Vec<String>], b: &[Vec<String>]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut counts: HashMap<&Vec<String>, usize> = HashMap::new();
    for row in b {
        *counts.entry(row).or_default() += 1;
    }
    for row in a {
        match counts.get_mut(row) {
            Some(count) if *count > 0 => *count -= 1,
            _ => return false,
        }
    }
    true
}

// --- tableCellEdit ----------------------------------------------------------------

/// The session's clock position: the document's `ttTimestamp` entry for
/// our replica (index `tt_index`; `replica_index` is its 1-based rank).
#[derive(Debug, Clone, Copy)]
struct TextClock {
    replica_index: u32,
    tt_index: usize,
    style_floor: u64,
}

/// `TopotextClockSource`: a table's document-global topotext clock, scoped
/// to the writing replica (`tableCellEdit.ts`).
pub trait TopotextClockSource {
    /// 1-based `CharID.replicaID` of the writing replica.
    fn replica_index(&self) -> u32;
    /// `take(units)`: the first clock of a fresh run; advances the text clock.
    fn take(&mut self, units: u32) -> u32;
    /// `takeTombstoneAnchor(previousClock)`: `max(previous + 8, floor)`,
    /// advancing the style clock past it.
    fn take_tombstone_anchor(&mut self, previous_clock: u32) -> Result<u32>;
}

/// The write session's clock: `TextClock` over the live `ttTimestamp`.
struct SessionClock<'a> {
    clock: TextClock,
    tt: &'a mut topotext::VectorTimestamp,
}

impl SessionClock<'_> {
    fn entry(&mut self) -> &mut Clock {
        &mut self.tt.clock[self.clock.tt_index]
    }
}

impl TopotextClockSource for SessionClock<'_> {
    fn replica_index(&self) -> u32 {
        self.clock.replica_index
    }

    fn take(&mut self, units: u32) -> u32 {
        let counter = &mut self.entry().replica_clock[0];
        let value = counter.clock.unwrap_or(0);
        counter.clock = Some(value.wrapping_add(units));
        value
    }

    fn take_tombstone_anchor(&mut self, previous_clock: u32) -> Result<u32> {
        let floor = self.clock.style_floor;
        let Some(style) = self.entry().replica_clock.get_mut(1) else {
            return fail("Table's topotext clock entry is missing its style clock - refusing to guess");
        };
        let assigned = (u64::from(previous_clock) + TOMBSTONE_STYLE_CLOCK_BIAS).max(floor);
        style.clock = Some(u64::from(style.clock.unwrap_or(0)).max(assigned + 1) as u32);
        Ok(assigned as u32)
    }
}

/// `TableCellDocument`.
#[derive(Debug, Clone, PartialEq)]
pub struct TableCellDocument {
    pub text: String,
    pub runs: Vec<TextRun>,
    pub attribute_runs: Vec<AttributeRun>,
}

/// `parseCellDocument`.
pub fn parse_cell_document(s: &topotext::String) -> Result<TableCellDocument> {
    Ok(TableCellDocument {
        text: s.string.clone().unwrap_or_default(),
        runs: s.substring.iter().map(parse_text_run).collect::<Result<Vec<_>>>()?,
        attribute_runs: s.attribute_run.clone(),
    })
}

/// `encodeCellDocument` (no clock table: cells draw on the document's).
pub fn encode_cell_document(doc: &TableCellDocument) -> topotext::String {
    topotext::String {
        string: Some(doc.text.clone()),
        substring: doc.runs.iter().map(encode_text_run).collect(),
        attribute_run: doc.attribute_runs.clone(),
        ..Default::default()
    }
}

/// `newCellDocument`.
pub fn new_cell_document() -> TableCellDocument {
    TableCellDocument {
        text: String::new(),
        runs: initial_runs(),
        attribute_runs: Vec::new(),
    }
}

fn visible_length(runs: &[TextRun]) -> u64 {
    runs.iter().filter(|r| !r.tombstone).map(|r| u64::from(r.length)).sum()
}

/// `applyCellTextEdit`: one splice (`computeSplice`), clocks from `clock`.
pub fn apply_cell_text_edit(
    cell: &mut TableCellDocument,
    new_text: &str,
    clock: &mut dyn TopotextClockSource,
) -> Result<bool> {
    if cell.text == new_text {
        return Ok(false);
    }
    validate_cell_invariants(cell)?;
    let (start, delete_length, insert) = compute_splice16(&utf16(&cell.text), &utf16(new_text));
    if delete_length > 0 {
        cell_tombstone_visible_range(cell, start, delete_length, clock)?;
    }
    if !insert.is_empty() {
        cell_insert_visible_text(cell, start, insert.len(), clock)?;
    }
    adjust_attribute_runs(
        &mut cell.attribute_runs,
        start,
        delete_length,
        insert.len(),
        false,
        "Cell attribute runs are shorter than the deleted range - document model out of sync",
    )?;
    cell.text = new_text.to_string();
    validate_cell_invariants(cell)?;
    Ok(true)
}

/// `tombstoneVisibleRange` (cells and ordering mirrors).
pub fn cell_tombstone_visible_range(
    cell: &mut TableCellDocument,
    start: usize,
    length: usize,
    clock: &mut dyn TopotextClockSource,
) -> Result<()> {
    let end = start + length;
    if end as u64 > visible_length(&cell.runs) {
        return fail("Cell tombstone range extends past the end of its visible text - CRDT model out of sync");
    }
    for_each_visible_piece(&mut cell.runs, start, end, |target| {
        let previous = target.anchor.clock;
        target.tombstone = true;
        target.anchor = RunCoord {
            replica: clock.replica_index(),
            clock: clock.take_tombstone_anchor(previous)?,
        };
        Ok(())
    })
}

/// `insertVisibleText` (cells and ordering mirrors): a new run under the
/// caller's replica at visible position `start`.
pub fn cell_insert_visible_text(
    cell: &mut TableCellDocument,
    start: usize,
    length: usize,
    clock: &mut dyn TopotextClockSource,
) -> Result<()> {
    if start as u64 > visible_length(&cell.runs) {
        return fail("Cell insertion point is past the end of its visible text - CRDT model out of sync");
    }
    let insert_index = find_insert_index(&mut cell.runs, start)?;
    let coord = RunCoord {
        replica: clock.replica_index(),
        clock: clock.take(length as u32),
    };
    insert_run_at(
        &mut cell.runs,
        insert_index,
        TextRun {
            coord,
            length: length as u32,
            anchor: RunCoord {
                replica: clock.replica_index(),
                clock: 0,
            },
            tombstone: false,
            sequence: Vec::new(),
        },
    );
    Ok(())
}

/// `validateCellInvariants`.
pub fn validate_cell_invariants(cell: &TableCellDocument) -> Result<()> {
    let text_length = len16(&cell.text) as u64;
    let visible = visible_length(&cell.runs);
    if visible != text_length {
        return fail(format!(
            "Cell's visible run lengths ({visible}) do not match its text length ({text_length}) - refusing to touch this cell"
        ));
    }
    let attribute_length: u64 = cell.attribute_runs.iter().map(|r| u64::from(r.len())).sum();
    if attribute_length != text_length {
        return fail(format!(
            "Cell's attribute run lengths ({attribute_length}) do not match its text length ({text_length}) - refusing to touch this cell"
        ));
    }
    validate_child_edges(&cell.runs, "cell")
}

// --- the write session ------------------------------------------------------------

struct Session {
    replica_index: usize,
    base_clock: u64,
    highest_tick: u64,
    text_clock: TextClock,
}

impl Session {
    fn element_stamp(&mut self) -> (u64, u64) {
        self.highest_tick = self.highest_tick.max(self.base_clock + 2);
        (self.replica_index as u64, self.base_clock + 2)
    }

    fn mark_structural_save(&mut self) {
        self.highest_tick = self.highest_tick.max(self.base_clock + 2);
    }
}

fn session_clock<'a>(doc: &'a mut TableDocument, session: &Session) -> SessionClock<'a> {
    SessionClock {
        clock: session.text_clock,
        tt: tt_mut(doc),
    }
}

fn tt_mut(doc: &mut TableDocument) -> &mut topotext::VectorTimestamp {
    doc.document.tt_timestamp.get_or_insert_with(Default::default)
}

fn version_mut(doc: &mut TableDocument) -> &mut VectorTimestamp {
    doc.document.version.get_or_insert_with(Default::default)
}

fn uuid_of(clock: &Clock) -> &[u8] {
    clock.replica_uuid.as_deref().unwrap_or(&[])
}

/// Every mergeable string of the document (cells and ordering mirrors).
fn for_each_string(doc: &mut TableDocument, mut visit: impl FnMut(&mut topotext::String) -> Result<()>) -> Result<()> {
    for entry in &mut doc.document.object {
        if let Some(s) = &mut entry.string {
            visit(s)?;
        }
        if let Some(contents) = entry
            .ts_ordered_set
            .as_mut()
            .and_then(|o| o.array.as_mut())
            .and_then(|a| a.array.as_mut())
            .and_then(|a| a.contents.as_mut())
        {
            visit(contents)?;
        }
    }
    Ok(())
}

fn remap_char_ids(doc: &mut TableDocument, map: impl Fn(u32) -> Result<u32>) -> Result<()> {
    for_each_string(doc, |s| {
        for run in &mut s.substring {
            for id in [&mut run.char_id, &mut run.timestamp].into_iter().flatten() {
                let replica = id.replica_id.unwrap_or(0);
                if replica != 0 {
                    id.replica_id = Some(map(replica)?);
                }
            }
        }
        Ok(())
    })
}

fn normalize_topotext_order(doc: &mut TableDocument, replica_uuid: &[u8; 16]) -> Result<()> {
    let Some(tt) = &doc.document.tt_timestamp else {
        return Ok(());
    };
    if tt.clock.len() < 2 {
        return Ok(());
    }
    let mut order: Vec<usize> = (0..tt.clock.len()).collect();
    order.sort_by(|&a, &b| compare_bytes(uuid_of(&tt.clock[a]), uuid_of(&tt.clock[b])).cmp(&0));
    if order.iter().enumerate().all(|(new, &old)| new == old) {
        return Ok(());
    }
    if !tt.clock.iter().any(|c| uuid_of(c) == replica_uuid) {
        return fail(
            "Table's topotext clock table isn't in sorted UUID order and isn't this tool's own residue - refusing to edit",
        );
    }
    let old_to_new: HashMap<u32, u32> = order
        .iter()
        .enumerate()
        .map(|(new, &old)| (old as u32 + 1, new as u32 + 1))
        .collect();
    let tt = tt_mut(doc);
    let reordered: Vec<Clock> = order.iter().map(|&old| tt.clock[old].clone()).collect();
    tt.clock = reordered;
    remap_char_ids(doc, |old| match old_to_new.get(&old) {
        Some(&next) => Ok(next),
        None => fail(format!(
            "Table run references topotext replica {old}, outside the clock table - refusing to edit"
        )),
    })
}

fn require_key_index(doc: &TableDocument, name: &str) -> Result<i64> {
    let index = key_index(&doc.document, name);
    if index == -1 {
        return fail(format!(
            "Table's key-name table is missing \"{name}\" - refusing to guess its index"
        ));
    }
    Ok(index)
}

fn begin_write_session(doc: &mut TableDocument, replica_uuid: &[u8; 16]) -> Result<Session> {
    let replica_count = doc.version().element.len();
    if replica_count == 0 {
        return fail(
            "Table document has an empty version vector - refusing to edit a document shaped like nothing ever captured",
        );
    }
    for (i, element) in doc.version().element.iter().enumerate() {
        let claimed = element.replica_index.unwrap_or(0);
        if claimed != i as u64 {
            return fail(format!(
                "Table version vector element {i} claims replica {claimed} - refusing to edit"
            ));
        }
    }
    if doc.uuid_table().len() < replica_count {
        return fail("Table UUID table is shorter than its version vector - refusing to edit");
    }
    let replica_index = match doc.uuid_table()[..replica_count].iter().position(|u| u == replica_uuid) {
        Some(index) => index,
        None => {
            doc.document.uuid_item.insert(replica_count, replica_uuid.to_vec());
            shift_identity_uuid_indexes(doc, replica_count as u64)?;
            version_mut(doc).element.push(Element {
                replica_index: Some(replica_count as u64),
                clock: Some(0),
                subclock: Some(0),
                ..Default::default()
            });
            replica_count
        }
    };
    let base_clock = doc.version().element[replica_index].clock.unwrap_or(0);

    if doc.document.tt_timestamp.is_none() {
        return fail("Table document has no topotext clock table (ttTimestamp) - refusing to edit");
    }
    let existing = doc
        .document
        .tt_timestamp
        .as_ref()
        .and_then(|tt| tt.clock.iter().position(|c| uuid_of(c) == replica_uuid));
    let tt_index = match existing {
        Some(index) => index,
        None => {
            let tt = doc.document.tt_timestamp.as_ref().map_or(&[][..], |tt| &tt.clock[..]);
            let position = tt
                .iter()
                .filter(|c| compare_bytes(uuid_of(c), replica_uuid) < 0)
                .count();
            if position < tt.len() {
                let threshold = position as u32 + 1;
                remap_char_ids(doc, |old| Ok(if old >= threshold { old + 1 } else { old }))?;
            }
            tt_mut(doc).clock.insert(
                position,
                Clock {
                    replica_uuid: Some(replica_uuid.to_vec()),
                    replica_clock: vec![
                        ReplicaClock {
                            clock: Some(0),
                            ..Default::default()
                        },
                        ReplicaClock {
                            clock: Some(1),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            );
            position
        }
    };
    let clock = &tt_mut(doc).clock[tt_index];
    if clock.replica_clock.is_empty() {
        return fail("Table's topotext clock entry has no text counter - refusing to guess");
    }
    let style_floor = clock
        .replica_clock
        .get(1)
        .map_or(0, |c| u64::from(c.clock.unwrap_or(0)));
    Ok(Session {
        replica_index,
        base_clock,
        highest_tick: 0,
        text_clock: TextClock {
            replica_index: tt_index as u32 + 1,
            tt_index,
            style_floor,
        },
    })
}

fn finalize_write_session(doc: &mut TableDocument, session: &Session) {
    let clock = session.highest_tick.max(session.base_clock + 1);
    version_mut(doc).element[session.replica_index].clock = Some(clock);
}

fn shift_identity_uuid_indexes(doc: &mut TableDocument, inserted_at: u64) -> Result<()> {
    let key = require_key_index(doc, "UUIDIndex")?;
    for entry in &mut doc.document.object {
        let Some(current) = identity_uuid_index(entry, key) else {
            continue;
        };
        if current < inserted_at {
            return fail(format!(
                "Table identity object references UUID-table index {current}, inside the replica segment ({inserted_at}) - refusing to edit"
            ));
        }
        if let Some(value) = entry
            .custom
            .as_mut()
            .and_then(|c| c.map_entry.first_mut())
            .and_then(|m| m.value.as_mut())
        {
            value.unsigned_integer_value = Some(current + 1);
        }
    }
    Ok(())
}

// --- applying a plan -------------------------------------------------------------

/// `applyTableEdit` with an injectable random source (called twice per
/// inserted row/column: content identity, then ordering identity).
pub fn apply_table_edit_with(
    doc: &mut TableDocument,
    desired_grid: &[Vec<String>],
    replica_uuid: &[u8; 16],
    random: RandomSource,
) -> Result<bool> {
    let num_columns = desired_grid.first().map_or(0, Vec::len);
    if desired_grid.is_empty() || num_columns == 0 {
        return fail("Cannot edit a table down to no rows or no columns - delete the table from the note instead");
    }
    if desired_grid.iter().any(|row| row.len() != num_columns) {
        return fail("Every row of an edited table must have the same number of columns");
    }

    normalize_topotext_order(doc, replica_uuid)?;
    validate_table_document_invariants(doc)?;
    let plan = diff_table_grid(&grid_from_table_document(doc)?, desired_grid);
    match &plan {
        TableEditPlan::Noop => return Ok(false),
        TableEditPlan::Unsupported(reason) => return fail(reason.clone()),
        _ => {}
    }

    let mut session = begin_write_session(doc, replica_uuid)?;
    match plan {
        TableEditPlan::CellEdits(edits) => apply_cell_edits(doc, &session, &edits)?,
        TableEditPlan::InsertRows { position, rows } => {
            for (offset, cells) in rows.iter().enumerate() {
                insert_row_at(doc, &mut session, position + offset, cells, random)?;
            }
        }
        TableEditPlan::DeleteRows { position, count } => {
            for _ in 0..count {
                delete_row_at(doc, &mut session, position)?;
            }
        }
        TableEditPlan::InsertColumns { position, columns } => {
            for (offset, cells) in columns.iter().enumerate() {
                insert_column_at(doc, &mut session, position + offset, cells, random)?;
            }
        }
        TableEditPlan::DeleteColumns { position, count } => {
            for _ in 0..count {
                delete_column_at(doc, &mut session, position)?;
            }
        }
        TableEditPlan::Noop | TableEditPlan::Unsupported(_) => {}
    }
    finalize_write_session(doc, &session);

    validate_table_document_invariants(doc)?;
    if grid_from_table_document(doc)? != desired_grid {
        return fail("Table edit did not produce the desired grid - refusing to write the result");
    }
    Ok(true)
}

fn apply_cell_edits(doc: &mut TableDocument, session: &Session, edits: &[CellEdit]) -> Result<()> {
    let resolved = resolve_table(doc)?;
    for edit in edits {
        if edit.row >= resolved.rows.len() || edit.column >= resolved.columns.len() {
            return fail(format!(
                "Cell edit references row {}/column {} outside the table",
                edit.row, edit.column
            ));
        }
        let Some(cell) = resolved.cells.get(&(edit.row, edit.column)) else {
            return fail(format!("No cell found at row {}, column {}", edit.row, edit.column));
        };
        let Some(string) = doc
            .document
            .object
            .get(cell.text_ref as usize)
            .and_then(|e| e.string.as_ref())
        else {
            return fail(format!("Table pool[{}] is not a cell-text object", cell.text_ref));
        };
        let mut cell_doc = parse_cell_document(string)?;
        apply_cell_text_edit(&mut cell_doc, &edit.text, &mut session_clock(doc, session))?;
        doc.document.object[cell.text_ref as usize].string = Some(encode_cell_document(&cell_doc));
    }
    Ok(())
}

struct IdentityPair {
    ordering_ref: u32,
    content_ref: u32,
    ordering_uuid: Vec<u8>,
}

fn ref_to(index: u32) -> ObjectID {
    ObjectID {
        object_index: Some(index),
        ..Default::default()
    }
}

fn stamped_element(key_ref: u32, value_ref: u32, stamp: (u64, u64)) -> crdt::dictionary::Element {
    crdt::dictionary::Element {
        key: Some(ref_to(key_ref)),
        value: Some(ref_to(value_ref)),
        timestamp: Some(VectorTimestamp {
            element: vec![Element {
                replica_index: Some(stamp.0),
                clock: Some(stamp.1),
                subclock: Some(0),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn push_object(doc: &mut TableDocument, entry: DocObject) -> u32 {
    doc.document.object.push(entry);
    doc.document.object.len() as u32 - 1
}

fn mint_identity_pair(doc: &mut TableDocument, random: RandomSource) -> Result<IdentityPair> {
    let key = require_key_index(doc, "UUIDIndex")?;
    let Some(identity_type) = doc.document.type_item.iter().position(|t| t == IDENTITY_TYPE_NAME) else {
        return fail(format!(
            "Table's type table is missing \"{IDENTITY_TYPE_NAME}\" - refusing to guess an identity object type"
        ));
    };
    let mut mint = |doc: &mut TableDocument| {
        let uuid = random().to_vec();
        doc.document.uuid_item.push(uuid.clone());
        let uuid_index = doc.document.uuid_item.len() as u64 - 1;
        let reference = push_object(
            doc,
            DocObject {
                custom: Some(CustomObject {
                    type_: Some(identity_type as i32),
                    map_entry: vec![MapEntry {
                        key: Some(key as i32),
                        value: Some(ObjectID {
                            unsigned_integer_value: Some(uuid_index),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        (reference, uuid)
    };
    let (content_ref, _) = mint(doc);
    let (ordering_ref, ordering_uuid) = mint(doc);
    Ok(IdentityPair {
        ordering_ref,
        content_ref,
        ordering_uuid,
    })
}

fn require_dictionary<'a>(doc: &'a mut TableDocument, pool_ref: u32, label: &str) -> Result<&'a mut Dictionary> {
    match doc
        .document
        .object
        .get_mut(pool_ref as usize)
        .and_then(|e| e.dictionary.as_mut())
    {
        Some(dictionary) => Ok(dictionary),
        None => fail(format!("Expected {label} (pool[{pool_ref}]) to be a dictionary")),
    }
}

fn require_ordered_set(doc: &mut TableDocument, pool_ref: u32) -> Result<&mut OrderedSet> {
    let well_formed = doc
        .document
        .object
        .get(pool_ref as usize)
        .and_then(|e| e.ts_ordered_set.as_ref())
        .is_some_and(|o| {
            o.array
                .as_ref()
                .is_some_and(|a| a.array.as_ref().is_some_and(|s| s.contents.is_some()) && a.dictionary.is_some())
                && o.set.is_some()
        });
    if !well_formed {
        return fail(format!("Table pool[{pool_ref}] is not a well-formed OrderedSet"));
    }
    doc.document.object[pool_ref as usize]
        .ts_ordered_set
        .as_mut()
        .ok_or_else(|| super::DocError::Invalid(format!("Table pool[{pool_ref}] is not a well-formed OrderedSet")))
}

fn cell_text_of(cells: &[String], position: usize) -> &str {
    cells.get(position).map_or("", String::as_str)
}

fn insert_row_at(
    doc: &mut TableDocument,
    session: &mut Session,
    position: usize,
    cells: &[String],
    random: RandomSource,
) -> Result<()> {
    if position
        > parse_ordered_set(&doc.document, doc.cr_rows_ref)?
            .array_uuid_indexes
            .len()
    {
        return fail(format!("Row insertion position {position} is out of bounds"));
    }
    let stamp = session.element_stamp();
    let column_positions = compute_positions(&doc.document, &parse_ordered_set(&doc.document, doc.cr_columns_ref)?)?;
    let pair = mint_identity_pair(doc, random)?;
    insert_into_ordered_set(doc, session, doc.cr_rows_ref, position, &pair, stamp)?;

    for (column_ref, row_map_ref) in parse_ref_pair_list(&doc.document, doc.cell_columns_ref, "cellColumns")? {
        let column_uuid = uuid_index_of_ref(&doc.document, resolve_ref(&column_ref, "cellColumns column ref")?)?;
        let Some(&column_position) = column_positions.get(&column_uuid) else {
            return fail("Table column reference does not resolve to a known column position");
        };
        let cell_ref = push_cell_text(doc, session, cell_text_of(cells, column_position))?;
        let row_map = resolve_ref(&row_map_ref, "cellColumns row-map ref")?;
        require_dictionary(doc, row_map, "column row-map")?
            .element
            .push(stamped_element(pair.content_ref, cell_ref, stamp));
    }
    Ok(())
}

fn insert_column_at(
    doc: &mut TableDocument,
    session: &mut Session,
    position: usize,
    cells: &[String],
    random: RandomSource,
) -> Result<()> {
    if position
        > parse_ordered_set(&doc.document, doc.cr_columns_ref)?
            .array_uuid_indexes
            .len()
    {
        return fail(format!("Column insertion position {position} is out of bounds"));
    }
    let stamp = session.element_stamp();
    let row_content_refs = content_identity_refs_by_row_position(doc)?;
    let pair = mint_identity_pair(doc, random)?;
    insert_into_ordered_set(doc, session, doc.cr_columns_ref, position, &pair, stamp)?;

    let mut elements = Vec::with_capacity(row_content_refs.len());
    for (row_position, &row_content_ref) in row_content_refs.iter().enumerate() {
        let cell_ref = push_cell_text(doc, session, cell_text_of(cells, row_position))?;
        elements.push(stamped_element(row_content_ref, cell_ref, stamp));
    }
    let row_map_ref = push_object(
        doc,
        DocObject {
            dictionary: Some(Dictionary {
                element: elements,
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let cell_columns_ref = doc.cell_columns_ref;
    require_dictionary(doc, cell_columns_ref, "cellColumns")?
        .element
        .push(stamped_element(pair.content_ref, row_map_ref, stamp));
    Ok(())
}

fn content_identity_refs_by_row_position(doc: &TableDocument) -> Result<Vec<u32>> {
    let pool = &doc.document;
    let row_positions = compute_positions(pool, &parse_ordered_set(pool, doc.cr_rows_ref)?)?;
    let row_count = parse_ordered_set(pool, doc.cr_rows_ref)?.array_uuid_indexes.len();
    let mut refs: Vec<Option<u32>> = vec![None; row_count];
    for (_, row_map_ref) in parse_ref_pair_list(pool, doc.cell_columns_ref, "cellColumns")? {
        let row_map = resolve_ref(&row_map_ref, "cellColumns row-map ref")?;
        for (row_ref, _) in parse_ref_pair_list(pool, row_map, "column row-map")? {
            let reference = resolve_ref(&row_ref, "row-map row ref")?;
            if let Some(&position) = row_positions.get(&uuid_index_of_ref(pool, reference)?)
                && refs[position].is_none()
            {
                refs[position] = Some(reference);
            }
        }
    }
    refs.iter()
        .enumerate()
        .map(|(position, r)| match r {
            Some(r) => Ok(*r),
            None => fail(format!(
                "No existing row-map entry reveals row {position}'s content identity - refusing to guess"
            )),
        })
        .collect()
}

fn push_cell_text(doc: &mut TableDocument, session: &Session, text: &str) -> Result<u32> {
    let mut cell = new_cell_document();
    if !text.is_empty() {
        apply_cell_text_edit(&mut cell, text, &mut session_clock(doc, session))?;
    }
    Ok(push_object(
        doc,
        DocObject {
            string: Some(encode_cell_document(&cell)),
            ..Default::default()
        },
    ))
}

fn renumber_attachments(attachments: &mut [ArrayAttachment]) {
    for (index, attachment) in attachments.iter_mut().enumerate() {
        attachment.attachment_index = Some(index as u64);
    }
}

/// `editMirror`: splice the FFFC ordering mirror, then restore its shape.
fn edit_mirror(
    doc: &mut TableDocument,
    ordered_set_ref: u32,
    session: &Session,
    splice: impl FnOnce(&mut TableCellDocument, &mut dyn TopotextClockSource) -> Result<()>,
) -> Result<()> {
    let contents = require_ordered_set(doc, ordered_set_ref)?
        .array
        .as_ref()
        .and_then(|a| a.array.as_ref())
        .and_then(|s| s.contents.clone())
        .unwrap_or_default();
    let mut mirror = parse_cell_document(&contents)?;
    splice(&mut mirror, &mut session_clock(doc, session))?;
    let visible = visible_length(&mirror.runs) as usize;
    mirror.text = ORC.repeat(visible);
    mirror.attribute_runs = (0..visible).map(|_| AttributeRun::with_length(1)).collect();
    validate_cell_invariants(&mirror)?;
    if let Some(string_array) = require_ordered_set(doc, ordered_set_ref)?
        .array
        .as_mut()
        .and_then(|a| a.array.as_mut())
    {
        string_array.contents = Some(encode_cell_document(&mirror));
    }
    Ok(())
}

fn insert_into_ordered_set(
    doc: &mut TableDocument,
    session: &Session,
    ordered_set_ref: u32,
    position: usize,
    pair: &IdentityPair,
    stamp: (u64, u64),
) -> Result<()> {
    {
        let ordered_set = require_ordered_set(doc, ordered_set_ref)?;
        if let Some(string_array) = ordered_set.array.as_mut().and_then(|a| a.array.as_mut()) {
            string_array.attachments.insert(
                position,
                ArrayAttachment {
                    attachment_index: Some(0),
                    contents: Some(pair.ordering_uuid.clone()),
                    ..Default::default()
                },
            );
            renumber_attachments(&mut string_array.attachments);
        }
    }
    edit_mirror(doc, ordered_set_ref, session, |mirror, clock| {
        cell_insert_visible_text(mirror, position, 1, clock)
    })?;
    let ordered_set = require_ordered_set(doc, ordered_set_ref)?;
    if let Some(dictionary) = ordered_set.array.as_mut().and_then(|a| a.dictionary.as_mut()) {
        dictionary
            .element
            .push(stamped_element(pair.ordering_ref, pair.content_ref, stamp));
    }
    if let Some(set) = ordered_set.set.as_mut() {
        set.element
            .push(stamped_element(pair.ordering_ref, pair.ordering_ref, stamp));
    }
    Ok(())
}

/// `findIndex` over a dictionary's elements whose key resolves to `position`
/// (errors propagate from the elements visited before the match).
fn find_element_at_position(
    pool: &crdt::Document,
    dictionary: &Dictionary,
    positions: &HashMap<u64, usize>,
    position: usize,
    label: &str,
) -> Result<Option<usize>> {
    for (index, element) in dictionary.element.iter().enumerate() {
        if let Some(key) = &element.key
            && positions.get(&uuid_index_of_ref(pool, resolve_ref(key, label)?)?) == Some(&position)
        {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn delete_row_at(doc: &mut TableDocument, session: &mut Session, position: usize) -> Result<()> {
    let row_set = parse_ordered_set(&doc.document, doc.cr_rows_ref)?;
    if position >= row_set.array_uuid_indexes.len() {
        return fail(format!("Row deletion position {position} is out of bounds"));
    }
    if row_set.array_uuid_indexes.len() == 1 {
        return fail("Refusing to delete a table's last row");
    }
    let row_positions = compute_positions(&doc.document, &row_set)?;
    let mut freed = Vec::new();
    for (_, row_map_ref) in parse_ref_pair_list(&doc.document, doc.cell_columns_ref, "cellColumns")? {
        let row_map = resolve_ref(&row_map_ref, "cellColumns row-map ref")?;
        let dictionary = require_dictionary(doc, row_map, "column row-map")?.clone();
        if let Some(index) =
            find_element_at_position(&doc.document, &dictionary, &row_positions, position, "row-map row ref")?
        {
            let removed = require_dictionary(doc, row_map, "column row-map")?
                .element
                .remove(index);
            if let Some(value) = &removed.value {
                freed.push(resolve_ref(value, "freed cell ref")?);
            }
        }
    }
    remove_from_ordered_set(doc, session, doc.cr_rows_ref, position)?;
    compact_pool(doc, &freed)
}

fn delete_column_at(doc: &mut TableDocument, session: &mut Session, position: usize) -> Result<()> {
    let column_set = parse_ordered_set(&doc.document, doc.cr_columns_ref)?;
    if position >= column_set.array_uuid_indexes.len() {
        return fail(format!("Column deletion position {position} is out of bounds"));
    }
    if column_set.array_uuid_indexes.len() == 1 {
        return fail("Refusing to delete a table's last column");
    }
    let column_positions = compute_positions(&doc.document, &column_set)?;
    let cell_columns_ref = doc.cell_columns_ref;
    let dictionary = require_dictionary(doc, cell_columns_ref, "cellColumns")?.clone();
    let mut freed = Vec::new();
    if let Some(index) = find_element_at_position(
        &doc.document,
        &dictionary,
        &column_positions,
        position,
        "cellColumns column ref",
    )? {
        let removed = require_dictionary(doc, cell_columns_ref, "cellColumns")?
            .element
            .remove(index);
        if let Some(value) = &removed.value {
            let row_map_ref = resolve_ref(value, "cellColumns row-map ref")?;
            for (_, cell_ref) in parse_ref_pair_list(&doc.document, row_map_ref, "column row-map")? {
                freed.push(resolve_ref(&cell_ref, "freed cell ref")?);
            }
            freed.push(row_map_ref);
        }
    }
    remove_from_ordered_set(doc, session, doc.cr_columns_ref, position)?;
    compact_pool(doc, &freed)
}

fn remove_from_ordered_set(
    doc: &mut TableDocument,
    session: &mut Session,
    ordered_set_ref: u32,
    position: usize,
) -> Result<()> {
    require_ordered_set(doc, ordered_set_ref)?;
    let positions = compute_positions(&doc.document, &parse_ordered_set(&doc.document, ordered_set_ref)?)?;
    session.mark_structural_save();
    if let Some(string_array) = require_ordered_set(doc, ordered_set_ref)?
        .array
        .as_mut()
        .and_then(|a| a.array.as_mut())
    {
        string_array.attachments.remove(position);
        renumber_attachments(&mut string_array.attachments);
    }
    edit_mirror(doc, ordered_set_ref, session, |mirror, clock| {
        cell_tombstone_visible_range(mirror, position, 1, clock)
    })?;

    let set = require_ordered_set(doc, ordered_set_ref)?
        .set
        .clone()
        .unwrap_or_default();
    let mut kept = Vec::with_capacity(set.element.len());
    for element in set.element {
        let drop = match &element.key {
            Some(key) => {
                positions.get(&uuid_index_of_ref(
                    &doc.document,
                    resolve_ref(key, "set self-pair key")?,
                )?) == Some(&position)
            }
            None => false,
        };
        if !drop {
            kept.push(element);
        }
    }
    if let Some(set) = require_ordered_set(doc, ordered_set_ref)?.set.as_mut() {
        set.element = kept;
    }
    Ok(())
}

// --- pool compaction -----------------------------------------------------------

fn remap_required(reference: u32, remap: &HashMap<u32, u32>) -> Result<u32> {
    match remap.get(&reference) {
        Some(&next) => Ok(next),
        None => fail(format!(
            "Pool compaction: reference to a removed pool index {reference} - refusing to guess"
        )),
    }
}

/// `compactPool`: physically removes `removed_refs` and remaps every
/// remaining `objectIndex` (and the cached refs).
pub fn compact_pool(doc: &mut TableDocument, removed_refs: &[u32]) -> Result<()> {
    if removed_refs.is_empty() {
        return Ok(());
    }
    let removed: HashSet<u32> = removed_refs.iter().copied().collect();
    let mut remap = HashMap::new();
    let mut shift = 0u32;
    for old in 0..doc.document.object.len() as u32 {
        if removed.contains(&old) {
            shift += 1;
            continue;
        }
        remap.insert(old, old - shift);
    }
    for (index, entry) in doc.document.object.iter_mut().enumerate() {
        if !removed.contains(&(index as u32)) {
            remap_entry(entry, &remap)?;
        }
    }
    doc.cr_rows_ref = remap_required(doc.cr_rows_ref, &remap)?;
    doc.cr_columns_ref = remap_required(doc.cr_columns_ref, &remap)?;
    doc.cell_columns_ref = remap_required(doc.cell_columns_ref, &remap)?;
    let mut descending: Vec<u32> = removed.into_iter().collect();
    descending.sort_unstable_by(|a, b| b.cmp(a));
    for reference in descending {
        if (reference as usize) < doc.document.object.len() {
            doc.document.object.remove(reference as usize);
        }
    }
    Ok(())
}

fn remap_object_id(id: &mut ObjectID, remap: &HashMap<u32, u32>) -> Result<()> {
    if let Some(index) = id.object_index {
        id.object_index = Some(remap_required(index, remap)?);
    }
    Ok(())
}

fn remap_dictionary(dictionary: &mut Dictionary, remap: &HashMap<u32, u32>) -> Result<()> {
    for element in &mut dictionary.element {
        if let Some(key) = &mut element.key {
            remap_object_id(key, remap)?;
        }
        if let Some(value) = &mut element.value {
            remap_object_id(value, remap)?;
        }
        if let Some(contents) = element.index.as_mut().and_then(|i| i.contents.as_mut()) {
            remap_object_id(contents, remap)?;
        }
    }
    Ok(())
}

fn remap_entry(entry: &mut DocObject, remap: &HashMap<u32, u32>) -> Result<()> {
    for register in [
        &mut entry.register_latest,
        &mut entry.register_greatest,
        &mut entry.register_least,
    ]
    .into_iter()
    .flatten()
    {
        if let Some(contents) = &mut register.contents {
            remap_object_id(contents, remap)?;
        }
    }
    for dictionary in [&mut entry.set, &mut entry.ordered_set, &mut entry.dictionary]
        .into_iter()
        .flatten()
    {
        remap_dictionary(dictionary, remap)?;
    }
    if let Some(oneof) = &mut entry.oneof {
        for element in &mut oneof.element {
            if let Some(value) = &mut element.value {
                remap_object_id(value, remap)?;
            }
        }
    }
    if let Some(custom) = &mut entry.custom {
        for map_entry in &mut custom.map_entry {
            if let Some(value) = &mut map_entry.value {
                remap_object_id(value, remap)?;
            }
        }
    }
    if let Some(dictionary) = entry.array.as_mut().and_then(|a| a.dictionary.as_mut()) {
        remap_dictionary(dictionary, remap)?;
    }
    if let Some(ordered_set) = &mut entry.ts_ordered_set {
        if let Some(dictionary) = ordered_set.array.as_mut().and_then(|a| a.dictionary.as_mut()) {
            remap_dictionary(dictionary, remap)?;
        }
        if let Some(set) = &mut ordered_set.set {
            remap_dictionary(set, remap)?;
        }
    }
    Ok(())
}

// --- structural invariants --------------------------------------------------------

/// `validateTableDocumentInvariants`.
pub fn validate_table_document_invariants(doc: &TableDocument) -> Result<()> {
    let pool = &doc.document;
    if pool.start_version.as_ref().is_some_and(|v| !v.element.is_empty()) {
        return fail(
            "Table document carries a populated startVersion - it is a CRDT delta, not a full document, and this engine only edits full documents",
        );
    }
    let version = doc.version();
    let mut failures: Vec<String> = Vec::new();
    let replica_count = version.element.len() as u64;
    if replica_count == 0 {
        failures.push("version vector is empty".into());
    }
    for (i, element) in version.element.iter().enumerate() {
        let claimed = element.replica_index.unwrap_or(0);
        if claimed != i as u64 {
            failures.push(format!("version.element[{i}] claims replica {claimed}"));
        }
    }

    let key = key_index(pool, "UUIDIndex");
    for (reference, entry) in pool.object.iter().enumerate() {
        if let Some(index) = identity_uuid_index(entry, key)
            && (index < replica_count || index >= pool.uuid_item.len() as u64)
        {
            failures.push(format!(
                "identity object r{reference} references UUID-table index {index} outside the identity segment"
            ));
        }
    }

    let check_stamp = |label: &str, timestamp: Option<&VectorTimestamp>, failures: &mut Vec<String>| {
        for element in timestamp.map_or(&[][..], |t| &t.element[..]) {
            let replica = element.replica_index.unwrap_or(0);
            let clock = element.clock.unwrap_or(0);
            match usize::try_from(replica).ok().and_then(|r| version.element.get(r)) {
                None => failures.push(format!(
                    "{label}: stamped by replica {replica}, not in the version vector"
                )),
                Some(known) => {
                    let known_clock = known.clock.unwrap_or(0);
                    if clock > known_clock {
                        failures.push(format!(
                            "{label}: stamp clock {clock} exceeds replica {replica}'s vector clock {known_clock}"
                        ));
                    }
                }
            }
        }
    };
    for (reference, entry) in pool.object.iter().enumerate() {
        for dictionary in [&entry.set, &entry.ordered_set, &entry.dictionary]
            .into_iter()
            .flatten()
        {
            for element in &dictionary.element {
                check_stamp(
                    &format!("r{reference} dictionary element"),
                    element.timestamp.as_ref(),
                    &mut failures,
                );
            }
        }
        if let Some(ordered_set) = &entry.ts_ordered_set {
            for element in ordered_set.set.iter().flat_map(|s| &s.element) {
                check_stamp(
                    &format!("r{reference} set element"),
                    element.timestamp.as_ref(),
                    &mut failures,
                );
            }
            for element in ordered_set
                .array
                .iter()
                .filter_map(|a| a.dictionary.as_ref())
                .flat_map(|d| &d.element)
            {
                check_stamp(
                    &format!("r{reference} redirect element"),
                    element.timestamp.as_ref(),
                    &mut failures,
                );
            }
        }
    }

    for axis_ref in [doc.cr_rows_ref, doc.cr_columns_ref] {
        let label = if axis_ref == doc.cr_rows_ref {
            "crRows"
        } else {
            "crColumns"
        };
        let ordered_set = pool
            .object
            .get(axis_ref as usize)
            .and_then(|e| e.ts_ordered_set.as_ref());
        let string_array = ordered_set
            .and_then(|o| o.array.as_ref())
            .and_then(|a| a.array.as_ref());
        let (Some(ordered_set), Some(string_array)) = (ordered_set, string_array) else {
            failures.push(format!("{label}: not a well-formed OrderedSet"));
            continue;
        };
        let Some(mirror) = &string_array.contents else {
            failures.push(format!("{label}: not a well-formed OrderedSet"));
            continue;
        };
        let attachments = &string_array.attachments;
        let visible: u64 = mirror
            .substring
            .iter()
            .filter(|s| s.tombstone.unwrap_or(0) == 0)
            .map(|s| u64::from(s.length.unwrap_or(0)))
            .sum();
        if visible != attachments.len() as u64 {
            failures.push(format!(
                "{label}: mirror visible length {visible} != {} attachments",
                attachments.len()
            ));
        }
        if mirror.string.as_deref().unwrap_or("") != ORC.repeat(attachments.len()) {
            failures.push(format!("{label}: mirror text isn't one U+FFFC per attachment"));
        }
        for (index, attachment) in attachments.iter().enumerate() {
            let carried = attachment.attachment_index.unwrap_or(0);
            if carried != index as u64 {
                failures.push(format!(
                    "{label}: attachments[{index}] carries attachmentIndex {carried}"
                ));
            }
        }
        let positions = match parse_ordered_set(pool, axis_ref).and_then(|set| compute_positions(pool, &set)) {
            Ok(positions) => positions,
            Err(cause) => {
                failures.push(format!("{label}: {cause}"));
                continue;
            }
        };
        let mut set_positions: Vec<usize> = Vec::new();
        for element in ordered_set.set.iter().flat_map(|s| &s.element) {
            let Some(key) = &element.key else {
                failures.push(format!("{label}: set self-pair with no key"));
                continue;
            };
            match positions.get(&uuid_index_of_ref(pool, resolve_ref(key, "set self-pair key")?)?) {
                None => failures.push(format!(
                    "{label}: set self-pair references an entry with no live position"
                )),
                Some(&position) => set_positions.push(position),
            }
        }
        let unique = set_positions.iter().collect::<HashSet<_>>().len();
        if unique != attachments.len() || set_positions.len() != attachments.len() {
            failures.push(format!(
                "{label}: set self-pairs cover {unique}/{} vs {} live entries",
                set_positions.len(),
                attachments.len()
            ));
        }
    }

    match resolve_table(doc) {
        Ok(resolved) => {
            for row in 0..resolved.rows.len() {
                for column in 0..resolved.columns.len() {
                    if !resolved.cells.contains_key(&(row, column)) {
                        failures.push(format!("no cell object at row {row}, column {column}"));
                    }
                }
            }
        }
        Err(cause) => failures.push(cause.to_string()),
    }

    match &pool.tt_timestamp {
        None => failures.push("document has no topotext clock table (ttTimestamp)".into()),
        Some(tt) => {
            for i in 1..tt.clock.len() {
                if compare_bytes(uuid_of(&tt.clock[i - 1]), uuid_of(&tt.clock[i])) >= 0 {
                    failures.push(format!(
                        "ttTimestamp entries {} and {i} are not in strict sorted UUID order",
                        i - 1
                    ));
                }
            }
            let mut max_text_end: IndexMap<u32, u64> = IndexMap::new();
            let mut max_style_anchor: IndexMap<u32, u64> = IndexMap::new();
            for entry in &pool.object {
                let mirror = entry
                    .ts_ordered_set
                    .as_ref()
                    .and_then(|o| o.array.as_ref())
                    .and_then(|a| a.array.as_ref())
                    .and_then(|s| s.contents.as_ref());
                for s in [entry.string.as_ref(), mirror].into_iter().flatten() {
                    for run in &s.substring {
                        if let Some(id) = &run.char_id {
                            let replica = id.replica_id.unwrap_or(0);
                            if replica != 0 {
                                let end = u64::from(id.clock.unwrap_or(0)) + u64::from(run.length.unwrap_or(0));
                                let slot = max_text_end.entry(replica).or_insert(0);
                                *slot = (*slot).max(end);
                            }
                        }
                        if let Some(id) = &run.timestamp {
                            let replica = id.replica_id.unwrap_or(0);
                            if replica != 0 {
                                let slot = max_style_anchor.entry(replica).or_insert(0);
                                *slot = (*slot).max(u64::from(id.clock.unwrap_or(0)));
                            }
                        }
                    }
                }
            }
            let counter = |replica: u32, which: usize| {
                tt.clock
                    .get(replica as usize - 1)
                    .and_then(|c| c.replica_clock.get(which))
                    .map(|c| u64::from(c.clock.unwrap_or(0)))
            };
            for (&replica, &end) in &max_text_end {
                let clock = counter(replica, 0);
                if clock.is_none_or(|c| end > c) {
                    failures.push(format!(
                        "topotext replica {replica}: run coordinates reach {end}, beyond its text clock {}",
                        clock.map_or("(none)".to_string(), |c| c.to_string())
                    ));
                }
            }
            for (&replica, &anchor) in &max_style_anchor {
                let clock = counter(replica, 1);
                if clock.is_none_or(|c| anchor > c) {
                    failures.push(format!(
                        "topotext replica {replica}: style anchors reach {anchor}, beyond its style clock {}",
                        clock.map_or("(none)".to_string(), |c| c.to_string())
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        return fail(format!(
            "Table document violates structural invariants:\n  {}",
            failures.join("\n  ")
        ));
    }
    Ok(())
}
