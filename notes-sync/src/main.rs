//! The `icloud-notes-sync` CLI: clone, pull, push, status, restore, history,
//! diff (originally derived from icloud-md), plus `sync` (push, then pull, in
//! one run, for the Notes app) and `vault-info`.
//!
//! clone, pull, push, sync and restore take the vault lock the Notes app holds
//! (`cmd::lock`); status, history, diff, push --dry-run and vault-info only
//! read, and don't.
//!
//! Output follows the product contract every iCloud tool shares
//! ([`icloud_session::output`]): text on a terminal, the JSON envelope
//! `{"ok":true,"data","summary","meta"?}` when piped (or with `--json`; clone,
//! pull and push put their text report in `meta.lines`, diff its text in
//! `meta.text`, which is how the Notes app logs them), only `data` with
//! `--quiet`, `--styled` forces text,
//! `--ids-only` / `--count` where a command lists something. A failure is
//! the contract error object `{"ok":false,"error":{"code","message","hint"?}}`
//! on stdout in a machine mode, `error:` / `hint:` on stderr in text mode.
//! Status lines and machine progress (`icloud-notes:progress:...`) go to
//! stderr in a machine mode.
//!
//! Exit codes: 0 ok, 1 error, 2 usage, 3 not signed in / not set up, 4 not
//! found, 5 a service unavailable (the table every iCloud tool shares;
//! `cmd::errors`). `status`, `push --dry-run` and `diff` report their
//! changes in the result and exit 0; `sync` reports both halves in its data
//! and exits with its worse half, a sign-in above all.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use icloud_notes_sync::cmd::lock::lock_vault;
use icloud_notes_sync::cmd::output::{error_json, usage_error};
use icloud_notes_sync::cmd::plan::{PlanEntryKind, PlanResolution, RenderPlanOptions, render_plan};
use icloud_notes_sync::cmd::{
    self, NoProgress, NoticeLevel, SyncNotice, SyncProgress, clone, diff, history, pull, push, restore, status, sync,
    vault_info,
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
                  go to stderr, and a failure is \
                  {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout (text mode: error: and \
                  hint: lines on stderr).\n\
                  Exit codes: 0 ok, 1 error, 2 usage (also ambiguous_tracked_file, already_cloned_directory), \
                  3 not signed in or not set up (sign_in_required, session_rejected: icloud-session sign-in; \
                  not_cloned_directory, account_mismatch, notes_unavailable, ...: the error code tells them apart), \
                  4 not found (untracked_file, unknown_version_snapshot), 5 a service unavailable (offline, network, \
                  session_service, keyring_unavailable, bad_response, iCloud failing a request; an HTTP 4xx \
                  cloudkit_request_failed is 1). status, push --dry-run and diff \
                  report their changes in the result and exit 0.\n\
                  clone, pull, push, sync and restore take the vault's lock, shared with the Notes app (icloud-notes); \
                  a busy lock is the error vault_busy (exit 1).",
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
    Pull {
        directory: Option<PathBuf>,
        /// In a filename-as-title vault, report the renames a remote retitle needs instead of performing them
        #[arg(long)]
        defer_renames: bool,
    },
    /// Reconcile local disk state up to iCloud (creates, updates, moves, deletes)
    Push {
        directory: Option<PathBuf>,
        /// Report what would be pushed without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Push, then pull, in one run over one connection (what the Notes app does)
    #[command(
        after_help = "data is {\"push\": HALF, \"pull\": HALF or null, \"vault_info\": vault-info's \
                      answer or null}, even when a half failed; HALF is {\"ok\", \"exit_code\", \"result\" (push's or \
                      pull's own data), \"lines\" (its human report)} or {\"ok\", \"exit_code\", \"error\" \
                      ({\"code\",\"message\",\"hint\"?})}. A failed half is reported only there: no separate error \
                      object. The pull runs unless the push found the sign-in gone or iCloud out of reach (null \
                      then). Exit code: 3 if either half exited 3 (sign-in required, or the vault not set up for this \
                      account), else the higher of the two halves' codes (5 unavailable, 4 not found, 2 usage, 1 error)."
    )]
    Sync { directory: Option<PathBuf> },
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
    /// Diff two snapshots, or one snapshot against the current remote copy
    #[command(
        after_help = "<ref> is a snapshot id (diffed against the current remote copy) or <from>..<to> (two \
                            snapshot ids) - ids come from \"icloud-notes-sync history <file>\"."
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
                "{} note(s) written with content this tool couldn't fully parse - read-only",
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
    if s.skipped_new_unsyncable > 0 || s.dropped_unsyncable > 0 {
        lines.push((
            false,
            format!(
                "{} new unsyncable note(s) skipped, {} note(s) dropped from tracking (no longer syncable)",
                s.skipped_new_unsyncable, s.dropped_unsyncable
            ),
        ));
    }
    if s.unshared_untracked > 0 {
        lines.push((
            false,
            format!(
                "{} shared note(s) no longer shared with you - local copies left in place, untracked",
                s.unshared_untracked
            ),
        ));
    }
    lines.extend(notice_lines(&s.notices));
    lines
}

fn pull_summary(s: &pull::PullSummary) -> String {
    if s.changes.is_empty() {
        "Already up to date.".into()
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
    let options = if r.dry_run {
        RenderPlanOptions {
            preview: true,
            unchanged: Some(r.unchanged),
        }
    } else {
        RenderPlanOptions::default()
    };
    lines.extend(
        render_plan(&entries, &|file| display_path(target, file), options)
            .into_iter()
            .map(|line| (false, line)),
    );
    lines
}

fn push_summary(r: &push::PushResult, target: &Path) -> String {
    match r.pushed {
        Some(pushed) => format!(
            "Pushed {} from {}",
            plural(pushed, "change", "changes"),
            target.display()
        ),
        None => format!(
            "{} to push, {} unchanged",
            plural(r.entries.len(), "change", "changes"),
            r.unchanged
        ),
    }
}

/// One half of `sync`'s data: `{ok, exit_code, result, lines}`, or `{ok,
/// exit_code, error}` when it failed.
fn sync_half_json<T: serde::Serialize>(
    half: &Result<T, cmd::Error>,
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
            "exit_code": error.exit_code(),
            "error": error_json(&CliError::from(error)),
        }),
    }
}

