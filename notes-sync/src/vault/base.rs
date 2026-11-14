//! Base copies: `<state dir>/base/<recordName>.md`, the body-only
//! last-synced text - the merge ancestor for pull's diff3. Originally
//! derived from icloud-md.

use std::path::{Path, PathBuf};

use crate::cmd::errors::Error;

use super::local::{read_text, split_options};
use super::state::{BASE_DIR_NAME, TitleMode, plain_record_name, state_subdir};
use crate::md::frontmatter::split_frontmatter;

fn base_copy_path(target_dir: &Path, record_name: &str) -> Result<PathBuf, Error> {
    let record_name = plain_record_name(record_name)?;
    Ok(state_subdir(target_dir, BASE_DIR_NAME).join(format!("{record_name}.md")))
}

/// `readBaseCopy`: `None` when there is none.
pub fn read_base_copy(target_dir: &Path, record_name: &str) -> Result<Option<String>, Error> {
    read_text(&base_copy_path(target_dir, record_name)?)
}

/// `writeBaseCopy`, whole or not at all: a truncated base would be a wrong
/// ancestor for the next merge.
pub fn write_base_copy(target_dir: &Path, record_name: &str, content: &str) -> Result<(), Error> {
    let path = base_copy_path(target_dir, record_name)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    super::local::write_atomic(&path, content.as_bytes(), false)?;
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
    super::attachments::safe_unlink(&base_copy_path(target_dir, record_name)?)
}
