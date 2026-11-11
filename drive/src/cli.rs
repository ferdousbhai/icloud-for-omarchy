//! The command line. iCloud Drive is a folder, so there is nothing here for
//! reading or writing files: only `status`, a sync on demand, and turning
//! the background service on and off. Output follows the shared product
//! contract ([`icloud_session::output`]): text on a terminal, the JSON
//! envelope when piped. Exit codes are the table every iCloud tool shares
//! (docs/CLI.md).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use icloud_session::cli::{EXIT_ERROR, EXIT_NOT_FOUND, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{self, CliError, Mode, OutputFlags, Response};
use icloud_session::time::rfc3339;
use serde_json::json;

use crate::engine::{self, Engine, Outcome};
use crate::remote::{self, DriveApi, ROOT_ID};
use crate::state::State;

const SERVICE: &str = "icloud-drive.service";
/// How long `sync` waits for the background service's pass to end.
const SYNC_WAIT: Duration = Duration::from_secs(120);

const AFTER_HELP: &str = "\
iCloud Drive is the folder ~/iCloud Drive: read and write files there with
anything. This command only reports on the sync and turns it on and off.

Conflicts keep both files: yours is renamed `name (conflicted copy <host>
<date>).ext` and iCloud's version takes the name. A file removed in iCloud
goes to the trash here; one removed here goes to Recently Deleted in iCloud.
App folders (Pages, Shortcuts), packages and hidden names are not synced.

Output: text on a terminal, a JSON envelope {\"ok\":true,\"data\",\"summary\"}
when piped. --json forces the envelope, --styled text, --quiet prints only
the data, --ids-only one line per change a sync made, --count their number. A
failure is {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (io, error, busy, http 4xx), or a sync that left some items for later
  2 usage; too_many_deletions (run `icloud-drive sync --yes` after checking
    `icloud-drive sync --dry-run`); other_account
  3 sign_in_required, session_rejected, drive_unavailable; status while signed out
  4 folder_missing
  5 offline, network, session_service, keyring_unavailable, bad_response, http 5xx";

#[derive(Parser)]
#[command(
    name = "icloud-drive",
    version,
    about = "Keeps ~/iCloud Drive in sync with iCloud Drive",
    after_help = AFTER_HELP
)]
struct Args {
    #[command(flatten)]
    out: OutputFlags,
    /// The local folder (default: ~/iCloud Drive).
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Where the sync state is kept (default: ~/.local/state/icloud-drive).
    #[arg(long, global = true, value_name = "DIR", hide = true)]
    state_dir: Option<PathBuf>,
    /// Sync one folder of iCloud Drive (its drivewsid) instead of all of it.
    #[arg(long, global = true, value_name = "ID", hide = true)]
    top: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Whether the sync is on, when it last ran, and what it left out.
    Status,
    /// Sync once, now.
    Sync {
        /// Say what would be done, and do nothing.
        #[arg(long)]
        dry_run: bool,
        /// Go ahead with a sync that removes many items at once.
        #[arg(long = "yes", alias = "allow-deletes")]
        allow_deletes: bool,
    },
    /// Turn the background sync on, now and at every login.
    Start,
    /// Turn the background sync off.
    Stop,
    /// Sync again and again (what the background service runs).
    #[command(hide = true)]
    Run {
        /// Seconds between syncs.
        #[arg(long, default_value_t = 30)]
        interval: u64,
    },
}

struct Paths {
    folder: PathBuf,
    state_dir: PathBuf,
    top: String,
}

impl Paths {
    fn of(args: &Args) -> Paths {
        // The service's unit can set these without changing its command.
        let env = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
        Paths {
            folder: args
                .dir
                .clone()
                .or_else(|| env("ICLOUD_DRIVE_DIR").map(PathBuf::from))
                .unwrap_or_else(engine::default_folder),
            state_dir: args
                .state_dir
                .clone()
                .or_else(|| env("ICLOUD_DRIVE_STATE_DIR").map(PathBuf::from))
                .unwrap_or_else(engine::default_state_dir),
            top: args
                .top
                .clone()
                .or_else(|| env("ICLOUD_DRIVE_TOP").map(|v| v.to_string_lossy().into_owned()))
                .unwrap_or_else(|| ROOT_ID.to_string()),
        }
    }
}

