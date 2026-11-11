//! The local folder: what is in it, and the few changes sync makes there.
//! Removed files go to the desktop's trash, never away for good.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// What one scan saw of a file or folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub is_dir: bool,
    /// Bytes; 0 for a folder.
    pub size: u64,
    /// Modification time in Unix nanoseconds; 0 for a folder.
    pub mtime_ns: i64,
    /// The inode: the same file after a rename keeps it.
    pub ino: u64,
}

/// A path the scan left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub reason: &'static str,
}

/// The folder's contents by path relative to it (`a/b.txt`), parents before
/// children. Hidden names are left out silently (they hold this program's
/// temp files and the desktop's own), other unsyncable ones are reported.
pub fn scan(root: &Path) -> io::Result<(BTreeMap<String, Entry>, Vec<Skipped>)> {
    let mut entries = BTreeMap::new();
    let mut skipped = Vec::new();
    walk(root, "", &mut entries, &mut skipped)?;
    Ok((entries, skipped))
}

fn walk(dir: &Path, prefix: &str, entries: &mut BTreeMap<String, Entry>, skipped: &mut Vec<Skipped>) -> io::Result<()> {
    let mut names: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    names.sort_by_key(|e| e.file_name());
    // Names that differ only by case are one name to iCloud: neither is
    // synced, since there is no telling which of them iCloud's one is.
    let mut by_lower: BTreeMap<String, usize> = BTreeMap::new();
    for item in &names {
        *by_lower
            .entry(item.file_name().to_string_lossy().to_lowercase())
            .or_default() += 1;
    }
    for item in names {
        let os_name = item.file_name();
        let path = |name: &str| {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            }
        };
        let Some(name) = os_name.to_str() else {
            skipped.push(Skipped {
                path: path(&os_name.to_string_lossy()),
                reason: "its name is not valid text",
            });
            continue;
        };
        if name.starts_with('.') {
            // A download a crash cut short (engine.rs names them so); only
            // one sync runs at a time, so none is still being written.
            if name.starts_with(".icloud-drive-") && name.ends_with(".part") {
                let _ = fs::remove_file(item.path());
            }
            continue;
        }
        let rel = path(name);
        if let Some(reason) = unsyncable_name(name) {
            skipped.push(Skipped { path: rel, reason });
            continue;
        }
        let meta = fs::symlink_metadata(item.path())?;
        if meta.file_type().is_symlink() {
            skipped.push(Skipped {
                path: rel,
                reason: "it is a link",
            });
            continue;
        }
        if !meta.is_dir() && !meta.is_file() {
            skipped.push(Skipped {
                path: rel,
                reason: "it is not a regular file",
            });
            continue;
        }
        if by_lower[&name.to_lowercase()] > 1 {
            skipped.push(Skipped {
                path: rel,
                reason: "another name here differs only by case",
            });
            continue;
        }
        entries.insert(rel.clone(), entry_of(&meta));
        if meta.is_dir() {
            walk(&item.path(), &rel, entries, skipped)?;
        }
    }
    Ok(())
}

/// Why iCloud would refuse this name, if it would.
pub fn unsyncable_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name == "." || name == ".." {
        return Some("it has no name");
    }
    if name.contains(':') || name.contains('\n') || name.contains('\r') {
        return Some("iCloud does not take ':' or a line break in a name");
    }
    if name.len() > 255 {
        return Some("its name is too long");
    }
    None
}

fn entry_of(meta: &fs::Metadata) -> Entry {
    Entry {
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        mtime_ns: if meta.is_dir() {
            0
        } else {
            meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
        },
        ino: meta.ino(),
    }
}

/// One path as a scan would see it; `None` when nothing is there.
pub fn stat(path: &Path) -> io::Result<Option<Entry>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(entry_of(&meta))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Sets a file's modification time (Unix milliseconds).
pub fn set_mtime(path: &Path, mtime_ms: i64) -> io::Result<()> {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms.max(0) as u64);
    fs::File::options().write(true).open(path)?.set_modified(time)
}

