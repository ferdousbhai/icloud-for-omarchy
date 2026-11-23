//! Leaving iCloud for Omacloud, one app at a time (`icloud-<app> leave`).
//!
//! Notes, Reminders and iCloud Drive each keep a folder of plain files that
//! iCloud is the truth for. Leaving hands that folder to Omacloud, which then
//! carries it between computers, so the data is kept once, not twice:
//!
//! 1. Omacloud must be installed, set up, and syncing the folder's folder of
//!    home ([`omacloud_ready`]); otherwise leaving refuses with the command
//!    that fixes it, and nothing changes.
//! 2. The app syncs with iCloud a last time, so nothing made here is lost.
//! 3. It records that it left ([`record_left`]): from then on nothing of the
//!    app talks to iCloud on this computer ([`left`], [`left_error`]).
//!    iCloud keeps its copy as it was, untouched.
//! 4. It takes the [`MARKER`] out of the folder ([`release`]) and asks
//!    Omacloud to sync ([`omacloud_sync`]): Omacloud takes the folder in,
//!    merging with what another computer that left already sent, deleting
//!    nothing.
//!
//! While iCloud keeps a folder, its app puts the [`MARKER`] in it
//! ([`hold`]): Omacloud leaves a folder holding one on that computer, so
//! the same files are never synced by both.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use icloud_session::cli::EXIT_ERROR;
use icloud_session::output::CliError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store;

/// The file that tells Omacloud another sync keeps this folder
/// (omacloud-core's `HELD_MARKER`; empty).
pub const MARKER: &str = ".nosync";

/// How to install Omacloud, as its site gives it.
pub const OMACLOUD_INSTALL: &str = "curl -fsSL https://omacloud.computer/install.sh | sudo bash";

/// The `omacloud` binary: `ICLOUD_OMACLOUD_BIN` for tests, else from PATH.
fn omacloud_bin() -> PathBuf {
    std::env::var_os("ICLOUD_OMACLOUD_BIN").map_or_else(|| PathBuf::from("omacloud"), PathBuf::from)
}

/// The record that an app left iCloud on this computer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Left {
    pub version: u32,
    /// When, in seconds since the epoch.
    pub at: u64,
    /// The folder handed to Omacloud.
    pub folder: PathBuf,
}

const FILE: &str = "left-icloud.json";

/// `$XDG_DATA_HOME/<app>/left-icloud.json`.
pub fn left_file(app: &str) -> Option<PathBuf> {
    store::data_dir(app).map(|d| d.join(FILE))
}

/// Whether `app` (`icloud-notes`, `icloud-reminders`, `icloud-drive`) left
/// iCloud on this computer.
pub fn left(app: &str) -> io::Result<Option<Left>> {
    match left_file(app) {
        Some(f) => store::read_json(&f),
        None => Ok(None),
    }
}

/// Records that `app` left iCloud, handing over `folder`.
pub fn record_left(app: &str, folder: &Path) -> io::Result<Left> {
    let file = left_file(app).ok_or_else(|| io::Error::other("no $XDG_DATA_HOME or $HOME"))?;
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let left = Left {
        version: 1,
        at,
        folder: folder.to_path_buf(),
    };
    store::write_json(&file, &left)?;
    Ok(left)
}

/// What every command that would reach iCloud answers once the app left:
/// `left_icloud` (not set up for iCloud here).
pub fn left_error(what: &str, left: &Left) -> CliError {
    CliError::new(
        "left_icloud",
        EXIT_ERROR,
        format!(
            "{what} left iCloud on this computer; Omacloud syncs {} now",
            left.folder.display()
        ),
    )
    .hint(format!("change the files in {}", left.folder.display()))
}

/// Puts the [`MARKER`] in `folder`, if the folder is there and lacks one.
pub fn hold(folder: &Path) -> io::Result<()> {
    if !folder.is_dir() {
        return Ok(());
    }
    let marker = folder.join(MARKER);
    if marker.exists() {
        return Ok(());
    }
    fs::write(marker, b"")
}

