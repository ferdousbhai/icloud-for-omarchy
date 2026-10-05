//! `.icloud-md/state.json`. Ports icloud-md `src/notes/cloneState.ts`.
//!
//! Byte-exactness: 2-space JSON, trailing newline, `undefined` keys omitted,
//! and key order. JavaScript objects serialize in insertion order, and
//! icloud-md is not consistent about that order across write paths:
//!
//! - `readCloneState` rebuilds every object in a fixed order
//!   ([`READ_ORDER`], [`NOTE_READ_ORDER`], ...), with *every* key present
//!   (some `undefined`), so a read-modify-write (`push`, `status`, the pull
//!   note-update spreads) keeps that order - a spread `{...entry, x}` keeps
//!   `x` where it was;
//! - a fresh `clone` writes [`CLONE_WRITE_ORDER`], `pull` writes
//!   [`PULL_WRITE_ORDER`], and `writeCloneState` then spreads
//!   `{...state, layoutVersion, generator}`, which keeps both keys in place
//!   when the object already had them (a read state) and appends them
//!   otherwise;
//! - note and folder entries built in place keep their construction site's
//!   order (e.g. [`NOTE_CREATE_ORDER`] for push's create).
//!
//! So [`CloneState`], [`NoteEntry`] and [`FolderEntry`] carry an optional
//! `key_order` (`None` = the read order). Serialization walks it, emitting
//! the keys whose value is present, then any other present key (one
//! assigned after construction, which JavaScript appends).

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cmd::errors::Error;
use crate::js::is_record;

pub const STATE_DIR_NAME: &str = ".icloud-md";
pub const STATE_FILE_NAME: &str = "state.json";
/// Never bumped by this port; vaults above it are refused
/// (`VaultFromNewerTool`).
pub const CURRENT_LAYOUT_VERSION: u32 = 3;

/// `CloneState` keys as `readCloneState` builds them.
const READ_ORDER: &[&str] = &[
    "layoutVersion",
    "generator",
    "titleMode",
    "account",
    "syncToken",
    "sharedZoneSyncTokens",
    "replicaId",
    "notes",
    "folders",
    "sharerHomes",
    "attachments",
    "tableAttachments",
    "trashed",
];

/// The object `runClone` hands `writeCloneState`.
pub const CLONE_WRITE_ORDER: &[&str] = &[
    "account",
    "titleMode",
    "syncToken",
    "sharedZoneSyncTokens",
    "notes",
    "folders",
    "sharerHomes",
    "attachments",
    "tableAttachments",
];

/// The object `runPull` hands `writeCloneState`.
pub const PULL_WRITE_ORDER: &[&str] = &[
    "account",
    "syncToken",
    "sharedZoneSyncTokens",
    "replicaId",
    "titleMode",
    "notes",
    "folders",
    "sharerHomes",
    "attachments",
    "tableAttachments",
    "trashed",
];

/// `CloneStateNoteEntry` keys as `readCloneState` builds them.
const NOTE_READ_ORDER: &[&str] = &[
    "file",
    "recordChangeTag",
    "modificationDate",
    "sharedZoneOwner",
    "unpublishableReason",
    "folderRecordName",
    "pendingRename",
    "frontmatterTitle",
];

/// A note entry built by clone's and pull's "new note" paths.
pub const NOTE_ADD_ORDER: &[&str] = &[
    "file",
    "recordChangeTag",
    "modificationDate",
    "sharedZoneOwner",
    "unpublishableReason",
    "folderRecordName",
    "frontmatterTitle",
];

/// A note entry built by push's create.
pub const NOTE_CREATE_ORDER: &[&str] = &[
    "file",
    "recordChangeTag",
    "modificationDate",
    "folderRecordName",
    "sharedZoneOwner",
];

/// `CloneStateFolderEntry` keys as `readCloneState` (and `buildVaultLayout`)
/// build them.
const FOLDER_READ_ORDER: &[&str] = &["name", "parentRecordName", "dirName", "sharedZoneOwner", "permission"];

/// A folder entry built by push's folder create.
pub const FOLDER_CREATE_ORDER: &[&str] = &["name", "dirName", "parentRecordName"];

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

