//! The note CRDT document. Ports icloud-md `src/notes/noteDocument.ts`.
//!
//! Offsets, lengths and clocks count UTF-16 code units, as in icloud-md and
//! on the wire; `text` is held as a `String` and converted where needed.

use std::collections::HashMap;

use super::js::{from_utf16, utf16, utf16_len};
use super::proto::topotext::vector_timestamp::{Clock, clock::ReplicaClock};
use super::proto::topotext::{self, AttributeRun, CharID, Substring};
use super::proto::{Message, versioned_document};
use super::text::parse_versioned_document;
use super::{DocError, Result};

pub(crate) const SENTINEL_CLOCK: u32 = 0xffff_ffff;

/// `RunCoord`: a topotext `CharID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunCoord {
    pub replica: u32,
    pub clock: u32,
}

/// `TextRun`: one topotext `Substring`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    pub coord: RunCoord,
    /// UTF-16 length.
    pub length: u32,
    pub anchor: RunCoord,
    pub tombstone: bool,
    /// Outgoing child edges: 0-based indexes into the runs array, always
    /// later indexes (topological order). Empty on the end sentinel.
    pub sequence: Vec<u32>,
}

/// `ReplicaEntry`: one `VectorTimestamp.Clock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaEntry {
    /// 16-byte replica UUID.
    pub id: Vec<u8>,
    /// First entry is the replica's text clock, the second its style
    /// (formatting-op) clock; later entries preserved verbatim.
    pub counters: Vec<u32>,
}

/// `NoteDocument`. `attribute_runs` are the protobuf messages themselves,
/// opaque to editing logic (only `.length` is touched) and carrying unknown
/// fields through.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteDocument {
    /// `versioned_document.Document.serializationVersion`.
    pub root_serialization_version: u32,
    /// `versioned_document.Version.serializationVersion`.
    pub version_serialization_version: u32,
    /// `versioned_document.Version.minimumSupportedVersion`.
    pub minimum_supported_version: u32,
    pub text: String,
    pub runs: Vec<TextRun>,
    pub replicas: Vec<ReplicaEntry>,
    pub attribute_runs: Vec<AttributeRun>,
}

/// `ApplyTextEditOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyTextEditOptions {
    /// 16-byte replica UUID identifying this tool's edits.
    pub replica_id: [u8; 16],
}

/// `Splice`: one hunk of `computeSplices` (UTF-16 offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub delete_length: usize,
    pub insert_text: String,
}

fn invalid(message: impl Into<String>) -> DocError {
    DocError::Invalid(message.into())
}

/// `parseNoteDocument`: raw (decompressed) versioned-document bytes → model.
pub fn parse_note_document(raw: &[u8]) -> Result<NoteDocument> {
    let versioned = parse_versioned_document(raw)?;
    let version = &versioned.wrapper.version[0];
    let s = topotext::String::decode(&versioned.data)?;
    let Some(timestamp) = s.timestamp else {
        return Err(invalid(
            "Note document is missing its replica clock table (String field 4)",
        ));
    };
    let runs = s.substring.iter().map(parse_text_run).collect::<Result<Vec<_>>>()?;
    let replicas = timestamp
        .clock
        .iter()
        .map(parse_replica_entry)
        .collect::<Result<Vec<_>>>()?;
    Ok(NoteDocument {
        root_serialization_version: versioned.wrapper.serialization_version.unwrap_or(0),
        version_serialization_version: version.serialization_version.unwrap_or(0),
        minimum_supported_version: version.minimum_supported_version.unwrap_or(0),
        text: s.string.unwrap_or_default(),
        runs,
        replicas,
        attribute_runs: s.attribute_run,
    })
}

