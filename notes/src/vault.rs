//! Where the vault is and what state it is in: cloned (the engine's state
//! file is there) or handed to Omacloud (`leave`). The window, the command
//! line and `icloud-notes --sync` all come through [`Vault::locate`] before
//! anything opens the vault, which makes it the one place a vault an earlier
//! release left behind is moved.

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{CWD, RenameFlags, renameat_with};
use serde_json::Value;

use crate::files::{absolute_clean, canonical_or_clean, file_stamp};
use crate::lock::{Taken, VaultLock, lock_path_for};

/// How long the move of an old vault waits for whoever has it open.
const BUSY_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vault {
    /// As given (`--vault`, `ICLOUD_NOTES_VAULT`) or `<Documents>/Notes`.
    root: String,
    /// Why an old vault could not move to `<Documents>/Notes`, for the window to show.
    pub notice: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// The XDG Documents directory, read from `user-dirs.dirs` exactly as Qt
/// reads it (QStandardPaths), odd files included: the Qt app that came
/// before found the vault there, so this finds the same one. The last line that starts with
/// the name counts, quotes around the whole value go, a leading `$HOME` is
/// replaced as text and one final slash dropped. `~/Documents` without a
/// file or a line, and where that reading gives no absolute path.
fn documents() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"));
    let named = fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| {
        let value = text
            .lines()
            .filter_map(|line| line.strip_prefix("XDG_DOCUMENTS_DIR="))
            .next_back()?;
        let value = match value.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
            Some(unquoted) if !unquoted.is_empty() => unquoted,
            _ => value,
        };
        let (mut path, rest) = match value.strip_prefix("$HOME") {
            Some(rest) => (home().into_os_string(), rest),
            None => (OsString::new(), value),
        };
        let whole = path.len() + rest.len();
        path.push(rest.strip_suffix('/').filter(|_| whole > 1).unwrap_or(rest));
        Some(PathBuf::from(path)).filter(|p| p.is_absolute())
    });
    named.unwrap_or_else(|| home().join("Documents"))
}

/// Says where the vault is, or why it did not move, to whoever is there to
/// read it: a terminal, or the journal of a unit whose stderr goes there
/// (the timer's `--sync`). Never into a pipe: a command leaves stderr empty
/// for whoever reads its output.
fn notice(text: &str) {
    let stderr = io::stderr();
    let journal = || {
        let stream = std::env::var("JOURNAL_STREAM").ok()?;
        let (device, inode) = stream.split_once(':')?;
        let stat = rustix::fs::fstat(&stderr).ok()?;
        Some(device.parse() == Ok(stat.st_dev) && inode.parse() == Ok(stat.st_ino))
    };
    if stderr.is_terminal() || journal() == Some(true) {
        // Not eprintln!, which panics when the reader has gone.
        let _ = writeln!(stderr.lock(), "icloud-notes: {text}");
    }
}

fn is_vault(dir: &Path) -> bool {
    dir.join(".icloud-notes").is_dir() || dir.join(".icloud-md").is_dir() // layout 3
}

