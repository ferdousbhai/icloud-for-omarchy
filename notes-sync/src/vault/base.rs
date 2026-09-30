//! Base copies: `.icloud-md/base/<recordName>.md`, the body-only
//! last-synced text - the merge ancestor for pull's diff3. Ports icloud-md
//! `src/notes/baseCopy.ts`.

use std::path::{Path, PathBuf};

use crate::cmd::errors::Error;

use super::state::STATE_DIR_NAME;

fn base_copy_path(target_dir: &Path, record_name: &str) -> PathBuf {
    target_dir
        .join(STATE_DIR_NAME)
        .join("base")
        .join(format!("{record_name}.md"))
}

/// `readBaseCopy`: `None` when there is none.
pub fn read_base_copy(target_dir: &Path, record_name: &str) -> Result<Option<String>, Error> {
    match std::fs::read(base_copy_path(target_dir, record_name)) {
        Ok(bytes) => Ok(Some(super::local::decode_utf8(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `writeBaseCopy`.
pub fn write_base_copy(target_dir: &Path, record_name: &str, content: &str) -> Result<(), Error> {
    let path = base_copy_path(target_dir, record_name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// `removeBaseCopy`: a missing copy is fine.
pub fn remove_base_copy(target_dir: &Path, record_name: &str) -> Result<(), Error> {
    super::attachments::safe_unlink(&base_copy_path(target_dir, record_name))
}
