//! The command line. iCloud Drive is a folder, so there is nothing here for
//! reading or writing files: only `status`, a sync on demand, and turning
//! the background service on and off. Output follows the shared product
//! contract ([`icloud_session::output`]): text on a terminal, the JSON
//! envelope when piped. Exit codes are the table every iCloud tool shares
//! (docs/CLI.md).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use icloud_app::leave::{self, Left};
use icloud_session::cli::{EXIT_ERROR, EXIT_NOT_FOUND, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{self, CliError, Mode, OutputFlags, Response};
use icloud_session::time::rfc3339;
use serde_json::json;

use crate::engine::{self, Engine, Outcome};
use crate::remote::{self, DriveApi, ROOT_ID};
use crate::state::State;

const SERVICE: &str = "icloud-drive.service";
/// The app's name for [`leave`]'s record.
const APP: &str = "icloud-drive";
/// How long the background service rests between syncs.
const INTERVAL: Duration = Duration::from_secs(30);
/// How long `sync` waits for the background service's pass to end.
const SYNC_WAIT: Duration = Duration::from_secs(120);

const AFTER_HELP: &str = "\
iCloud Drive is the folder ~/iCloud Drive: read and write files there with
anything. This command only reports on the sync and turns it on and off.

Conflicts keep both files: yours is renamed `name (conflicted copy <host>
<date>).ext` and iCloud's version takes the name. A file removed in iCloud
goes to the trash here; one removed here goes to Recently Deleted in iCloud.
App folders (Pages, Shortcuts), packages and hidden names are not synced.

`leave` hands ~/iCloud Drive to Omacloud for good on this computer: a last
sync, then the folder syncs through Omacloud and nothing here talks to iCloud
Drive again (iCloud keeps its copy as it was). Omacloud must be installed, set
up and syncing the folder (`omacloud folders add ~/iCloud Drive`); `--to DIR`
moves the folder into one Omacloud already syncs instead.

Output: text on a terminal, a JSON envelope {\"ok\":true,\"data\",\"summary\"}
when piped. --json forces the envelope, --styled text, --quiet prints only
the data, --ids-only one line per change a sync made, --count their number. A
failure is {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (io, error, busy, http 4xx), or a sync that left some items for later
  2 usage; too_many_deletions (run `icloud-drive sync --yes` after checking
    `icloud-drive sync --dry-run`); other_account; other_folder; other_top
  3 sign_in_required, session_rejected, drive_unavailable; status while signed out;
    left_icloud (sync, start or leave after leaving); omacloud_missing,
    omacloud_not_set_up, omacloud_folder_off (leave: what Omacloud lacks)
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
    /// The local folder for `status` and `sync` (default: ~/iCloud Drive,
    /// the one the background sync keeps).
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Where the sync state is kept (default: ~/.local/state/icloud-drive).
    /// One state serves one folder.
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
        #[arg(long = "yes")]
        allow_deletes: bool,
    },
    /// Turn the background sync on, now and at every login.
    Start,
    /// Turn the background sync off.
    Stop,
    /// Leave iCloud Drive for Omacloud on this computer: a last sync, then
    /// the folder syncs through Omacloud only.
    Leave {
        /// Move the folder here first: a folder that does not exist yet (or
        /// is empty), in one Omacloud syncs, on the same disk.
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        /// Leave without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Sync again and again (what the background service runs).
    #[command(hide = true)]
    Run,
}

struct Paths {
    folder: PathBuf,
    state_dir: PathBuf,
    top: String,
}

impl Paths {
    fn of(args: &Args) -> Paths {
        Paths {
            folder: args.dir.clone().unwrap_or_else(engine::default_folder),
            state_dir: args.state_dir.clone().unwrap_or_else(engine::default_state_dir),
            top: args.top.clone().unwrap_or_else(|| ROOT_ID.to_string()),
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
    // The service runs with no options, so it only ever keeps the default
    // folder: saying "on" for another one would be untrue.
    let own_folder = args.dir.is_some() || args.state_dir.is_some() || args.top.is_some();
    let result = match args.command {
        Cmd::Start | Cmd::Stop | Cmd::Leave { .. } if own_folder => Err(CliError::new(
            "usage",
            EXIT_USAGE,
            "the background sync keeps ~/iCloud Drive only; --dir does not apply to start, stop and leave",
        )
        .hint("sync another folder by hand: `icloud-drive --dir DIR --state-dir DIR sync`")),
        Cmd::Status => status(&paths),
        Cmd::Sync { .. } | Cmd::Start if left_here().is_some() => {
            Err(leave::left_error("iCloud Drive", &left_here().expect("checked")))
        }
        Cmd::Sync { dry_run, allow_deletes } => sync_once(&paths, dry_run, allow_deletes, mode),
        Cmd::Leave { to, yes } => leave_icloud(
            &paths,
            to.as_deref(),
            yes,
            mode,
            || {
                let _lock = lock(&paths, SYNC_WAIT)?;
                let outcome = one_sync(&paths, false, false, Box::new(|_| {})).map_err(|e| failure(&e))?;
                record(&paths, None);
                Ok(outcome)
            },
            || service(&["disable", "--now", SERVICE], "off", &paths).map(|_| ()),
        ),
        Cmd::Start => service(&["enable", "--now", SERVICE], "on", &paths),
        Cmd::Stop => service(&["disable", "--now", SERVICE], "off", &paths).inspect(|_| record(&paths, None)),
        Cmd::Run => run_forever(&paths, INTERVAL),
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
        E::Remote(remote::Error::Listing { .. } | remote::Error::Partial { .. }) => {
            CliError::new("bad_response", EXIT_UNAVAILABLE, e.to_string())
                .hint("iCloud gave an incomplete answer; nothing was changed. Try again later")
        }
        E::FolderMissing(folder) => CliError::new("folder_missing", EXIT_NOT_FOUND, e.to_string()).hint(format!(
            "put {} back (mount its disk, or move it back); nothing is removed in iCloud while it is missing",
            folder.display()
        )),
        E::TooManyDeletions { .. } => CliError::new("too_many_deletions", EXIT_USAGE, e.to_string())
            .hint("see them with `icloud-drive sync --dry-run`, then run `icloud-drive sync --yes`"),
        E::OtherAccount { .. } => CliError::new("other_account", EXIT_USAGE, e.to_string()).hint(
            "sign in to that account again, or sync this one into a folder of its own: \
             `icloud-drive --dir DIR --state-dir DIR sync`",
        ),
        E::OtherFolder { .. } => CliError::new("other_folder", EXIT_USAGE, e.to_string())
            .hint("give this folder a state of its own: `icloud-drive --dir DIR --state-dir DIR sync`"),
        E::OtherTop { .. } => CliError::new("other_top", EXIT_USAGE, e.to_string()).hint(
            "give this folder of iCloud Drive a local folder and a state of its own: \
             `icloud-drive --top ID --dir DIR --state-dir DIR sync`",
        ),
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
    engine::bind(&state, api.dsid(), &paths.folder, &paths.top)?;
    let mut engine = Engine {
        remote: &api,
        state,
        root: paths.folder.clone(),
        top: engine::top_node(&paths.top),
        trash_dir: engine::default_trash_dir(),
        host: hostname(),
        progress,
    };
    let outcome = engine.sync(dry_run, allow_deletes)?;
    if !dry_run {
        // iCloud keeps this folder, so Omacloud leaves it alone. Hidden
        // names are never synced, so the marker stays on this computer.
        if let Err(e) = leave::hold(&paths.folder) {
            icloud_app::cli::warning(&format!("could not mark {} as iCloud's: {e}", paths.folder.display()));
        }
    }
    Ok(outcome)
}

/// The record that iCloud Drive left iCloud here. One that does not read
/// counts as left, so nothing reaches iCloud by mistake.
fn left_here() -> Option<Left> {
    leave::left(APP).unwrap_or_else(|_| {
        Some(Left {
            version: 1,
            at: 0,
            folder: engine::default_folder(),
        })
    })
}

/// `leave`: the steps are in [`icloud_app::leave`]. `last_sync` is the final
/// sync with iCloud and `stop` turns the background service off; both are
/// passed in so tests run without iCloud or systemd.
fn leave_icloud(
    paths: &Paths,
    to: Option<&Path>,
    yes: bool,
    mode: Mode,
    last_sync: impl FnOnce() -> Result<Outcome, CliError>,
    stop: impl FnOnce() -> Result<(), CliError>,
) -> Result<Response, CliError> {
    if let Some(left) = left_here() {
        return Err(leave::left_error("iCloud Drive", &left));
    }
    let io = |e: std::io::Error| failure(&engine::Error::Io(e));
    let folder = paths.folder.clone();
    let target = match to {
        None => folder.clone(),
        Some(to) => {
            let to = if to.is_absolute() {
                to.to_path_buf()
            } else {
                std::env::current_dir().map_err(io)?.join(to)
            };
            if to.starts_with(&folder) {
                return Err(icloud_app::cli::usage("--to must be outside the folder it moves"));
            }
            // empty or absent: nothing there is replaced
            if fs::read_dir(&to).is_ok_and(|mut d| d.next().is_some()) {
                return Err(CliError::new(
                    "exists",
                    EXIT_USAGE,
                    format!(
                        "{} is not empty; the folder moves only into an empty or new one",
                        to.display()
                    ),
                ));
            }
            if !to.parent().is_some_and(Path::is_dir) {
                return Err(CliError::new(
                    "folder_missing",
                    EXIT_NOT_FOUND,
                    format!("{} is not there to move into", to.parent().unwrap_or(&to).display()),
                ));
            }
            to
        }
    };
    leave::omacloud_ready(&target)?;
    let question = format!(
        "Leave iCloud Drive for Omacloud on this computer? After a last sync, {} syncs through Omacloud only; \
         iCloud keeps its copy as it is now.",
        target.display()
    );
    leave::confirm(mode, yes, "iCloud Drive", &question)?;

    // a last sync: everything made here reaches iCloud first
    let outcome = last_sync()?;
    if !outcome.problems.is_empty() {
        return Err(CliError::new(
            "sync_incomplete",
            EXIT_ERROR,
            format!(
                "the last sync left {} items for later, so iCloud Drive was not left",
                outcome.problems.len()
            ),
        )
        .hint("see `icloud-drive sync`, then leave again"));
    }
    let mut warnings = Vec::new();
    if let Err(e) = stop() {
        // the service stops by itself at its next pass: it finds the record
        warnings.push(format!("the background sync did not turn off: {}", e.message));
    }
    // held while the record is written and the state moves, so no pass of
    // the service runs in between
    let held = lock(paths, SYNC_WAIT)?;
    let left = leave::record_left(APP, &target).map_err(io)?;
    // the sync state is kept: what was in sync with iCloud when it was left
    if paths.state_dir.exists() {
        let aside = icloud_app::store::data_dir(APP).map(|d| d.join(format!("left-icloud-state-{}", left.at)));
        let moved = aside.as_ref().map(|aside| {
            fs::create_dir_all(aside.parent().expect("in the data directory"))
                .and_then(|()| fs::rename(&paths.state_dir, aside))
        });
        match moved {
            Some(Ok(())) => {}
            Some(Err(e)) => warnings.push(format!("the sync state stays in {}: {e}", paths.state_dir.display())),
            None => warnings.push("no data directory to keep the sync state in".to_string()),
        }
    }
    drop(held);
    leave::release(&folder).map_err(io)?;
    if target != folder && folder.exists() {
        if target.is_dir() {
            fs::remove_dir(&target).map_err(io)?;
        }
        fs::rename(&folder, &target).map_err(|e| {
            CliError::new(
                "io",
                EXIT_ERROR,
                format!(
                    "{} could not move to {} ({e}); iCloud Drive left iCloud and the folder stays where it was",
                    folder.display(),
                    target.display()
                ),
            )
            .hint(format!(
                "move it yourself, or have Omacloud sync it: `omacloud folders add {}`",
                folder.display()
            ))
        })?;
    }
    let synced = match leave::omacloud_sync() {
        Ok(()) => true,
        Err(e) => {
            warnings.push(format!(
                "Omacloud did not sync yet ({}); its next sync takes the folder in",
                e.message
            ));
            false
        }
    };
    for w in &warnings {
        icloud_app::cli::warning(w);
    }
    let summary = format!("iCloud Drive left iCloud: Omacloud syncs {} now", target.display());
    let text = format!("{summary}\nOn your other computers, run `icloud-drive leave` too.");
    Ok(Response::new(
        json!({
            "folder": target,
            "left_at": rfc3339(i64::try_from(left.at).unwrap_or(0)),
            "omacloud_synced": synced,
            "warnings": warnings,
        }),
        summary,
    )
    .human(text))
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
        // left iCloud meanwhile: never another pass, and a clean exit, so
        // systemd does not start it again
        if let Some(left) = left_here() {
            println!("{}", leave::left_error("iCloud Drive", &left).message);
            return Ok(Response::silent());
        }
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
    if let Some(left) = left_here() {
        let summary = format!("iCloud Drive left iCloud: Omacloud syncs {}", left.folder.display());
        return Ok(Response::new(
            json!({
                "sync": "off",
                "left": {"at": rfc3339(i64::try_from(left.at).unwrap_or(0)), "folder": left.folder},
                "folder": left.folder,
            }),
            summary.clone(),
        )
        .human(summary));
    }
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
    let not_synced = state.not_synced().map_err(db)?;
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

    /// A home with `~/iCloud Drive` (a file and the marker) and its sync
    /// state, the data directory there, and a stand-in `omacloud` whose
    /// `status` says `synced` syncs (a path under home) and which logs its
    /// calls.
    struct Home {
        dir: tempfile::TempDir,
        paths: Paths,
    }

    // the environment is the process's: one test sets it at a time
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl Home {
        fn new(synced: &str) -> Home {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let h = dir.path();
            let folder = h.join("iCloud Drive");
            fs::create_dir_all(folder.join("Docs")).unwrap();
            fs::write(folder.join("Docs/a.txt"), "a").unwrap();
            leave::hold(&folder).unwrap();
            let state_dir = h.join("state/icloud-drive");
            fs::create_dir_all(&state_dir).unwrap();
            fs::write(state_dir.join("state.sqlite"), "rows").unwrap();
            let status = format!(
                r#"{{"ok":true,"data":{{"set_up":true,"folder":"{}","folders":{{"synced":[{{"path":"{}"}}]}}}}}}"#,
                h.display(),
                h.join(synced).display()
            );
            let bin = h.join("omacloud");
            fs::write(
                &bin,
                format!(
                    "#!/bin/sh\necho \"$1\" >> '{}'\nif [ \"$1\" = status ]; then echo '{status}'; else echo '{{\"ok\":true,\"data\":null}}'; fi\n",
                    h.join("omacloud.log").display()
                ),
            )
            .unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            // SAFETY: only tests holding ENV touch the environment
            unsafe {
                std::env::set_var("XDG_DATA_HOME", h.join("data"));
                std::env::set_var("ICLOUD_OMACLOUD_BIN", &bin);
            }
            Home {
                paths: Paths {
                    folder,
                    state_dir,
                    top: ROOT_ID.to_string(),
                },
                dir,
            }
        }

        fn leave(&self, to: Option<&Path>, problems: usize) -> Result<Response, CliError> {
            leave_icloud(
                &self.paths,
                to,
                true,
                Mode::Json,
                || {
                    Ok(Outcome {
                        problems: (0..problems).map(|i| format!("p{i}")).collect(),
                        ..Outcome::default()
                    })
                },
                || Ok(()),
            )
        }

        fn omacloud_calls(&self) -> String {
            fs::read_to_string(self.dir.path().join("omacloud.log")).unwrap_or_default()
        }
    }

    #[test]
    fn leave_refuses_without_omacloud_and_changes_nothing() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = Home::new("iCloud Drive");
        unsafe { std::env::set_var("ICLOUD_OMACLOUD_BIN", "/nonexistent/omacloud") };
        let e = home.leave(None, 0).unwrap_err();
        assert_eq!(e.code, "omacloud_missing");
        assert!(left_here().is_none());
        assert!(home.paths.folder.join(leave::MARKER).is_file());
        assert!(home.paths.state_dir.exists());
    }

    #[test]
    fn leave_refuses_while_omacloud_skips_the_folder() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = Home::new("Work");
        let e = home.leave(None, 0).unwrap_err();
        assert_eq!(e.code, "omacloud_folder_off");
        assert!(e.hint.unwrap().contains("omacloud folders add ~/iCloud Drive"));
        assert!(left_here().is_none());
    }

    #[test]
    fn leave_stops_when_the_last_sync_leaves_something() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = Home::new("iCloud Drive");
        assert_eq!(home.leave(None, 1).unwrap_err().code, "sync_incomplete");
        assert!(left_here().is_none());
        assert!(home.paths.folder.join(leave::MARKER).is_file());
    }

    #[test]
    fn leave_hands_the_folder_to_omacloud_once() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = Home::new("iCloud Drive");
        home.leave(None, 0).unwrap();
        let left = left_here().expect("recorded");
        assert_eq!(left.folder, home.paths.folder);
        assert!(!home.paths.folder.join(leave::MARKER).exists());
        assert_eq!(fs::read_to_string(home.paths.folder.join("Docs/a.txt")).unwrap(), "a");
        // the state is kept aside, not deleted
        assert!(!home.paths.state_dir.exists());
        let aside = home
            .dir
            .path()
            .join(format!("data/icloud-drive/left-icloud-state-{}", left.at));
        assert_eq!(fs::read_to_string(aside.join("state.sqlite")).unwrap(), "rows");
        assert_eq!(home.omacloud_calls(), "status\nsync\n");
        // and once only; status reports it without making a new state
        assert_eq!(home.leave(None, 0).unwrap_err().code, "left_icloud");
        assert!(status(&home.paths).is_ok());
        assert!(!home.paths.state_dir.exists());
    }

    #[test]
    fn leave_to_moves_the_folder_into_one_omacloud_syncs() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let home = Home::new("Documents");
        let docs = home.dir.path().join("Documents");
        fs::create_dir_all(docs.join("full/x")).unwrap();
        assert_eq!(home.leave(Some(&docs.join("full")), 0).unwrap_err().code, "exists");
        let to = docs.join("iCloud Drive");
        home.leave(Some(&to), 0).unwrap();
        assert!(!home.paths.folder.exists());
        assert_eq!(fs::read_to_string(to.join("Docs/a.txt")).unwrap(), "a");
        assert!(!to.join(leave::MARKER).exists());
        assert_eq!(left_here().unwrap().folder, to);
    }
}
