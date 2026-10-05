//! The vault's state directory and `state.json`. Originally derived from
//! icloud-md (`cloneState.ts`).
//!
//! Layout 4 keeps everything in `.icloud-notes/` (state.json, base/,
//! history/, conflict-backups/); layout 3 and older kept it in `.icloud-md/`.
//! Every path into the state directory is built here ([`state_dir`],
//! [`state_subdir`], [`state_file_path`]): a layout 3 vault that hasn't been
//! migrated yet (the move happens under the vault lock, `vault::migrate`) is
//! read where it is.
//!
//! state.json is 2-space JSON with a trailing newline, absent values omitted,
//! keys in the structs' field order. Reading accepts any key order and
//! ignores unknown keys inside entries; unknown top-level keys are kept and
//! written back after the known ones.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cloudkit::SharedDatabaseCursor;
use crate::cmd::errors::Error;
use crate::js::is_record;

/// The vault's state directory (layout 4 on).
pub const STATE_DIR_NAME: &str = ".icloud-notes";
/// Where layout 3 and older kept the state. After the move it holds only a
/// tombstone state.json, so an older engine refuses the vault as written by
/// a newer version instead of treating it as not cloned.
pub const LEGACY_STATE_DIR_NAME: &str = ".icloud-md";
pub const STATE_FILE_NAME: &str = "state.json";
/// Merge bases: `base/<recordName>.md`.
pub const BASE_DIR_NAME: &str = "base";
/// Version snapshots and epochs: `history/<recordName>/`.
pub const HISTORY_DIR_NAME: &str = "history";
/// Copies the Notes app makes before replacing a note with unreadable
/// conflict markers.
pub const CONFLICT_BACKUPS_DIR_NAME: &str = "conflict-backups";
/// The subdirectories the 3 → 4 migration moves.
pub const STATE_SUBDIR_NAMES: &[&str] = &[BASE_DIR_NAME, HISTORY_DIR_NAME, CONFLICT_BACKUPS_DIR_NAME];

/// The layout this build writes; vaults above it are refused
/// (`VaultFromNewerTool`).
pub const CURRENT_LAYOUT_VERSION: u32 = 4;
/// The last layout kept in `.icloud-md/`. Read-only commands read such a
/// vault in place; commands that take the lock migrate it first.
pub const LEGACY_LAYOUT_VERSION: u32 = 3;

/// The directory holding the vault's live state.json: `.icloud-notes/` when
/// it has one, else `.icloud-md/` when that has one (a layout 3 vault not
/// migrated yet), else `.icloud-notes/` (a fresh clone).
pub fn state_dir(target_dir: &Path) -> PathBuf {
    let current = target_dir.join(STATE_DIR_NAME);
    if current.join(STATE_FILE_NAME).exists() {
        return current;
    }
    let legacy = target_dir.join(LEGACY_STATE_DIR_NAME);
    if legacy.join(STATE_FILE_NAME).exists() {
        return legacy;
    }
    current
}

/// Whether the live state is still in `.icloud-md/`.
pub fn is_legacy_state_dir(target_dir: &Path) -> bool {
    state_dir(target_dir).file_name().is_some_and(|n| n == LEGACY_STATE_DIR_NAME)
}

/// `name` (base, history, conflict-backups) in the state directory. While
/// the state is still in `.icloud-md/`, a subdirectory an interrupted
/// migration already moved is found in `.icloud-notes/`.
pub fn state_subdir(target_dir: &Path, name: &str) -> PathBuf {
    let current = target_dir.join(STATE_DIR_NAME).join(name);
    if is_legacy_state_dir(target_dir) && !current.exists() {
        return target_dir.join(LEGACY_STATE_DIR_NAME).join(name);
    }
    current
}

pub fn state_file_path(target_dir: &Path) -> PathBuf {
    state_dir(target_dir).join(STATE_FILE_NAME)
}

/// `TitleMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TitleMode {
    /// The title is the note's first line (the default).
    #[default]
    #[serde(rename = "in-body")]
    InBody,
    /// Obsidian-shaped: the file name is the title, the file holds the body.
    #[serde(rename = "filename")]
    Filename,
}

