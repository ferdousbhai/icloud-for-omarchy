//! The `icloud-notes-sync` CLI. Ports icloud-md `src/cli.ts` for the verbs
//! kept by the port: clone, pull, push, status, restore, history, diff.
//!
//! clone, pull, push and restore take the vault lock the Notes app holds
//! (`cmd::lock`); status, history, diff, push --dry-run and vault-info only
//! read, and don't.
//!
//! Exit codes: 0 ok, 1 known error, 2 sign-in required, 3 `status`/`push
//! --dry-run` has entries or `diff` found differences, 64 usage, 70 internal
//! (the table every iCloud tool shares; docs/CLI.md). `--json` is global
//! (before or after the verb): stdout carries only the JSON result,
//! everything else goes to stderr, an error as one JSON line last.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use icloud_notes_sync::cmd::errors::{EXIT_HAS_ENTRIES, EXIT_OK};
use icloud_notes_sync::cmd::lock::lock_vault;
use icloud_notes_sync::cmd::output::{OutputContext, TOOL};
use icloud_notes_sync::cmd::plan::{RenderPlanOptions, render_plan};
use icloud_notes_sync::cmd::{
    self, NoProgress, NoticeLevel, SyncNotice, SyncProgress, clone, diff, history, pull, push, restore, status,
    vault_info,
};
use icloud_notes_sync::vault::local::{display_path, find_vault_root};

#[derive(Parser)]
#[command(
    name = "icloud-notes-sync",
    about = "Sync iCloud Notes with a folder of Markdown files",
    after_help = "Exit codes: 0 ok, 1 error, 2 sign-in required (icloud-session sign-in), 3 status/push --dry-run \
                  has entries or diff found differences, 64 usage, 70 internal error.\n\
                  With --json, stdout is only the JSON result and an error is one JSON line on stderr: \
                  {\"error\":{\"code\",\"message\",\"exit_code\",\"hint\"}}.\n\
                  clone, pull, push and restore take the vault's lock, shared with the Notes app (icloud-notes); \
                  a busy lock is the error vault_busy (exit 1).",
    disable_version_flag = true
)]
struct Cli {
    /// Emit machine-readable JSON on stdout instead of human-readable text
    #[arg(long, global = true)]
    json: bool,

    /// Print the version
    #[arg(short = 'V', long)]
    version: bool,

    /// How long clone/pull/push/restore wait for the vault's lock: default 30 when another run or a
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
    /// What the Notes app reads from the vault's state: title mode, default folder, tracked notes (JSON only)
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

/// `--json` progress: one `icloud-md:progress:...` line per event on stderr
/// (the prefix is what wrapping processes match).
struct MachineProgress {
    processed: usize,
    total: usize,
}

impl SyncProgress for MachineProgress {
    fn on_fetch_page(&mut self, records_so_far: usize) {
        eprintln!("icloud-md:progress:fetch:{records_so_far}");
    }
    fn on_process_start(&mut self, total_records: usize) {
        self.total = total_records;
        eprintln!("icloud-md:progress:process-start:{total_records}");
    }
    fn on_record_processed(&mut self) {
        self.processed += 1;
        eprintln!("icloud-md:progress:process:{}/{}", self.processed, self.total);
    }
    fn on_process_complete(&mut self) {
        eprintln!("icloud-md:progress:process-done");
    }
}

fn main() -> ExitCode {
    let cli = match icloud_session::cli::parse::<Cli>(TOOL) {
        Ok(cli) => cli,
        Err(code) => return ExitCode::from(code),
    };
    let ctx = OutputContext { json: cli.json };

    if cli.version {
        let version = env!("CARGO_PKG_VERSION");
        if ctx.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "version": version })).unwrap()
            );
        } else {
            println!("{version}");
        }
        return ExitCode::from(EXIT_OK);
    }
    let Some(command) = cli.command else {
        use clap::CommandFactory;
        eprintln!("{}", Cli::command().render_help());
        return ExitCode::from(ctx.emit_usage_error("no command given"));
    };

    let wait = cli.wait.map(std::time::Duration::from_secs);
    match run(command, ctx, wait) {
        Ok(code) => ExitCode::from(code),
        Err(error) => ExitCode::from(ctx.emit_error(&error)),
    }
}

/// An explicit directory wins; otherwise walk up from the cwd to the vault,
/// falling back to "." so a not-cloned error names where the user stood.
fn resolve_target_dir(directory: Option<PathBuf>) -> Result<PathBuf, cmd::Error> {
    if let Some(dir) = directory {
        return Ok(dir);
    }
    Ok(find_vault_root(Path::new("."))?.unwrap_or_else(|| PathBuf::from(".")))
}

fn print_notices(notices: &[SyncNotice]) {
    for notice in notices {
        match notice.level {
            NoticeLevel::Warn => eprintln!("{}", notice.message),
            NoticeLevel::Info => println!("{}", notice.message),
        }
    }
}

