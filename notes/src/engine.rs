//! The sync engine, icloud-notes-sync, as a separate process: where it is,
//! what it says about the vault (`vault-info`), and a run of it with the
//! vault's lock handed down.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{Access, access};
use rustix::io::{FdFlags, fcntl_setfd};
use serde_json::{Map, Value};

use crate::files::{absolute_clean, os_error_text};

/// The engine's name, as the log shows a run of it.
pub const TOOL: &str = "icloud-notes-sync";
/// Where the icloud-notes package installs it, off PATH.
const PACKAGED: &str = "/usr/lib/icloud-notes/icloud-notes-sync";
/// How long `vault-info` may take.
const VAULT_INFO_LIMIT: Duration = Duration::from_secs(10);

fn executable(path: &Path) -> Option<PathBuf> {
    (path.is_file() && access(path, Access::EXEC_OK).is_ok()).then(|| absolute_clean(path))
}

/// The engine to run: `$ICLOUD_NOTES_SYNC_BIN` when set (tests, development;
/// nothing else is tried then), else the packaged one, else icloud-notes-sync
/// on PATH (a development build). `None` when none is executable.
pub fn sync_tool_path() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("ICLOUD_NOTES_SYNC_BIN").filter(|v| !v.is_empty()) {
        return executable(Path::new(&named));
    }
    executable(Path::new(PACKAGED)).or_else(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).find_map(|dir| executable(&dir.join(TOOL)))
    })
}

/// What the engine knows of the vault (the app never reads its state file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultInfo {
    /// "in-body" (a note's title is its first line) or "filename".
    pub title_mode: String,
    /// The directory of the account's default folder ("Notes").
    pub default_folder_dir: String,
    /// The vault-relative files the engine tracks.
    pub tracked: HashSet<String>,
    /// Why a tracked file is read-only here, by file.
    pub read_only: HashMap<String, String>,
    /// A note's body as last synced, by apple-note-id.
    pub base_files: HashMap<String, PathBuf>,
}

impl Default for VaultInfo {
    /// No engine, or no answer yet: nothing is known.
    fn default() -> Self {
        VaultInfo::from_answer(&Map::new())
    }
}

impl VaultInfo {
    /// From `vault-info`'s answer `{titleMode, defaultFolderDir, vault,
    /// notes: [{id, file, readOnlyReason?, baseFile}]}`.
    pub fn from_answer(answer: &Map<String, Value>) -> VaultInfo {
        let text = |object: &Map<String, Value>, key: &str| {
            object.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
        };
        let mut info = VaultInfo {
            title_mode: if text(answer, "titleMode") == "filename" {
                "filename"
            } else {
                "in-body"
            }
            .into(),
            default_folder_dir: text(answer, "defaultFolderDir"),
            tracked: HashSet::new(),
            read_only: HashMap::new(),
            base_files: HashMap::new(),
        };
        let vault = text(answer, "vault");
        let notes = answer.get("notes").and_then(Value::as_array).into_iter().flatten();
        for note in notes.filter_map(Value::as_object) {
            let (file, reason, base) = (text(note, "file"), text(note, "readOnlyReason"), text(note, "baseFile"));
            if file.is_empty() {
                continue;
            }
            if !reason.is_empty() {
                info.read_only.insert(file.clone(), reason);
            }
            if !base.is_empty() {
                info.base_files
                    .insert(text(note, "id"), PathBuf::from(format!("{vault}/{base}")));
            }
            info.tracked.insert(file);
        }
        info
    }
}

/// A process's exit code, or the signal that ended it.
fn exit_code(status: ExitStatus) -> i32 {
    status.code().or(status.signal()).unwrap_or(-1)
}

