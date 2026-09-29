//! Per-record version snapshots (`.icloud-md/history/<recordName>/*.json`)
//! and tracked-file resolution. Ports icloud-md `src/notes/versionHistory.ts`
//! and `trackedFile.ts`. Owner: workstream D.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::js::{self, posix};
use super::rt;
use super::state::{CloneState, NoteEntry, STATE_DIR_NAME, to_js_json};
use crate::cmd::errors::Error;

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
    target_dir.join(STATE_DIR_NAME).join("history")
}

fn record_history_dir(target_dir: &Path, record_name: &str) -> PathBuf {
    history_dir(target_dir).join(record_name)
}

/// `${ms}-${seq}-${shortId}.json`.
pub(crate) fn capture_file_name(captured_at_ms: i64, seq: usize, id: &str) -> String {
    let short: String = id.chars().filter(|c| *c != '-').take(8).collect();
    format!("{captured_at_ms}-{seq:06}-{short}.json")
}

/// Every `*.json` in `dir`, sorted by name (JS default sort: UTF-16 order,
/// the same as byte order for these ASCII names), parsed.
pub(crate) fn read_json_dir<T: serde::de::DeserializeOwned>(dir: &Path) -> Result<Vec<T>, Error> {
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
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let path = dir.join(&name);
        let raw = super::local::decode_utf8(&std::fs::read(&path)?);
        out.push(serde_json::from_str(&raw).map_err(|e| Error::Internal(format!("{}: {e}", path.display())))?);
    }
    Ok(out)
}

/// `recordVersion`: append a snapshot unless it repeats the latest one's
/// bytes; returns whether one was written.
pub fn record_version(target_dir: &Path, input: &VersionSnapshotInput) -> Result<bool, Error> {
    let existing = list_versions(target_dir, &input.record_name)?;
    if existing
        .last()
        .is_some_and(|last| last.value_base64 == input.value_base64)
    {
        return Ok(false);
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
    let dir = record_history_dir(target_dir, &input.record_name);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
    std::fs::write(
        dir.join(capture_file_name(captured_at, existing.len(), &id)),
        to_js_json(&json),
    )?;
    Ok(true)
}

/// `listVersions`: oldest first.
pub fn list_versions(target_dir: &Path, record_name: &str) -> Result<Vec<VersionSnapshot>, Error> {
    read_json_dir(&record_history_dir(target_dir, record_name))
}

// --- trackedFile.ts --------------------------------------------------------

/// `TrackedNote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedNote {
    pub record_name: String,
    pub entry: NoteEntry,
}

/// Anything with a vault-relative `file`.
pub trait HasFile {
    fn file(&self) -> &str;
}
impl HasFile for NoteEntry {
    fn file(&self) -> &str {
        &self.file
    }
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
pub fn match_tracked_file<'a, T: HasFile>(
    entries: &'a IndexMap<String, T>,
    file_arg: &str,
    target_dir: &Path,
    cwd: &Path,
) -> Result<Option<(&'a String, &'a T)>, Error> {
    if let Some(root_relative) = vault_relative_path(file_arg, target_dir, cwd)
        && let Some(exact) = entries.iter().find(|(_, e)| e.file() == root_relative)
    {
        return Ok(Some(exact));
    }
    let base = posix::basename(file_arg);
    let by_basename: Vec<_> = entries
        .iter()
        .filter(|(_, e)| posix::basename(e.file()) == base)
        .collect();
    if by_basename.len() > 1 {
        return Err(Error::AmbiguousTrackedFile {
            base_name: base.to_owned(),
            candidates: by_basename.iter().map(|(_, e)| e.file().to_owned()).collect(),
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
        if let Some(found) = list_versions(target_dir, record_name)?.into_iter().find(|s| s.id == id) {
            return Ok(found);
        }
    }
    Err(Error::UnknownVersionSnapshot {
        id: id.to_owned(),
        file: file_arg.to_owned(),
    })
}
