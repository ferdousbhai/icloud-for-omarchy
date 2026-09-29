//! `.icloud-md/state.json`. Ports icloud-md `src/notes/cloneState.ts`.
//! Owner: workstream D.
//!
//! Byte-exactness: 2-space JSON, trailing newline, `undefined` keys omitted
//! (hence every `skip_serializing_if`), and key order. The structs below
//! serialize in the order `readCloneState` rebuilds objects, which is what a
//! read-modify-write command (pull, push, status) writes back. icloud-md is
//! NOT consistent about order everywhere, so a fixed serde order is not
//! enough on its own:
//! - a fresh `clone` writes `account, titleMode, syncToken,
//!   sharedZoneSyncTokens, notes, folders, sharerHomes, attachments,
//!   tableAttachments` and then appends `layoutVersion, generator` last
//!   (`writeCloneState` spreads `{...state, layoutVersion, generator}`);
//! - note entries built in place keep the construction site's order, e.g.
//!   push's create writes `file, recordChangeTag, modificationDate,
//!   folderRecordName, sharedZoneOwner`, and pull's `...renameFields` puts
//!   `pendingRename` after `frontmatterTitle`; spreads (`{...entry, x}`) keep
//!   the existing position of `x`.
//!
//! `write_clone_state` has to reproduce those orders (e.g. by serializing
//! through `serde_json::Value` with per-site key order, or a key-order
//! vector carried alongside) for `diff -r` against icloud-md to pass.
#![allow(unused_variables)]

use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::cmd::errors::Error;

pub const STATE_DIR_NAME: &str = ".icloud-md";
pub const STATE_FILE_NAME: &str = "state.json";
/// Never bumped by this port; vaults above it are refused
/// (`VaultFromNewerTool`).
pub const CURRENT_LAYOUT_VERSION: u32 = 3;

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteEntry {
    pub file: String,
    pub record_change_tag: String,
    /// ms epoch (a JS number; always integral in practice).
    pub modification_date: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_zone_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpublishable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_record_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter_title: Option<String>,
}

/// `CloneStateFolderEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_record_name: Option<String>,
    pub dir_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_zone_owner: Option<String>,
    /// "READ_WRITE" / "READ_ONLY" for shared folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
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

/// `CloneState`, in `readCloneState`'s key order (see the module doc).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_version: Option<u32>,
    /// `"icloud-notes-sync X.Y.Z"` when written by this crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(default)]
    pub title_mode: TitleMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_token: Option<String>,
    /// Keyed by the zone owner's recordName.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_zone_sync_tokens: Option<IndexMap<String, String>>,
    /// base64 of 16 random bytes; set by the first push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_id: Option<String>,
    pub notes: IndexMap<String, NoteEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folders: Option<IndexMap<String, FolderEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharer_homes: Option<IndexMap<String, SharerHomeEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<IndexMap<String, AttachmentEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_attachments: Option<IndexMap<String, TableAttachmentEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed: Option<IndexMap<String, TrashedEntry>>,
}

/// The state file as raw JSON (for migrations).
pub type RawStateFile = serde_json::Map<String, serde_json::Value>;

/// `readCloneState`: `Ok(None)` when there is no state file; validation
/// errors are `CorruptStateFile` with icloud-md's messages;
/// `layoutVersion != 3` is `UnsupportedVaultLayout` (or `VaultFromNewerTool`
/// above 3).
pub fn read_clone_state(target_dir: &Path) -> Result<Option<CloneState>, Error> {
    todo!()
}

/// `writeCloneState`: stamps `layoutVersion: 3` and the generator, writes
/// atomically (temp file + rename).
pub fn write_clone_state(target_dir: &Path, state: &CloneState) -> Result<(), Error> {
    todo!()
}

/// `readRawStateFile`.
pub fn read_raw_state_file(target_dir: &Path) -> Result<Option<RawStateFile>, Error> {
    todo!()
}

/// `writeRawStateFile`.
pub fn write_raw_state_file(target_dir: &Path, state: &RawStateFile) -> Result<(), Error> {
    todo!()
}
