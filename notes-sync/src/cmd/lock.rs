//! The vault lock the Notes app (icloud-notes) shares with this tool, so a
//! pull, push, clone or restore never runs while the app or another run is
//! changing the same vault. Not in icloud-md (docs/PORT_PLAN.md §1).
//!
//! An exclusive `flock` on a file outside the vault, never removed (removing
//! a flock file races the next locker). The holder writes a description of
//! itself into it ("Notes (pid 12)", "icloud-notes-sync (pid 34)") for
//! whoever finds it busy. The path must be exactly what icloud-notes'
//! `NotesBackend::lockPath()` computes (notes/src/notesbackend.cpp):
//!
//! - key: the vault's canonical path, or its absolute, cleaned path while
//!   it doesn't exist yet;
//! - `$XDG_RUNTIME_DIR/icloud-notes-<fnv>.lock`, or without a runtime
//!   directory `<key's parent>/.icloud-notes-<fnv>.lock`, where `<fnv>` is
//!   the 64-bit FNV-1a hash of the key's bytes in 16 lowercase hex digits.
//!
//! A caller that already holds the lock (the app runs this tool with its
//! lock held for its whole lifetime) passes the locked descriptor down as
//! `ICLOUD_NOTES_LOCK_FD=<fd>`. It is used only after checking it is open
//! on this vault's lock file and holds the lock, so a stale or wrong value
//! just falls back to locking as usual.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use super::errors::Error;

/// The environment variable naming an inherited, already locked descriptor.
pub const LOCK_FD_ENV: &str = "ICLOUD_NOTES_LOCK_FD";

/// How long a run waits for another run or a background sync by default.
/// The Notes window holds the lock for as long as it is open, so it is
/// never waited for unless `--wait` asks.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// How the Notes window describes itself in the lock file.
const APP_HOLDER_PREFIX: &str = "Notes (pid";

/// The 64-bit FNV-1a hash.
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `path` made absolute with `.` and `..` resolved lexically (Qt's
/// `QDir::cleanPath(QFileInfo::absoluteFilePath())`).
fn absolute_clean(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The vault's lock file (see the module doc).
pub fn lock_path(vault: &Path) -> PathBuf {
    let key = std::fs::canonicalize(vault).unwrap_or_else(|_| absolute_clean(vault));
    let hash = format!("{:016x}", fnv1a64(key.as_os_str().as_encoded_bytes()));
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join(format!("icloud-notes-{hash}.lock")),
        None => key.parent().unwrap_or(&key).join(format!(".icloud-notes-{hash}.lock")),
    }
}

/// Holds the vault's lock until dropped.
#[derive(Debug)]
pub struct VaultLock {
    file: File,
    /// Taken here (not inherited): the description is ours to clear.
    own: bool,
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        if self.own {
            let _ = self.file.set_len(0);
        }
        // Closing the descriptor releases an own lock; an inherited one
        // stays with the caller's copy.
    }
}

/// The descriptor `ICLOUD_NOTES_LOCK_FD` names, if it is open on `path` and
/// holds its lock.
fn inherited_lock(path: &Path) -> Option<File> {
    let fd: RawFd = std::env::var(LOCK_FD_ENV).ok()?.trim().parse().ok()?;
    if fd < 3 {
        return None;
    }
    // /proc/self/fd/N follows to the open file without touching the
    // descriptor, so an unrelated or closed number is never adopted.
    let open = std::fs::metadata(format!("/proc/self/fd/{fd}")).ok()?;
    let lock = std::fs::metadata(path).ok()?;
    if (open.dev(), open.ino()) != (lock.dev(), lock.ino()) {
        return None;
    }
    // SAFETY: the descriptor is open (checked above), was handed to this
    // process for this purpose, and nothing else in it uses it.
    let file = unsafe { File::from_raw_fd(fd) };
    // flock on the caller's own open file description succeeds; anyone
    // else's lock makes this fail.
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(_) => {
            std::mem::forget(file); // not ours: leave the descriptor alone
            None
        }
    }
}

/// Who holds the lock at `path`, as it described itself.
fn holder(path: &Path) -> String {
    let mut text = String::new();
    if let Ok(file) = File::open(path) {
        let _ = file.take(256).read_to_string(&mut text);
    }
    let text = text.trim();
    if text.is_empty() {
        "another sync".into()
    } else {
        text.into()
    }
}

/// Takes the lock of the vault at `vault`, waiting up to `wait` for another
/// holder (`None`: [`DEFAULT_WAIT`], or not at all while the Notes window
/// holds it). A busy lock is [`Error::VaultBusy`].
pub fn lock_vault(vault: &Path, wait: Option<Duration>) -> Result<VaultLock, Error> {
    let path = lock_path(vault);
    if let Some(file) = inherited_lock(&path) {
        return Ok(VaultLock { file, own: false });
    }
    let lock_error = |e: std::io::Error| Error::VaultLock {
        path: path.display().to_string(),
        reason: e.to_string(),
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(lock_error)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::Error(e)) => return Err(lock_error(e)),
            Err(std::fs::TryLockError::WouldBlock) => {}
        }
        let holder = holder(&path);
        let app = holder.starts_with(APP_HOLDER_PREFIX);
        let limit = wait.unwrap_or(if app { Duration::ZERO } else { DEFAULT_WAIT });
        if started.elapsed() >= limit {
            return Err(Error::VaultBusy { holder, app });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let owner = format!("icloud-notes-sync (pid {})", std::process::id());
    if file.set_len(0).is_ok() {
        let _ = file.write_all_at(owner.as_bytes(), 0);
    }
    Ok(VaultLock { file, own: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_the_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn absolute_clean_resolves_dots_lexically() {
        assert_eq!(absolute_clean(Path::new("/a/./b/../c/")), PathBuf::from("/a/c"));
        assert_eq!(absolute_clean(Path::new("/..")), PathBuf::from("/"));
    }
}
