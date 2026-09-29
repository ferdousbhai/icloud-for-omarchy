//! 3-way merge and 2-way line diff: a hand port of node-diff3 3.2.1 (MIT; Tony
//! Garnock-Jones, LShift Ltd., Bryan Housel - see NOTICE) - `LCS`, `diffIndices`, `diff3MergeRegions`, `mergeDiff3`,
//! `diffComm` - plus icloud-md's `src/notes/mergeConflict.ts`. Myers-based
//! crates align differently, so this is a port, not a dependency.
//! Owner: workstream C.
#![allow(unused_variables)]

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

/// node-diff3 `mergeDiff3(a, o, b, options)` → `{conflict, result}`.
pub fn merge_diff3(a: &[&str], o: &[&str], b: &[&str], options: &MergeDiff3Options) -> (bool, Vec<String>) {
    todo!()
}

/// node-diff3 `diffComm(buffer1, buffer2)`.
pub fn diff_comm(a: &[&str], b: &[&str]) -> Vec<CommHunk> {
    todo!()
}

/// `mergeNoteVersions(base, local, remote)`: split on `\n`, `mergeDiff3(local,
/// base, remote, {label: {a: "local", o: "base", b: "remote"},
/// excludeFalseConflicts: true})`, join with `\n`. (The plan's
/// `merge_diff3(base, local, remote)`.)
pub fn merge_note_versions(base: &str, local: &str, remote: &str) -> MergeOutcome {
    todo!()
}

/// `hasConflictMarkers`: `/^(<{7}( .*)?|\|{7}( .*)?|={7}|>{7}( .*)?)$/m`.
pub fn has_conflict_markers(text: &str) -> bool {
    todo!()
}