impl TitleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TitleMode::InBody => "in-body",
            TitleMode::Filename => "filename",
        }
    }
}

/// A tracked note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteEntry {
    pub file: String,
    pub record_change_tag: String,
    /// ms epoch.
    pub modification_date: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_zone_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpublishable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_record_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_rename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmatter_title: Option<String>,
}

impl NoteEntry {
    /// `{ file, recordChangeTag, modificationDate }`.
    pub fn new(file: impl Into<String>, record_change_tag: impl Into<String>, modification_date: i64) -> Self {
        NoteEntry {
            file: file.into(),
            record_change_tag: record_change_tag.into(),
            modification_date,
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> Value {
        sv(self)
    }
}

/// A folder (or shared folder) of the account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_record_name: Option<String>,
    pub dir_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_zone_owner: Option<String>,
    /// "READ_WRITE" / "READ_ONLY" for shared folders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

impl FolderEntry {
    pub fn new(name: impl Into<String>, dir_name: impl Into<String>) -> Self {
        FolderEntry {
            name: name.into(),
            dir_name: dir_name.into(),
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> Value {
        sv(self)
    }
}

/// The directory a sharer's notes go in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharerHomeEntry {
    pub name: String,
    pub dir_name: String,
}

/// A downloaded attachment file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentEntry {
    /// Vault-root-relative, e.g. `Notes/attachments/_7130093.jpeg`.
    pub file: String,
    pub media_record_name: String,
    pub media_file_checksum: String,
    pub note_record_name: String,
}

/// A table attachment and the note it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableAttachmentEntry {
    pub note_record_name: String,
}

/// A note moved to Recently Deleted by a push.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashedEntry {
    pub file: String,
    /// ms epoch.
    pub trashed_at: i64,
}

/// The account the vault is bound to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub apple_id: String,
    pub dsid: String,
}

/// state.json.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_version: Option<u32>,
    /// `"icloud-notes-sync X.Y.Z"` when written by this crate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// `None` only for a hand-built state that never set it; a read state
    /// always has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_mode: Option<TitleMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_token: Option<String>,
    /// Keyed by the zone owner's recordName.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_zone_sync_tokens: Option<IndexMap<String, String>>,
    /// Where the shared `changes/database` listing resumes; absent (an
    /// older vault, or a malformed entry) lists every shared zone from
    /// scratch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_database: Option<SharedDatabaseCursor>,
    /// base64 of 16 random bytes; set by the first push.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replica_id: Option<String>,
    pub notes: IndexMap<String, NoteEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folders: Option<IndexMap<String, FolderEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharer_homes: Option<IndexMap<String, SharerHomeEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<IndexMap<String, AttachmentEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_attachments: Option<IndexMap<String, TableAttachmentEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trashed: Option<IndexMap<String, TrashedEntry>>,
    /// Top-level keys this build doesn't know, kept as read (written after
    /// the known ones).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The top-level keys [`CloneState`] knows.
const KNOWN_KEYS: &[&str] = &[
    "layoutVersion",
    "generator",
    "titleMode",
    "account",
    "syncToken",
    "sharedZoneSyncTokens",
    "sharedDatabase",
    "replicaId",
    "notes",
    "folders",
    "sharerHomes",
    "attachments",
    "tableAttachments",
    "trashed",
];

impl CloneState {
    /// The title mode, in-body unless the state says filename.
    pub fn mode(&self) -> TitleMode {
        self.title_mode.unwrap_or_default()
    }

    pub fn to_json(&self) -> Value {
        sv(self)
    }
}

fn sv<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("state entry serializes")
}

/// The state file as raw JSON (for migrations).
pub type RawStateFile = Map<String, Value>;

/// A path as `path.join` would print it (normalized), for error messages.
pub fn node_display(path: &Path) -> String {
    crate::js::posix::normalize(&path.to_string_lossy())
}

/// `JSON.stringify(value, null, 2) + "\n"`.
pub fn to_js_json(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON value serializes");
    text.push('\n');
    text
}

