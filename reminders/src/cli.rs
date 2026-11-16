//! The command line: `icloud-reminders sync` syncs with iCloud and applies
//! the edits made to the Markdown files in `~/Documents/Reminders` at once
//! (the folder's code is [`folder`]; reading and changing reminders is done
//! in those files), `icloud-reminders background` is what the systemd timer
//! runs, and with no command it opens the app. Output follows the shared
//! product contract ([`icloud_session::output`]): text on a terminal, the
//! JSON envelope when piped, `--json`/`--styled`/`--quiet`/`--ids-only`/
//! `--count` to choose.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok; 1 error, or a `sync` whose folder step
//! reported a problem (with the data); 2 usage; 3 not signed in, or iCloud
//! still refusing the session (`session_rejected`, HTTP 401/421); 4 not
//! found; 5 offline, the keyring locked, an unreadable answer or an iCloud
//! server error (HTTP 5xx). The JSON `code` tells them apart. A failure in
//! a machine mode is `{"ok":false,"error":{"code","message","hint"?}}` on
//! stdout; in text mode `error:`/`hint:` lines on stderr.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use icloud_app::cli::{no_data_dir, other, session_failure, warn, warning};
use icloud_session::cli::{EXIT_ERROR, EXIT_NOT_FOUND};
use icloud_session::output::{self, CliError, Mode, OutputFlags, Response};
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::cloudkit::{self, SessionTransport};
use crate::due;
use crate::folder;
use crate::notify;
use crate::service::{self, Service};
use crate::store::Store;

const AFTER_HELP: &str = "\
Reminders are read and changed in ~/Documents/Reminders, one Markdown
checklist per list (Work.md): tick a box to complete, untick to reopen,
add a line to add, remove a line to delete (more than 3 lines that are
most of the list are refused and written back), indented lines under one
are its notes, and the text after 📅 is its due date (2026-10-10,
\"2026-10-10 17:30\", today, tomorrow, either followed by a time, a time
alone, or +30m, +2h, +3d). The file name is the list's name: a new file
makes a list (once unchanged for 10 seconds), renaming a file renames its
list, and removing one deletes its list and its reminders (once gone on two
runs at least 50 seconds apart; not when another list file went too, the
folder is empty, or the list is shared: the file is then written back).
Groups, sections, subtasks, alerts, priority and flags are made in the app.
The timer applies a changed file within a minute; sync applies it now and
reports what it did.

A record this version cannot read is skipped, named on stderr (and as
warning in sync's and background's data), and kept as last synced; the
rest syncs.

