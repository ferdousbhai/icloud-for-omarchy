//! Working-file state, file times and vault-root discovery. Ports icloud-md `src/notes/localFileState.ts`, `noteTimestamps.ts` and `src/vaultRoot.ts`.
//! Owner: workstream D.
#![allow(unused_variables)]

use std::path::{Path, PathBuf};

use super::state::{STATE_DIR_NAME, STATE_FILE_NAME};

/// `findVaultRoot`: walk up from `start_dir` to the directory holding
/// `.icloud-md/state.json`, git-style.
pub fn find_vault_root(start_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut dir = std::path::absolute(start_dir)?;
    loop {
        match std::fs::metadata(dir.join(STATE_DIR_NAME).join(STATE_FILE_NAME)) {
            Ok(_) => return Ok(Some(dir)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

/// `displayPath`: a vault-root-relative path shown relative to the cwd.
pub fn display_path(target_dir: &Path, file: &str) -> String {
    todo!()
}