/// `encodeNoteDocument`: model → raw bytes (not compressed). Fails where
/// protobuf-es's `toBinary` throws (an attribute run missing a `required`
/// field, say).
pub fn encode_note_document(doc: &NoteDocument) -> Result<Vec<u8>> {
    let s = topotext::String {
        string: Some(doc.text.clone()),
        substring: doc.runs.iter().map(encode_text_run).collect(),
        timestamp: Some(topotext::VectorTimestamp {
            clock: doc.replicas.iter().map(encode_replica_entry).collect(),
            ..Default::default()
        }),
        attribute_run: doc.attribute_runs.clone(),
        ..Default::default()
    };
    let wrapper = versioned_document::Document {
        serialization_version: Some(doc.root_serialization_version),
        version: vec![versioned_document::Version {
            serialization_version: Some(doc.version_serialization_version),
            minimum_supported_version: Some(doc.minimum_supported_version),
            data: Some(s.encode()?),
            ..Default::default()
        }],
        ..Default::default()
    };
    Ok(wrapper.encode()?)
}

/// `noteDocumentRoundTrips`: `encode(parse(raw)) == raw`, byte for byte.
pub fn note_document_round_trips(raw: &[u8]) -> bool {
    match parse_note_document(raw).and_then(|doc| encode_note_document(&doc)) {
        Ok(reencoded) => reencoded == raw,
        Err(_) => false,
    }
}

/// `applyTextEdit`: per-hunk CRDT edit in place; false if the text is
/// unchanged.
pub fn apply_text_edit(doc: &mut NoteDocument, new_text: &str, options: &ApplyTextEditOptions) -> Result<bool> {
    if doc.text == new_text {
        return Ok(false);
    }
    validate_document_invariants(doc)?;

    let splices = compute_splices(&doc.text, new_text);
    let replica_index = ensure_replica(doc, &options.replica_id);
    let style_clock_floor = style_clock_seed(doc, replica_index)?;
    let mut max_assigned_style_clock: i64 = -1;

    let mut structural_change = false;
    let mut inserted_new_run = false;
    let mut delta: i64 = 0;
    for splice in &splices {
        let start = (splice.start as i64 + delta) as usize;
        let insert_len = utf16_len(&splice.insert_text);
        if splice.delete_length > 0 {
            let assigned = tombstone_visible_range(doc, start, splice.delete_length, replica_index, style_clock_floor)?;
            max_assigned_style_clock = max_assigned_style_clock.max(assigned);
            structural_change = true;
        }
        if insert_len > 0 {
            let new_run = insert_visible_text(doc, start, insert_len, replica_index)?;
            inserted_new_run = inserted_new_run || new_run;
            structural_change = new_run || structural_change;
        }
        adjust_attribute_runs(
            &mut doc.attribute_runs,
            start,
            splice.delete_length,
            insert_len,
            true,
            "Attribute runs are shorter than the deleted range - document model out of sync",
        )?;
        delta += insert_len as i64 - splice.delete_length as i64;
    }

    if structural_change && let Some(replica) = doc.replicas.get_mut(replica_index as usize - 1) {
        let value = (max_assigned_style_clock + 1)
            .max(i64::from(style_clock_floor))
            .max(i64::from(inserted_new_run));
        set_counter(&mut replica.counters, 1, value as u32);
    }

    doc.text = new_text.to_string();
    validate_document_invariants(doc)?;
    Ok(true)
}

fn set_counter(counters: &mut Vec<u32>, index: usize, value: u32) {
    if counters.len() <= index {
        counters.resize(index + 1, 0);
    }
    counters[index] = value;
}

/// `buildInitialNoteDocument`: a fresh document holding `text`.
pub fn build_initial_note_document(text: &str, replica_id: &[u8; 16]) -> Result<NoteDocument> {
    if text.is_empty() {
        return Err(invalid(
            "A new note needs some text - refusing to create an empty document",
        ));
    }
    let mut doc = NoteDocument {
        root_serialization_version: 0,
        version_serialization_version: 0,
        minimum_supported_version: 0,
        text: String::new(),
        runs: initial_runs(),
        replicas: Vec::new(),
        attribute_runs: Vec::new(),
    };
    apply_text_edit(
        &mut doc,
        text,
        &ApplyTextEditOptions {
            replica_id: *replica_id,
        },
    )?;
    Ok(doc)
}