Output: human-readable text on a terminal, a JSON envelope
{\"ok\":true,\"data\",\"summary\",\"meta\"?} when piped. --json forces the
envelope, --styled text, --quiet prints only the data, --ids-only the ids
(background: the notified reminders'), --count the number of results
(changed records for sync; notified for background). Flags combine:
ids-only > count > quiet > json > styled. A failure is
{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (error, http 4xx), or sync's folder step reported a problem
    (the data says which: folder.restored, folder.warnings)
  2 usage
  3 sign_in_required (icloud-session sign-in), session_rejected (iCloud
    keeps refusing the session although icloud-sessiond still holds it:
    sign in again)
  4 not_found
  5 offline, keyring_unavailable (the keyring holding the session is
    locked: unlock it), bad_response (iCloud answered with something
    unreadable), http 5xx";

#[derive(Parser)]
#[command(
    name = "icloud-reminders",
    version,
    about = "iCloud Reminders from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    #[command(flatten)]
    out: OutputFlags,
    /// Keep the cache in DIR (default ~/.local/share/icloud-reminders).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sync with iCloud and apply the files' edits now.
    ///
    /// Fetches what changed in iCloud, then runs the folder step the timer
    /// runs: the edits made to the files in ~/Documents/Reminders are sent
    /// to iCloud and the files rewritten as iCloud has them. Exits 1, with
    /// the data, when that step reported a problem: a file put back because
    /// it no longer listed most of its list (folder.restored), or a warning
    /// (a list that failed, a conflict copy, a removed list file written
    /// back, a new file not made into a list or still settling).
    ///
    /// JSON: {lists, reminders, changed, full, warning?, folder: {added,
    /// edited, deleted, written, restored: [{list, file, kept}], warnings,
    /// lists_created: [name], lists_renamed: [{from, to}], lists_deleted:
    /// [{list, file, reminders}], lists_kept: [{list, file, why}]} | null}
    #[command(after_help = AFTER_HELP)]
    Sync {
        /// Fetch every reminder again, not only the changes.
        #[arg(long)]
        full: bool,
    },
    /// What the systemd timer runs every minute: sync when the last one is
    /// over 4 minutes old, keep ~/Documents/Reminders (the lists as Markdown
    /// checklists) and iCloud in step, then notify about reminders that
    /// just fell due.
    ///
    /// A changed file is sent at once. iCloud stays where the reminders are
    /// kept.
    ///
    /// A sync that cannot run (signed out, offline) is reported and the
    /// notifications still come from the cache; that exits 0.
    ///
    /// JSON: {synced, sync_error, notified: [{id, title}], folder: {added, edited, deleted, written, restored, warnings, lists_created, lists_renamed, lists_deleted, lists_kept} | null, warning?}
    #[command(after_help = AFTER_HELP)]
    Background {
        /// Sync even if the last sync is recent.
        #[arg(long)]
        sync: bool,
    },
}

/// A failure: the contract's error object and exit code.
type Failure = CliError;

type Outcome = Result<Response, Failure>;

/// A file of this computer's that cannot be read or written.
fn file(e: std::io::Error) -> Failure {
    other(e.to_string())
}

/// A Reminders failure as the contract's code, exit code, message and hint:
/// icloud-session's failures as every iCloud tool reports them.
fn cloud_error(e: cloudkit::Error) -> Failure {
    use cloudkit::Error as C;
    match e {
        C::Session(e) => session_failure(&e),
        C::Gone(_) => CliError::new("not_found", EXIT_NOT_FOUND, e.to_string()),
        C::CloudKit { .. } | C::Other(_) => other(e.to_string()),
    }
}

/// Runs the command line on the process arguments and returns the exit code.
pub fn run() -> u8 {
    let args = match output::parse::<Args>() {
        Ok(args) => args,
        Err(code) => return code,
    };
    let mode = args.out.mode();
    let transport = SessionTransport::default();
    let result = (|| {
        let dir = args.data_dir.or_else(Store::default_dir).ok_or_else(no_data_dir)?;
        let cli = Cli {
            mode,
            svc: Service::new(&transport, Store::new(dir)),
            local: due::local_zone().map_err(other)?,
        };
        cli.dispatch(args.command)
    })();
    match result {
        Ok(resp) => resp.print(mode),
        Err(e) => e.report(mode),
    }
}

struct Cli<'a> {
    mode: Mode,
    svc: Service<'a>,
    local: TimeZone,
}

impl Cli<'_> {
    fn dispatch(&self, command: Command) -> Outcome {
        match command {
            Command::Sync { full } => self.sync(full),
            Command::Background { sync } => self.background(sync),
        }
    }

    /// One folder run under its lock: the files' changes to iCloud and
    /// iCloud's state to the files, with the state saved whatever happened.
    /// `None` without a home directory to keep the folder in.
    fn folder_run(&self) -> Result<Option<folder::Report>, Failure> {
        let store = &self.svc.store;
        let _lock = store.folder_lock().map_err(file)?;
        let Some(dir) = folder::default_dir() else {
            return Ok(None);
        };
        let mut state = store.folder().map_err(file)?.unwrap_or_default();
        let now = folder::clock().to_zoned(self.local.clone());
        let result = folder::sync(&self.svc, &dir, &mut state, &now);
        store.save_folder(&state).map_err(file)?;
        Ok(Some(result.map_err(cloud_error)?))
    }

    /// The folder step as the timer runs it: a run that failed is a run
    /// with one warning, and each file put back is told in a notification
    /// (whoever emptied it may be at the phone).
    fn folder_step(&self) -> Option<folder::Report> {
        let report = self.folder_run().unwrap_or_else(|e| {
            Some(folder::Report {
                warnings: vec![e.message],
                ..folder::Report::default()
            })
        });
        for r in report.iter().flat_map(|r| &r.restored) {
            let body = format!(
                "It no longer listed {} reminders, most of the list, so they were put back and none was deleted. To delete that many, use the app.",
                r.kept
            );
            if let Err(e) = notify::send(&format!("Reminders: {} restored", r.file), &body) {
                warning(&format!("could not notify about {}: {e}", r.file));
            }
        }
        for d in report.iter().flat_map(|r| &r.lists_deleted) {
            let headline = format!("Deleted list {} and its {} reminders", d.list, d.reminders);
            let body = format!("Its file {} was removed from the folder.", d.file);
            if let Err(e) = notify::send(&headline, &body) {
                warning(&format!("could not notify about {}: {e}", d.file));
            }
        }
        for k in report.iter().flat_map(|r| &r.lists_kept) {
            let body = format!(
                "It was removed from the folder, but the list {} was not deleted: {}. The file was written back.",
                k.list, k.why
            );
            if let Err(e) = notify::send(&format!("Reminders: {} put back", k.file), &body) {
                warning(&format!("could not notify about {}: {e}", k.file));
            }
        }
        report
    }

    fn sync(&self, full: bool) -> Outcome {
        let report = self.svc.sync(full).map_err(cloud_error)?;
        let mut folder_report = self.folder_step();
        // A new file still settling: said here, where someone is waiting
        // for its list, and not by the timer, which makes it a minute later.
        for report in folder_report.iter_mut() {
            for name in std::mem::take(&mut report.waiting) {
                report.warnings.push(format!(
                    "{name}: not read yet, as it changed in the last {} seconds; its list is made once it has not (the timer does so within a minute, or sync again)",
                    folder::SETTLE_MS / 1000
                ));
            }
        }
        let mut out = json!({
            "lists": report.lists,
            "reminders": report.reminders,
            "changed": report.changed,
            "full": report.full,
            "folder": folder_report.as_ref().map_or(Value::Null, folder_json),
        });
        warn(&mut out, report.warning());
        let mut text = format!(
            "Synced: {} lists, {} reminders ({} changed{})",
            report.lists,
            report.reminders,
            report.changed,
            if report.full { ", full" } else { "" }
        );
        let mut exit = 0;
        if let Some(f) = &folder_report {
            text.push_str(&format!("; {}", folder_summary(f)));
            for r in &f.restored {
                warning(&format!(
                    "folder: {} no longer listed {} reminders, most of the list: none was deleted and the lines were written back (delete that many in the app, or a few lines at a time)",
                    r.file, r.kept
                ));
            }
            for w in &f.warnings {
                warning(&format!("folder: {w}"));
            }
            if !f.restored.is_empty() || !f.warnings.is_empty() {
                exit = EXIT_ERROR;
            }
        }
        Ok(Response::new(out, text).count(report.changed).exit(exit))
    }

    fn background(&self, force_sync: bool) -> Outcome {
        let store = &self.svc.store;
        let attempt = store.notified().map_err(file)?.sync_attempt_ms;
        let synced_ms = self.svc.cache().map_err(cloud_error)?.synced_ms;
        // The last sync, or the last attempt when that failed.
        let last = synced_ms.max(attempt);
        let stale = last.is_none_or(|ms| service::now_ms() - ms >= service::SYNC_EVERY.as_millis() as i64);
        // A changed file in the folder is sent now, not at the next sync,
        // unless the last sync failed: that waits its turn.
        let last_failed = attempt > synced_ms;
        let pending = match (store.folder().map_err(file)?, folder::default_dir()) {
            (Some(state), Some(dir)) => folder::pending(&state, &dir),
            _ => false,
        };
        let attempted = (force_sync || stale || (pending && !last_failed)).then(service::now_ms);
        let mut synced = false;
        let mut sync_error = None;
        let mut sync_warning = None;
        if attempted.is_some() {
            match self.svc.sync(false) {
                Ok(report) => {
                    synced = true;
                    sync_warning = report.warning();
                }
                Err(e) => {
                    warning(&format!("not synced: {e}"));
                    sync_error = Some(e.to_string());
                }
            }
        }
        // The folder, before the cache is locked for the notifications: its
        // writes lock it themselves. Changed files need the sync just made.
        let folder_report = if synced || !pending { self.folder_step() } else { None };
        let _lock = store.lock().map_err(file)?;
        let cache = store.cache().map_err(file)?;
        let previous = store.notified().map_err(file)?;
        let (fire, mut state) = notify::due_now(&cache, &previous, jiff::Timestamp::now(), &self.local);
        state.sync_attempt_ms = attempted.or(state.sync_attempt_ms);
        let mut notified = Vec::new();
        let mut ids = Vec::new();
        for r in fire {
            match notify::send(&r.title, &notify::text(r, &cache, &self.local)) {
                Ok(()) => {
                    notified.push(json!({ "id": bare(&r.id), "title": r.title }));
                    ids.push(r.uuid().to_owned());
                }
                Err(e) => {
                    warning(&format!("could not notify about \"{}\": {e}", r.title));
                    // Try again next minute.
                    state.fired.remove(&r.id);
                }
            }
        }
        store
            .save_notified(&state)
            .map_err(|e| other(format!("cannot save notified.json: {e}")))?;
        let count = notified.len();
        let mut out = json!({ "synced": synced, "sync_error": sync_error, "notified": notified });
        warn(&mut out, sync_warning);
        let mut text = format!("Notified about {count} reminder(s)");
        let mut quiet = count == 0;
        out["folder"] = folder_report.as_ref().map_or(Value::Null, folder_json);
        if let Some(report) = folder_report.as_ref().filter(|r| !r.quiet()) {
            for w in &report.warnings {
                warning(&format!("folder: {w}"));
            }
            text.push_str(&format!("; {}", folder_summary(report)));
            quiet = false;
        }
        // The timer runs this every minute in text mode: nothing to say,
        // nothing in the journal.
        if quiet && self.mode == Mode::Human {
            return Ok(Response::silent());
        }
        Ok(Response::new(out, text).ids(ids))
    }
}