/// Write `contents` to `path` atomically: a temp file beside it, then a
/// rename.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, contents)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Write `state` stamped with the current layout version and generator,
/// atomically, into `.icloud-notes/`.
pub fn write_clone_state(target_dir: &Path, state: &CloneState) -> Result<(), Error> {
    let mut stamped = state.clone();
    stamped.layout_version = Some(CURRENT_LAYOUT_VERSION);
    stamped.generator = Some(crate::GENERATOR.to_owned());
    for key in KNOWN_KEYS {
        stamped.extra.remove(*key);
    }
    let Value::Object(map) = stamped.to_json() else {
        unreachable!("CloneState serializes to an object")
    };
    write_raw_state_file(target_dir, &map)
}

/// The live state file as raw JSON; `None` when there is none.
pub fn read_raw_state_file(target_dir: &Path) -> Result<Option<RawStateFile>, Error> {
    let path = state_file_path(target_dir);
    let Some(parsed) = read_json(&path)? else {
        return Ok(None);
    };
    match parsed {
        Value::Object(map) => Ok(Some(map)),
        // An array state file has no keys, which every later check treats
        // like an empty object.
        Value::Array(_) => Ok(Some(Map::new())),
        _ => Err(Error::CorruptStateFile(format!(
            "{} does not look like a valid state file (not a JSON object).",
            node_display(&path)
        ))),
    }
}

/// Write a raw state file: layout 4 and on into `.icloud-notes/`, older ones
/// (a migration step before the move) into `.icloud-md/`. Unchanged state
/// isn't rewritten.
pub fn write_raw_state_file(target_dir: &Path, state: &RawStateFile) -> Result<(), Error> {
    let version = state.get("layoutVersion").and_then(Value::as_f64).unwrap_or(0.0);
    let dir_name = if version >= f64::from(CURRENT_LAYOUT_VERSION) {
        STATE_DIR_NAME
    } else {
        LEGACY_STATE_DIR_NAME
    };
    let dir = target_dir.join(dir_name);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(STATE_FILE_NAME);
    let bytes = to_js_json(&Value::Object(state.clone()));
    // Unchanged state isn't rewritten: its mtime is what the app watches.
    if std::fs::read(&path).is_ok_and(|on_disk| on_disk == bytes.as_bytes()) {
        return Ok(());
    }
    write_atomic(&path, bytes.as_bytes())?;
    Ok(())
}

pub(crate) fn read_json(path: &Path) -> Result<Option<Value>, Error> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let text = String::from_utf8_lossy(&raw);
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    serde_json::from_str(text)
        .map(Some)
        .map_err(|e| Error::Internal(format!("{}: {e}", path.display())))
}

/// Read and validate the vault's state without migrating it: `Ok(None)`
/// when there is no state file. A layout 3 state still in `.icloud-md/` is
/// read as it is; validation errors are `CorruptStateFile`.
pub fn read_clone_state(target_dir: &Path) -> Result<Option<CloneState>, Error> {
    let path = state_file_path(target_dir);
    match read_json(&path)? {
        None => Ok(None),
        Some(value) => assert_clone_state(&value, &path, target_dir).map(Some),
    }
}

fn obj(v: &Value) -> Option<Map<String, Value>> {
    match v {
        Value::Object(m) => Some(m.clone()),
        Value::Array(_) => Some(Map::new()),
        _ => None,
    }
}

