//! Reading and writing the vault's files the way the Qt app does, so that a
//! note reads the same text in both and a save writes the same bytes.

use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::text::left_utf16;

pub fn bytes_of(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

/// `path` made absolute (from the working directory) with its `.` and `..`
/// resolved by name alone: what a path that does not exist yet is known by.
pub fn absolute_clean(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The canonical path of something that exists, else its cleaned absolute path.
pub fn canonical_or_clean(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| absolute_clean(path))
}

/// An I/O error as the system words it ("Permission denied").
pub fn os_error_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Bytes as the text of a note. Carriage returns go (every one, as a text
/// mode read drops them), a byte order mark picks the encoding and goes
/// too, and each byte that is not UTF-8 becomes one U+FFFD.
pub fn decode(bytes: &[u8]) -> String {
    let bytes: Vec<u8> = bytes.iter().copied().filter(|&b| b != b'\r').collect();
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|pair| unit(*pair)).collect();
        String::from_utf16_lossy(&units)
    };
    let utf32 = |rest: &[u8], scalar: fn([u8; 4]) -> u32| {
        let chars = rest.as_chunks::<4>().0.iter().map(|four| scalar(*four));
        chars
            .map(|c| char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    match bytes.as_slice() {
        [0xff, 0xfe, 0, 0, rest @ ..] => return utf32(rest, u32::from_le_bytes),
        [0, 0, 0xfe, 0xff, rest @ ..] => return utf32(rest, u32::from_be_bytes),
        [0xff, 0xfe, rest @ ..] => return utf16(rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => return utf16(rest, u16::from_be_bytes),
        _ => {}
    }
    utf8(&bytes)
}

/// UTF-8 bytes as text: without a leading byte order mark, and each byte
/// that is not UTF-8 as one U+FFFD.
pub fn utf8(bytes: &[u8]) -> String {
    utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
}

/// Bytes as text, each byte that is not UTF-8 as one U+FFFD.
pub fn utf8_lossy(mut rest: &[u8]) -> String {
    let mut out = String::with_capacity(rest.len());
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                out.push_str(valid);
                return out;
            }
            Err(error) => {
                let (valid, bad) = rest.split_at(error.valid_up_to());
                out.push_str(std::str::from_utf8(valid).expect("valid up to here"));
                let invalid = error.error_len().unwrap_or(bad.len());
                out.extend(std::iter::repeat_n(char::REPLACEMENT_CHARACTER, invalid));
                rest = &bad[invalid..];
            }
        }
    }
}

/// The file's text ([`decode`]), "" when it cannot be read; with `limit`,
/// its first so many UTF-16 units.
pub fn read_text(path: &Path, limit: Option<usize>) -> String {
    let mut bytes = Vec::new();
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    // No character takes more than four bytes.
    let most = limit.map_or(u64::MAX, |units| units as u64 * 4 + 16);
    if file.take(most).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = decode(&bytes);
    match limit {
        Some(units) => left_utf16(&text, units),
        None => text,
    }
}

