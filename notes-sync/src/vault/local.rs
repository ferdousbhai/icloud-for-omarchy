//! Working-file state, file times and vault-root discovery. Originally
//! derived from icloud-md.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::base::read_base_copy;
use super::state::{LEGACY_STATE_DIR_NAME, NoteEntry, STATE_DIR_NAME, STATE_FILE_NAME, TitleMode};
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

/// A temp file's path beside `path` (so the rename stays on one
/// filesystem): `.<pid>.<n>.tmp`, hidden, and short whatever `path`'s name
/// is - a name built on a note's (up to 243 bytes of it) could pass the 255
/// a file name holds.
fn temp_sibling(path: &Path) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.with_file_name(format!(".{}.{n}.tmp", std::process::id()))
}

/// Writes `contents` to `path` whole or not at all: a temp file beside it,
/// synced, then a rename, so a crash or a full disk leaves the old file or
/// the new one, never a truncated one. With `keep_permissions`, an existing
/// file's permissions carry over.
pub(crate) fn write_atomic(path: &Path, contents: &[u8], keep_permissions: bool) -> std::io::Result<()> {
    let tmp = temp_sibling(path);
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents)?;
        if keep_permissions && let Ok(meta) = std::fs::metadata(path) {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        crate::crash::point(crate::crash::TEMP_FILE);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The largest process id Linux hands out is one below this
/// (`PID_MAX_LIMIT`).
const PID_LIMIT: u32 = 4_194_304;

/// The process id in the name of a temporary file this engine leaves
/// beside what it writes: [`temp_sibling`]'s `.<pid>.<n>.tmp`, and, with
/// `downloads` (in an `attachments/` directory), icloud-session's
/// `.<file name>.<pid>.<n>.tmp` for a download. Exactly as those are
/// written: `<pid>` and `<n>` are plain decimal numbers (no sign, no
/// leading zero), the pid one a process can have and the counter one that
/// fits its 64 bits. `None` for any other name, a file-sync tool's own
/// temporaries (`.syncthing.*.tmp`) among them.
fn temporary_pid(name: &str, downloads: bool) -> Option<u32> {
    let stem = name.strip_prefix('.')?.strip_suffix(".tmp")?;
    let mut parts = stem.rsplitn(3, '.');
    let number = |part: &str| -> Option<u64> {
        let plain = part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0'));
        plain.then(|| part.parse().ok())?
    };
    let (n, pid, file_name) = (parts.next()?, parts.next()?, parts.next());
    let ours = match file_name {
        None => true,
        Some(file_name) => downloads && !file_name.is_empty() && !stem.starts_with("syncthing."),
    };
    number(n)?;
    let pid = u32::try_from(number(pid)?).ok()?;
    (ours && (1..=PID_LIMIT).contains(&pid)).then_some(pid)
}

/// Removes from `dir` the temporaries ([`temporary_pid`]) of processes that
/// are no longer running: what a run that was killed mid-write left there.
/// Only regular files. Best effort, and only where running processes can be
/// told (`/proc`).
pub(crate) fn remove_stale_temporaries(dir: &Path, downloads: bool) {
    let proc = Path::new("/proc");
    if !proc.join("self").exists() {
        return;
    }
    for name in file_names_in(dir).unwrap_or_default() {
        let path = dir.join(&name);
        if temporary_pid(&name, downloads).is_some_and(|pid| !proc.join(pid.to_string()).exists())
            && std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file())
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Clears what a killed run left half-written in the vault
/// ([`remove_stale_temporaries`]): in the vault's top directory and every
/// directory of `dirs` (the layout's), in the `attachments/` of each (where
/// a download cut off can be as large as the attachment, and a file-sync
/// tool would carry it to other devices), and in the engine's own state
/// directory and its base copies. For a command that holds the vault's
/// lock: no other run of this engine is writing there.
pub fn remove_stale_temporaries_in_vault(target_dir: &Path, dirs: &[String]) {
    for dir in std::iter::once("").chain(dirs.iter().map(String::as_str)) {
        let dir = target_dir.join(dir);
        remove_stale_temporaries(&dir, false);
        remove_stale_temporaries(&dir.join("attachments"), true);
    }
    remove_stale_temporaries(&super::state::state_dir(target_dir), false);
    remove_stale_temporaries(
        &super::state::state_subdir(target_dir, super::state::BASE_DIR_NAME),
        false,
    );
}

/// rename(2) that fails with `AlreadyExists` instead of replacing what is
/// at `to` (a file, a directory, or a symbolic link, dangling or not):
/// `renameat2(RENAME_NOREPLACE)`, so a file that turns up at `to` after the
/// caller found the name free (one a file-sync tool delivers, say) is never
/// lost. On a filesystem without it, a check and then a plain rename.
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Err(Errno::INVAL | Errno::NOSYS) => {
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(from, to)?;
        }
        result => result?,
    }
    crate::crash::point(crate::crash::RENAME);
    Ok(())
}