fn run(command: Command, ctx: OutputContext, wait: Option<std::time::Duration>) -> Result<u8, cmd::Error> {
    let mut on_status = |message: &str| ctx.status(message);
    let mut machine = MachineProgress { processed: 0, total: 0 };
    let mut quiet = NoProgress;
    let progress: &mut dyn SyncProgress = if ctx.json { &mut machine } else { &mut quiet };

    match command {
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
            let summary = clone::run_clone(&directory, progress, &mut on_status, &options)?;
            ctx.emit_result(&summary, |s| {
                println!(
                    "Cloned {} notes (plus {} shared with you) into {}, {} attachment(s) downloaded",
                    s.written,
                    s.written_shared,
                    directory.display(),
                    s.attachments_downloaded
                );
                if s.written_unpublishable > 0 {
                    println!(
                        "{} note(s) written with content this tool couldn't fully parse - read-only",
                        s.written_unpublishable
                    );
                }
                println!(
                    "Skipped: {} deleted, {} undecodable",
                    s.skipped_deleted, s.skipped_undecodable
                );
                print_notices(&s.notices);
            });
            Ok(EXIT_OK)
        }
        Command::Pull {
            directory,
            defer_renames,
        } => {
            let target = resolve_target_dir(directory)?;
            let _lock = lock_vault(&target, wait)?;
            let summary = pull::run_pull(&target, progress, &mut on_status, &pull::PullOptions { defer_renames })?;
            ctx.emit_result(&summary, |s| {
                for line in pull::render_pull_report(s, &|file| display_path(&target, file)) {
                    println!("{line}");
                }
                if s.skipped_new_unsyncable > 0 || s.dropped_unsyncable > 0 {
                    println!(
                        "{} new unsyncable note(s) skipped, {} note(s) dropped from tracking (no longer syncable)",
                        s.skipped_new_unsyncable, s.dropped_unsyncable
                    );
                }
                if s.unshared_untracked > 0 {
                    println!(
                        "{} shared note(s) no longer shared with you - local copies left in place, untracked",
                        s.unshared_untracked
                    );
                }
                print_notices(&s.notices);
            });
            Ok(EXIT_OK)
        }
        Command::Push { directory, dry_run } => {
            let target = resolve_target_dir(directory)?;
            let _lock = if dry_run {
                None
            } else {
                Some(lock_vault(&target, wait)?)
            };
            let result = push::run_push(&target, &mut on_status, &push::PushOptions { dry_run })?;
            ctx.emit_result(&result, |r| {
                print_notices(&r.notices);
                for entry in &r.entries {
                    if let Some(outcome) = &entry.outcome {
                        println!("{}", outcome.message);
                    }
                }
                if let Some(pushed) = r.pushed {
                    println!("Pushed {pushed} note(s) from {}", target.display());
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
                for line in render_plan(&entries, &|file| display_path(&target, file), options) {
                    println!("{line}");
                }
            });
            Ok(if result.dry_run && !result.entries.is_empty() {
                EXIT_HAS_ENTRIES
            } else {
                EXIT_OK
            })
        }
        Command::Status { directory } => {
            let target = resolve_target_dir(directory)?;
            let result = status::run_status(&target, &mut on_status)?;
            ctx.emit_result(&result, |r| {
                print_notices(&r.notices);
                let options = RenderPlanOptions {
                    preview: true,
                    unchanged: Some(r.unchanged),
                };
                for line in render_plan(&r.entries, &|file| display_path(&target, file), options) {
                    println!("{line}");
                }
            });
            Ok(if result.entries.is_empty() {
                EXIT_OK
            } else {
                EXIT_HAS_ENTRIES
            })
        }
        Command::Restore { file, directory } => {
            let target = resolve_target_dir(directory)?;
            let _lock = lock_vault(&target, wait)?;
            let result = restore::run_restore(&target, &file)?;
            ctx.emit_result(&result, |r| {
                println!("Restored {} to match the last synced copy.", r.file)
            });
            Ok(EXIT_OK)
        }
        Command::VaultInfo { directory } => {
            let target = resolve_target_dir(directory)?;
            let info = vault_info::run_vault_info(&target)?;
            ctx.emit_result(&info, |i| println!("{}", serde_json::to_string_pretty(i).unwrap()));
            Ok(EXIT_OK)
        }
        Command::History {
            file,
            directory,
            records,
        } => {
            let target = resolve_target_dir(directory)?;
            let result = history::run_history(&target, &file, &history::HistoryOptions { records })?;
            ctx.emit_result(&result, print_history);
            Ok(EXIT_OK)
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
                    let message = format!("Invalid ref \"{reference}\" - expected a snapshot id or <from>..<to>.");
                    return Ok(ctx.emit_usage_error(&message));
                }
            };
            let target = resolve_target_dir(directory)?;
            let result = diff::run_diff(&target, &file, from, to, &mut on_status)?;
            ctx.emit_result(&result, |r| println!("{}", diff::render_diff_result(r)));
            Ok(if result.has_differences() {
                EXIT_HAS_ENTRIES
            } else {
                EXIT_OK
            })
        }
    }
}

fn print_history(result: &history::HistoryResult) {
    match result {
        history::HistoryResult::Records { records } => {
            if records.is_empty() {
                println!("No version history recorded yet.");
            }
            for row in records {
                println!(
                    "{}  {}  {}  (changeTag {})",
                    row.id, row.timestamp, row.label, row.record_change_tag
                );
            }
        }
        history::HistoryResult::Epochs { epochs } => {
            if epochs.is_empty() {
                println!("No version history recorded yet.");
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
                println!("{line}");
            }
        }
    }
}