/// Whole or not at all: the text goes to a temporary file that replaces the
/// note only once every byte is written, so a full disk never truncates it.
/// A note that is a symbolic link stays one: its target is replaced.
///
/// Replacing a file asks nothing of the file itself, only of its folder, so
/// a note this user may not write is refused here: read-only is left as it
/// is, and one that cannot be read was not read, so nothing known is saved.
pub fn write_text(path: &Path, text: &str) -> bool {
    let target = if path.is_symlink() {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let Some(dir) = target.parent() else {
        return false;
    };
    let existing = fs::metadata(&target).ok();
    if existing.is_some() && rustix::fs::access(&target, rustix::fs::Access::WRITE_OK).is_err() {
        return false;
    }
    // A short name of its own, not the note's and more: a note whose name
    // is as long as a name may be is saved too. Hidden, so one a killed run
    // left is no note and is not synced; random, so it is in no later
    // save's way.
    let create = || {
        let mut taken = None;
        for _ in 0..16 {
            let random = RandomState::new().build_hasher().finish() as u32;
            let temporary = dir.join(format!(".note-{random:08x}.tmp"));
            match File::options()
                .write(true)
                .create_new(true)
                .mode(0o666)
                .open(&temporary)
            {
                Ok(file) => return Ok((file, temporary)),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => taken = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(taken.expect("sixteen names were taken"))
    };
    let Ok((mut file, temporary)) = create() else {
        return false;
    };
    let written = (|| {
        // The note keeps the permissions it had.
        if let Some(existing) = existing {
            file.set_permissions(fs::Permissions::from_mode(existing.permissions().mode()))?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written.is_ok()
}

/// A file's modification time, in milliseconds since the epoch (0 without one).
pub fn modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis() as i64)
}

/// A file's mtime and size, "-" when there is none: what tells a changed
/// state file from the one last read.
pub fn file_stamp(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(metadata) => format!("{}:{}", modified_ms(&metadata), metadata.len()),
        Err(_) => "-".into(),
    }
}

/// Dot-directories (the sync engine's bookkeeping, .git) hold no notes.
fn is_hidden(relative: &str) -> bool {
    relative.split('/').any(|part| part.starts_with('.'))
}

/// Attachment bundles are data owned by notes, even when a file ends in .md.
pub fn is_note_path(relative: &str) -> bool {
    !is_hidden(relative) && !relative.split('/').any(|part| part.to_lowercase() == "attachments")
}

/// The entries of `dir` that are not hidden, in the directory's own order:
/// `(name, path, is a directory, is a symbolic link)`.
fn entries(dir: &Path) -> Vec<(String, PathBuf, bool, bool)> {
    let Ok(listing) = fs::read_dir(dir) else {
        return Vec::new();
    };
    listing
        .flatten()
        .filter_map(|entry| {
            // A name that is not text names no note or folder.
            let name = entry.file_name().into_string().ok()?;
            let path = entry.path();
            let metadata = fs::metadata(&path).ok()?; // what a link points to
            let link = entry.file_type().ok()?.is_symlink();
            (!name.starts_with('.') && (metadata.is_dir() || metadata.is_file()))
                .then(|| (name, path, metadata.is_dir(), link))
        })
        .collect()
}

/// Every note file under `dir`, subfolders included: `visit(its path
/// relative to dir, its absolute path)`, until `visit` returns false. A
/// folder that is a symbolic link is not entered.
pub fn for_each_note_file(dir: &Path, visit: &mut dyn FnMut(&str, &Path) -> bool) {
    fn walk(dir: &Path, prefix: &str, visit: &mut dyn FnMut(&str, &Path) -> bool) -> bool {
        for (name, path, is_dir, link) in entries(dir) {
            let relative = format!("{prefix}{name}");
            if is_dir {
                if !link && !walk(&path, &format!("{relative}/"), visit) {
                    return false;
                }
            } else if name.to_lowercase().ends_with(".md") && is_note_path(&relative) && !visit(&relative, &path) {
                return false;
            }
        }
        true
    }
    walk(dir, "", visit);
}

/// Every folder under `dir` that can hold notes, vault-relative.
pub fn note_folders(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        for (name, path, is_dir, link) in entries(dir) {
            if !is_dir {
                continue;
            }
            let relative = format!("{prefix}{name}");
            // Downloaded attachment bundles are not folders either.
            if is_note_path(&relative) {
                out.push(relative.clone());
            }
            if !link {
                walk(&path, &format!("{relative}/"), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_decoded_as_the_qt_app_decodes_it() {
        assert_eq!(decode(b"# T\r\na\rb\r\nc\n"), "# T\nab\nc\n");
        assert_eq!(decode(b"\xef\xbb\xbf# Bom\n"), "# Bom\n");
        assert_eq!(decode(b"\xff\xfe#\x00 \x00U\x00\n\x00"), "# U\n");
        assert_eq!(decode(b"\xfe\xff\x00#\x00\n"), "#\n");
        assert_eq!(
            decode(b"\xe2\x82 x \xff\xfe y \xf0\x9f\x98 z \xc0\xaf \xed\xa0\x80 q \xe2"),
            "\u{fffd}\u{fffd} x \u{fffd}\u{fffd} y \u{fffd}\u{fffd}\u{fffd} z \u{fffd}\u{fffd} \u{fffd}\u{fffd}\u{fffd} q \u{fffd}"
        );
    }

    #[test]
    fn a_write_replaces_the_file_whole_and_keeps_its_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("a.md");
        assert!(write_text(&note, "one\n"));
        fs::set_permissions(&note, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(write_text(&note, "two\n"));
        assert_eq!(fs::read_to_string(&note).unwrap(), "two\n");
        assert_eq!(fs::metadata(&note).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read_text(&note, Some(2)), "tw");
        assert_eq!(read_text(&dir.path().join("none.md"), None), "");
        assert!(!write_text(&dir.path().join("no/dir/a.md"), "x"));
        // A note this user may not write stays as it is.
        fs::set_permissions(&note, fs::Permissions::from_mode(0o400)).unwrap();
        if File::options().write(true).open(&note).is_err() {
            assert!(!write_text(&note, "three\n"));
            assert_eq!(fs::read_to_string(&note).unwrap(), "two\n");
        }
        // A name as long as a name may be leaves no room for a longer one beside it.
        let long = dir.path().join("n".repeat(255));
        assert!(write_text(&long, "one\n") && write_text(&long, "two\n"));
        fs::remove_file(long).unwrap();
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file is left"
        );
    }

    #[test]
    fn notes_and_folders_leave_out_hidden_and_attachment_directories() {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "a.md",
            "B.MD",
            "notes.txt",
            ".hidden.md",
            "Work/b.md",
            "Work/Sub/c.md",
            "Work/attachments/d.md",
            "Work/Attachments/e.md",
            ".icloud-notes/base/f.md",
        ] {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        let mut notes = Vec::new();
        for_each_note_file(dir.path(), &mut |relative, _| {
            notes.push(relative.to_string());
            true
        });
        notes.sort();
        assert_eq!(notes, ["B.MD", "Work/Sub/c.md", "Work/b.md", "a.md"]);
        let mut folders = note_folders(dir.path());
        folders.sort();
        assert_eq!(folders, ["Work", "Work/Sub"]);
        let mut seen = 0;
        for_each_note_file(dir.path(), &mut |_, _| {
            seen += 1;
            false
        });
        assert_eq!(seen, 1, "it stops when told to");
    }

    #[test]
    fn a_path_is_cleaned_by_name() {
        assert_eq!(absolute_clean(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert!(absolute_clean(Path::new("x")).is_absolute());
    }
}