/// The zero-length replica-0 origin run pointing at the end sentinel.
pub(crate) fn initial_runs() -> Vec<TextRun> {
    vec![
        TextRun {
            coord: RunCoord { replica: 0, clock: 0 },
            length: 0,
            anchor: RunCoord { replica: 0, clock: 0 },
            tombstone: false,
            sequence: vec![1],
        },
        TextRun {
            coord: RunCoord {
                replica: 0,
                clock: SENTINEL_CLOCK,
            },
            length: 0,
            anchor: RunCoord {
                replica: 0,
                clock: SENTINEL_CLOCK,
            },
            tombstone: false,
            sequence: Vec::new(),
        },
    ]
}

// --- child-edge surgery -----------------------------------------------------

fn shift_child_edges(runs: &mut [TextRun], threshold: u32, delta: u32) {
    for run in runs {
        for edge in &mut run.sequence {
            if *edge >= threshold {
                *edge += delta;
            }
        }
    }
}

/// `splitRunAt`: splits `runs[index]` at `offset` into head and tail; the
/// tail lands at `index + 1`.
pub fn split_run_at(runs: &mut Vec<TextRun>, index: usize, offset: u32) -> Result<()> {
    let ok = runs.get(index).is_some_and(|run| offset > 0 && offset < run.length);
    if !ok {
        return Err(invalid(format!(
            "Cannot split run {index} at offset {offset} - CRDT model out of sync"
        )));
    }
    shift_child_edges(runs, index as u32 + 1, 1);
    let run = &mut runs[index];
    let tail = TextRun {
        coord: RunCoord {
            replica: run.coord.replica,
            clock: run.coord.clock.wrapping_add(offset),
        },
        length: run.length - offset,
        anchor: run.anchor,
        tombstone: run.tombstone,
        sequence: std::mem::replace(&mut run.sequence, vec![index as u32 + 1]),
    };
    run.length = offset;
    runs.insert(index + 1, tail);
    Ok(())
}

/// `insertRunAt`: splices a fresh `run` in at `index` and into the child
/// graph between its new array neighbours.
pub fn insert_run_at(runs: &mut Vec<TextRun>, index: usize, mut run: TextRun) {
    shift_child_edges(runs, index as u32, 1);
    if index > 0 && index - 1 < runs.len() {
        let predecessor = &mut runs[index - 1];
        if let Some(edge) = predecessor.sequence.iter().position(|&c| c == index as u32 + 1) {
            predecessor.sequence[edge] = index as u32;
            run.sequence = vec![index as u32 + 1];
        } else {
            run.sequence = std::mem::replace(&mut predecessor.sequence, vec![index as u32]);
        }
    } else {
        run.sequence = vec![index as u32 + 1];
    }
    runs.insert(index, run);
}

// --- parsing ---------------------------------------------------------------

/// `parseTextRun`.
pub fn parse_text_run(run: &Substring) -> Result<TextRun> {
    let (Some(coord), Some(anchor)) = (&run.char_id, &run.timestamp) else {
        return Err(invalid("Substring is missing charID, length, or timestamp"));
    };
    let mut tombstone = false;
    if let Some(value) = run.tombstone {
        if value != 1 {
            return Err(invalid(format!(
                "Substring tombstone flag has unexpected value {value}"
            )));
        }
        tombstone = true;
    }
    Ok(TextRun {
        coord: RunCoord {
            replica: coord.replica_id.unwrap_or(0),
            clock: coord.clock.unwrap_or(0),
        },
        length: run.length.unwrap_or(0),
        anchor: RunCoord {
            replica: anchor.replica_id.unwrap_or(0),
            clock: anchor.clock.unwrap_or(0),
        },
        tombstone,
        sequence: run.child.clone(),
    })
}

