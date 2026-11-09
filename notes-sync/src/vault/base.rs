//! Base copies: `<state dir>/base/<recordName>.md`, the body-only
//! last-synced text - the merge ancestor for pull's diff3. Originally
//! derived from icloud-md.

use std::path::{Path, PathBuf};

use crate::cmd::errors::Error;

use super::local::split_options;
use super::state::{BASE_DIR_NAME, TitleMode, state_subdir};
use crate::md::frontmatter::split_frontmatter;

fn base_copy_path(target_dir: &Path, record_name: &str) -> PathBuf {
    state_subdir(target_dir, BASE_DIR_NAME).join(format!("{record_name}.md"))
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

/// The base copy of a note whose file reads `file_text`: the body that file
/// reads back as, so a file just written is clean. Not always the body it
/// was composed from: in an in-body vault the envelope takes the blank lines
/// that open a body.
pub fn write_base_copy_of(
    target_dir: &Path,
    record_name: &str,
    file_text: &str,
    title_mode: TitleMode,
) -> Result<(), Error> {
    let body = split_frontmatter(file_text, split_options(title_mode)).body;
    write_base_copy(target_dir, record_name, &body)
}

/// `removeBaseCopy`: a missing copy is fine.
pub fn remove_base_copy(target_dir: &Path, record_name: &str) -> Result<(), Error> {
    super::attachments::safe_unlink(&base_copy_path(target_dir, record_name))
}
