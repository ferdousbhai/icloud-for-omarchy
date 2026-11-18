//! `leave`: the vault leaves iCloud for Omacloud (`icloud_app::leave` has the
//! design). After a last sync, the vault's state directory moves out of the
//! vault into `$XDG_DATA_HOME/icloud-notes/left-icloud-state/` (kept, never
//! deleted), the marker goes, and Omacloud is asked to sync. From then on
//! every command that reaches iCloud refuses with `left_icloud` for this
//! vault; `background` skips (step `left`); `vault-info` still answers, from
//! the kept state, for the Notes app's title mode.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use icloud_app::leave::{self as shared, Left};
use icloud_session::output::CliError;

use crate::vault::state::{LEGACY_STATE_DIR_NAME, STATE_DIR_NAME, STATE_FILE_NAME};

/// The data directory's name: the Notes app's.
pub const APP: &str = "icloud-notes";

/// Where a vault that left keeps its engine state.
pub fn kept_dir() -> Option<PathBuf> {
    icloud_app::store::data_dir(APP).map(|d| d.join("left-icloud-state"))
}

fn same(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| std::path::absolute(p).unwrap_or(p.to_path_buf()));
    canon(a) == canon(b)
}

/// The record, when this vault left iCloud on this computer (another vault
/// left says nothing about this one).
pub fn left_here(vault: &Path) -> Option<Left> {
    shared::left(APP).ok().flatten().filter(|l| same(&l.folder, vault))
}

/// `left_icloud` for a vault that left.
pub fn refuse_if_left(vault: &Path) -> Result<(), CliError> {
    match left_here(vault) {
        Some(left) => Err(shared::left_error("Notes", &left)),
        None => Ok(()),
    }
}

/// The title mode a vault that left had: `"filename"` or `"in-body"`.
pub fn kept_title_mode() -> &'static str {
    let mode = kept_dir()
        .and_then(|d| fs::read(d.join(STATE_DIR_NAME).join(STATE_FILE_NAME)).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("titleMode").and_then(|m| m.as_str()).map(str::to_owned));
    if mode.as_deref() == Some("filename") {
        "filename"
    } else {
        "in-body"
    }
}

/// Moves `from` to `to`: a rename, or a copy then removal across file
/// systems.
fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            copy_dir(from, to)?;
            fs::remove_dir_all(from)
        }
        other => other,
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// What leaving did.
#[derive(Debug, serde::Serialize)]
pub struct LeaveResult {
    pub folder: String,
    pub left_at: u64,
    /// Where the engine's state went, if any was in the vault.
    pub kept_state: Option<String>,
    /// Whether Omacloud synced at once (its daemon syncs soon otherwise).
    pub omacloud_synced: bool,
    /// Whether the vault had left already.
    pub already: bool,
}

/// After the last sync (or again, to finish a leave cut short): records it,
/// moves the state out, takes the marker away, asks Omacloud to sync.
pub fn finish(vault: &Path, already: Option<Left>) -> Result<LeaveResult, CliError> {
    let io_err = |e: io::Error| CliError::new("io", icloud_session::cli::EXIT_ERROR, e.to_string());
    let was_left = already.is_some();
    let left = match already {
        Some(l) => l,
        None => shared::record_left(APP, vault).map_err(io_err)?,
    };
    let mut kept_state = None;
    if let Some(kept) = kept_dir() {
        for name in [STATE_DIR_NAME, LEGACY_STATE_DIR_NAME] {
            let from = vault.join(name);
            if !from.is_dir() {
                continue;
            }
            let to = kept.join(name);
            if to.exists() {
                // an earlier leave's, of a vault cloned again since
                let aside = kept.join(format!("{name}.{}", left.at));
                fs::rename(&to, aside).map_err(io_err)?;
            }
            move_dir(&from, &to).map_err(io_err)?;
            if name == STATE_DIR_NAME {
                kept_state = Some(to.display().to_string());
            }
        }
    }
    shared::release(vault).map_err(io_err)?;
    let omacloud_synced = match shared::omacloud_sync() {
        Ok(()) => true,
        Err(e) => {
            icloud_app::cli::warning(&format!("{}; Omacloud's daemon syncs it soon", e.message));
            false
        }
    };
    Ok(LeaveResult {
        folder: left.folder.display().to_string(),
        left_at: left.at,
        kept_state,
        omacloud_synced,
        already: was_left,
    })
}
