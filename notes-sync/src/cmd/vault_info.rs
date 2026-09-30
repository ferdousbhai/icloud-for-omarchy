//! `vault-info`: what the Notes app (icloud-notes) needs from the vault's
//! state, so it never parses `.icloud-md/state.json` itself. Read-only (no
//! migration, no lock). Not in icloud-md.

use std::path::Path;

use serde::Serialize;

use super::errors::Error;
use super::lock::lock_path;
use crate::doc::encode::DEFAULT_FOLDER_RECORD_NAME;
use crate::vault::state::{STATE_DIR_NAME, read_clone_state, state_file_path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    /// The vault directory, absolute.
    pub vault: String,
    /// Whether it holds a clone (`.icloud-md/state.json`).
    pub cloned: bool,
    /// The tool's own directory in the vault (`.icloud-md`), absolute.
    pub state_dir: String,
    /// The state file: vault-info's answer changes only when it does.
    pub state_file: String,
    /// `"in-body"` or `"filename"`.
    pub title_mode: &'static str,
    /// The directory of the account's default folder, when known.
    pub default_folder_dir: Option<String>,
    /// Every tracked note.
    pub notes: Vec<TrackedNote>,
    /// The vault's lock file (shared with the app; see `cmd::lock`).
    pub lock_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedNote {
    /// The note's apple-note-id (its CloudKit recordName).
    pub id: String,
    /// Vault-relative path.
    pub file: String,
    /// Why push will never send it (it follows "this note"), when read-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<String>,
    /// Where its last-synced body (the merge base) is kept, vault-relative;
    /// there is none until the note has synced.
    pub base_file: String,
}

pub fn run_vault_info(target_dir: &Path) -> Result<VaultInfo, Error> {
    let vault = std::path::absolute(target_dir)?;
    let state_dir = vault.join(STATE_DIR_NAME);
    let state = read_clone_state(&vault)?;
    let (title_mode, default_folder_dir, notes) = match &state {
        None => ("in-body", None, Vec::new()),
        Some(state) => {
            let default_folder_dir = state
                .folders
                .as_ref()
                .and_then(|f| f.get(DEFAULT_FOLDER_RECORD_NAME))
                .map(|f| f.dir_name.clone());
            let notes = state
                .notes
                .iter()
                .map(|(id, entry)| TrackedNote {
                    id: id.clone(),
                    file: entry.file.clone(),
                    read_only_reason: entry.unpublishable_reason.clone().filter(|r| !r.is_empty()),
                    base_file: format!("{STATE_DIR_NAME}/base/{id}.md"),
                })
                .collect();
            (state.mode().as_str(), default_folder_dir, notes)
        }
    };
    Ok(VaultInfo {
        vault: vault.display().to_string(),
        cloned: state.is_some(),
        state_dir: state_dir.display().to_string(),
        state_file: state_file_path(&vault).display().to_string(),
        title_mode,
        default_folder_dir,
        notes,
        lock_path: lock_path(&vault).display().to_string(),
    })
}