/// `CloneStateNoteEntry`.
#[derive(Debug, Clone, Default)]
pub struct NoteEntry {
    pub file: String,
    pub record_change_tag: String,
    /// ms epoch (a JS number; always integral in practice).
    pub modification_date: i64,
    pub shared_zone_owner: Option<String>,
    pub unpublishable_reason: Option<String>,
    pub folder_record_name: Option<String>,
    pub pending_rename: Option<String>,
    pub frontmatter_title: Option<String>,
    /// JSON key order; `None` = [`NOTE_READ_ORDER`].
    pub key_order: Option<Vec<&'static str>>,
}

impl PartialEq for NoteEntry {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file
            && self.record_change_tag == other.record_change_tag
            && self.modification_date == other.modification_date
            && self.shared_zone_owner == other.shared_zone_owner
            && self.unpublishable_reason == other.unpublishable_reason
            && self.folder_record_name == other.folder_record_name
            && self.pending_rename == other.pending_rename
            && self.frontmatter_title == other.frontmatter_title
    }
}
impl Eq for NoteEntry {}

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

    pub fn with_order(mut self, order: &[&'static str]) -> Self {
        self.key_order = Some(order.to_vec());
        self
    }

    fn get(&self, key: &str) -> Option<Value> {
        let s = |v: &Option<String>| v.as_ref().map(|v| Value::String(v.clone()));
        match key {
            "file" => Some(self.file.clone().into()),
            "recordChangeTag" => Some(self.record_change_tag.clone().into()),
            "modificationDate" => Some(self.modification_date.into()),
            "sharedZoneOwner" => s(&self.shared_zone_owner),
            "unpublishableReason" => s(&self.unpublishable_reason),
            "folderRecordName" => s(&self.folder_record_name),
            "pendingRename" => s(&self.pending_rename),
            "frontmatterTitle" => s(&self.frontmatter_title),
            _ => None,
        }
    }

    pub fn to_json(&self) -> Value {
        ordered_object(
            self.key_order.as_deref().unwrap_or(NOTE_READ_ORDER),
            NOTE_READ_ORDER,
            |k| self.get(k),
        )
    }
}

/// `CloneStateFolderEntry`.
#[derive(Debug, Clone, Default)]
pub struct FolderEntry {
    pub name: String,
    pub parent_record_name: Option<String>,
    pub dir_name: String,
    pub shared_zone_owner: Option<String>,
    /// "READ_WRITE" / "READ_ONLY" for shared folders.
    pub permission: Option<String>,
    /// JSON key order; `None` = [`FOLDER_READ_ORDER`].
    pub key_order: Option<Vec<&'static str>>,
}

impl PartialEq for FolderEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.parent_record_name == other.parent_record_name
            && self.dir_name == other.dir_name
            && self.shared_zone_owner == other.shared_zone_owner
            && self.permission == other.permission
    }
}
impl Eq for FolderEntry {}

impl FolderEntry {
    pub fn new(name: impl Into<String>, dir_name: impl Into<String>) -> Self {
        FolderEntry {
            name: name.into(),
            dir_name: dir_name.into(),
            ..Default::default()
        }
    }

    fn get(&self, key: &str) -> Option<Value> {
        let s = |v: &Option<String>| v.as_ref().map(|v| Value::String(v.clone()));
        match key {
            "name" => Some(self.name.clone().into()),
            "parentRecordName" => s(&self.parent_record_name),
            "dirName" => Some(self.dir_name.clone().into()),
            "sharedZoneOwner" => s(&self.shared_zone_owner),
            "permission" => s(&self.permission),
            _ => None,
        }
    }

    pub fn to_json(&self) -> Value {
        ordered_object(
            self.key_order.as_deref().unwrap_or(FOLDER_READ_ORDER),
            FOLDER_READ_ORDER,
            |k| self.get(k),
        )
    }
}

/// `CloneStateSharerHomeEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharerHomeEntry {
    pub name: String,
    pub dir_name: String,
}

