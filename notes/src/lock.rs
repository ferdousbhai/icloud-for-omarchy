//! The vault's lock: an exclusive flock on a lock file, which
//! icloud-notes-sync takes too (its cmd/lock.rs). The window holds it for as
//! long as it is open, `icloud-notes --sync` and command line changes for
//! their run. The file is never removed (removing a flock file races with
//! the next locker); it holds only the holder's own description, for whoever
//! waits, and lives in the runtime directory.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::files::{absolute_clean, bytes_of};

/// Where the lock of the vault at `vault` is: in the runtime directory, or
/// beside the vault without one, never inside it (a clone wants a fresh or
/// empty directory). One per vault, named by a hash of its canonical path
/// (its cleaned absolute path until it exists). icloud-notes-sync computes
/// the same path: change both or neither.
pub fn lock_path_for(vault: &Path) -> PathBuf {
    let key = std::fs::canonicalize(vault).unwrap_or_else(|_| absolute_clean(vault));
    // 64-bit FNV-1a
    let hash = bytes_of(&key).iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let name = format!("icloud-notes-{hash:016x}.lock");
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join(name),
        None => key.parent().unwrap_or(&key).join(format!(".{name}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    Locked,
    /// Someone else holds it.
    Busy,
    /// The file cannot be opened at all.
    Failed,
}

/// The lock of one vault, held from a [`VaultLock::try_lock`] that answered
/// [`Taken::Locked`] until [`VaultLock::release`] or the drop.
#[derive(Debug)]
pub struct VaultLock {
    path: PathBuf,
    file: Option<File>,
}

impl VaultLock {
    pub fn new(path: PathBuf) -> Self {
        Self { path, file: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Never blocks. The holder describes itself as `owner`
    /// ("Notes (pid 12)"), which [`VaultLock::holder`] reads back to whoever
    /// waits. The descriptor is close-on-exec: only the engine is handed it,
    /// on purpose (`engine::start`).
    pub fn try_lock(&mut self, owner: &str) -> Taken {
        if self.file.is_some() {
            return Taken::Locked;
        }
        let opened = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.path);
        let Ok(mut file) = opened else {
            return Taken::Failed;
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Taken::Busy,
            Err(std::fs::TryLockError::Error(_)) => return Taken::Failed,
        }
        if file.set_len(0).is_ok() {
            let _ = file.write_all(owner.as_bytes());
        }
        self.file = Some(file);
        Taken::Locked
    }

    pub fn release(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.set_len(0);
        }
    }

    pub fn is_held(&self) -> bool {
        self.file.is_some()
    }

    /// The locked descriptor, for the engine to inherit.
    pub fn fd(&self) -> Option<RawFd> {
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Who holds the lock, as it described itself; empty when nobody said.
    pub fn holder(&self) -> String {
        let read = || -> io::Result<String> {
            let mut text = Vec::new();
            File::open(&self.path)?.take(256).read_to_end(&mut text)?;
            Ok(String::from_utf8_lossy(&text).trim().to_string())
        };
        read().unwrap_or_default()
    }
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_and_it_says_who() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.lock");
        let mut first = VaultLock::new(path.clone());
        let mut second = VaultLock::new(path.clone());
        assert_eq!(first.holder(), "");
        assert_eq!(first.try_lock("Notes (pid 1)"), Taken::Locked);
        assert_eq!(first.try_lock("again"), Taken::Locked);
        assert_eq!(second.try_lock("someone else"), Taken::Busy);
        assert_eq!(second.holder(), "Notes (pid 1)");
        assert!(first.fd().is_some() && second.fd().is_none());
        first.release();
        assert_eq!(std::fs::read(&path).unwrap(), b"", "the description goes with the lock");
        assert_eq!(second.try_lock("someone else"), Taken::Locked);
        assert_eq!(first.holder(), "someone else");
        let missing = dir.path().join("no/such/dir/v.lock");
        assert_eq!(VaultLock::new(missing).try_lock("x"), Taken::Failed);
    }
}
