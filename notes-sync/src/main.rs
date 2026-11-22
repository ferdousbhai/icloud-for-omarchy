//! The `icloud-notes-sync` CLI: clone, pull, push, status, restore, history,
//! diff (originally derived from icloud-md), plus `sync` (push, then pull, in
//! one run, for the Notes app), `background` (a sync when one is due, what the
//! Notes app runs every minute; `cmd::background`) and `vault-info`.
//!
//! clone, pull, push, sync, restore and a background that syncs take the vault
//! lock the Notes app holds (`cmd::lock`); status, history, diff, push
//! --dry-run, vault-info and a background that does not sync only read, and
//! don't.
//!
//! Output follows the product contract every iCloud tool shares
//! ([`icloud_session::output`]): text on a terminal, the JSON envelope
//! `{"ok":true,"data","summary","meta"?}` when piped (or with `--json`; clone,
//! pull and push put their text report in `meta.lines`, diff its text in
//! `meta.text`, which is how the Notes app logs them), only `data` with
//! `--quiet`, `--styled` forces text,
//! `--ids-only` / `--count` where a command lists something. A failure is
//! the contract error object `{"ok":false,"error":{"code","message","hint"?}}`
//! on one line of stderr in a machine mode (its last line, after any status
//! lines), `error:` / `hint:` on stderr in text mode: stdout only ever holds a
//! result. Status lines and machine progress (`icloud-notes:progress:...`)
//! go to stderr in a machine mode.
//!
//! Exit codes are Omarchy's: 0 on success, 1 on any failure; the error's
//! `code` tells failures apart (`cmd::errors`). `status`, `push --dry-run`
//! and `diff` report their changes in the result and exit 0; a `push` that
//! had a change refused reports its result and exits 1; `sync` reports both
//! halves in its data and exits 1 when either failed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use icloud_notes_sync::cmd::lock::lock_vault;
use icloud_notes_sync::cmd::output::{error_json, usage_error};
use icloud_notes_sync::cmd::plan::{PlanEntry, PlanEntryKind, PlanResolution, render_plan};
use icloud_notes_sync::cmd::{
    self, NoProgress, NoticeLevel, SyncNotice, SyncProgress, background, clone, diff, history, leave, pull, push,
    restore, status, sync, vault_info,
};
use icloud_notes_sync::vault::local::{display_path, find_vault_root};
use icloud_session::output::{self, CliError, Mode, OutputFlags, Response};

#[derive(Parser)]
#[command(
    name = "icloud-notes-sync",
    about = "Sync iCloud Notes with a folder of Markdown files",
    after_help = "Output: text on a terminal; when piped (or with --json) the JSON envelope \
                  {\"ok\":true,\"data\",\"summary\",\"meta\"?} (clone, pull and push: their text report in \
                  meta.lines; diff: meta.text); --quiet prints only data (the command's result); \
                  --styled forces text; --count prints how many entries (status, push, pull changes, history, \
                  vault-info notes) and --ids-only their files (status, push, restore) or snapshot ids \
                  (history). The flags combine: ids-only > count > quiet > json > styled. In those machine \
                  modes stdout is one JSON document; status lines and progress (icloud-notes:progress:...) \
                  go to stderr too, and a failure is \
                  {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on the last line of stderr (text \
                  mode: error: and hint: lines on stderr); stdout holds only a result.\n\
                  Exit codes: 0 on success, 1 on any failure. The error's code says which: usage, \
                  ambiguous_tracked_file, already_cloned_directory, too_many_deletions (most of the vault's note \
                  files are gone, and push --yes deletes the notes); sign_in_required, session_rejected \
                  (icloud-session sign-in); not_cloned_directory, account_mismatch, notes_unavailable, ... (not set \
                  up for this account); untracked_file, unknown_version_snapshot (not found); offline, network, \
                  session_service, keyring_unavailable, bad_response, cloudkit_request_failed (a service \
                  unavailable or failing). status, push --dry-run and diff \
                  report their changes in the result and exit 0.\n\
                  clone, pull, push, sync, restore and a background that syncs take the vault's lock, shared with the Notes app (icloud-notes); \
                  a busy lock is the error vault_busy.",
    version
)]
struct Cli {
    #[command(flatten)]
    out: OutputFlags,