/// Takes the [`MARKER`] out of `folder`.
pub fn release(folder: &Path) -> io::Result<()> {
    match fs::remove_file(folder.join(MARKER)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Leaving is optional and for good, so it is never one keystroke away: at a
/// terminal the person types `leave`; anywhere else it needs `--yes`, which
/// an agent passes only when the person asked to leave iCloud for `what`.
pub fn confirm(mode: icloud_session::output::Mode, yes: bool, what: &str, question: &str) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    if !icloud_session::output::interactive(mode) {
        return Err(CliError::new(
            "usage",
            EXIT_ERROR,
            format!("leaving iCloud for {what} is for good on this computer, and nobody can be asked here"),
        )
        .hint("pass --yes only when the person asked to leave iCloud for this"));
    }
    eprintln!("{}", icloud_session::output::terminal_safe(question));
    eprint!("Type leave to hand {what} to Omacloud, anything else to stop: ");
    let mut line = String::new();
    _ = std::io::stdin().read_line(&mut line);
    if line.trim() == "leave" {
        Ok(())
    } else {
        Err(CliError::new("cancelled", EXIT_ERROR, "cancelled: nothing changed"))
    }
}

/// Runs `omacloud <args> --json`, giving the envelope's `data`. A failure's
/// error object is looked for on stderr, its last line (where Omarchy's
/// command lines put it), and then on stdout (where an older omacloud put
/// it); its `code` names the failure, as `omacloud_<code>`, whatever the
/// exit code said.
fn omacloud(args: &[&str]) -> Result<Value, CliError> {
    let out = Command::new(omacloud_bin()).args(args).arg("--json").output();
    let out = match out {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(CliError::new(
                "omacloud_missing",
                EXIT_ERROR,
                "leaving iCloud hands the folder to Omacloud, which isn't installed",
            )
            .hint(format!(
                "install it with `{OMACLOUD_INSTALL}`, then run `omacloud init`"
            )));
        }
        Err(e) => {
            return Err(CliError::new(
                "omacloud",
                EXIT_ERROR,
                format!("omacloud did not run: {e}"),
            ));
        }
    };
    let v: Value = icloud_session::output::error_line(&out.stderr)
        .filter(|_| !out.status.success())
        .map_or_else(|| serde_json::from_slice(&out.stdout), Ok)
        .map_err(|_| {
            CliError::new(
                "omacloud",
                EXIT_ERROR,
                format!(
                    "omacloud {} gave no answer: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            )
        })?;
    if v["ok"].as_bool() == Some(true) {
        return Ok(v["data"].clone());
    }
    let code = v["error"]["code"].as_str().unwrap_or("error");
    let message = v["error"]["message"].as_str().unwrap_or("omacloud failed");
    let mut e = CliError::new(&format!("omacloud_{code}"), EXIT_ERROR, format!("omacloud: {message}"));
    if let Some(h) = v["error"]["hint"].as_str() {
        e = e.hint(h);
    }
    Err(e)
}

/// That Omacloud is installed, set up, and syncs `folder` once its
/// [`MARKER`] goes: `folder` lies in a folder of home Omacloud syncs here
/// (or, syncing one whole folder, in that folder). Each failure names the
/// command that fixes it.
pub fn omacloud_ready(folder: &Path) -> Result<(), CliError> {
    let status = match omacloud(&["status"]) {
        // not set up: status answers, or fails with its own code
        Err(e) if e.code == "omacloud_not_set_up" || e.code == "omacloud_not_found" => return Err(not_set_up()),
        other => other?,
    };
    if status["set_up"].as_bool() != Some(true) {
        return Err(not_set_up());
    }
    let Some(home) = status["folder"].as_str().map(PathBuf::from) else {
        return Err(not_set_up());
    };
    let Ok(rel) = folder.strip_prefix(&home) else {
        return Err(CliError::new(
            "omacloud_folder_off",
            EXIT_ERROR,
            format!("Omacloud syncs {}, not {}", home.display(), folder.display()),
        ));
    };
    let synced: Option<Vec<PathBuf>> = status["folders"]["synced"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["path"].as_str().map(PathBuf::from)).collect());
    match synced {
        // one whole folder: everything in it syncs
        None => Ok(()),
        Some(synced) if synced.iter().any(|s| folder.starts_with(s)) => Ok(()),
        Some(_) => {
            let top = rel
                .components()
                .next()
                .map_or_else(String::new, |c| c.as_os_str().to_string_lossy().into_owned());
            Err(CliError::new(
                "omacloud_folder_off",
                EXIT_ERROR,
                format!("Omacloud doesn't sync ~/{top} on this computer"),
            )
            .hint(format!(
                "turn it on with `omacloud folders add ~/{top}`, then leave again"
            )))
        }
    }
}

fn not_set_up() -> CliError {
    CliError::new(
        "omacloud_not_set_up",
        EXIT_ERROR,
        "leaving iCloud hands the folder to Omacloud, which isn't set up on this computer",
    )
    .hint("run `omacloud init` (or open the Omacloud app), then leave again")
}

/// Asks Omacloud to sync now, so it takes in the folder just released.
pub fn omacloud_sync() -> Result<(), CliError> {
    omacloud(&["sync"]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in `omacloud` that prints `status` for `status` and `ok` for
    /// anything else.
    fn fake(dir: &Path, status: &str) -> PathBuf {
        let bin = dir.join("omacloud");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\nif [ \"$1\" = status ]; then cat <<'EOF'\n{status}\nEOF\nelse echo '{{\"ok\":true,\"data\":null}}'; fi\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    // the environment is the process's: one test sets it at a time
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn ready_with(status: &str, folder: &str) -> Result<(), CliError> {
        let _g = ENV.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests holding ENV are the only ones touching it
        unsafe { std::env::set_var("ICLOUD_OMACLOUD_BIN", fake(dir.path(), status)) };
        let r = omacloud_ready(Path::new(folder));
        unsafe { std::env::remove_var("ICLOUD_OMACLOUD_BIN") };
        r
    }

    #[test]
    fn ready_when_the_folder_is_in_a_synced_one() {
        let st = r#"{"ok":true,"data":{"set_up":true,"folder":"/h","folders":{"synced":[{"name":"Documents","path":"/h/Documents"}]}}}"#;
        assert!(ready_with(st, "/h/Documents/Notes").is_ok());
        let e = ready_with(st, "/h/iCloud Drive").unwrap_err();
        assert_eq!(e.code, "omacloud_folder_off");
        assert!(e.hint.unwrap().contains("omacloud folders add ~/iCloud Drive"));
    }

    #[test]
    fn ready_syncing_one_whole_folder() {
        let st = r#"{"ok":true,"data":{"set_up":true,"folder":"/h","folders":null}}"#;
        assert!(ready_with(st, "/h/Documents/Notes").is_ok());
        assert_eq!(
            ready_with(st, "/elsewhere/Notes").unwrap_err().code,
            "omacloud_folder_off"
        );
    }

    #[test]
    fn not_set_up_and_missing() {
        let st = r#"{"ok":false,"error":{"code":"not_set_up","message":"not set up"}}"#;
        assert_eq!(
            ready_with(st, "/h/Documents/Notes").unwrap_err().code,
            "omacloud_not_set_up"
        );
        let st = r#"{"ok":true,"data":{"set_up":false}}"#;
        assert_eq!(
            ready_with(st, "/h/Documents/Notes").unwrap_err().code,
            "omacloud_not_set_up"
        );
        let _g = ENV.lock().unwrap();
        unsafe { std::env::set_var("ICLOUD_OMACLOUD_BIN", "/nonexistent/omacloud") };
        let e = omacloud_ready(Path::new("/h/Documents/Notes")).unwrap_err();
        unsafe { std::env::remove_var("ICLOUD_OMACLOUD_BIN") };
        assert_eq!(e.code, "omacloud_missing");
        assert!(e.hint.unwrap().contains(OMACLOUD_INSTALL));
    }

    #[test]
    fn marker_held_and_released() {
        let dir = tempfile::tempdir().unwrap();
        hold(dir.path()).unwrap();
        assert!(dir.path().join(MARKER).is_file());
        hold(dir.path()).unwrap();
        release(dir.path()).unwrap();
        assert!(!dir.path().join(MARKER).exists());
        release(dir.path()).unwrap();
        // a folder not there yet gets none
        hold(&dir.path().join("absent")).unwrap();
        assert!(!dir.path().join("absent").exists());
    }
}