/// Reads a pipe to its end on a thread of its own, so a child that fills
/// one never waits on the other.
fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// `<engine> --quiet vault-info <vault>`: its answer, or `None` when it
/// gave none in time (it is stopped then).
pub fn vault_info(tool: &Path, vault: &Path) -> Option<VaultInfo> {
    let mut child = Command::new(tool)
        .args(["--quiet", "vault-info"])
        .arg(vault)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = drain(child.stdout.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < VAULT_INFO_LIMIT => thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let output = output.join().ok()?;
    if status.code() != Some(0) {
        return None;
    }
    let answer: Value = serde_json::from_slice(&output).unwrap_or(Value::Null);
    Some(VaultInfo::from_answer(answer.as_object().unwrap_or(&Map::new())))
}

/// A started run of the engine.
#[derive(Debug)]
pub struct Running {
    child: Child,
    stdout: thread::JoinHandle<Vec<u8>>,
    stderr: thread::JoinHandle<Vec<u8>>,
}

/// What a run left: its exit code, its whole stdout (JSON, in the machine
/// modes every run uses) and its stderr (progress and warnings).
#[derive(Debug, Default)]
pub struct Finished {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Starts `<tool> <args>` in `vault` with the lock held on `lock_fd`.
///
/// icloud-notes-sync takes the vault's lock itself (clone, pull, push,
/// restore), which the caller already holds: it gets the locked descriptor
/// as `ICLOUD_NOTES_LOCK_FD`, which it checks is open on this vault's lock
/// file and holds it before trusting it. The descriptor is close-on-exec
/// everywhere else; only this child keeps it.
pub fn start(tool: &Path, args: &[String], vault: &Path, lock_fd: RawFd) -> io::Result<Running> {
    let mut command = Command::new(tool);
    command
        .args(args)
        .current_dir(vault)
        .env("ICLOUD_NOTES_LOCK_FD", lock_fd.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: runs in the child between fork and exec, and makes one system
    // call (fcntl) on a descriptor the parent keeps open for as long as it
    // holds the lock; nothing is allocated or locked.
    unsafe {
        command.pre_exec(move || {
            fcntl_setfd(BorrowedFd::borrow_raw(lock_fd), FdFlags::empty())?;
            Ok(())
        });
    }
    // Said as the Qt app's process runner says it, which the log shows.
    let child_setup = |what: &str, e: io::Error| {
        io::Error::new(
            e.kind(),
            format!("Child process set up failed: {what}: {}", os_error_text(&e)),
        )
    };
    std::fs::read_dir(vault).map_err(|e| child_setup("chdir", e))?;
    let mut child = command.spawn()?;
    Ok(Running {
        stdout: drain(child.stdout.take()),
        stderr: drain(child.stderr.take()),
        child,
    })
}

impl Running {
    /// Waits for the run to end.
    pub fn wait(mut self) -> Finished {
        let exit_code = self.child.wait().map_or(-1, exit_code);
        Finished {
            exit_code,
            stdout: self.stdout.join().unwrap_or_default(),
            stderr: self.stderr.join().unwrap_or_default(),
        }
    }

    /// Stops the run (the window closing under it).
    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn vault_info_is_read_from_the_engines_answer() {
        let answer = json!({
            "titleMode": "filename",
            "defaultFolderDir": "Notes",
            "vault": "/v",
            "notes": [
                {"id": "a", "file": "Notes/A.md", "baseFile": ".icloud-notes/base/a.md"},
                {"id": "b", "file": "B.md", "readOnlyReason": "is too large", "baseFile": ""},
                {"id": "c", "file": ""},
                "junk",
            ],
        });
        let info = VaultInfo::from_answer(answer.as_object().unwrap());
        assert_eq!(
            (info.title_mode.as_str(), info.default_folder_dir.as_str()),
            ("filename", "Notes")
        );
        assert_eq!(
            info.tracked,
            HashSet::from(["Notes/A.md".to_string(), "B.md".to_string()])
        );
        assert_eq!(
            info.read_only,
            HashMap::from([("B.md".to_string(), "is too large".to_string())])
        );
        assert_eq!(
            info.base_files,
            HashMap::from([("a".to_string(), PathBuf::from("/v/.icloud-notes/base/a.md"))])
        );
        let nothing = VaultInfo::default();
        assert_eq!(nothing.title_mode, "in-body");
        assert!(nothing.tracked.is_empty());
    }
}