    /// How long clone/pull/push/sync/restore wait for the vault's lock: default 30 when another run or a
    /// background sync holds it, none while the Notes window does
    #[arg(long, global = true, value_name = "SECS")]
    wait: Option<u64>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch all Notes into a fresh local directory
    Clone {
        directory: PathBuf,
        /// Carry each note's title in its file name rather than as the file's first line (a whole-vault choice made at clone time)
        #[arg(long)]
        filename_as_title: bool,
        /// Clone as this account (an Apple ID, or its dsid); must be the account icloud-session is signed in as
        #[arg(long, value_name = "appleId")]
        account: Option<String>,
        /// Accepted for compatibility; never opens a sign-in window anyway
        #[arg(long)]
        non_interactive: bool,
    },
    /// Fetch changes since the last clone/pull (defaults to the current directory)
    Pull { directory: Option<PathBuf> },
    /// Reconcile local disk state up to iCloud (creates, updates, moves, deletes)
    Push {
        directory: Option<PathBuf>,
        /// Report what would be pushed without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Delete the notes whose files are gone even when that is most of the vault (otherwise the error too_many_deletions)
        #[arg(long)]
        yes: bool,
    },
    /// Push, then pull, in one run over one connection (what the Notes app does)
    #[command(
        after_help = "data is {\"push\": HALF, \"pull\": HALF or null, \"vault_info\": vault-info's \
                      answer or null}, even when a half failed; HALF is {\"ok\", \"exit_code\" (0 or 1), \"result\" (push's or \
                      pull's own data), \"lines\" (its human report)} or {\"ok\", \"exit_code\", \"error\" \
                      ({\"code\",\"message\",\"hint\"?})}. A failed half is reported only there: no separate error \
                      object. A push that ran but had a change refused (push_rejected) carries its result \
                      and lines as well as the error. The pull runs unless the push found the sign-in gone or iCloud out of reach (null \
                      then). Exit code: 0 when both halves worked, else 1; each failed half's error code says why."
    )]
    Sync {
        directory: Option<PathBuf>,
        /// As push --yes: delete the notes whose files are gone even when that is most of the vault
        #[arg(long)]
        yes: bool,
    },
    /// Preview exactly what the next push will do (requires signing in)
    Status { directory: Option<PathBuf> },
    /// Discard a tracked note's local edits, reverting it to the last synced copy
    Restore { file: String, directory: Option<PathBuf> },
    /// List a note's epoch timeline, newest first
    History {
        file: String,
        directory: Option<PathBuf>,
        /// Flat per-record snapshot listing instead of the epoch timeline
        #[arg(long)]
        records: bool,
    },
    /// What the Notes app reads from the vault's state: title mode, default folder, tracked notes (JSON, also in text mode)
    VaultInfo { directory: Option<PathBuf> },
    /// What the Notes app runs every minute: a sync when one is due or local changes wait, else nothing
    #[command(
        after_help = "Syncs (as sync does, never with --yes) when the last sync is --every old or there was none, \
                      or sooner when the vault has local changes to push made since the last sync started (found \
                      from the vault alone: no network, no lock); never while one of them is younger than 10 s. A \
                      change a sync left behind (refused, or the sync failed) waits for the next due sync or a new \
                      edit. A due sync with nobody signed in to icloud-session, or icloud-session out of reach, is \
                      skipped and counts as a failed one. Every sync (this one or sync) is recorded in \
                      background.json beside the vault's state.\n\
                      data is {\"synced\", \"step\"} (step: due, changes, settling, idle, signed_out or \
                      session_unavailable), plus sync's \"push\", \"pull\" and \"vault_info\" when it synced. Exit 0 \
                      when it did not sync, else sync's exit code; text mode prints nothing then."
    )]
    Background {
        directory: Option<PathBuf>,
        /// A sync is due once the last one is this old (the window asks for 60 while it is in front)
        #[arg(long, value_name = "SECS", default_value_t = background::DEFAULT_EVERY_SECS)]
        every: u64,
    },
    /// Leave iCloud for Omacloud: a last sync, then Omacloud syncs the folder and nothing here talks to iCloud
    #[command(
        after_help = "Needs Omacloud installed, set up and syncing the vault's folder of home (omacloud_missing, \
                      omacloud_not_set_up, omacloud_folder_off, each with the command that fixes it; \
                      nothing changes). Then a sync as sync does (never deleting most of the vault); if it fails, \
                      nothing changes. Then the vault's state (.icloud-notes) moves to \
                      $XDG_DATA_HOME/icloud-notes/left-icloud-state/, the .nosync marker leaves the vault, and \
                      omacloud sync runs (a failure there is a warning). iCloud keeps its copy as it was. From \
                      then on clone, pull, push, sync, status, restore, history and diff refuse with left_icloud \
                      and background skips (step left). Asks on a terminal; otherwise needs --yes. Run \
                      again, it finishes a leave cut short.\n\
                      data is {\"folder\", \"left_at\" (seconds since the epoch), \"kept_state\", \
                      \"omacloud_synced\", \"already\"}."
    )]
    Leave {
        directory: Option<PathBuf>,
        /// Leave without asking
        #[arg(long)]
        yes: bool,
    },
    /// Diff two snapshots, or one snapshot against the current remote copy
    #[command(
        after_help = "<ref> is a snapshot id (diffed against the current remote copy) or <from>..<to> (two \
                            snapshot ids) - ids come from \"icloud-notes history <file>\"."
    )]
    Diff {
        file: String,
        #[arg(name = "ref")]
        reference: String,
        directory: Option<PathBuf>,
    },
}