fn half_summary<T>(name: &str, half: Option<&Result<T, cmd::Error>>) -> String {
    match half {
        Some(Ok(_)) => format!("{name} ok"),
        Some(Err(error)) => format!("{name} failed ({})", error.code()),
        None => format!("{name} not run"),
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
            non_interactive,
        } => {
            let options = clone::CloneOptions {
                filename_as_title,
                account,
                non_interactive,
            };
            let _lock = lock_vault(&directory, wait)?;
            let s = clone::run_clone(&directory, progress, &mut on_status, &options)?;
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
        Command::Pull {
            directory,
            defer_renames,
        } => {
            let target = resolve_target_dir(directory)?;
            let _lock = lock_vault(&target, wait)?;
            let s = pull::run_pull(&target, progress, &mut on_status, &pull::PullOptions { defer_renames })?;
            let lines = pull_lines(&s, &target);
            let report = report_lines(&lines);
            Response::new(&s, pull_summary(&s))
                .human(human(mode, lines))
                .meta("lines", report)
                .count(s.changes.len())
        }
        Command::Push { directory, dry_run } => {
            let target = resolve_target_dir(directory)?;
            let _lock = if dry_run {
                None
            } else {
                Some(lock_vault(&target, wait)?)
            };
            let r = push::run_push(&target, &mut on_status, &push::PushOptions { dry_run })?;
            let lines = push_lines(&r, &target);
            let report = report_lines(&lines);
            Response::new(&r, push_summary(&r, &target))
                .human(human(mode, lines))
                .meta("lines", report)
                .count(r.entries.len())
                .ids(r.entries.iter().map(|e| e.entry.file.clone()).collect())
        }
        Command::Sync { directory } => {
            let target = resolve_target_dir(directory)?;
            let _lock = lock_vault(&target, wait)?;
            let outcome = sync::run_sync(&target, progress, &mut on_status);
            let data = serde_json::json!({
                "push": sync_half_json(&outcome.push, |r| push_lines(r, &target)),
                "pull": outcome.pull.as_ref().map(|p| sync_half_json(p, |s| pull_lines(s, &target))),
                "vault_info": vault_info::run_vault_info(&target).ok(),
            });
            let summary = format!(
                "{}, {}",
                half_summary("push", Some(&outcome.push)),
                half_summary("pull", outcome.pull.as_ref())
            );
            // Text mode: each half's report, or its error on stderr, as push
            // and pull alone would print them. A machine mode has it all in
            // the data (no separate error object).
            let mut lines = Vec::new();
            if mode == Mode::Human {
                match &outcome.push {
                    Ok(r) => lines.extend(push_lines(r, &target)),
                    Err(error) => {
                        CliError::from(error).report(mode);
                    }
                }
                match &outcome.pull {
                    Some(Ok(s)) => lines.extend(pull_lines(s, &target)),
                    Some(Err(error)) => {
                        CliError::from(error).report(mode);
                    }
                    // The push's error says why (no sign-in, or no network).
                    None => eprintln!("Not pulled."),
                }
            }
            let text = human(mode, lines);
            Response::new(data, summary).human(text).exit(outcome.exit_code())
        }
        Command::Status { directory } => {
            let target = resolve_target_dir(directory)?;
            let r = status::run_status(&target, &mut on_status)?;
            let mut lines = notice_lines(&r.notices);
            let options = RenderPlanOptions {
                preview: true,
                unchanged: Some(r.unchanged),
            };
            lines.extend(
                render_plan(&r.entries, &|file| display_path(&target, file), options)
                    .into_iter()
                    .map(|line| (false, line)),
            );
            let summary = format!(
                "{} to push, {} unchanged",
                plural(r.entries.len(), "change", "changes"),
                r.unchanged
            );
            let text = human(mode, lines);
            Response::new(&r, summary)
                .human(text)
                .count(r.entries.len())
                .ids(r.entries.iter().map(|e| e.file.clone()).collect())
        }
        Command::Restore { file, directory } => {
            let target = resolve_target_dir(directory)?;
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
            let r = history::run_history(&target, &file, &history::HistoryOptions { records })?;
            let (ids, summary): (Vec<String>, String) = match &r {
                history::HistoryResult::Records { records } => (
                    records.iter().map(|row| row.id.clone()).collect(),
                    plural(records.len(), "snapshot", "snapshots"),
                ),
                history::HistoryResult::Epochs { epochs } => (
                    epochs.iter().map(|e| e.id.clone()).collect(),
                    plural(epochs.len(), "epoch", "epochs"),
                ),
            };
            Response::new(&r, summary).human(history_text(&r)).ids(ids)
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
            let r = diff::run_diff(&target, &file, from, to, &mut on_status)?;
            let summary = if r.has_differences() {
                "Differences found"
            } else {
                "No differences"
            };
            let text = diff::render_diff_result(&r);
            Response::new(&r, summary).human(text.clone()).meta("text", text)
        }
    })
}

/// `history`'s human report.
fn history_text(result: &history::HistoryResult) -> String {
    let mut lines = Vec::new();
    match result {
        history::HistoryResult::Records { records } => {
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
        history::HistoryResult::Epochs { epochs } => {
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