/// `name (conflicted copy <host> <date>).ext`, a name not in use beside `path`.
pub fn conflict_path(path: &Path, host: &str, date: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut n = 1;
    loop {
        let suffix = if n == 1 { String::new() } else { format!(" {n}") };
        let candidate = path.with_file_name(format!("{stem} (conflicted copy {host} {date}{suffix}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Moves a file or folder to the desktop's trash (the XDG trash in the
/// home folder: `files/` plus a `.trashinfo`), so a removal that came from
/// iCloud can be undone here. Returns where it went.
pub fn trash(path: &Path, trash_dir: &Path, now_rfc3339: &str) -> io::Result<PathBuf> {
    let files = trash_dir.join("files");
    let info = trash_dir.join("info");
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut n = 1;
    let (dest, info_path) = loop {
        let candidate = if n == 1 { name.clone() } else { format!("{name}.{n}") };
        let info_path = info.join(format!("{candidate}.trashinfo"));
        // Creating the info file claims the name.
        match fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
            Ok(_) => break (files.join(&candidate), info_path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    };
    let moved = fs::rename(path, &dest);
    if let Err(e) = moved {
        let _ = fs::remove_file(&info_path);
        return Err(e);
    }
    // The spec's date is local time without a zone; UTC is close enough for
    // sorting and never wrong by more than the offset.
    let deleted = now_rfc3339.trim_end_matches('Z');
    fs::write(
        &info_path,
        format!("[Trash Info]\nPath={}\nDeletionDate={deleted}\n", percent_encode(path)),
    )?;
    Ok(dest)
}

fn percent_encode(path: &Path) -> String {
    let mut out = String::new();
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_lists_parents_first_and_leaves_out_what_cannot_sync() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/c.txt"), "c").unwrap();
        fs::write(root.join("a/x:y.txt"), "bad").unwrap();
        fs::write(root.join(".hidden"), "h").unwrap();
        fs::write(root.join("a/.icloud-drive-0123.part"), "cut short").unwrap();
        fs::write(root.join("Case.txt"), "1").unwrap();
        fs::write(root.join("case.txt"), "2").unwrap();
        std::os::unix::fs::symlink("a", root.join("link")).unwrap();
        let (entries, skipped) = scan(root).unwrap();
        let paths: Vec<_> = entries.keys().cloned().collect();
        assert_eq!(paths, ["a", "a/b", "a/b/c.txt"]);
        assert_eq!(entries["a/b/c.txt"].size, 1);
        let reasons: Vec<_> = skipped.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(reasons, ["Case.txt", "a/x:y.txt", "case.txt", "link"]);
        assert!(!root.join("a/.icloud-drive-0123.part").exists());
        assert!(root.join(".hidden").exists());
    }

    #[test]
    fn conflict_names_keep_the_extension_and_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.pdf");
        let first = conflict_path(&path, "desk", "2026-11-11");
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            "report (conflicted copy desk 2026-11-11).pdf"
        );
        fs::write(&first, "x").unwrap();
        let second = conflict_path(&path, "desk", "2026-11-11");
        assert_eq!(
            second.file_name().unwrap().to_str().unwrap(),
            "report (conflicted copy desk 2026-11-11 2).pdf"
        );
    }

    #[test]
    fn trash_moves_the_file_and_records_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("my file.txt");
        fs::write(&file, "x").unwrap();
        let trash_dir = dir.path().join("Trash");
        let dest = trash(&file, &trash_dir, "2026-11-11T10:00:00Z").unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "x");
        let info = fs::read_to_string(trash_dir.join("info/my file.txt.trashinfo")).unwrap();
        assert!(info.contains("my%20file.txt"), "{info}");
        assert!(info.contains("DeletionDate=2026-11-11T10:00:00\n"), "{info}");
        // A second file of the same name gets its own slot.
        fs::write(&file, "y").unwrap();
        let dest2 = trash(&file, &trash_dir, "2026-11-11T10:00:01Z").unwrap();
        assert_ne!(dest, dest2);
    }
}