/// Earlier releases kept the vault in `<Documents>/icloud-notes`. It moves
/// by one rename within `<Documents>` (one filesystem: nothing is copied),
/// and from then on only `<Documents>/Notes` is read. Nothing in or about
/// the vault records where it is, so the rename is all there is to it. The
/// move holds the old vault's lock, so it never pulls the vault from under a
/// run that has it open: a Notes window of the earlier release still running
/// after the update, its engine, or a second process moving it at the same
/// moment (which, once it has the lock, finds the old folder gone).
///
/// "Notes" is a common folder name, so one already there is looked at:
/// empty, it is removed and the vault takes its place; a folder of the
/// user's own (not a vault) while the old folder is one, the vault stays
/// where it is, with a notice saying why; both vaults, Notes wins and the old
/// one is named for merging by hand. While the old vault stays busy past
/// `busy_wait`, or the rename fails, this process keeps to the old folder and
/// the next start tries again: the move is put off, never half done.
/// RENAME_NOREPLACE leaves a Notes folder that appeared meanwhile alone.
///
/// Returns the vault to use, and the notice for the window.
fn move_old_vault(documents: &Path, busy_wait: Duration) -> (PathBuf, Option<String>) {
    let old = documents.join("icloud-notes");
    let new = documents.join("Notes");
    // What is at Notes: nothing (or an empty directory, which the move
    // replaces), or a folder that stops it. For the latter, the vault to use.
    let blocked_by = || -> Option<(PathBuf, Option<String>)> {
        let metadata = fs::symlink_metadata(&new).ok()?;
        if metadata.is_dir() && fs::read_dir(&new).is_ok_and(|mut entries| entries.next().is_none()) {
            return None;
        }
        if !is_vault(&new) && is_vault(&old) {
            let notice = format!(
                "{} already exists and isn't a Notes vault; keeping {}. Move or rename that folder and restart to \
                 finish the move.",
                new.display(),
                old.display()
            );
            self::notice(&notice);
            return Some((old.clone(), Some(notice)));
        }
        notice(&format!(
            "both {old} and {new} exist; using {new}. {old} is the notes folder of an earlier release, left untouched: \
             merge it into {new} by hand, then remove it.",
            old = old.display(),
            new = new.display()
        ));
        Some((new.clone(), None))
    };
    if !old.is_dir() {
        return (new, None);
    }
    if let Some(vault) = blocked_by() {
        return vault;
    }

    let mut lock = VaultLock::new(lock_path_for(&old));
    let owner = format!("Notes moving its folder (pid {})", std::process::id());
    let waited = Instant::now();
    let mut taken = lock.try_lock(&owner);
    while taken == Taken::Busy && waited.elapsed() < busy_wait {
        std::thread::sleep(Duration::from_millis(50));
        taken = lock.try_lock(&owner);
    }
    let later = |why: String| {
        notice(&format!(
            "not moving {} to {} yet: {why}. Using {} until a later start moves it.",
            old.display(),
            new.display(),
            old.display()
        ));
    };
    if taken != Taken::Locked {
        let holder = lock.holder();
        later(if taken == Taken::Failed {
            format!("its lock {} cannot be opened", lock.path().display())
        } else if holder.is_empty() {
            "another run has it open".into()
        } else {
            format!("{holder} has it open")
        });
        return (old, None);
    }
    // Whoever held the lock may have moved it, or made Notes, meanwhile.
    if !old.is_dir() {
        return (new, None);
    }
    if let Some(vault) = blocked_by() {
        return vault;
    }
    let _ = fs::remove_dir(&new); // an empty Notes; one that filled meanwhile stays, and stops the rename
    let moved = renameat_with(CWD, &old, CWD, &new, RenameFlags::NOREPLACE)
        .map_err(io::Error::from)
        .or_else(|e| match e.kind() {
            // a filesystem without RENAME_NOREPLACE
            io::ErrorKind::InvalidInput => fs::rename(&old, &new),
            _ => Err(e),
        });
    if let Err(e) = moved {
        if matches!(
            e.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
        ) && let Some(vault) = blocked_by()
        {
            return vault;
        }
        notice(&format!(
            "cannot move {} to {} ({}). Using {} until a later start moves it.",
            old.display(),
            new.display(),
            crate::files::os_error_text(&e),
            old.display()
        ));
        return (old, None);
    }
    notice(&format!(
        "moved the notes folder from {} to {}, once, after the update: Notes keeps its notes in {} from now on.",
        old.display(),
        new.display(),
        new.display()
    ));
    (new, None)
}

impl Vault {
    /// The vault at `root`, whatever is in Documents.
    pub fn at(root: impl Into<String>) -> Vault {
        Vault {
            root: root.into(),
            notice: None,
        }
    }

    /// The vault of this run: `named` (`--vault DIR`, made absolute), else
    /// `ICLOUD_NOTES_VAULT` as it is, else `<Documents>/Notes` after the move
    /// of an older release's folder. Called once per process.
    pub fn locate(named: Option<&str>) -> Vault {
        if let Some(dir) = named {
            // A slash at its end stays, as the Qt app keeps one.
            let slash = if dir.ends_with('/') { "/" } else { "" };
            let clean = absolute_clean(Path::new(dir));
            return Vault::at(format!("{}{slash}", clean.to_string_lossy().trim_end_matches('/')));
        }
        if let Some(dir) = std::env::var_os("ICLOUD_NOTES_VAULT").filter(|dir| !dir.is_empty()) {
            return Vault::at(dir.to_string_lossy());
        }
        let (root, notice) = move_old_vault(&documents(), BUSY_WAIT);
        Vault {
            root: root.to_string_lossy().into_owned(),
            notice,
        }
    }

    /// The vault's directory as text, for messages and results.
    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn path(&self) -> &Path {
        Path::new(&self.root)
    }

    /// The vault's absolute path (the engine is given this one).
    pub fn absolute(&self) -> PathBuf {
        absolute_clean(self.path())
    }

    /// A folder of the vault ("" is the vault itself).
    pub fn folder(&self, folder: &str) -> PathBuf {
        if folder.is_empty() {
            self.path().to_path_buf()
        } else {
            self.path().join(folder)
        }
    }