fn opt_str(m: &Map<String, Value>, key: &str) -> Option<String> {
    m.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn num(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// The layout version check for a state file read without migrating.
fn check_layout_version(root: &Map<String, Value>, file_path: &Path, target_dir: &Path) -> Result<u32, Error> {
    let version = root.get("layoutVersion").and_then(Value::as_f64).unwrap_or(0.0);
    let legacy = file_path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == LEGACY_STATE_DIR_NAME);
    if version > f64::from(CURRENT_LAYOUT_VERSION) {
        return Err(Error::VaultFromNewerTool {
            target_dir: node_display(target_dir),
            vault_version: version as u64,
            supported_version: CURRENT_LAYOUT_VERSION,
        });
    }
    if version == f64::from(CURRENT_LAYOUT_VERSION) && !legacy {
        return Ok(CURRENT_LAYOUT_VERSION);
    }
    if version == f64::from(LEGACY_LAYOUT_VERSION) {
        return Ok(LEGACY_LAYOUT_VERSION);
    }
    if version >= 2.0 && version < f64::from(LEGACY_LAYOUT_VERSION) {
        return Err(Error::VaultNeedsUpdate {
            target_dir: node_display(target_dir),
            vault_version: version as u64,
        });
    }
    Err(Error::UnsupportedVaultLayout {
        target_dir: node_display(target_dir),
    })
}

fn assert_clone_state(value: &Value, file_path: &Path, target_dir: &Path) -> Result<CloneState, Error> {
    let fp = node_display(file_path);
    let corrupt = |msg: String| Error::CorruptStateFile(msg);
    if let Some(moved_to) = value.get("movedTo").and_then(Value::as_str) {
        return Err(corrupt(format!(
            "{fp} says the vault's state moved to {moved_to}/, but there is no state file there."
        )));
    }
    let root = match value {
        Value::Object(m) if m.get("notes").is_some_and(is_record) => m,
        _ => {
            return Err(corrupt(format!(
                "{fp} does not look like a valid state file (missing \"notes\" object)."
            )));
        }
    };

    let layout_version = check_layout_version(root, file_path, target_dir)?;

    let mut notes = IndexMap::new();
    for (record_name, entry) in obj(&root["notes"]).unwrap_or_default() {
        let m = match &entry {
            Value::Object(m)
                if m.get("file").is_some_and(Value::is_string)
                    && m.get("recordChangeTag").is_some_and(Value::is_string)
                    && m.get("modificationDate").is_some_and(Value::is_number) =>
            {
                m
            }
            _ => {
                return Err(corrupt(format!(
                    "{fp} has a malformed entry for note \"{record_name}\"."
                )));
            }
        };
        notes.insert(
            record_name,
            NoteEntry {
                file: opt_str(m, "file").unwrap_or_default(),
                record_change_tag: opt_str(m, "recordChangeTag").unwrap_or_default(),
                modification_date: num(m.get("modificationDate")).unwrap_or_default(),
                shared_zone_owner: opt_str(m, "sharedZoneOwner"),
                unpublishable_reason: opt_str(m, "unpublishableReason"),
                folder_record_name: opt_str(m, "folderRecordName"),
                pending_rename: opt_str(m, "pendingRename"),
                frontmatter_title: opt_str(m, "frontmatterTitle"),
            },
        );
    }

    let folders = match root.get("folders").and_then(obj) {
        None => None,
        Some(entries) => {
            let mut folders = IndexMap::new();
            for (record_name, entry) in entries {
                let m = match &entry {
                    Value::Object(m)
                        if m.get("name").is_some_and(Value::is_string)
                            && m.get("dirName").is_some_and(Value::is_string) =>
                    {
                        m
                    }
                    _ => {
                        return Err(corrupt(format!(
                            "{fp} has a malformed entry for folder \"{record_name}\"."
                        )));
                    }
                };
                folders.insert(
                    record_name,
                    FolderEntry {
                        name: opt_str(m, "name").unwrap_or_default(),
                        parent_record_name: opt_str(m, "parentRecordName"),
                        dir_name: opt_str(m, "dirName").unwrap_or_default(),
                        shared_zone_owner: opt_str(m, "sharedZoneOwner"),
                        permission: opt_str(m, "permission"),
                    },
                );
            }
            Some(folders)
        }
    };

    let sharer_homes = match root.get("sharerHomes").and_then(obj) {
        None => None,
        Some(entries) => {
            let mut homes = IndexMap::new();
            for (owner, entry) in entries {
                let m = match &entry {
                    Value::Object(m)
                        if m.get("name").is_some_and(Value::is_string)
                            && m.get("dirName").is_some_and(Value::is_string) =>
                    {
                        m
                    }
                    _ => {
                        return Err(corrupt(format!(
                            "{fp} has a malformed entry for sharer home \"{owner}\"."
                        )));
                    }
                };
                homes.insert(
                    owner,
                    SharerHomeEntry {
                        name: opt_str(m, "name").unwrap_or_default(),
                        dir_name: opt_str(m, "dirName").unwrap_or_default(),
                    },
                );
            }
            Some(homes)
        }
    };

    let replica_id = opt_str(root, "replicaId");

    let shared_zone_sync_tokens = match root.get("sharedZoneSyncTokens").and_then(obj) {
        None => None,
        Some(entries) => {
            let mut tokens = IndexMap::new();
            for (owner, token) in entries {
                let Some(token) = token.as_str() else {
                    return Err(corrupt(format!(
                        "{fp} has a malformed shared-zone syncToken for owner \"{owner}\"."
                    )));
                };
                tokens.insert(owner, token.to_owned());
            }
            Some(tokens)
        }
    };

    // Only a cache: a malformed entry is dropped rather than refused.
    let shared_database = root
        .get("sharedDatabase")
        .and_then(|v| serde_json::from_value::<SharedDatabaseCursor>(v.clone()).ok());

    let attachments = match root.get("attachments").and_then(obj) {
        None => None,
        Some(entries) => {
            let mut out = IndexMap::new();
            for (record_name, entry) in entries {
                let m = match &entry {
                    Value::Object(m)
                        if ["file", "mediaRecordName", "mediaFileChecksum", "noteRecordName"]
                            .iter()
                            .all(|k| m.get(*k).is_some_and(Value::is_string)) =>
                    {
                        m
                    }
                    _ => {
                        return Err(corrupt(format!(
                            "{fp} has a malformed entry for attachment \"{record_name}\"."
                        )));
                    }
                };
                out.insert(
                    record_name,
                    AttachmentEntry {
                        file: opt_str(m, "file").unwrap_or_default(),
                        media_record_name: opt_str(m, "mediaRecordName").unwrap_or_default(),
                        media_file_checksum: opt_str(m, "mediaFileChecksum").unwrap_or_default(),
                        note_record_name: opt_str(m, "noteRecordName").unwrap_or_default(),
                    },
                );
            }
            Some(out)
        }
    };

    let table_attachments = match root.get("tableAttachments").and_then(obj) {
        None => None,
        Some(entries) => {
            let mut out = IndexMap::new();
            for (record_name, entry) in entries {
                let Some(note) = entry.as_object().and_then(|m| opt_str(m, "noteRecordName")) else {
                    return Err(corrupt(format!(
                        "{fp} has a malformed entry for table attachment \"{record_name}\"."
                    )));
                };
                out.insert(record_name, TableAttachmentEntry { note_record_name: note });
            }
            Some(out)
        }
    };

    let trashed = match root.get("trashed").and_then(obj) {
        None => None,
        Some(entries) => {
            let mut out = IndexMap::new();
            for (record_name, entry) in entries {
                let m = match &entry {
                    Value::Object(m)
                        if m.get("file").is_some_and(Value::is_string)
                            && m.get("trashedAt").is_some_and(Value::is_number) =>
                    {
                        m
                    }
                    _ => {
                        return Err(corrupt(format!(
                            "{fp} has a malformed entry for trashed note \"{record_name}\"."
                        )));
                    }
                };
                out.insert(
                    record_name,
                    TrashedEntry {
                        file: opt_str(m, "file").unwrap_or_default(),
                        trashed_at: num(m.get("trashedAt")).unwrap_or_default(),
                    },
                );
            }
            Some(out)
        }
    };

    let account = match root.get("account") {
        None => None,
        Some(v) => match v.as_object() {
            Some(m)
                if m.get("appleId").is_some_and(Value::is_string) && m.get("dsid").is_some_and(Value::is_string) =>
            {
                Some(Account {
                    apple_id: opt_str(m, "appleId").unwrap_or_default(),
                    dsid: opt_str(m, "dsid").unwrap_or_default(),
                })
            }
            _ => return Err(corrupt(format!("{fp} has a malformed \"account\" field."))),
        },
    };

    let extra = root
        .iter()
        .filter(|(k, _)| !KNOWN_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    Ok(CloneState {
        layout_version: Some(layout_version),
        generator: opt_str(root, "generator"),
        title_mode: Some(if root.get("titleMode").and_then(Value::as_str) == Some("filename") {
            TitleMode::Filename
        } else {
            TitleMode::InBody
        }),
        account,
        sync_token: opt_str(root, "syncToken"),
        shared_zone_sync_tokens,
        shared_database,
        replica_id,
        notes,
        folders,
        sharer_homes,
        attachments,
        table_attachments,
        trashed,
        extra,
    })
}