/// Machine-mode progress: one `icloud-notes:progress:...` line per event on
/// stderr (the prefix is what wrapping processes match).
struct MachineProgress {
    processed: usize,
    total: usize,
}

impl SyncProgress for MachineProgress {
    fn on_fetch_page(&mut self, records_so_far: usize) {
        eprintln!("icloud-notes:progress:fetch:{records_so_far}");
    }
    fn on_process_start(&mut self, total_records: usize) {
        self.total = total_records;
        eprintln!("icloud-notes:progress:process-start:{total_records}");
    }
    fn on_record_processed(&mut self) {
        self.processed += 1;
        eprintln!("icloud-notes:progress:process:{}/{}", self.processed, self.total);
    }
    fn on_process_complete(&mut self) {
        eprintln!("icloud-notes:progress:process-done");
    }
}

fn main() -> ExitCode {
    let cli = match output::parse::<Cli>() {
        Ok(cli) => cli,
        Err(code) => return ExitCode::from(code),
    };
    let mode = cli.out.mode();

    let Some(command) = cli.command else {
        if !mode.is_machine() {
            use clap::CommandFactory;
            eprintln!("{}", Cli::command().render_help());
        }
        return ExitCode::from(usage_error("no command given").report(mode));
    };

    let wait = cli.wait.map(std::time::Duration::from_secs);
    ExitCode::from(match run(command, mode, wait) {
        Ok(response) => response.print(mode),
        Err(error) => error.report(mode),
    })
}

/// An explicit directory wins; otherwise walk up from the cwd to the vault,
/// falling back to "." so a not-cloned error names where the user stood.
fn resolve_target_dir(directory: Option<PathBuf>) -> Result<PathBuf, cmd::Error> {
    if let Some(dir) = directory {
        return Ok(dir);
    }
    Ok(find_vault_root(Path::new("."))?.unwrap_or_else(|| PathBuf::from(".")))
}

/// One line of a human report, and whether it is a warning (stderr).
type Line = (bool, String);

/// The text-mode report: warnings go to stderr now (text mode only; in a
/// machine mode they are in the data's `notices`), the rest is returned for
/// stdout.
fn human(mode: Mode, lines: Vec<Line>) -> String {
    let mut out = Vec::new();
    for (warn, line) in lines {
        if !warn {
            out.push(line);
        } else if mode == Mode::Human {
            eprintln!("{}", output::terminal_safe(&line));
        }
    }
    out.join("\n")
}