    pub fn lock_path(&self) -> PathBuf {
        lock_path_for(self.path())
    }

    /// The engine's state directory: `.icloud-notes` (layout 4), or
    /// `.icloud-md` for a vault an older engine left, which the next sync
    /// moves (`.icloud-md` then keeps only a tombstone state.json that an
    /// older engine refuses as written by a newer version).
    pub fn state_dir(&self) -> PathBuf {
        // Named as the Qt app names them, so a path printed from here reads the same.
        let current = PathBuf::from(format!("{}/.icloud-notes", self.root));
        let legacy = PathBuf::from(format!("{}/.icloud-md", self.root));
        if !current.join("state.json").exists() && legacy.join("state.json").exists() {
            return legacy;
        }
        current
    }

    fn state_file(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }

    /// A clone leaves a state.json in the engine's state directory.
    pub fn cloned(&self) -> bool {
        self.state_file().exists()
    }

    /// What tells a changed state file from the one last read.
    pub fn state_stamp(&self) -> String {
        file_stamp(&self.state_file())
    }

    /// Where a note is copied before unreadable conflict markers are taken
    /// out of it: the engine's own state directory, which neither it nor
    /// this app reads as notes (the engine takes conflict-backups/ along
    /// when it moves the state).
    pub fn conflict_backup_dir(&self) -> PathBuf {
        self.state_dir().join("conflict-backups")
    }

    /// The vault left iCloud on this computer (`icloud-notes leave`):
    /// icloud-notes-sync's record names it, as the engine compares them.
    /// Omacloud syncs its files then, and nothing here syncs it with iCloud.
    pub fn left_icloud(&self) -> bool {
        let Some(record) = icloud_app::leave::left_file("icloud-notes") else {
            return false;
        };
        let folder = fs::read(record)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|record| Some(record.get("folder")?.as_str()?.to_string()))
            .unwrap_or_default();
        !folder.is_empty() && canonical_or_clean(Path::new(&folder)) == canonical_or_clean(self.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_WAIT: Duration = Duration::ZERO;

    fn vault(dir: &Path, note: &str) {
        fs::create_dir_all(dir.join(".icloud-notes")).unwrap();
        fs::write(dir.join(note), "# A note\n").unwrap();
    }

    #[test]
    fn the_old_vault_moves_once_and_only_when_nothing_is_in_the_way() {
        let documents = tempfile::tempdir().unwrap();
        let (old, new) = (documents.path().join("icloud-notes"), documents.path().join("Notes"));
        assert_eq!(move_old_vault(documents.path(), NO_WAIT), (new.clone(), None));
        assert!(!new.exists(), "looking makes no folder");

        vault(&old, "Old.md");
        fs::create_dir(&new).unwrap(); // empty: it gives way
        assert_eq!(move_old_vault(documents.path(), NO_WAIT), (new.clone(), None));
        assert!(!old.exists() && new.join("Old.md").is_file());

        // A folder of the owner's own stops the move, with a notice.
        fs::rename(&new, &old).unwrap();
        fs::create_dir(&new).unwrap();
        fs::write(new.join("shopping.txt"), "mine").unwrap();
        let (used, notice) = move_old_vault(documents.path(), NO_WAIT);
        assert_eq!(used, old);
        assert!(
            notice
                .unwrap()
                .ends_with("Move or rename that folder and restart to finish the move.")
        );

        // Both vaults: Notes wins, the old one is left.
        vault(&new, "New.md");
        assert_eq!(move_old_vault(documents.path(), NO_WAIT), (new.clone(), None));
        assert!(old.join("Old.md").is_file() && !new.join("Old.md").exists());
    }

    #[test]
    fn the_state_directory_is_the_one_holding_a_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::at(dir.path().to_string_lossy());
        assert!(!v.cloned());
        assert_eq!(v.state_stamp(), "-");
        fs::create_dir_all(dir.path().join(".icloud-md")).unwrap();
        fs::write(dir.path().join(".icloud-md/state.json"), "{}").unwrap();
        assert!(v.cloned());
        assert_eq!(v.state_dir(), dir.path().join(".icloud-md"));
        fs::create_dir_all(dir.path().join(".icloud-notes")).unwrap();
        fs::write(dir.path().join(".icloud-notes/state.json"), "{}").unwrap();
        assert_eq!(
            v.conflict_backup_dir(),
            dir.path().join(".icloud-notes/conflict-backups")
        );
        assert_ne!(v.state_stamp(), "-");
    }
}
