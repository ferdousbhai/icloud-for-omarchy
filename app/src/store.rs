//! Where an app keeps its files, and how: one directory under
//! `XDG_DATA_HOME`, a `flock` around every read-modify-write (the window,
//! the command line and a timer may run at once), and JSON files replaced
//! whole (a temp file, then rename). A file that does not parse is an error
//! naming it, never silently reset.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// `$XDG_DATA_HOME/<app>` (`~/.local/share/<app>`; a relative
/// `XDG_DATA_HOME` is ignored, as the XDG spec says); `None` without either.
pub fn data_dir(app: &str) -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join(app))
}

/// Held while it lives; dropping it unlocks.
pub struct Lock(#[allow(dead_code)] File);

/// Takes the exclusive `flock` on `<dir>/<name>`, making `dir` if needed;
/// waits while another process holds it.
pub fn lock(dir: &Path, name: &str) -> io::Result<Lock> {
    fs::create_dir_all(dir)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(Lock(f))
}

/// The error for a file of the app's that does not read as what it wrote.
pub fn damaged(path: &Path, e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{} is damaged ({e}); delete it to start over", path.display()))
}

/// `None` when the file does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| damaged(path, e))
}

/// [`read_json`] for a file with a top-level `version`: `None` too when
/// another version of the app wrote it.
pub fn read_json_version<T: DeserializeOwned>(path: &Path, version: u32) -> io::Result<Option<T>> {
    let Some(v) = read_json::<serde_json::Value>(path)? else {
        return Ok(None);
    };
    if v.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(version)) {
        return Ok(None);
    }
    serde_json::from_value(v).map(Some).map_err(|e| damaged(path, e))
}

pub fn write_json<T: Serialize>(path: &Path, v: &T) -> io::Result<()> {
    write_atomic(path, &serde_json::to_vec(v).expect("serializable"))
}

/// Replaces `path` with `bytes`: a new file beside it, synced, then renamed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("a file in the data directory");
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_files_round_trip_and_a_damaged_one_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("cache.json");
        assert_eq!(read_json::<Vec<u32>>(&path).unwrap(), None);
        write_json(&path, &serde_json::json!({ "version": 2, "n": [1, 2] })).unwrap();
        let v: serde_json::Value = read_json_version(&path, 2).unwrap().unwrap();
        assert_eq!(v["n"][1], 2);
        assert_eq!(read_json_version::<serde_json::Value>(&path, 3).unwrap(), None);
        // Only the file is left: no temp file beside it.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);

        fs::write(&path, b"{").unwrap();
        let e = read_json::<serde_json::Value>(&path).unwrap_err().to_string();
        assert!(
            e.starts_with(&format!("{} is damaged (", path.display())) && e.ends_with("delete it to start over"),
            "{e}"
        );
    }

    #[test]
    fn the_lock_makes_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let _lock = lock(&data, "lock").unwrap();
        assert!(data.join("lock").is_file());
    }
}