/// Every line of a human report, warnings included, for the envelope's
/// `meta.lines` (clone, pull, push): the Notes app runs them with `--json`, so
/// it gets the error code of a failure and still logs the text report.
fn report_lines(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|(_, line)| line.clone()).collect()
}

fn notice_lines(notices: &[SyncNotice]) -> Vec<Line> {
    notices
        .iter()
        .map(|n| (n.level == NoticeLevel::Warn, n.message.clone()))
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `clone`'s human report.
fn clone_lines(s: &clone::CloneSummary, directory: &Path) -> Vec<Line> {
    let mut lines = vec![(
        false,
        format!(
            "Cloned {} notes (plus {} shared with you) into {}, {} attachment(s) downloaded",
            s.written,
            s.written_shared,
            directory.display(),
            s.attachments_downloaded
        ),
    )];
    if s.written_unpublishable > 0 {
        lines.push((
            false,
            format!(
                "{} note(s) written read-only: which of their text their formatting is for can't be told",
                s.written_unpublishable
            ),
        ));
    }
    lines.push((
        false,
        format!(
            "Skipped: {} deleted, {} undecodable",
            s.skipped_deleted, s.skipped_undecodable
        ),
    ));
    lines.extend(notice_lines(&s.notices));
    lines
}

/// `pull`'s human report.
fn pull_lines(s: &pull::PullSummary, target: &Path) -> Vec<Line> {
    let mut lines: Vec<Line> = pull::render_pull_report(s, &|file| display_path(target, file))
        .into_iter()
        .map(|line| (false, line))
        .collect();
    if s.skipped_new_unsyncable > 0 {
        lines.push((
            false,
            format!("{} new unsyncable note(s) skipped", s.skipped_new_unsyncable),
        ));
    }
    if s.unshared_untracked > 0 {
        lines.push((
            false,
            format!(
                "{} shared note(s) no longer shared with you - removed here (a copy edited here is kept, not synced)",
                s.unshared_untracked
            ),
        ));
    }
    lines.extend(notice_lines(&s.notices));
    lines
}

fn pull_summary(s: &pull::PullSummary) -> String {
    if s.changes.is_empty() {
        pull::UP_TO_DATE.into()
    } else {
        format!(
            "Pulled {}: {} added, {} updated, {} merged, {} removed",
            plural(s.changes.len(), "change", "changes"),
            s.added,
            s.updated,
            s.merged,
            s.removed
        )
    }
}

/// `push`'s (and `push --dry-run`'s) human report.
fn push_lines(r: &push::PushResult, target: &Path) -> Vec<Line> {
    let mut lines = notice_lines(&r.notices);
    for entry in &r.entries {
        if let Some(outcome) = &entry.outcome {
            lines.push((false, outcome.message.clone()));
        }
    }
    if let Some(pushed) = r.pushed {
        // `pushed` counts folder changes too; name them apart.
        let folders = r
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e.entry.kind,
                    PlanEntryKind::CreateFolder | PlanEntryKind::RenameFolder | PlanEntryKind::DeleteFolder
                ) && e.entry.resolution == PlanResolution::Ready
                    && e.outcome.as_ref().is_some_and(|o| o.succeeded)
            })
            .count();
        let notes = pushed.saturating_sub(folders);
        let what = match (notes, folders) {
            (_, 0) => format!("{notes} note(s)"),
            (0, _) => format!("{folders} folder change(s)"),
            _ => format!("{notes} note(s) and {folders} folder change(s)"),
        };
        lines.push((false, format!("Pushed {what} from {}", target.display())));
    }
    let entries: Vec<_> = r.entries.iter().map(|e| e.entry.clone()).collect();
    let unchanged = r.dry_run.then_some(r.unchanged);
    lines.extend(plan_lines(&entries, unchanged, target));
    lines
}

