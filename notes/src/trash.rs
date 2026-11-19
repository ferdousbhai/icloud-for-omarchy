//! Moving a note or a folder to the desktop's trash (the freedesktop.org
//! Trash specification), where a file manager finds and restores it.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::files::{absolute_clean, bytes_of};

/// `$XDG_DATA_HOME` (`~/.local/share` when unset or relative).
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

/// The top directory of the filesystem `path` is on.
fn top_directory(path: &Path, device: u64) -> PathBuf {
    let mut top = path.to_path_buf();
    while let Some(parent) = top.parent() {
        if fs::metadata(parent).map_or(true, |m| m.dev() != device) {
            break;
        }
        top = parent.to_path_buf();
    }
    top
}

/// The trash directory for a file on `device`, and the directory the
/// record's path is relative to (`None`: an absolute path, the home trash).
fn trash_for(parent: &Path, device: u64, data_home: Option<&Path>) -> (PathBuf, Option<PathBuf>) {
    // The home trash is made when first needed, and so may the data
    // directory be: what is there of its path says which filesystem it is on.
    if let Some(home) = data_home
        && home
            .ancestors()
            .find_map(|dir| fs::metadata(dir).ok())
            .is_some_and(|m| m.dev() == device)
    {
        return (home.join("Trash"), None);
    }
    // Another filesystem: its own trash, which a file is moved to without
    // being copied. The shared one an administrator made, else this user's.
    let top = top_directory(parent, device);
    let user = rustix::process::getuid().as_raw();
    let shared = top.join(".Trash");
    let usable = fs::symlink_metadata(&shared).is_ok_and(|m| m.is_dir() && m.permissions().mode() & 0o1000 != 0);
    let trash = if usable {
        shared.join(user.to_string())
    } else {
        top.join(format!(".Trash-{user}"))
    };
    (trash, Some(top))
}

/// A path as a trash record holds it: percent-encoded but for the
/// unreserved characters and the slashes.
fn encoded(path: &Path) -> String {
    let mut out = String::new();
    for &byte in bytes_of(path) {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out += &format!("%{byte:02X}");
        }
    }
    out
}

/// Moves the file or directory at `path` to the trash, with the record a
/// file manager restores it from. The name there is its own, or its own and
/// a number when the trash holds one of that name.
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    move_to_trash_of(path, data_home().as_deref())
}

/// [`move_to_trash`], the home trash being `data_home`'s.
fn move_to_trash_of(path: &Path, data_home: Option<&Path>) -> io::Result<()> {
    let path = absolute_clean(path);
    let metadata = fs::symlink_metadata(&path)?;
    let (parent, name) = path
        .parent()
        .zip(path.file_name())
        .ok_or_else(|| io::Error::other("no name"))?;
    let (trash, relative_to) = trash_for(parent, metadata.dev(), data_home);
    let (files, info) = (trash.join("files"), trash.join("info"));
    for dir in [&files, &info] {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let recorded = relative_to.and_then(|top| path.strip_prefix(top).ok().map(Path::to_path_buf));
    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encoded(recorded.as_deref().unwrap_or(&path)),
        jiff::Zoned::now().strftime("%Y-%m-%dT%H:%M:%S")
    );
    // The record is made first and never over another: it claims the name.
    for attempt in 1.. {
        let mut trashed = name.to_os_string();
        if attempt > 1 {
            trashed.push(format!("-{attempt}"));
        }
        let mut record_name = trashed.clone();
        record_name.push(".trashinfo");
        let record_path = info.join(record_name);
        let mut file = match fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&record_path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let target = files.join(&trashed);
        let moved = file.write_all(record.as_bytes()).and_then(|()| {
            if fs::symlink_metadata(&target).is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            fs::rename(&path, &target)
        });
        match moved {
            Ok(()) => return Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&record_path);
                if e.kind() != io::ErrorKind::AlreadyExists {
                    return Err(e);
                }
            }
        }
    }
    unreachable!("the loop returns")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trashed_file_keeps_its_name_or_gains_a_number_and_has_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        // The data directory itself is not there yet: the home trash is still the one.
        fs::create_dir_all(dir.path().join("vault/Sp ace")).unwrap();
        let note = dir.path().join("vault/Sp ace/dé.md");
        for _ in 0..2 {
            fs::write(&note, "text").unwrap();
            move_to_trash_of(&note, Some(&data)).unwrap();
            assert!(!note.exists());
        }
        let trash = data.join("Trash");
        assert_eq!(fs::read_to_string(trash.join("files/dé.md")).unwrap(), "text");
        assert_eq!(fs::read_to_string(trash.join("files/dé.md-2")).unwrap(), "text");
        let record = fs::read_to_string(trash.join("info/dé.md-2.trashinfo")).unwrap();
        let (path, date) = record
            .strip_prefix("[Trash Info]\nPath=")
            .and_then(|rest| rest.trim_end().split_once("\nDeletionDate="))
            .unwrap();
        assert_eq!(path, encoded(&note));
        assert!(path.ends_with("/vault/Sp%20ace/d%C3%A9.md"), "{path}");
        assert_eq!((date.len(), &date[10..11]), (19, "T"), "{date}");
        assert_eq!(
            fs::metadata(trash.join("files")).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // A folder goes whole.
        move_to_trash_of(&dir.path().join("vault/Sp ace"), Some(&data)).unwrap();
        assert!(trash.join("files/Sp ace").is_dir() && trash.join("info/Sp ace.trashinfo").is_file());
        assert!(move_to_trash_of(&note, Some(&data)).is_err(), "nothing there to trash");
    }

    #[test]
    fn a_record_path_is_percent_encoded() {
        assert_eq!(encoded(Path::new("/a b/dé l%.md")), "/a%20b/d%C3%A9%20l%25.md");
        assert_eq!(encoded(Path::new("/plain-._~/x")), "/plain-._~/x");
    }
}
