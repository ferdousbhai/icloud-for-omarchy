//! Per-record version snapshots (`<state dir>/history/<recordName>/*.json`)
//! and tracked-file resolution. Originally derived from icloud-md.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::rt;
use super::state::{CloneState, HISTORY_DIR_NAME, NoteEntry, state_subdir, to_js_json};
use crate::cmd::errors::Error;
use crate::js::{self, posix};

/// `VersionSnapshot`, in the key order `recordVersion` writes it
/// (`{...input, id, timestamp}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionSnapshot {
    #[serde(default)]
    pub record_name: String,
    /// "Note" | "Attachment".
    #[serde(default)]
    pub record_type: String,
    /// "TextDataEncrypted" | "MergeableDataEncrypted".
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub record_change_tag: String,
    #[serde(default)]
    pub value_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_record_name: Option<String>,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub timestamp: String,
}

/// `VersionSnapshotInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSnapshotInput {
    pub record_name: String,
    pub record_type: String,
    pub field: String,
    pub record_change_tag: String,
    pub value_base64: String,
    pub note_record_name: Option<String>,
}

impl VersionSnapshotInput {
    /// A Note record's `TextDataEncrypted`.
    pub fn note(record_name: &str, record_change_tag: &str, value_base64: &str) -> Self {
        VersionSnapshotInput {
            record_name: record_name.into(),
            record_type: "Note".into(),
            field: "TextDataEncrypted".into(),
            record_change_tag: record_change_tag.into(),
            value_base64: value_base64.into(),
            note_record_name: None,
        }
    }

    /// A table Attachment's `MergeableDataEncrypted`.
    pub fn table(record_name: &str, record_change_tag: &str, value_base64: &str, note_record_name: &str) -> Self {
        VersionSnapshotInput {
            record_name: record_name.into(),
            record_type: "Attachment".into(),
            field: "MergeableDataEncrypted".into(),
            record_change_tag: record_change_tag.into(),
            value_base64: value_base64.into(),
            note_record_name: Some(note_record_name.into()),
        }
    }
}

pub(crate) fn history_dir(target_dir: &Path) -> PathBuf {
    state_subdir(target_dir, HISTORY_DIR_NAME)
}

fn record_history_dir(target_dir: &Path, record_name: &str) -> PathBuf {
    history_dir(target_dir).join(record_name)
}

/// Versions a record keeps whatever their age (and epochs a note keeps).
pub const HISTORY_KEEP_RECENT: usize = 20;
/// Beyond the newest [`HISTORY_KEEP_RECENT`], the newest version of each
/// day this many days back is kept; everything older goes.
pub const HISTORY_RETENTION_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;