/// The plan's listing: of what a push did, or (with the `unchanged` count)
/// the preview `status` and `push --dry-run` share.
fn plan_lines(entries: &[PlanEntry], unchanged: Option<usize>, target: &Path) -> Vec<Line> {
    render_plan(entries, &|file| display_path(target, file), unchanged)
        .into_iter()
        .map(|line| (false, line))
        .collect()
}

fn preview_summary(changes: usize, unchanged: usize) -> String {
    format!(
        "{} to push, {unchanged} unchanged",
        plural(changes, "change", "changes")
    )
}

fn push_summary(r: &push::PushResult, target: &Path) -> String {
    match r.pushed {
        Some(pushed) => format!(
            "Pushed {} from {}",
            plural(pushed, "change", "changes"),
            target.display()
        ),
        None => preview_summary(r.entries.len(), r.unchanged),
    }
}

/// One half of `sync`'s data: `{ok, exit_code, result, lines}`, or `{ok,
/// exit_code, error}` when it failed.
/// A failed half's error is converted to its `CliError` once by the caller
/// (a sign-in error asks icloud-sessiond why, a D-Bus round trip).
fn sync_half_json<T: serde::Serialize>(
    half: &Result<&T, CliError>,
    lines: impl FnOnce(&T) -> Vec<Line>,
) -> serde_json::Value {
    match half {
        Ok(result) => serde_json::json!({
            "ok": true,
            "exit_code": cmd::errors::EXIT_OK,
            "result": result,
            "lines": lines(result).into_iter().map(|(_, line)| line).collect::<Vec<_>>(),
        }),
        Err(error) => serde_json::json!({
            "ok": false,
            "exit_code": error.exit,
            "error": error_json(error),
        }),
    }
}

/// The error of a push that ran but had changes refused (`push_rejected`):
/// the first one's message, and how many more there were.
fn push_failure_json(result: &push::PushResult) -> Option<serde_json::Value> {
    let failures = result.failures();
    let first = failures.first()?;
    let message = match failures.len() - 1 {
        0 => (*first).to_owned(),
        more => format!("{first} (and {more} more; see the log)"),
    };
    Some(serde_json::json!({ "code": "push_rejected", "message": message }))
}

fn half_summary<T>(name: &str, half: Option<&Result<T, CliError>>) -> String {
    match half {
        Some(Ok(_)) => format!("{name} ok"),
        Some(Err(error)) => format!("{name} failed ({})", error.code),
        None => format!("{name} not run"),
    }
}

/// `sync` (and `background` once it syncs), with the vault locked: its
/// data, summary, text report and exit code. The run is recorded for
/// `background` (when it started, and whether it worked).
fn run_sync_command(
    target: &Path,
    mode: Mode,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    yes: bool,
) -> (serde_json::Value, String, String, u8) {
    let started = icloud_notes_sync::vault::rt::now_ms();
    hold(target);
    let options = push::PushOptions {
        dry_run: false,
        allow_deletes: yes,
    };
    let outcome = sync::run_sync(target, progress, on_status, &options);
    background::record_run(target, started, outcome.exit_code() == cmd::errors::EXIT_OK);
    // Each failed half's error, converted once for the data and the text
    // report alike.
    let push = outcome.push.as_ref().map_err(CliError::from);
    let pull = outcome.pull.as_ref().map(|p| p.as_ref().map_err(CliError::from));
    let mut push_half = sync_half_json(&push, |r| push_lines(r, target));
    // The push run again after the pull failed: the first one's result
    // stays, with that error.
    let push_again = outcome.push_again.as_ref().map(CliError::from);
    let push_failure = match &push_again {
        Some(error) => Some(error_json(error)),
        None => outcome.push.as_ref().ok().and_then(push_failure_json),
    };
    if let Some(error) = &push_failure {
        push_half["ok"] = false.into();
        push_half["exit_code"] = push_again.as_ref().map_or(cmd::errors::EXIT_ERROR, |e| e.exit).into();
        push_half["error"] = error.clone();
    }
    let data = serde_json::json!({
        "push": push_half,
        "pull": pull.as_ref().map(|p| sync_half_json(p, |s| pull_lines(s, target))),
        "vault_info": vault_info::run_vault_info(target).ok(),
    });
    let summary = format!(
        "{}, {}",
        if let Some(error) = &push_again {
            format!("push failed ({})", error.code)
        } else if push_failure.is_some() {
            "push failed (push_rejected)".to_owned()
        } else {
            half_summary("push", Some(&push))
        },
        half_summary("pull", pull.as_ref())
    );
    // Text mode: each half's report, or its error on stderr, as push and
    // pull alone would print them. A machine mode has it all in the data
    // (no separate error object).
    let mut lines = Vec::new();
    if mode == Mode::Human {
        match &push {
            Ok(r) => lines.extend(push_lines(r, target)),
            Err(error) => {
                error.report(mode);
            }
        }
        if let Some(error) = &push_again {
            error.report(mode);
        }
        match &pull {
            Some(Ok(s)) => lines.extend(pull_lines(s, target)),
            Some(Err(error)) => {
                error.report(mode);
            }
            // The push's error says why (no sign-in, or no network).
            None => eprintln!("Not pulled."),
        }
    }
    let text = human(mode, lines);
    (data, summary, text, outcome.exit_code())
}

