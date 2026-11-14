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
    crate::crash::point(crate::crash::BASE_COPY);
    remove_sent_copy(target_dir, record_name)
}

fn remove_sent_copy(target_dir: &Path, record_name: &str) -> Result<(), Error> {
    match sent_copy_path(target_dir, record_name)? {
        Some(path) => super::attachments::safe_unlink(&path),
        None => Ok(()),
    }
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
    remove_sent_copy(target_dir, record_name)?;
    super::attachments::safe_unlink(&base_copy_path(target_dir, record_name)?)
}

/// Where the text a push is sending for a note is kept until the push
/// records it (`<state dir>/base/.<recordName>.sent`: a record name never
/// starts with a dot, so no base copy has the name). `None` for a record name
/// too long to fit in it, which simply keeps none.
fn sent_copy_path(target_dir: &Path, record_name: &str) -> Result<Option<PathBuf>, Error> {
    let name = format!(".{}.sent", plain_record_name(record_name)?);
    Ok((name.len() <= 255).then(|| state_subdir(target_dir, BASE_DIR_NAME).join(name)))
}

/// Keeps `content`, the body a push is about to send for the note, until
/// a base copy is written for it ([`write_base_copy`], which takes it out).
/// A push cut short after iCloud took the text leaves it: the note in iCloud
/// then reads as this copy, which is the right ancestor for merging the file
/// (edited again since, perhaps) with it ([`merge_base`]). With the older
/// base copy, the file's later edits read as a conflict with the push's own.
pub fn write_sent_copy(target_dir: &Path, record_name: &str, content: &str) -> Result<(), Error> {
    let Some(path) = sent_copy_path(target_dir, record_name)? else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    super::local::write_atomic(&path, content.as_bytes(), false)?;
    crate::crash::point(crate::crash::BASE_COPY);
    Ok(())
}

/// What a push cut short was sending for the note ([`write_sent_copy`]),
/// if it left it.
pub fn read_sent_copy(target_dir: &Path, record_name: &str) -> Result<Option<String>, Error> {
    match sent_copy_path(target_dir, record_name)? {
        Some(path) => read_text(&path),
        None => Ok(None),
    }
}

/// The ancestor to merge the note's file with `remote_text`, the note's
/// body in iCloud: what a push cut short sent ([`write_sent_copy`]) when
/// iCloud has exactly that, else the base copy (empty when there is none).
pub fn merge_base(target_dir: &Path, record_name: &str, remote_text: &str) -> Result<String, Error> {
    match read_sent_copy(target_dir, record_name)? {
        Some(sent) if sent == remote_text => Ok(sent),
        _ => Ok(read_base_copy(target_dir, record_name)?.unwrap_or_default()),
    }
}