pub fn run() -> u8 {
    let args = match output::parse::<Args>() {
        Ok(args) => args,
        Err(code) => return code,
    };
    let mode = args.out.mode();
    let paths = Paths::of(&args);
    let result = match args.command {
        Cmd::Status => status(&paths),
        Cmd::Sync { dry_run, allow_deletes } => sync_once(&paths, dry_run, allow_deletes, mode),
        Cmd::Start => service(&["enable", "--now", SERVICE], "on", &paths),
        Cmd::Stop => service(&["disable", "--now", SERVICE], "off", &paths).inspect(|_| record(&paths, None)),
        Cmd::Run { interval } => run_forever(&paths, Duration::from_secs(interval.max(5))),
    };
    match result {
        Ok(response) => response.print(mode),
        Err(e) => e.report(mode),
    }
}

fn failure(e: &engine::Error) -> CliError {
    use engine::Error as E;
    match e {
        E::Remote(remote::Error::Session(e)) => output::session_failure(e),
        E::Remote(remote::Error::NotOffered(_)) => CliError::new("drive_unavailable", EXIT_SIGN_IN, e.to_string())
            .hint("turn on iCloud Drive for this Apple ID"),
        E::Remote(remote::Error::Listing { .. }) => CliError::new("bad_response", EXIT_UNAVAILABLE, e.to_string())
            .hint("iCloud gave an incomplete answer; nothing was changed. Try again later"),
        E::FolderMissing(folder) => CliError::new("folder_missing", EXIT_NOT_FOUND, e.to_string()).hint(format!(
            "put {} back, or remove the state folder to start over",
            folder.display()
        )),
        E::TooManyDeletions { .. } => CliError::new("too_many_deletions", EXIT_USAGE, e.to_string())
            .hint("see them with `icloud-drive sync --dry-run`, then run `icloud-drive sync --yes`"),
        E::OtherAccount { .. } => CliError::new("other_account", EXIT_USAGE, e.to_string())
            .hint("sign in to that account again, or use another folder with --dir"),
        E::Io(_) => CliError::new("io", EXIT_ERROR, e.to_string()),
        _ => CliError::new("error", EXIT_ERROR, e.to_string()),
    }
}

/// One sync at a time: the background service and `sync` share the state.
/// The service holds the lock only while it syncs, so `sync` waits up to
/// `wait` for the service's pass to end.
fn lock(paths: &Paths, wait: Duration) -> Result<fs::File, CliError> {
    fs::create_dir_all(&paths.state_dir).map_err(|e| failure(&engine::Error::Io(e)))?;
    let file = fs::File::create(paths.state_dir.join("lock")).map_err(|e| failure(&engine::Error::Io(e)))?;
    let until = Instant::now().checked_add(wait);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if until.is_none_or(|u| Instant::now() < u) => {
                std::thread::sleep(Duration::from_secs(1))
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(CliError::new("busy", EXIT_ERROR, "a sync is running already")
                    .hint("the background service is in the middle of a sync; try again in a few minutes"));
            }
            Err(fs::TryLockError::Error(e)) => return Err(failure(&engine::Error::Io(e))),
        }
    }
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this computer".to_string())
}

fn one_sync(
    paths: &Paths,
    dry_run: bool,
    allow_deletes: bool,
    progress: Box<dyn Fn(&str)>,
) -> Result<Outcome, engine::Error> {
    let api = DriveApi::connect()?;
    let state = State::open(&paths.state_dir)?;
    engine::bind_account(&state, api.dsid(), &paths.folder)?;
    let mut engine = Engine {
        remote: &api,
        state,
        root: paths.folder.clone(),
        top: engine::top_node(&paths.top),
        trash_dir: engine::default_trash_dir(),
        host: hostname(),
        progress,
    };
    engine.sync(dry_run, allow_deletes)
}