/// A folder run as `{added, edited, deleted, written, restored, warnings}`.
fn folder_json(report: &folder::Report) -> Value {
    let restored: Vec<Value> = report
        .restored
        .iter()
        .map(|r| json!({ "list": r.list, "file": r.file, "kept": r.kept }))
        .collect();
    let renamed: Vec<Value> = report
        .lists_renamed
        .iter()
        .map(|(from, to)| json!({ "from": from, "to": to }))
        .collect();
    let deleted: Vec<Value> = report
        .lists_deleted
        .iter()
        .map(|d| json!({ "list": d.list, "file": d.file, "reminders": d.reminders }))
        .collect();
    let kept: Vec<Value> = report
        .lists_kept
        .iter()
        .map(|k| json!({ "list": k.list, "file": k.file, "why": k.why }))
        .collect();
    json!({
        "added": report.added,
        "edited": report.edited,
        "deleted": report.deleted,
        "written": report.written,
        "restored": restored,
        "warnings": report.warnings,
        "lists_created": report.lists_created,
        "lists_renamed": renamed,
        "lists_deleted": deleted,
        "lists_kept": kept,
    })
}

fn folder_summary(report: &folder::Report) -> String {
    let mut text = format!(
        "Folder: {} added, {} edited, {} deleted in iCloud; {} file(s) written",
        report.added,
        report.edited,
        report.deleted,
        report.written.len()
    );
    let (made, renamed, deleted) = (
        report.lists_created.len(),
        report.lists_renamed.len(),
        report.lists_deleted.len(),
    );
    if made + renamed + deleted > 0 {
        text.push_str(&format!("; lists: {made} made, {renamed} renamed, {deleted} deleted"));
    }
    text
}

/// `List/ABC` and `Reminder/ABC` as `ABC`.
fn bare(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, rest)| rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn failures_use_the_shared_codes_exits_and_hints() {
        use icloud_session::Error as S;
        let code = |e: cloudkit::Error| {
            let f = cloud_error(e);
            (f.code, f.exit)
        };
        // icloud-session's failures read as in every iCloud tool.
        assert_eq!(code(S::Network("x".into()).into()), ("network".into(), 5));
        let rejected = cloud_error(cloudkit::Error::from(S::Http {
            status: 421,
            body: String::new(),
        }));
        assert_eq!((rejected.code.as_str(), rejected.exit), ("session_rejected", 3));
        assert_eq!(rejected.hint, Some(icloud_app::cli::sign_in_hint()));
        // Reminders' own.
        let refused = cloudkit::Error::CloudKit {
            code: "x".into(),
            reason: "y".into(),
        };
        assert_eq!(code(refused), ("error".into(), 1));
        assert_eq!(code(cloudkit::Error::Gone("x".into())), ("not_found".into(), 4));
        assert_eq!(code(cloudkit::Error::Other("x".into())), ("error".into(), 1));
    }
}