/// `CloneStateAttachmentEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentEntry {
    /// Vault-root-relative, e.g. `Notes/attachments/_7130093.jpeg`.
    pub file: String,
    pub media_record_name: String,
    pub media_file_checksum: String,
    pub note_record_name: String,
}

/// `CloneStateTableAttachmentEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableAttachmentEntry {
    pub note_record_name: String,
}

/// `CloneStateTrashedEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashedEntry {
    pub file: String,
    /// ms epoch.
    pub trashed_at: i64,
}

/// `CloneStateAccount`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub apple_id: String,
    pub dsid: String,
}

/// `CloneState`.
#[derive(Debug, Clone, Default)]
pub struct CloneState {
    pub layout_version: Option<u32>,
    /// `"icloud-notes-sync X.Y.Z"` when written by this crate.
    pub generator: Option<String>,
    /// `None` only for a hand-built state that never set it (TS
    /// `titleMode?:`); a read state always has it.
    pub title_mode: Option<TitleMode>,
    pub account: Option<Account>,
    pub sync_token: Option<String>,
    /// Keyed by the zone owner's recordName.
    pub shared_zone_sync_tokens: Option<IndexMap<String, String>>,
    /// base64 of 16 random bytes; set by the first push.
    pub replica_id: Option<String>,
    pub notes: IndexMap<String, NoteEntry>,
    pub folders: Option<IndexMap<String, FolderEntry>>,
    pub sharer_homes: Option<IndexMap<String, SharerHomeEntry>>,
    pub attachments: Option<IndexMap<String, AttachmentEntry>>,
    pub table_attachments: Option<IndexMap<String, TableAttachmentEntry>>,
    pub trashed: Option<IndexMap<String, TrashedEntry>>,
    /// JSON key order; `None` = [`READ_ORDER`].
    pub key_order: Option<Vec<&'static str>>,
}

impl PartialEq for CloneState {
    fn eq(&self, other: &Self) -> bool {
        self.layout_version == other.layout_version
            && self.generator == other.generator
            && self.title_mode == other.title_mode
            && self.account == other.account
            && self.sync_token == other.sync_token
            && self.shared_zone_sync_tokens == other.shared_zone_sync_tokens
            && self.replica_id == other.replica_id
            && self.notes == other.notes
            && self.folders == other.folders
            && self.sharer_homes == other.sharer_homes
            && self.attachments == other.attachments
            && self.table_attachments == other.table_attachments
            && self.trashed == other.trashed
    }
}
impl Eq for CloneState {}

impl CloneState {
    /// `state.titleMode === "filename" ? "filename" : "in-body"`.
    pub fn mode(&self) -> TitleMode {
        self.title_mode.unwrap_or_default()
    }

    fn get(&self, key: &str) -> Option<Value> {
        fn map<V>(m: &Option<IndexMap<String, V>>, f: impl Fn(&V) -> Value) -> Option<Value> {
            m.as_ref()
                .map(|m| Value::Object(m.iter().map(|(k, v)| (k.clone(), f(v))).collect()))
        }
        match key {
            "layoutVersion" => self.layout_version.map(Value::from),
            "generator" => self.generator.clone().map(Value::from),
            "titleMode" => self.title_mode.map(|m| Value::from(m.as_str())),
            "account" => self.account.as_ref().map(sv),
            "syncToken" => self.sync_token.clone().map(Value::from),
            "sharedZoneSyncTokens" => map(&self.shared_zone_sync_tokens, |v| Value::from(v.clone())),
            "replicaId" => self.replica_id.clone().map(Value::from),
            "notes" => Some(Value::Object(
                self.notes.iter().map(|(k, v)| (k.clone(), v.to_json())).collect(),
            )),
            "folders" => map(&self.folders, FolderEntry::to_json),
            "sharerHomes" => map(&self.sharer_homes, sv),
            "attachments" => map(&self.attachments, sv),
            "tableAttachments" => map(&self.table_attachments, sv),
            "trashed" => map(&self.trashed, sv),
            _ => None,
        }
    }

    /// The object as JavaScript would hold it (see the module doc).
    pub fn to_json(&self) -> Value {
        ordered_object(self.key_order.as_deref().unwrap_or(READ_ORDER), READ_ORDER, |k| {
            self.get(k)
        })
    }
}