fn sync_once(paths: &Paths, dry_run: bool, allow_deletes: bool, mode: Mode) -> Result<Response, CliError> {
    let _lock = lock(paths, SYNC_WAIT)?;
    let progress: Box<dyn Fn(&str)> = if mode == Mode::Human && !dry_run {
        Box::new(|line| eprintln!("{line}"))
    } else {
        Box::new(|_| {})
    };
    let outcome = one_sync(paths, dry_run, allow_deletes, progress).map_err(|e| {
        let error = failure(&e);
        record(paths, Some(&error));
        error
    })?;
    record(paths, None);
    let n = outcome.done.len();
    let summary = match (dry_run, n) {
        (true, 0) => "Nothing to do: already in sync".to_string(),
        (true, n) => format!("{n} changes would be made"),
        (false, 0) if outcome.problems.is_empty() => "In sync".to_string(),
        (false, n) if outcome.problems.is_empty() => format!("{n} changes made"),
        (false, n) => format!("{n} changes made, {} left for later", outcome.problems.len()),
    };
    let mut text = Vec::new();
    if dry_run {
        text.extend(outcome.done.iter().cloned());
    }
    text.extend(outcome.problems.iter().map(|p| format!("failed: {p}")));
    text.extend(outcome.pending.iter().map(|p| format!("still being written: {p}")));
    text.push(summary.clone());
    let exit = if outcome.problems.is_empty() { 0 } else { EXIT_ERROR };
    Ok(Response::new(&outcome, summary)
        .human(text.join("\n"))
        .ids(outcome.done.clone())
        .count(n)
        .exit(exit))
}

/// Notes the last failure (or clears it) for `status`.
fn record(paths: &Paths, error: Option<&CliError>) {
    let Ok(state) = State::open(&paths.state_dir) else {
        return;
    };
    let _ = match error {
        Some(e) => state
            .set_meta("last_error", &e.message)
            .and_then(|()| state.set_meta("last_error_code", &e.code))
            .and_then(|()| state.set_meta("last_error_hint", e.hint.as_deref().unwrap_or(""))),
        None => state
            .clear_meta("last_error")
            .and_then(|()| state.clear_meta("last_error_code"))
            .and_then(|()| state.clear_meta("last_error_hint")),
    };
}

fn run_forever(paths: &Paths, interval: Duration) -> Result<Response, CliError> {
    let mut last_code = String::new();
    loop {
        // Held only for the pass, so a `sync` from the command line can run
        // between passes.
        let held = lock(paths, Duration::MAX)?;
        let result = one_sync(paths, false, false, Box::new(|line| println!("{line}")));
        match result {
            Ok(outcome) => {
                for problem in &outcome.problems {
                    eprintln!("failed: {problem}");
                }
                record(paths, None);
                last_code.clear();
            }
            Err(e) => {
                let error = failure(&e);
                // Said once, not every half minute.
                if error.code != last_code {
                    eprintln!("{}", error.message);
                    last_code.clone_from(&error.code);
                }
                record(paths, Some(&error));
            }
        }
        drop(held);
        std::thread::sleep(interval);
    }
}

fn service_state(what: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "--quiet", what, SERVICE])
        .status()
        .is_ok_and(|s| s.success())
}

