//! 3-way merge and 2-way line diff: a hand port of node-diff3 3.2.1 (MIT; Tony
//! Garnock-Jones, LShift Ltd., Bryan Housel - see NOTICE) - `LCS`,
//! `diffIndices`, `diff3MergeRegions`, `diff3Merge`, `mergeDiff3`, `diffComm` -
//! plus icloud-md's `src/notes/mergeConflict.ts`. Myers-based crates align
//! differently, so this is a port, not a dependency. Owner: workstream C.

use std::collections::HashMap;

/// `MergeOutcome` (mergeConflict.ts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    pub text: String,
    pub has_conflict: bool,
}

/// node-diff3 `diffComm` output hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommHunk {
    /// `{common: [...]}`
    Common(Vec<String>),
    /// `{buffer1: [...], buffer2: [...]}`
    Diff { buffer1: Vec<String>, buffer2: Vec<String> },
}

/// node-diff3 `mergeDiff3` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeDiff3Options {
    /// Marker labels: `<<<<<<< {a}`, `||||||| {o}`, `>>>>>>> {b}`.
    pub label_a: Option<String>,
    pub label_o: Option<String>,
    pub label_b: Option<String>,
    pub exclude_false_conflicts: bool,
}

impl Default for MergeDiff3Options {
    /// node-diff3's defaults: no labels, `excludeFalseConflicts: true`.
    fn default() -> Self {
        MergeDiff3Options {
            label_a: None,
            label_o: None,
            label_b: None,
            exclude_false_conflicts: true,
        }
    }
}

/// One link of the LCS candidate chain (`{buffer1index, buffer2index, chain}`).
struct Candidate {
    buffer1index: isize,
    buffer2index: isize,
    chain: Option<usize>,
}

/// `LCS`: Hunt-McIlroy. Returns the candidate arena and the index of the
/// chain head (`candidates[candidates.length - 1]`).
fn lcs(buffer1: &[&str], buffer2: &[&str]) -> (Vec<Candidate>, usize) {
    let mut equivalence_classes: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, item) in buffer2.iter().enumerate() {
        equivalence_classes.entry(item).or_default().push(j);
    }

    let mut arena = vec![Candidate {
        buffer1index: -1,
        buffer2index: -1,
        chain: None,
    }];
    let mut candidates: Vec<usize> = vec![0];
    let empty = Vec::new();

    for (i, item) in buffer1.iter().enumerate() {
        let buffer2indices = equivalence_classes.get(item).unwrap_or(&empty);
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

    let head = candidates[candidates.len() - 1];
    (arena, head)
}

/// node-diff3 `diffComm(buffer1, buffer2)`.
pub fn diff_comm(buffer1: &[&str], buffer2: &[&str]) -> Vec<CommHunk> {
    let (arena, head) = lcs(buffer1, buffer2);
    let mut result: Vec<CommHunk> = Vec::new();
    let mut tail1 = buffer1.len() as isize;
    let mut tail2 = buffer2.len() as isize;
    let mut common: Vec<String> = Vec::new();

    fn process_common(common: &mut Vec<String>, result: &mut Vec<CommHunk>) {
        if !common.is_empty() {
            common.reverse();
            result.push(CommHunk::Common(std::mem::take(common)));
        }
    }

    let mut candidate = Some(head);
    while let Some(index) = candidate {
        let cand = &arena[index];
        let mut different1 = Vec::new();
        let mut different2 = Vec::new();

        tail1 -= 1;
        while tail1 > cand.buffer1index {
            different1.push(buffer1[tail1 as usize].to_string());
            tail1 -= 1;
        }
        tail2 -= 1;
        while tail2 > cand.buffer2index {
            different2.push(buffer2[tail2 as usize].to_string());
            tail2 -= 1;
        }

        if !different1.is_empty() || !different2.is_empty() {
            process_common(&mut common, &mut result);
            different1.reverse();
            different2.reverse();
            result.push(CommHunk::Diff {
                buffer1: different1,
                buffer2: different2,
            });
        }

        if tail1 >= 0 {
            common.push(buffer1[tail1 as usize].to_string());
        }
        candidate = cand.chain;
    }

    process_common(&mut common, &mut result);
    result.reverse();
    result
}

/// `diffIndices` entry: `buffer1: [start, length]`, `buffer2: [start, length]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHunk {
    pub buffer1: (usize, usize),
    pub buffer2: (usize, usize),
}