/// While iCloud keeps the vault, Omacloud leaves it alone (`icloud_app::leave`).
fn hold(vault: &Path) {
    if let Err(e) = icloud_app::leave::hold(vault) {
        icloud_app::cli::warning(&format!("could not mark {} as iCloud's: {e}", vault.display()));
    }
}

fn run(command: Command, mode: Mode, wait: Option<std::time::Duration>) -> Result<Response, CliError> {
    let mut on_status = |message: &str| cmd::output::status(mode, message);
    let mut machine = MachineProgress { processed: 0, total: 0 };
    let mut quiet = NoProgress;
    let progress: &mut dyn SyncProgress = if mode.is_machine() { &mut machine } else { &mut quiet };

    Ok(match command {
        Command::Clone {
            directory,
            filename_as_title,
            account,
            non_interactive: _,
        } => {
            let options = clone::CloneOptions {
                filename_as_title,
                account,
            };
            leave::refuse_if_left(&directory)?;
            let _lock = lock_vault(&directory, wait)?;
            let s = clone::run_clone(&directory, progress, &options)?;
            hold(&directory);
            let summary = format!(
                "Cloned {} (plus {} shared with you) into {}",
                plural(s.written, "note", "notes"),
                s.written_shared,
                directory.display()
            );
            let lines = clone_lines(&s, &directory);
            let report = report_lines(&lines);
            Response::new(&s, summary)
                .human(human(mode, lines))
                .meta("lines", report)
        }
        Command::Pull { directory } => {
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let _lock = lock_vault(&target, wait)?;
            let s = pull::run_pull(&target, progress, &mut on_status)?;
            hold(&target);
            let lines = pull_lines(&s, &target);
            let report = report_lines(&lines);
            Response::new(&s, pull_summary(&s))
                .human(human(mode, lines))
                .meta("lines", report)
                .count(s.changes.len())
        }
        Command::Push {
            directory,
            dry_run,
            yes,
        } => {
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let _lock = if dry_run {
                None
            } else {
                Some(lock_vault(&target, wait)?)
            };
            let options = push::PushOptions {
                dry_run,
                allow_deletes: yes,
            };
            let r = push::run_push(&target, &mut on_status, &options)?;
            let lines = push_lines(&r, &target);
            let report = report_lines(&lines);
            // A change iCloud refused is in the result; the exit says so too.
            let exit = if r.failures().is_empty() {
                cmd::errors::EXIT_OK
            } else {
                cmd::errors::EXIT_ERROR
            };
            Response::new(&r, push_summary(&r, &target))
                .human(human(mode, lines))
                .meta("lines", report)
                .count(r.entries.len())
                .ids(r.entries.iter().map(|e| e.entry.file.clone()).collect())
                .exit(exit)
        }
        Command::Sync { directory, yes } => {
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let _lock = lock_vault(&target, wait)?;
            let (data, summary, text, exit) = run_sync_command(&target, mode, progress, &mut on_status, yes);
            Response::new(data, summary).human(text).exit(exit)
        }
        Command::Background { directory, every } => {
            let target = resolve_target_dir(directory)?;
            // left iCloud: the timer has nothing to do, and says nothing
            if leave::left_here(&target).is_some() {
                let data = serde_json::json!({ "synced": false, "step": "left" });
                return Ok(if mode == Mode::Human {
                    Response::silent()
                } else {
                    Response::new(data, "Not synced (left)")
                });
            }
            let now = icloud_notes_sync::vault::rt::now_ms();
            let changes = push::local_changes(&target)?;
            let every_ms = i64::try_from(every.saturating_mul(1000)).unwrap_or(i64::MAX);
            let step = background::decide(now, background::last_run(&target), &changes, every_ms);
            let skipped = |step: &str| {
                let data = serde_json::json!({ "synced": false, "step": step });
                if mode == Mode::Human {
                    Response::silent()
                } else {
                    Response::new(data, format!("Not synced ({step})"))
                }
            };
            if !step.syncs() {
                let name = serde_json::to_value(step).ok();
                return Ok(skipped(name.as_ref().and_then(|v| v.as_str()).unwrap_or("idle")));
            }
            // Nobody to sync as: no connection tried, and asked again only
            // when the next sync is due.
            let signed_out = match icloud_session::status() {
                Ok(status) if status.signed_in => None,
                Ok(_) => Some("signed_out"),
                Err(_) => Some("session_unavailable"),
            };
            if let Some(why) = signed_out {
                background::record_run(&target, now, false);
                return Ok(skipped(why));
            }
            let _lock = lock_vault(&target, wait)?;
            let (mut data, summary, text, exit) = run_sync_command(&target, mode, progress, &mut on_status, false);
            data["synced"] = true.into();
            data["step"] = serde_json::to_value(step).unwrap_or_default();
            Response::new(data, summary).human(text).exit(exit)
        }
        Command::Status { directory } => {
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let r = status::run_status(&target, &mut on_status)?;
            let mut lines = notice_lines(&r.notices);
            lines.extend(plan_lines(&r.entries, Some(r.unchanged), &target));
            let summary = preview_summary(r.entries.len(), r.unchanged);
            let text = human(mode, lines);
            Response::new(&r, summary)
                .human(text)
                .count(r.entries.len())
                .ids(r.entries.iter().map(|e| e.file.clone()).collect())
        }
        Command::Restore { file, directory } => {
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let _lock = lock_vault(&target, wait)?;
            let r = restore::run_restore(&target, &file)?;
            let summary = format!("Restored {} to match the last synced copy.", r.file);
            let ids = vec![r.file.clone()];
            Response::new(&r, summary).ids(ids)
        }
        Command::VaultInfo { directory } => {
            let target = resolve_target_dir(directory)?;
            let info = vault_info::run_vault_info(&target)?;
            let text = output::pretty(&info);
            let summary = format!(
                "{} ({}), {} tracked",
                info.vault,
                if info.cloned { "cloned" } else { "not cloned" },
                plural(info.notes.len(), "note", "notes")
            );
            Response::new(&info, summary).human(text).count(info.notes.len())
        }
        Command::History {
            file,
            directory,
            records,
        } => {
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let r = history::run_history(&target, &file, &history::HistoryOptions { records })?;
            let (ids, summary): (Vec<String>, String) = match &r {
                history::HistoryResult::Records { records, .. } => (
                    records.iter().map(|row| row.id.clone()).collect(),
                    plural(records.len(), "snapshot", "snapshots"),
                ),
                history::HistoryResult::Epochs { epochs, .. } => (
                    epochs.iter().map(|e| e.id.clone()).collect(),
                    plural(epochs.len(), "epoch", "epochs"),
                ),
            };
            // A file left out is a warning on stderr in text mode, and in
            // the data's `notices` otherwise.
            let mut lines = notice_lines(r.notices());
            lines.push((false, history_text(&r)));
            Response::new(&r, summary).human(human(mode, lines)).ids(ids)
        }
        Command::Diff {
            file,
            reference,
            directory,
        } => {
            let parts: Vec<&str> = reference.split("..").collect();
            let (from, to) = match parts.as_slice() {
                [from] if !from.is_empty() => (*from, None),
                [from, to] if !from.is_empty() && !to.is_empty() => (*from, Some(*to)),
                _ => {
                    return Err(usage_error(format!(
                        "Invalid ref \"{reference}\" - expected a snapshot id or <from>..<to>."
                    )));
                }
            };
            let target = resolve_target_dir(directory)?;
            leave::refuse_if_left(&target)?;
            let r = diff::run_diff(&target, &file, from, to)?;
            let summary = if r.has_differences() {
                "Differences found"
            } else {
                "No differences"
            };
            let text = diff::render_diff_result(&r);
            Response::new(&r, summary).human(text.clone()).meta("text", text)
        }
        Command::Leave { directory, yes } => {
            let target = resolve_target_dir(directory)?;
            let vault = std::path::absolute(&target).map_err(cmd::Error::from)?;
            let already = leave::left_here(&vault);
            if already.is_none() {
                if icloud_notes_sync::vault::state::read_clone_state(&vault)?.is_none() {
                    return Err(cmd::Error::NotClonedDirectory {
                        target_dir: vault.display().to_string(),
                    }
                    .into());
                }
                icloud_app::leave::omacloud_ready(&vault)?;
                icloud_app::leave::confirm(
                    mode,
                    yes,
                    "Notes",
                    &format!(
                        "Sync once more, then stop syncing {} with iCloud on this computer and let Omacloud sync it? iCloud keeps its copy as it is.",
                        vault.display()
                    ),
                )?;
                let _lock = lock_vault(&vault, wait)?;
                let (_, summary, text, exit) = run_sync_command(&vault, mode, progress, &mut on_status, false);
                if exit != cmd::errors::EXIT_OK {
                    if mode == Mode::Human && !text.is_empty() {
                        eprintln!("{}", output::terminal_safe(&text));
                    }
                    return Err(CliError::new(
                        "sync_failed",
                        exit,
                        format!("the last sync with iCloud failed ({summary}); nothing changed"),
                    )
                    .hint("run `icloud-notes sync` to see why, then leave again"));
                }
                let r = leave::finish(&vault, None)?;
                return Ok(leave_response(&r));
            }
            let r = leave::finish(&vault, already)?;
            leave_response(&r)
        }
    })
}