fn service(args: &[&str], now: &str, paths: &Paths) -> Result<Response, CliError> {
    let done = Command::new("systemctl").arg("--user").args(args).status();
    match done {
        Ok(status) if status.success() => {
            let summary = if now == "on" {
                format!("iCloud Drive sync is on: {}", paths.folder.display())
            } else {
                "iCloud Drive sync is off (the folder and its files stay)".to_string()
            };
            Ok(Response::new(json!({"sync": now, "folder": paths.folder}), summary))
        }
        Ok(_) => Err(
            CliError::new("error", EXIT_ERROR, format!("systemctl could not turn the sync {now}"))
                .hint(format!("see `systemctl --user status {SERVICE}`")),
        ),
        Err(e) => Err(CliError::new(
            "error",
            EXIT_ERROR,
            format!("systemctl could not be run: {e}"),
        )),
    }
}

fn status(paths: &Paths) -> Result<Response, CliError> {
    let db = |e: rusqlite::Error| failure(&engine::Error::Db(e));
    let state = State::open(&paths.state_dir).map_err(db)?;
    let (files, folders) = state.count().map_err(db)?;
    let last_sync = state
        .meta("last_sync_ms")
        .map_err(db)?
        .and_then(|v| v.parse::<i64>().ok())
        .map(|ms| rfc3339(ms / 1000));
    let last_error = state.meta("last_error").map_err(db)?;
    let hint = state.meta("last_error_hint").map_err(db)?.filter(|h| !h.is_empty());
    let code = state.meta("last_error_code").map_err(db)?;
    let not_synced = state.ignored_list().map_err(db)?;
    let running = service_state("is-active");
    let enabled = service_state("is-enabled");
    // None: icloud-sessiond could not be asked, which is not the same as signed out.
    let signed_in = icloud_session::status().ok().map(|s| s.signed_in);

    let data = json!({
        "sync": if running { "on" } else { "off" },
        "starts_at_login": enabled,
        "signed_in": signed_in,
        "folder": paths.folder,
        "files": files,
        "folders": folders,
        "last_sync": last_sync,
        "problem": last_error.as_ref().map(|message| json!({"code": code, "message": message, "hint": hint})),
        "not_synced": not_synced.iter().map(|(path, reason)| json!({"path": path, "reason": reason})).collect::<Vec<_>>(),
    });
    let mut text = vec![
        format!(
            "Sync: {}",
            if running {
                "on"
            } else {
                "off (turn it on with `icloud-drive start`)"
            }
        ),
        format!("Folder: {}", paths.folder.display()),
        format!("In sync: {files} files in {folders} folders"),
        format!("Last sync: {}", last_sync.as_deref().unwrap_or("never")),
    ];
    match signed_in {
        Some(false) => text.push("Not signed in to iCloud: run `icloud-session sign-in`".to_string()),
        None => text.push("Sign-in state unknown: icloud-sessiond did not answer".to_string()),
        Some(true) => {}
    }
    if let Some(message) = &last_error {
        text.push(format!("Problem: {message}"));
        if let Some(hint) = &hint {
            text.push(format!("  {hint}"));
        }
    }
    for (path, reason) in &not_synced {
        text.push(format!("Not synced: {path} ({reason})"));
    }
    let summary = match (&last_error, running, signed_in) {
        (_, _, Some(false)) => "Not signed in to iCloud".to_string(),
        (Some(message), _, _) => format!("iCloud Drive sync has a problem: {message}"),
        (None, true, _) => format!("iCloud Drive sync is on, {files} files in sync"),
        (None, false, _) => "iCloud Drive sync is off".to_string(),
    };
    let exit = if signed_in == Some(false) { EXIT_SIGN_IN } else { 0 };
    Ok(Response::new(data, summary).human(text.join("\n")).exit(exit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_waits_for_the_service_pass_then_gives_up_busy() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            folder: dir.path().join("Drive"),
            state_dir: dir.path().join("state"),
            top: ROOT_ID.to_string(),
        };
        let service = lock(&paths, Duration::ZERO).unwrap();
        let refused = lock(&paths, Duration::from_millis(1500)).unwrap_err();
        assert_eq!(refused.code, "busy");
        drop(service);
        assert!(lock(&paths, Duration::ZERO).is_ok());
    }
}