fn sv<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("state entry serializes")
}

/// Emit `order`'s present keys, then any other present key from `all`.
fn ordered_object(order: &[&str], all: &[&str], get: impl Fn(&str) -> Option<Value>) -> Value {
    let mut out = Map::new();
    for key in order.iter().chain(all.iter()) {
        if out.contains_key(*key) {
            continue;
        }
        if let Some(v) = get(key) {
            out.insert((*key).to_owned(), v);
        }
    }
    Value::Object(out)
}

/// The state file as raw JSON (for migrations).
pub type RawStateFile = Map<String, Value>;

pub fn state_file_path(target_dir: &Path) -> PathBuf {
    target_dir.join(STATE_DIR_NAME).join(STATE_FILE_NAME)
}

/// `targetDir` as Node's `path.join` would print it (normalized), for
/// error messages.
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
/// rename. (icloud-md writes in place; the bytes are the same.)
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
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

/// `writeCloneState`: `{...state, layoutVersion: 3, generator}`, written
/// atomically.
pub fn write_clone_state(target_dir: &Path, state: &CloneState) -> Result<(), Error> {
    let mut stamped = state.clone();
    stamped.layout_version = Some(CURRENT_LAYOUT_VERSION);
    stamped.generator = Some(crate::GENERATOR.to_owned());
    let Value::Object(map) = stamped.to_json() else {
        unreachable!("CloneState serializes to an object")
    };
    write_raw_state_file(target_dir, &map)
}

/// `readRawStateFile`.
pub fn read_raw_state_file(target_dir: &Path) -> Result<Option<RawStateFile>, Error> {
    let path = state_file_path(target_dir);
    let Some(parsed) = read_json(&path)? else {
        return Ok(None);
    };
    match parsed {
        Value::Object(map) => Ok(Some(map)),
        // `isRecord` also accepts arrays (typeof [] === "object"); an array
        // state file has no keys, which every later check treats alike.
        Value::Array(_) => Ok(Some(Map::new())),
        _ => Err(Error::CorruptStateFile(format!(
            "{} does not look like a valid state file (not a JSON object).",
            node_display(&path)
        ))),
    }
}

/// `writeRawStateFile`.
pub fn write_raw_state_file(target_dir: &Path, state: &RawStateFile) -> Result<(), Error> {
    let dir = target_dir.join(STATE_DIR_NAME);
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

fn read_json(path: &Path) -> Result<Option<Value>, Error> {
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

/// `readCloneState`: `Ok(None)` when there is no state file; validation
/// errors are `CorruptStateFile` with icloud-md's messages;
/// `layoutVersion != 3` is `UnsupportedVaultLayout`.
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

fn assert_clone_state(value: &Value, file_path: &Path, target_dir: &Path) -> Result<CloneState, Error> {
    let fp = node_display(file_path);
    let corrupt = |msg: String| Error::CorruptStateFile(msg);
    let root = match value {
        Value::Object(m) if m.get("notes").is_some_and(is_record) => m,
        _ => {
            return Err(corrupt(format!(
                "{fp} does not look like a valid state file (missing \"notes\" object)."
            )));
        }
    };

    if root.get("layoutVersion").and_then(Value::as_f64) != Some(f64::from(CURRENT_LAYOUT_VERSION)) {
        return Err(Error::UnsupportedVaultLayout {
            target_dir: node_display(target_dir),
        });
    }

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
                key_order: None,
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
                        key_order: None,
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

    Ok(CloneState {
        layout_version: Some(CURRENT_LAYOUT_VERSION),
        generator: opt_str(root, "generator"),
        title_mode: Some(if root.get("titleMode").and_then(Value::as_str) == Some("filename") {
            TitleMode::Filename
        } else {
            TitleMode::InBody
        }),
        account,
        sync_token: opt_str(root, "syncToken"),
        shared_zone_sync_tokens,
        replica_id,
        notes,
        folders,
        sharer_homes,
        attachments,
        table_attachments,
        trashed,
        key_order: None,
    })
}