/// node-diff3 `diffIndices(buffer1, buffer2)` (the `*Content` slices are
/// left to the caller).
pub fn diff_indices(buffer1: &[&str], buffer2: &[&str]) -> Vec<IndexHunk> {
    let (arena, head) = lcs(buffer1, buffer2);
    let mut result = Vec::new();
    let mut tail1 = buffer1.len() as isize;
    let mut tail2 = buffer2.len() as isize;

    let mut candidate = Some(head);
    while let Some(index) = candidate {
        let cand = &arena[index];
        let mismatch1 = tail1 - cand.buffer1index - 1;
        let mismatch2 = tail2 - cand.buffer2index - 1;
        tail1 = cand.buffer1index;
        tail2 = cand.buffer2index;
        if mismatch1 != 0 || mismatch2 != 0 {
            result.push(IndexHunk {
                buffer1: ((tail1 + 1) as usize, mismatch1 as usize),
                buffer2: ((tail2 + 1) as usize, mismatch2 as usize),
            });
        }
        candidate = cand.chain;
    }

    result.reverse();
    result
}

/// Which side a region's content comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    A,
    O,
    B,
}

/// `diff3MergeRegions` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeRegion {
    Stable {
        buffer: Side,
        start: usize,
        length: usize,
    },
    Unstable {
        a_start: usize,
        a_length: usize,
        o_start: usize,
        o_length: usize,
        b_start: usize,
        b_length: usize,
    },
}

struct Hunk {
    ab: Side,
    o_start: usize,
    o_length: usize,
    ab_start: usize,
    ab_length: usize,
}

/// node-diff3 `diff3MergeRegions(a, o, b)`.
pub fn diff3_merge_regions(a: &[&str], o: &[&str], b: &[&str]) -> Vec<MergeRegion> {
    let mut hunks: Vec<Hunk> = Vec::new();
    for (side, other) in [(Side::A, a), (Side::B, b)] {
        for item in diff_indices(o, other) {
            hunks.push(Hunk {
                ab: side,
                o_start: item.buffer1.0,
                o_length: item.buffer1.1,
                ab_start: item.buffer2.0,
                ab_length: item.buffer2.1,
            });
        }
    }
    // Array.prototype.sort is stable.
    hunks.sort_by_key(|hunk| hunk.o_start);

    let mut results = Vec::new();
    let mut curr_offset = 0usize;
    let advance_to = |end: usize, curr: &mut usize, results: &mut Vec<MergeRegion>| {
        if end > *curr {
            results.push(MergeRegion::Stable {
                buffer: Side::O,
                start: *curr,
                length: end - *curr,
            });
            *curr = end;
        }
    };

    let mut queue = hunks.into_iter().peekable();
    while let Some(hunk) = queue.next() {
        let region_start = hunk.o_start;
        let mut region_end = hunk.o_start + hunk.o_length;
        advance_to(region_start, &mut curr_offset, &mut results);
        let mut region_hunks = vec![hunk];

        while let Some(next) = queue.peek() {
            if next.o_start > region_end {
                break;
            }
            region_end = region_end.max(next.o_start + next.o_length);
            region_hunks.push(queue.next().unwrap());
        }

        if region_hunks.len() == 1 {
            let hunk = &region_hunks[0];
            if hunk.ab_length > 0 {
                results.push(MergeRegion::Stable {
                    buffer: hunk.ab,
                    start: hunk.ab_start,
                    length: hunk.ab_length,
                });
            }
        } else {
            // [abStart, abEnd, oStart, oEnd] per side.
            let mut bounds_a = [a.len() as isize, -1, o.len() as isize, -1];
            let mut bounds_b = [b.len() as isize, -1, o.len() as isize, -1];
            for hunk in &region_hunks {
                let bounds = if hunk.ab == Side::A {
                    &mut bounds_a
                } else {
                    &mut bounds_b
                };
                let o_start = hunk.o_start as isize;
                let o_end = o_start + hunk.o_length as isize;
                let ab_start = hunk.ab_start as isize;
                let ab_end = ab_start + hunk.ab_length as isize;
                bounds[0] = bounds[0].min(ab_start);
                bounds[1] = bounds[1].max(ab_end);
                bounds[2] = bounds[2].min(o_start);
                bounds[3] = bounds[3].max(o_end);
            }
            let rs = region_start as isize;
            let re = region_end as isize;
            let a_start = bounds_a[0] + (rs - bounds_a[2]);
            let a_end = bounds_a[1] + (re - bounds_a[3]);
            let b_start = bounds_b[0] + (rs - bounds_b[2]);
            let b_end = bounds_b[1] + (re - bounds_b[3]);
            results.push(MergeRegion::Unstable {
                a_start: a_start as usize,
                a_length: (a_end - a_start) as usize,
                o_start: region_start,
                o_length: region_end - region_start,
                b_start: b_start as usize,
                b_length: (b_end - b_start) as usize,
            });
        }
        curr_offset = region_end;
    }

    advance_to(o.len(), &mut curr_offset, &mut results);
    results
}