fn parse_replica_entry(entry: &Clock) -> Result<ReplicaEntry> {
    let id = entry.replica_uuid.clone().unwrap_or_default();
    if id.len() != 16 {
        return Err(invalid("Replica clock entry does not start with a 16-byte UUID"));
    }
    let counters = entry
        .replica_clock
        .iter()
        .map(|counter| {
            if counter.subclock.is_some() {
                return Err(invalid(
                    "Replica clock entry carries a subclock this tool doesn't understand - refusing to touch this note",
                ));
            }
            Ok(counter.clock.unwrap_or(0))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ReplicaEntry { id, counters })
}

// --- encoding --------------------------------------------------------------

/// `encodeTextRun`.
pub fn encode_text_run(run: &TextRun) -> Substring {
    Substring {
        char_id: Some(CharID {
            replica_id: Some(run.coord.replica),
            clock: Some(run.coord.clock),
            ..Default::default()
        }),
        length: Some(run.length),
        timestamp: Some(CharID {
            replica_id: Some(run.anchor.replica),
            clock: Some(run.anchor.clock),
            ..Default::default()
        }),
        tombstone: run.tombstone.then_some(1),
        child: run.sequence.clone(),
        ..Default::default()
    }
}

fn encode_replica_entry(entry: &ReplicaEntry) -> Clock {
    Clock {
        replica_uuid: Some(entry.id.clone()),
        replica_clock: entry
            .counters
            .iter()
            .map(|&clock| ReplicaClock {
                clock: Some(clock),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

// --- editing ---------------------------------------------------------------

fn is_high_surrogate(code: u16) -> bool {
    (0xd800..=0xdbff).contains(&code)
}

fn is_low_surrogate(code: u16) -> bool {
    (0xdc00..=0xdfff).contains(&code)
}

/// `computeSplice` over UTF-16 units: `(start, deleteLength, insert units)`.
pub(crate) fn compute_splice16(old: &[u16], new: &[u16]) -> (usize, usize, Vec<u16>) {
    let max_prefix = old.len().min(new.len());
    let mut prefix = 0;
    while prefix < max_prefix && old[prefix] == new[prefix] {
        prefix += 1;
    }
    while prefix > 0 && is_high_surrogate(old[prefix - 1]) {
        prefix -= 1;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let mut suffix = 0;
    while suffix < max_suffix && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    while suffix > 0 && is_low_surrogate(old[old.len() - suffix]) {
        suffix -= 1;
    }
    (
        prefix,
        old.len() - prefix - suffix,
        new[prefix..new.len() - suffix].to_vec(),
    )
}

/// `computeSplice`: the single spanning splice.
pub fn compute_splice(old_text: &str, new_text: &str) -> Splice {
    let (start, delete_length, insert) = compute_splice16(&utf16(old_text), &utf16(new_text));
    Splice {
        start,
        delete_length,
        insert_text: from_utf16(&insert),
    }
}

/// `computeSplices`: per-hunk splices (line-level LCS via node-diff3's
/// `diffIndices`, each hunk tightened with `computeSplice`).
pub fn compute_splices(old_text: &str, new_text: &str) -> Vec<Splice> {
    if old_text == new_text {
        return Vec::new();
    }
    let old_lines = split_lines_inclusive(&utf16(old_text));
    let new_lines = split_lines_inclusive(&utf16(new_text));
    let old_offsets = line_start_offsets(&old_lines);

    let mut splices = Vec::new();
    for hunk in diff_indices(&old_lines, &new_lines) {
        let old_start = old_offsets[hunk.buffer1.0];
        let old_hunk: Vec<u16> = old_lines[hunk.buffer1.0..hunk.buffer1.0 + hunk.buffer1.1].concat();
        let new_hunk: Vec<u16> = new_lines[hunk.buffer2.0..hunk.buffer2.0 + hunk.buffer2.1].concat();
        let (start, delete_length, insert) = compute_splice16(&old_hunk, &new_hunk);
        if delete_length == 0 && insert.is_empty() {
            continue;
        }
        splices.push(Splice {
            start: old_start + start,
            delete_length,
            insert_text: from_utf16(&insert),
        });
    }
    splices
}

fn split_lines_inclusive(text: &[u16]) -> Vec<Vec<u16>> {
    let mut lines: Vec<Vec<u16>> = text
        .split(|&u| u == u16::from(b'\n'))
        .map(|line| {
            let mut l = line.to_vec();
            l.push(u16::from(b'\n'));
            l
        })
        .collect();
    let last = lines.len() - 1;
    if lines[last] == [u16::from(b'\n')] {
        lines.pop();
    } else {
        lines[last].pop();
    }
    lines
}

fn line_start_offsets(lines: &[Vec<u16>]) -> Vec<usize> {
    let mut offsets = vec![0];
    for line in lines {
        offsets.push(offsets[offsets.len() - 1] + line.len());
    }
    offsets
}

/// One node-diff3 `diffIndices` hunk: `(start, length)` on each side.
pub(crate) struct DiffHunk {
    pub buffer1: (usize, usize),
    pub buffer2: (usize, usize),
}

/// node-diff3 3.2.1 `LCS` + `diffIndices` (the Hunt-McIlroy candidate
/// chain, same tie-breaking), over any comparable items.
pub(crate) fn diff_indices<T: Eq + std::hash::Hash>(buffer1: &[T], buffer2: &[T]) -> Vec<DiffHunk> {
    struct Candidate {
        buffer1index: isize,
        buffer2index: isize,
        chain: Option<usize>,
    }
    let mut equivalence: HashMap<&T, Vec<usize>> = HashMap::new();
    for (j, item) in buffer2.iter().enumerate() {
        equivalence.entry(item).or_default().push(j);
    }
    let mut arena = vec![Candidate {
        buffer1index: -1,
        buffer2index: -1,
        chain: None,
    }];
    let mut candidates: Vec<usize> = vec![0];
    for (i, item) in buffer1.iter().enumerate() {
        let empty = Vec::new();
        let buffer2indices = equivalence.get(item).unwrap_or(&empty);
        let mut r = 0usize;
        let mut c = candidates[0];
        for &j in buffer2indices {
            let j = j as isize;
            let mut s = r;
            while s < candidates.len() {
                if arena[candidates[s]].buffer2index < j
                    && (s == candidates.len() - 1 || arena[candidates[s + 1]].buffer2index > j)
                {
                    break;
                }
                s += 1;
            }
            if s < candidates.len() {
                arena.push(Candidate {
                    buffer1index: i as isize,
                    buffer2index: j,
                    chain: Some(candidates[s]),
                });
                let new_candidate = arena.len() - 1;
                if r == candidates.len() {
                    candidates.push(c);
                } else {
                    candidates[r] = c;
                }
                r = s + 1;
                c = new_candidate;
                if r == candidates.len() {
                    break;
                }
            }
        }
        if r == candidates.len() {
            candidates.push(c);
        } else {
            candidates[r] = c;
        }
    }
    let mut result = Vec::new();
    let mut tail1 = buffer1.len() as isize;
    let mut tail2 = buffer2.len() as isize;
    let mut candidate = Some(candidates[candidates.len() - 1]);
    while let Some(index) = candidate {
        let node = &arena[index];
        let mismatch1 = tail1 - node.buffer1index - 1;
        let mismatch2 = tail2 - node.buffer2index - 1;
        tail1 = node.buffer1index;
        tail2 = node.buffer2index;
        if mismatch1 != 0 || mismatch2 != 0 {
            result.push(DiffHunk {
                buffer1: ((tail1 + 1) as usize, mismatch1 as usize),
                buffer2: ((tail2 + 1) as usize, mismatch2 as usize),
            });
        }
        candidate = node.chain;
    }
    result.reverse();
    result
}

fn is_sentinel(run: &TextRun) -> bool {
    run.coord.clock == SENTINEL_CLOCK
}

/// `isSentinel`.
pub fn run_is_sentinel(run: &TextRun) -> bool {
    is_sentinel(run)
}

fn visible_length(runs: &[TextRun]) -> u64 {
    runs.iter().filter(|r| !r.tombstone).map(|r| u64::from(r.length)).sum()
}

/// Walks the visible range [start, end), splitting at the boundaries, and
/// calls `restamp(target)` on each covered visible run (`tombstoneVisibleRange`
/// / `restampVisibleRange` share this loop).
pub(crate) fn for_each_visible_piece(
    runs: &mut Vec<TextRun>,
    start: usize,
    end: usize,
    mut restamp: impl FnMut(&mut TextRun) -> Result<()>,
) -> Result<()> {
    let mut visible = 0usize;
    let mut i = 0usize;
    while i < runs.len() && visible < end {
        let run = &runs[i];
        if run.tombstone || run.length == 0 || is_sentinel(run) {
            i += 1;
            continue;
        }
        let run_start = visible;
        let run_end = run_start + run.length as usize;
        if run_end <= start {
            visible = run_end;
            i += 1;
            continue;
        }
        let mut target_index = i;
        let mut target_start = run_start;
        if start > run_start {
            split_run_at(runs, i, (start - run_start) as u32)?;
            target_index = i + 1;
            target_start = start;
        }
        if end < target_start + runs[target_index].length as usize {
            split_run_at(runs, target_index, (end - target_start) as u32)?;
        }
        restamp(&mut runs[target_index])?;
        visible = target_start + runs[target_index].length as usize;
        i = target_index + 1;
    }
    Ok(())
}

fn tombstone_visible_range(
    doc: &mut NoteDocument,
    start: usize,
    length: usize,
    replica_index: u32,
    style_clock_floor: u32,
) -> Result<i64> {
    let end = start + length;
    if end as u64 > visible_length(&doc.runs) {
        return Err(invalid(
            "Tombstone range extends past the end of the visible text - CRDT model out of sync",
        ));
    }
    let mut max_assigned: i64 = -1;
    for_each_visible_piece(&mut doc.runs, start, end, |target| {
        let assigned = (i64::from(target.anchor.clock) + 8).max(i64::from(style_clock_floor));
        target.tombstone = true;
        target.anchor = RunCoord {
            replica: replica_index,
            clock: assigned as u32,
        };
        max_assigned = max_assigned.max(assigned);
        Ok(())
    })?;
    Ok(max_assigned)
}

/// `applyFormattingOp`: restamps every visible substring overlapping the
/// ranges with (us, max(old + 1, floor)) and advances the style clock.
pub fn apply_formatting_op(doc: &mut NoteDocument, ranges: &[(usize, usize)], replica_id: &[u8; 16]) -> Result<()> {
    let replica_index = ensure_replica(doc, replica_id);
    let style_clock_floor = style_clock_seed(doc, replica_index)?;
    let mut max_assigned: i64 = -1;
    for &(start, end) in ranges {
        for_each_visible_piece(&mut doc.runs, start, end, |target| {
            let assigned = (i64::from(target.anchor.clock) + 1).max(i64::from(style_clock_floor));
            target.anchor = RunCoord {
                replica: replica_index,
                clock: assigned as u32,
            };
            max_assigned = max_assigned.max(assigned);
            Ok(())
        })?;
    }
    let value = (max_assigned + 1).max(i64::from(style_clock_floor));
    set_counter(&mut doc.replicas[replica_index as usize - 1].counters, 1, value as u32);
    Ok(())
}

/// Where visible position `start` falls in `runs` (splitting a run it lands
/// inside): the insertion index, as `insertVisibleText` computes it.
pub(crate) fn find_insert_index(runs: &mut Vec<TextRun>, start: usize) -> Result<usize> {
    let mut visible = 0usize;
    let mut insert_index = runs.len();
    let mut i = 0;
    while i < runs.len() {
        let run = &runs[i];
        if is_sentinel(run) {
            insert_index = i;
            break;
        }
        if run.tombstone || run.length == 0 {
            insert_index = i + 1;
            i += 1;
            continue;
        }
        let run_end = visible + run.length as usize;
        if start < run_end {
            let offset = start - visible;
            if offset == 0 {
                insert_index = i;
            } else {
                split_run_at(runs, i, offset as u32)?;
                insert_index = i + 1;
            }
            break;
        }
        visible = run_end;
        insert_index = i + 1;
        i += 1;
    }
    Ok(insert_index)
}

fn insert_visible_text(doc: &mut NoteDocument, start: usize, length: usize, replica_index: u32) -> Result<bool> {
    let clock = doc
        .replicas
        .get(replica_index as usize - 1)
        .and_then(|r| r.counters.first().copied())
        .ok_or_else(|| invalid("Replica entry has no text clock counter"))?;
    if start as u64 > visible_length(&doc.runs) {
        return Err(invalid(
            "Insertion point is past the end of the visible text - CRDT model out of sync",
        ));
    }
    let insert_index = find_insert_index(&mut doc.runs, start)?;
    let length = length as u32;

    if insert_index > 0 {
        let previous = &mut doc.runs[insert_index - 1];
        if !previous.tombstone
            && !is_sentinel(previous)
            && previous.coord.replica == replica_index
            && u64::from(previous.coord.clock) + u64::from(previous.length) == u64::from(clock)
        {
            previous.length += length;
            doc.replicas[replica_index as usize - 1].counters[0] = clock.wrapping_add(length);
            return Ok(false);
        }
    }

    insert_run_at(
        &mut doc.runs,
        insert_index,
        TextRun {
            coord: RunCoord {
                replica: replica_index,
                clock,
            },
            length,
            anchor: RunCoord {
                replica: replica_index,
                clock: 0,
            },
            tombstone: false,
            sequence: Vec::new(),
        },
    );
    doc.replicas[replica_index as usize - 1].counters[0] = clock.wrapping_add(length);
    Ok(true)
}

/// `ensureReplica`: the 1-based replica-table index for `replica_id`, adding
/// an entry (both clocks at the table's maxima) on first contact.
pub(crate) fn ensure_replica(doc: &mut NoteDocument, replica_id: &[u8; 16]) -> u32 {
    if let Some(existing) = doc.replicas.iter().position(|r| r.id == replica_id) {
        return existing as u32 + 1;
    }
    let max_text = doc
        .replicas
        .iter()
        .map(|r| r.counters.first().copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
    let max_op = doc
        .replicas
        .iter()
        .map(|r| r.counters.get(1).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
    doc.replicas.push(ReplicaEntry {
        id: replica_id.to_vec(),
        counters: vec![max_text, max_op],
    });
    doc.replicas.len() as u32
}

/// `styleClockSeed`.
fn style_clock_seed(doc: &NoteDocument, replica_index: u32) -> Result<u32> {
    let Some(our) = doc.replicas.get(replica_index as usize - 1) else {
        return Err(invalid("Replica entry is missing while seeding the style clock"));
    };
    let zero = vec![0u8; 16];
    let mut max_clock: i64 = -1;
    let mut max_holder: Option<&Vec<u8>> = None;
    for run in &doc.runs {
        if run.anchor.replica == 0 {
            continue;
        }
        let holder = doc
            .replicas
            .get(run.anchor.replica as usize - 1)
            .map(|r| &r.id)
            .unwrap_or(&zero);
        let clock = i64::from(run.anchor.clock);
        if clock > max_clock || (clock == max_clock && max_holder.is_some_and(|m| compare_bytes(holder, m) > 0)) {
            max_clock = clock;
            max_holder = Some(holder);
        }
    }
    let mut seed: i64 = 0;
    if let Some(holder) = max_holder {
        seed = max_clock + i64::from(compare_bytes(holder, &our.id) >= 0);
    }
    Ok(seed.max(i64::from(our.counters.get(1).copied().unwrap_or(0))) as u32)
}

/// Byte-lexicographic comparison (`compareBytes`), returning JS's sign.
pub(crate) fn compare_bytes(a: &[u8], b: &[u8]) -> i64 {
    for (x, y) in a.iter().zip(b) {
        if x != y {
            return i64::from(*x) - i64::from(*y);
        }
    }
    a.len() as i64 - b.len() as i64
}

/// `adjustAttributeRuns` (note bodies; `attachment_aware`) and the cell
/// variant in `tableCellEdit.ts` (which grows any run, attachment or not).
pub(crate) fn adjust_attribute_runs(
    runs: &mut Vec<AttributeRun>,
    start: usize,
    delete_length: usize,
    insert_length: usize,
    attachment_aware: bool,
    short_message: &str,
) -> Result<()> {
    let end = start + delete_length;
    let mut out: Vec<AttributeRun> = Vec::new();
    let mut visible = 0usize;
    for run in runs.iter() {
        let run_start = visible;
        let run_end = visible + run.len() as usize;
        visible = run_end;
        let overlap = (end.min(run_end) as i64 - start.max(run_start) as i64).max(0) as usize;
        if run.len() as usize > overlap {
            let mut piece = run.clone();
            piece.length = Some(run.len() - overlap as u32);
            out.push(piece);
        }
    }
    if visible < end {
        return Err(invalid(short_message));
    }

    if insert_length > 0 {
        let insert_length = insert_length as u32;
        let mut grown = false;
        let mut run_end = 0usize;
        for i in 0..out.len() {
            run_end += out[i].len() as usize;
            if start <= run_end {
                if !attachment_aware || out[i].attachment_info.is_none() {
                    out[i].length = Some(out[i].len() + insert_length);
                } else {
                    let mut piece = out[i].clone();
                    piece.attachment_info = None;
                    piece.length = Some(insert_length);
                    out.insert(if start == run_end { i + 1 } else { i }, piece);
                }
                grown = true;
                break;
            }
        }
        if !grown {
            match out.last_mut() {
                Some(last) if !attachment_aware || last.attachment_info.is_none() => {
                    last.length = Some(last.len() + insert_length);
                }
                Some(last) => {
                    let mut piece = last.clone();
                    piece.attachment_info = None;
                    piece.length = Some(insert_length);
                    out.push(piece);
                }
                None => out.push(AttributeRun::with_length(insert_length)),
            }
        }
    }
    *runs = out;
    Ok(())
}

// --- validation ------------------------------------------------------------

/// `validateDocumentInvariants`: `Err` with icloud-md's message when the runs
/// don't describe the text.
pub fn validate_document_invariants(doc: &NoteDocument) -> Result<()> {
    let text_length = utf16_len(&doc.text) as u64;
    let visible = visible_length(&doc.runs);
    if visible != text_length {
        return Err(invalid(format!(
            "Visible run lengths ({visible}) do not match note text length ({text_length}) - refusing to touch this note"
        )));
    }
    let attribute_length: u64 = doc.attribute_runs.iter().map(|r| u64::from(r.len())).sum();
    if attribute_length != text_length {
        return Err(invalid(format!(
            "Attribute run lengths ({attribute_length}) do not match note text length ({text_length}) - refusing to touch this note"
        )));
    }
    for run in &doc.runs {
        if is_sentinel(run) {
            continue;
        }
        if run.coord.replica as usize > doc.replicas.len() {
            return Err(invalid(format!(
                "Run references replica {} outside the replica table - refusing to touch this note",
                run.coord.replica
            )));
        }
        if run.coord.replica != 0 {
            let clock = doc.replicas[run.coord.replica as usize - 1]
                .counters
                .first()
                .copied()
                .unwrap_or(0);
            if u64::from(run.coord.clock) + u64::from(run.length) > u64::from(clock) {
                return Err(invalid(format!(
                    "Run clocks exceed replica {}'s counter ({}+{} > {clock}) - refusing to touch this note",
                    run.coord.replica, run.coord.clock, run.length
                )));
            }
        }
    }
    validate_child_edges(&doc.runs, "note")
}

/// `validateChildEdges`.
pub fn validate_child_edges(runs: &[TextRun], what: &str) -> Result<()> {
    for (index, run) in runs.iter().enumerate() {
        if is_sentinel(run) {
            continue;
        }
        if run.sequence.is_empty() {
            return Err(invalid(format!(
                "Run {index} has no child edge - refusing to touch this {what}"
            )));
        }
        for &child in &run.sequence {
            if child as usize <= index || child as usize >= runs.len() {
                return Err(invalid(format!(
                    "Run {index} has a child edge to {child}, outside the forward range - refusing to touch this {what}"
                )));
            }
        }
    }
    Ok(())
}