thread_local! {
    static PREVIEW: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` as a preview (`status`, `push --dry-run`): [`record_version`]
/// and [`super::epoch::record_epoch`] write nothing while it runs.
pub fn without_recording<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            PREVIEW.with(|p| p.set(self.0));
        }
    }
    let _restore = Restore(PREVIEW.with(|p| p.replace(true)));
    f()
}

pub(crate) fn recording_suppressed() -> bool {
    PREVIEW.with(Cell::get)
}

/// The first 8 hex digits of a uuid, as capture file names carry it.
pub(crate) fn short_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').take(8).collect()
}

/// `${ms}-${seq}-${shortId}.json`.
pub(crate) fn capture_file_name(captured_at_ms: i64, seq: usize, id: &str) -> String {
    let short = short_id(id);
    format!("{captured_at_ms}-{seq:06}-{short}.json")
}

/// `(ms, seq, shortId)` of a [`capture_file_name`]; `None` for any other name.
pub(crate) fn parse_capture_file_name(name: &str) -> Option<(i64, usize, &str)> {
    let mut parts = name.strip_suffix(".json")?.splitn(3, '-');
    let ms = parts.next()?.parse().ok()?;
    let seq = parts.next()?.parse().ok()?;
    Some((ms, seq, parts.next()?))
}

/// The `seq` for the next capture into a directory holding `names`: past
/// every recorded one, so pruning never makes a sequence number repeat.
/// (Unpruned, it is the count, as icloud-md numbers them.)
pub(crate) fn next_seq(names: &[String]) -> usize {
    names
        .iter()
        .filter_map(|n| parse_capture_file_name(n))
        .map(|(_, seq, _)| seq + 1)
        .max()
        .unwrap_or(0)
        .max(names.len())
}

/// Every `*.json` file name in `dir`, sorted (JS default sort: UTF-16
/// order, the same as byte order for these ASCII names). Capture names
/// start with a fixed-width millisecond time, so this is oldest first.
pub(crate) fn json_file_names(dir: &Path) -> Result<Vec<String>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(names)
}

pub(crate) fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let raw = super::local::decode_utf8(&std::fs::read(path)?);
    serde_json::from_str(&raw).map_err(|e| Error::Internal(format!("{}: {e}", path.display())))
}

/// Every `*.json` in `dir`, in [`json_file_names`] order, parsed.
pub(crate) fn read_json_dir<T: serde::de::DeserializeOwned>(dir: &Path) -> Result<Vec<T>, Error> {
    json_file_names(dir)?
        .iter()
        .map(|name| read_json_file(&dir.join(name)))
        .collect()
}

/// The first file in `dir` whose `id` is `id`, parsing only the files whose
/// name carries its short id (or isn't a capture file name).
pub(crate) fn find_json_by_id<T: serde::de::DeserializeOwned>(
    dir: &Path,
    id: &str,
    id_of: impl Fn(&T) -> &str,
) -> Result<Option<T>, Error> {
    let short = short_id(id);
    for name in json_file_names(dir)? {
        if parse_capture_file_name(&name).is_some_and(|(_, _, s)| s != short) {
            continue;
        }
        let found: T = read_json_file(&dir.join(&name))?;
        if id_of(&found) == id {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// `recordVersion`: append a snapshot unless it repeats the latest one's
/// bytes; returns whether one was written. Parses only the latest
/// snapshot, and after writing prunes the record ([`prune_history`]).
/// Writes nothing inside [`without_recording`].
pub fn record_version(target_dir: &Path, input: &VersionSnapshotInput) -> Result<bool, Error> {
    if recording_suppressed() {
        return Ok(false);
    }
    let dir = record_history_dir(target_dir, &input.record_name);
    let names = json_file_names(&dir)?;
    if let Some(last) = names.last() {
        let last: VersionSnapshot = read_json_file(&dir.join(last))?;
        if last.value_base64 == input.value_base64 {
            return Ok(false);
        }
    }
    let captured_at = rt::now_ms();
    let id = rt::random_uuid();
    let snapshot = VersionSnapshot {
        record_name: input.record_name.clone(),
        record_type: input.record_type.clone(),
        field: input.field.clone(),
        record_change_tag: input.record_change_tag.clone(),
        value_base64: input.value_base64.clone(),
        note_record_name: input.note_record_name.clone(),
        id: id.clone(),
        timestamp: js::iso_string(captured_at),
    };
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
    std::fs::write(
        dir.join(capture_file_name(captured_at, next_seq(&names), &id)),
        to_js_json(&json),
    )?;
    let note_record_name = input.note_record_name.as_deref().unwrap_or(&input.record_name);
    prune_history(target_dir, note_record_name, std::slice::from_ref(&input.record_name))?;
    Ok(true)
}

/// `listVersions`: oldest first.
pub fn list_versions(target_dir: &Path, record_name: &str) -> Result<Vec<VersionSnapshot>, Error> {
    read_json_dir(&record_history_dir(target_dir, record_name))
}

/// A record's latest snapshot, parsing only that one file.
pub fn latest_version(target_dir: &Path, record_name: &str) -> Result<Option<VersionSnapshot>, Error> {
    let dir = record_history_dir(target_dir, record_name);
    match json_file_names(&dir)?.last() {
        Some(name) => Ok(Some(read_json_file(&dir.join(name))?)),
        None => Ok(None),
    }
}

/// A record's snapshot by id, parsing only the files whose name could
/// carry it.
pub fn find_version(target_dir: &Path, record_name: &str, id: &str) -> Result<Option<VersionSnapshot>, Error> {
    find_json_by_id(
        &record_history_dir(target_dir, record_name),
        id,
        |s: &VersionSnapshot| &s.id,
    )
}

/// The retention policy over capture file names sorted oldest first: keep
/// the newest [`HISTORY_KEEP_RECENT`], plus the newest of each UTC day
/// within [`HISTORY_RETENTION_DAYS`] of `now_ms`, plus any name that isn't
/// a capture file name. One flag per name.
pub fn retention_keeps(names: &[String], now_ms: i64) -> Vec<bool> {
    let recent_from = names.len().saturating_sub(HISTORY_KEEP_RECENT);
    let daily_from = now_ms - HISTORY_RETENTION_DAYS * DAY_MS;
    let mut days_seen = HashSet::new();
    let mut keep = vec![false; names.len()];
    for (i, name) in names.iter().enumerate().rev() {
        let Some((ms, _, _)) = parse_capture_file_name(name) else {
            keep[i] = true;
            continue;
        };
        let newest_of_its_day = ms >= daily_from && days_seen.insert(ms.div_euclid(DAY_MS));
        keep[i] = i >= recent_from || newest_of_its_day;
    }
    keep
}

/// Bound a note's history: delete the note's epochs and `record_names`'
/// snapshots that [`retention_keeps`] lets go, except a snapshot a kept
/// epoch still points at. Reads only those directories' names, and parses
/// only kept epochs (small files), only when a snapshot is up for deletion.
/// Returns the number of files deleted.
pub fn prune_history(target_dir: &Path, note_record_name: &str, record_names: &[String]) -> Result<usize, Error> {
    let now = rt::now_ms();
    let mut deleted = 0;
    let epochs_dir = super::epoch::epoch_dir(target_dir, note_record_name);
    let epoch_names = json_file_names(&epochs_dir)?;
    let epoch_keeps = retention_keeps(&epoch_names, now);
    for (name, keep) in epoch_names.iter().zip(&epoch_keeps) {
        if !keep {
            deleted += remove_capture(&epochs_dir.join(name))?;
        }
    }
    // Short ids of the snapshots kept epochs point at; `None` when an epoch
    // can't be read, and then no snapshot is pruned.
    let mut referenced: Option<Option<HashSet<String>>> = None;
    for record_name in record_names {
        let dir = record_history_dir(target_dir, record_name);
        let names = json_file_names(&dir)?;
        let keeps = retention_keeps(&names, now);
        if keeps.iter().all(|k| *k) {
            continue;
        }
        let referenced = referenced.get_or_insert_with(|| {
            let mut ids = HashSet::new();
            for (name, _) in epoch_names.iter().zip(&epoch_keeps).filter(|(_, k)| **k) {
                let epoch: super::epoch::NoteEpoch = read_json_file(&epochs_dir.join(name)).ok()?;
                ids.extend(epoch.snapshots.values().flatten().map(|id| short_id(id)));
            }
            Some(ids)
        });
        let Some(referenced) = referenced else {
            continue;
        };
        for (name, keep) in names.iter().zip(&keeps) {
            let pinned = parse_capture_file_name(name).is_some_and(|(_, _, s)| referenced.contains(s));
            if !keep && !pinned {
                deleted += remove_capture(&dir.join(name))?;
            }
        }
    }
    Ok(deleted)
}

fn remove_capture(path: &Path) -> Result<usize, Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(1),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

// --- tracked-file resolution -----------------------------------------------

/// `TrackedNote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedNote {
    pub record_name: String,
    pub entry: NoteEntry,
}

fn node_resolve(base: &Path, p: &Path) -> String {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        let base = std::path::absolute(base).unwrap_or_else(|_| base.to_path_buf());
        base.join(p)
    };
    posix::normalize(&joined.to_string_lossy())
        .trim_end_matches('/')
        .to_owned()
}