/// `diff3Merge` output block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeBlock {
    Ok(Vec<String>),
    Conflict {
        a: Vec<String>,
        a_index: usize,
        o: Vec<String>,
        o_index: usize,
        b: Vec<String>,
        b_index: usize,
    },
}

fn slice(buffer: &[&str], start: usize, length: usize) -> Vec<String> {
    // Array.prototype.slice clamps.
    let start = start.min(buffer.len());
    let end = (start + length).min(buffer.len());
    buffer[start..end].iter().map(|s| s.to_string()).collect()
}

/// node-diff3 `diff3Merge(a, o, b, {excludeFalseConflicts})` on line arrays.
pub fn diff3_merge(a: &[&str], o: &[&str], b: &[&str], exclude_false_conflicts: bool) -> Vec<MergeBlock> {
    let mut results = Vec::new();
    let mut ok: Vec<String> = Vec::new();
    for region in diff3_merge_regions(a, o, b) {
        match region {
            MergeRegion::Stable { buffer, start, length } => {
                let source = match buffer {
                    Side::A => a,
                    Side::O => o,
                    Side::B => b,
                };
                ok.extend(slice(source, start, length));
            }
            MergeRegion::Unstable {
                a_start,
                a_length,
                o_start,
                o_length,
                b_start,
                b_length,
            } => {
                let a_content = slice(a, a_start, a_length);
                let b_content = slice(b, b_start, b_length);
                if exclude_false_conflicts && a_content == b_content {
                    ok.extend(a_content);
                } else {
                    if !ok.is_empty() {
                        results.push(MergeBlock::Ok(std::mem::take(&mut ok)));
                    }
                    results.push(MergeBlock::Conflict {
                        a: a_content,
                        a_index: a_start,
                        o: slice(o, o_start, o_length),
                        o_index: o_start,
                        b: b_content,
                        b_index: b_start,
                    });
                }
            }
        }
    }
    if !ok.is_empty() {
        results.push(MergeBlock::Ok(ok));
    }
    results
}

/// node-diff3 `mergeDiff3(a, o, b, options)` → `{conflict, result}`.
pub fn merge_diff3(a: &[&str], o: &[&str], b: &[&str], options: &MergeDiff3Options) -> (bool, Vec<String>) {
    let section = |marker: &str, label: &Option<String>| match label {
        Some(label) if !label.is_empty() => format!("{marker} {label}"),
        _ => marker.to_string(),
    };
    let a_section = section("<<<<<<<", &options.label_a);
    let o_section = section("|||||||", &options.label_o);
    let x_section = "=======".to_string();
    let b_section = section(">>>>>>>", &options.label_b);

    let mut conflict = false;
    let mut result = Vec::new();
    for block in diff3_merge(a, o, b, options.exclude_false_conflicts) {
        match block {
            MergeBlock::Ok(lines) => result.extend(lines),
            MergeBlock::Conflict { a, o, b, .. } => {
                conflict = true;
                result.push(a_section.clone());
                result.extend(a);
                result.push(o_section.clone());
                result.extend(o);
                result.push(x_section.clone());
                result.extend(b);
                result.push(b_section.clone());
            }
        }
    }
    (conflict, result)
}

/// `mergeNoteVersions(base, local, remote)`: split on `\n`, `mergeDiff3(local,
/// base, remote, {label: {a: "local", o: "base", b: "remote"},
/// excludeFalseConflicts: true})`, join with `\n`. (The plan's
/// `merge_diff3(base, local, remote)`.)
pub fn merge_note_versions(base: &str, local: &str, remote: &str) -> MergeOutcome {
    fn split(text: &str) -> Vec<&str> {
        text.split('\n').collect()
    }
    let options = MergeDiff3Options {
        label_a: Some("local".into()),
        label_o: Some("base".into()),
        label_b: Some("remote".into()),
        exclude_false_conflicts: true,
    };
    let (conflict, result) = merge_diff3(&split(local), &split(base), &split(remote), &options);
    MergeOutcome {
        text: result.join("\n"),
        has_conflict: conflict,
    }
}

/// `hasConflictMarkers`: `/^(<{7}( .*)?|\|{7}( .*)?|={7}|>{7}( .*)?)$/m`.
///
/// JS `m` mode: `^`/`$` match at line terminators (`\n`, `\r`, U+2028,
/// U+2029), and `.` matches anything but those.
pub fn has_conflict_markers(text: &str) -> bool {
    text.split(['\n', '\r', '\u{2028}', '\u{2029}']).any(|line| {
        let marker = |ch: char, labelled: bool| {
            let rest = line.strip_prefix(&ch.to_string().repeat(7));
            match rest {
                Some("") => true,
                Some(rest) => labelled && rest.starts_with(' '),
                None => false,
            }
        };
        marker('<', true) || marker('|', true) || marker('=', false) || marker('>', true)
    })
}