/// Writes a note file whole or not at all ([`write_atomic`]), so a crash or
/// a full disk never leaves a truncated note for push to upload. An existing
/// file keeps its permissions; a symlink keeps pointing where it did, its
/// target replaced.
pub fn write_note_file(path: &Path, contents: &str) -> Result<(), Error> {
    let path = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    write_atomic(&path, contents.as_bytes(), true)?;
    crate::crash::point(crate::crash::NOTE_FILE);
    Ok(())
}

/// The name of every entry in `dir`, in the order the directory lists them;
/// none when there is no such directory.
pub(crate) fn file_names_in(dir: &Path) -> Result<Vec<String>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
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

// --- file times --------------------------------------------------------------

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

// --- vault root --------------------------------------------------------------

/// `findVaultRoot`: walk up from `start_dir` to the directory holding
/// `.icloud-notes/state.json` (or a layout 3 vault's `.icloud-md/state.json`),
/// git-style.
pub fn find_vault_root(start_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut dir = std::path::absolute(start_dir)?;
    loop {
        for state_dir in [STATE_DIR_NAME, LEGACY_STATE_DIR_NAME] {
            match std::fs::metadata(dir.join(state_dir).join(STATE_FILE_NAME)) {
                Ok(_) => return Ok(Some(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
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

#[cfg(test)]
mod temporary_tests {
    use super::temporary_pid;

    /// Only the names this engine and icloud-session write.
    #[test]
    fn temporary_names_are_exactly_the_ones_written() {
        assert_eq!(temporary_pid(".1234.0.tmp", false), Some(1234));
        assert_eq!(temporary_pid(".photo.jpeg.1234.17.tmp", true), Some(1234));
        assert_eq!(temporary_pid(".a.b.c.4194304.0.tmp", true), Some(4_194_304));
        // A download's name only where downloads go.
        assert_eq!(temporary_pid(".photo.jpeg.1234.17.tmp", false), None);
        // Numbers a pid and a counter are never written as.
        for name in [
            ".report.0123.4.tmp",
            ".report.123.04.tmp",
            ".report.0.4.tmp",
            ".report.4194305.4.tmp",
            ".report.20251114.120000.tmp",
            ".report.123.99999999999999999999999.tmp",
            ".report.+12.3.tmp",
            ".report.12..tmp",
            "..12.3.tmp",
        ] {
            assert_eq!(temporary_pid(name, true), None, "{name}");
        }
        assert_eq!(temporary_pid(".012.3.tmp", false), None);
        assert_eq!(temporary_pid(".syncthing.Pie.md.1234.3.tmp", true), None);
        assert_eq!(temporary_pid("photo.jpeg.1234.17.tmp", true), None);
        assert_eq!(temporary_pid(".photo.jpeg.1234.17.part", true), None);
    }
}