/// `vaultRelativePath`.
fn vault_relative_path(file_arg: &str, target_dir: &Path, cwd: &Path) -> Option<String> {
    let root = node_resolve(Path::new("."), target_dir);
    let full = node_resolve(cwd, Path::new(file_arg));
    let root = if root.is_empty() { "/".to_owned() } else { root };
    let full = if full.is_empty() { "/".to_owned() } else { full };
    let relative = posix::relative(&root, &full);
    if relative.is_empty() || relative.starts_with("..") || relative.starts_with('/') {
        return None;
    }
    Some(relative)
}

/// `matchTrackedFile`: resolve `file_arg` against `cwd` first, then fall
/// back to a unique basename; several basename matches are
/// `AmbiguousTrackedFile`.
pub fn match_tracked_file<'a>(
    entries: &'a IndexMap<String, NoteEntry>,
    file_arg: &str,
    target_dir: &Path,
    cwd: &Path,
) -> Result<Option<(&'a String, &'a NoteEntry)>, Error> {
    if let Some(root_relative) = vault_relative_path(file_arg, target_dir, cwd)
        && let Some(exact) = entries.iter().find(|(_, e)| e.file == root_relative)
    {
        return Ok(Some(exact));
    }
    let base = posix::basename(file_arg);
    let by_basename: Vec<_> = entries
        .iter()
        .filter(|(_, e)| posix::basename(&e.file) == base)
        .collect();
    if by_basename.len() > 1 {
        return Err(Error::AmbiguousTrackedFile {
            base_name: base.to_owned(),
            candidates: by_basename.iter().map(|(_, e)| e.file.clone()).collect(),
        });
    }
    Ok(by_basename.into_iter().next())
}