fn leave_response(r: &leave::LeaveResult) -> Response {
    let text = format!(
        "{} {} iCloud on this computer: Omacloud syncs it{}. iCloud keeps its copy as it was.",
        r.folder,
        if r.already { "had left" } else { "left" },
        if r.omacloud_synced { " (synced now)" } else { "" }
    );
    Response::new(r, format!("{} left iCloud", r.folder)).human(text)
}

/// `history`'s human report.
fn history_text(result: &history::HistoryResult) -> String {
    let mut lines = Vec::new();
    match result {
        history::HistoryResult::Records { records, .. } => {
            if records.is_empty() {
                lines.push("No version history recorded yet.".to_owned());
            }
            for row in records {
                lines.push(format!(
                    "{}  {}  {}  (changeTag {})",
                    row.id, row.timestamp, row.label, row.record_change_tag
                ));
            }
        }
        history::HistoryResult::Epochs { epochs, .. } => {
            if epochs.is_empty() {
                lines.push("No version history recorded yet.".to_owned());
            }
            for epoch in epochs {
                let mut line = format!(
                    "{}  {}  changed: {}",
                    epoch.id,
                    epoch.timestamp,
                    epoch.changed.join(", ")
                );
                if !epoch.carried_over.is_empty() {
                    line.push_str(&format!("  (carried over: {})", epoch.carried_over.join(", ")));
                }
                lines.push(line);
            }
        }
    }
    lines.join("\n")
}
