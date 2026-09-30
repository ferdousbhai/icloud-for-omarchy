//! Working-file state, file times and vault-root discovery. Ports icloud-md
//! `src/notes/localFileState.ts`, `noteTimestamps.ts` and `src/vaultRoot.ts`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::base::read_base_copy;
use super::state::{NoteEntry, STATE_DIR_NAME, STATE_FILE_NAME, TitleMode};
use crate::cloudkit::CloudKitRecord;
use crate::cmd::errors::Error;
use crate::md::frontmatter::{SplitOptions, split_frontmatter};

/// `readFile(path, "utf-8")`: invalid sequences become U+FFFD, a BOM stays.
pub fn decode_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `readFile(path, "utf-8")`, `None` for ENOENT.
pub fn read_text(path: &Path) -> Result<Option<String>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(decode_utf8(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `SplitFrontmatterOptions` for a vault of `mode`.
pub fn split_options(mode: TitleMode) -> SplitOptions {
    SplitOptions {
        filename_as_title: mode == TitleMode::Filename,
    }
}

/// `LocalFileState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFileState {
    Clean,
    Modified,
    Missing,
}

/// `LocalNote`: the state plus, for a file that's there, its split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalNote {
    Missing,
    Present {
        state: LocalFileState,
        frontmatter: String,
        body: String,
    },
}

impl LocalNote {
    pub fn status(&self) -> LocalFileState {
        match self {
            LocalNote::Missing => LocalFileState::Missing,
            LocalNote::Present { state, .. } => *state,
        }
    }
}

/// `readLocalNote`: compares the body (frontmatter split off) with the base
/// copy; no base copy reads as modified.
pub fn read_local_note(
    target_dir: &Path,
    entry: &NoteEntry,
    record_name: &str,
    title_mode: TitleMode,
) -> Result<LocalNote, Error> {
    let Some(content) = read_text(&target_dir.join(&entry.file))? else {
        return Ok(LocalNote::Missing);
    };
    let envelope = split_frontmatter(&content, split_options(title_mode));
    let state = match read_base_copy(target_dir, record_name)? {
        Some(base) if base == envelope.body => LocalFileState::Clean,
        _ => LocalFileState::Modified,
    };
    Ok(LocalNote::Present {
        state,
        frontmatter: envelope.frontmatter,
        body: envelope.body,
    })
}

/// `localFileState`.
pub fn local_file_state(
    target_dir: &Path,
    entry: &NoteEntry,
    record_name: &str,
    title_mode: TitleMode,
) -> Result<LocalFileState, Error> {
    Ok(read_local_note(target_dir, entry, record_name, title_mode)?.status())
}

// --- noteTimestamps.ts -------------------------------------------------------

fn date_field_of(record: &CloudKitRecord, name: &str) -> i64 {
    record
        .fields
        .get(name)
        .and_then(|f| f.value.as_i64().or_else(|| f.value.as_f64().map(|v| v as i64)))
        .unwrap_or(0)
}

/// `modificationDateOf`: 0 when absent.
pub fn modification_date_of(record: &CloudKitRecord) -> i64 {
    date_field_of(record, "ModificationDate")
}

/// `creationDateOf`: 0 when absent.
pub fn creation_date_of(record: &CloudKitRecord) -> i64 {
    date_field_of(record, "CreationDate")
}

fn system_time(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms as u64)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}

/// `applyNoteFileTimes`: mtime = the note's modification date, atime = its
/// creation date (or the mtime); nothing when the record has no date.
pub fn apply_note_file_times(file_path: &Path, record: &CloudKitRecord) -> Result<(), Error> {
    let modification = modification_date_of(record);
    if modification == 0 {
        return Ok(());
    }
    let creation = creation_date_of(record);
    let mtime = system_time(modification);
    let atime = if creation != 0 { system_time(creation) } else { mtime };
    let file = std::fs::File::open(file_path)?;
    file.set_times(std::fs::FileTimes::new().set_accessed(atime).set_modified(mtime))?;
    Ok(())
}

/// `Math.round(stat.mtimeMs)`.
pub fn mtime_ms(path: &Path) -> Result<i64, Error> {
    let modified = std::fs::metadata(path)?.modified()?;
    let ms = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_nanos() as f64 / 1e6).round() as i64,
        Err(e) => -((e.duration().as_nanos() as f64 / 1e6).round() as i64),
    };
    Ok(ms)
}

// --- vaultRoot.ts ------------------------------------------------------------

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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    display_path_from(target_dir, file, &cwd)
}

/// `displayPath` with an explicit cwd.
pub fn display_path_from(target_dir: &Path, file: &str, cwd: &Path) -> String {
    use crate::js::posix;
    let abs = |p: &Path| -> String {
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        posix::normalize(&p.to_string_lossy())
    };
    let from = abs(cwd);
    let to = abs(&target_dir.join(file));
    let relative = posix::relative(&from, &to);
    if relative.is_empty() { ".".into() } else { relative }
}