/// `resolveTrackedNote`: `UntrackedFile` when nothing matches.
pub fn resolve_tracked_note(state: &CloneState, file_arg: &str, target_dir: &Path) -> Result<TrackedNote, Error> {
    let cwd = std::env::current_dir()?;
    resolve_tracked_note_from(state, file_arg, target_dir, &cwd)
}

/// `resolveTrackedNote` with an explicit cwd.
pub fn resolve_tracked_note_from(
    state: &CloneState,
    file_arg: &str,
    target_dir: &Path,
    cwd: &Path,
) -> Result<TrackedNote, Error> {
    match match_tracked_file(&state.notes, file_arg, target_dir, cwd)? {
        Some((record_name, entry)) => Ok(TrackedNote {
            record_name: record_name.clone(),
            entry: entry.clone(),
        }),
        None => Err(Error::UntrackedFile {
            file: file_arg.to_owned(),
            target_dir: target_dir.display().to_string(),
        }),
    }
}

/// `historyRecordNames`: the note itself, then its table attachments.
pub fn history_record_names(state: &CloneState, record_name: &str) -> Vec<String> {
    let mut names = vec![record_name.to_owned()];
    if let Some(tables) = &state.table_attachments {
        names.extend(
            tables
                .iter()
                .filter(|(_, e)| e.note_record_name == record_name)
                .map(|(k, _)| k.clone()),
        );
    }
    names
}

/// `findSnapshotById`: `UnknownVersionSnapshot` when none matches.
pub fn find_snapshot_by_id(
    target_dir: &Path,
    record_names: &[String],
    id: &str,
    file_arg: &str,
) -> Result<VersionSnapshot, Error> {
    for record_name in record_names {
        if let Some(found) = find_version(target_dir, record_name, id)? {
            return Ok(found);
        }
    }
    Err(Error::UnknownVersionSnapshot {
        id: id.to_owned(),
        file: file_arg.to_owned(),
    })
}
