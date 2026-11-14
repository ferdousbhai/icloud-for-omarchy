//! The command line: `icloud-reminders <command>` reads and changes iCloud
//! Reminders without GTK, through the same [`Service`] the app uses, and
//! `icloud-reminders background` is what the systemd timer runs. Output
//! follows the shared product contract ([`icloud_session::output`]): text on
//! a terminal, the JSON envelope when piped,
//! `--json`/`--styled`/`--quiet`/`--ids-only`/`--count` to choose.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok; 1 error; 2 usage, an ambiguous name, or
//! `delete` without `--yes` when nobody can be asked; 3 not signed in, or
//! iCloud still refusing the session (`session_rejected`, HTTP 401/421);
//! 4 not found; 5 offline, the keyring locked, an unreadable answer or an
//! iCloud server error (HTTP 5xx). The JSON `code` tells them apart. A
//! failure in a machine mode is `{"ok":false,"error":{"code","message","hint"?}}`
//! on stdout; in text mode `error:`/`hint:` lines on stderr.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_NOT_FOUND, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{
    self, CliError, KEYRING_HINT, Mode, NETWORK_HINT, OutputFlags, Response, SERVICE_TROUBLE_HINT,
    SESSION_SERVICE_HINT, SIGN_IN_HINT,
};
use icloud_session::time::rfc3339;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::cloudkit::{self, SessionTransport};
use crate::due::{self, Due};
use crate::folder;
use crate::model::{Change, List, Reminder};
use crate::notify;
use crate::service::{self, Service};
use crate::store::{Cache, Store};

const AFTER_HELP: &str = "\
REMINDER is a reminder's ID, its exact title (ignoring case), or a unique
part of it; open reminders are matched before completed ones. LIST is a
list's ID or name, or a unique part of the name.

WHEN is 2026-10-10 (all day), \"2026-10-10 17:30\", today, tomorrow, either
followed by a time (\"tomorrow 9:00\"), a time alone (17:30, today), or
+30m, +2h, +3d.

Reading commands sync first and fail with offline without a network;
--cached reads what the last sync left instead. A record this version
cannot read is skipped, named on stderr (and as warning in sync's and
background's data), and kept as last synced; the rest syncs.

Output: human-readable text on a terminal, a JSON envelope
{\"ok\":true,\"data\",\"summary\",\"meta\"?} when piped. --json forces the
envelope, --styled text, --quiet prints only the data, --ids-only the ids
(lists: the lists'; list, show, add, edit, complete, uncomplete, delete:
the reminders'; background: the notified reminders'), --count the number of
results (lists; reminders; changed records for sync; notified for
background; reminders changed in iCloud for folder start and sync). Flags combine: ids-only > count > quiet > json > styled. A
failure is {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout,
or error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (cancelled, error, http 4xx)
  2 usage, ambiguous, or delete without --yes when nobody can be asked
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
    /// Read the cache without syncing first.
    #[arg(long, global = true)]
    cached: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The lists, with how many open reminders each holds.
    ///
    /// JSON: [{id, name, color, open}]
    #[command(after_help = AFTER_HELP)]
    Lists,
    /// Open reminders, soonest due first (undated last).
    ///
    /// Every list's, or one list's. --completed lists only completed ones,
    /// --all both.
    ///
    /// JSON: [{id, list: {id, name}, title, notes, completed, completed_at,
    /// due: {date, time, all_day, time_zone, at} | null, flagged, priority,
    /// alerts}]
    #[command(after_help = AFTER_HELP)]
    List {
        #[arg(value_name = "LIST")]
        list: Option<String>,
        /// Only completed reminders.
        #[arg(long, conflicts_with = "all")]
        completed: bool,
        /// Open and completed reminders.
        #[arg(long)]
        all: bool,
    },
    /// One reminder, with its notes.
    ///
    /// JSON: the reminder, as in `list`.
    #[command(after_help = AFTER_HELP)]
    Show {
        #[arg(value_name = "REMINDER")]
        reminder: String,
    },
    /// Add a reminder.
    ///
    /// To --list, which is required unless the account has only one list:
    /// iCloud records no default list.
    ///
    /// JSON: {action: "add", reminder}
    #[command(after_help = AFTER_HELP)]
    Add {
        title: String,
        #[arg(long, value_name = "LIST")]
        list: Option<String>,
        #[arg(long, value_name = "TEXT")]
        notes: Option<String>,
        #[arg(long, value_name = "WHEN")]
        due: Option<String>,
    },
    /// Change a reminder's title, notes or due date.
    ///
    /// Alerts set on an Apple device keep their own time when the due date
    /// changes (`alerts` counts them); change those on the device.
    ///
    /// JSON: {action: "edit", reminder}
    #[command(after_help = AFTER_HELP)]
    Edit {
        #[arg(value_name = "REMINDER")]
        reminder: String,
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        #[arg(long, value_name = "TEXT")]
        notes: Option<String>,
        #[arg(long, value_name = "WHEN", conflicts_with = "no_due")]
        due: Option<String>,
        /// Remove the due date.
        #[arg(long)]
        no_due: bool,
    },
    /// Mark a reminder completed.
    ///
    /// JSON: {action: "complete", changed, reminder}; changed is false when it already was
    #[command(after_help = AFTER_HELP)]
    Complete {
        #[arg(value_name = "REMINDER")]
        reminder: String,
    },
    /// Mark a completed reminder open again.
    ///
    /// JSON: {action: "uncomplete", changed, reminder}; changed is false when it already was
    #[command(after_help = AFTER_HELP)]
    Uncomplete {
        #[arg(value_name = "REMINDER")]
        reminder: String,
    },
    /// Delete a reminder (to Recently Deleted on your Apple devices).
    ///
    /// Asks in text mode on a terminal (stdin and stdout); anywhere else
    /// --yes is required (a usage error, exit 2, otherwise).
    ///
    /// JSON: {action: "delete", reminder: {id, title}}
    #[command(after_help = AFTER_HELP)]
    Delete {
        #[arg(value_name = "REMINDER")]
        reminder: String,
        /// Do not ask; required when nobody can be asked (not a terminal, or a machine output mode).
        #[arg(long, short)]
        yes: bool,
    },
    /// Fetch what changed in iCloud.
    ///
    /// JSON: {lists, reminders, changed, full, warning?}
    #[command(after_help = AFTER_HELP)]
    Sync {
        /// Fetch every reminder again, not only the changes.
        #[arg(long)]
        full: bool,
    },
    /// What the systemd timer runs every minute: sync when the last one is
    /// over 4 minutes old, then notify about reminders that just fell due.
    ///
    /// A sync that cannot run (signed out, offline) is reported and the
    /// notifications still come from the cache; that exits 0.
    ///
    /// JSON: {synced, sync_error, notified: [{id, title}], warning?}
    #[command(after_help = AFTER_HELP)]
    Background {
        /// Sync even if the last sync is recent.
        #[arg(long)]
        sync: bool,
    },
    /// The reminders as Markdown checklists in a folder, one file per list,
    /// kept in step with iCloud both ways by the background timer.
    ///
    /// For what only carries files: Syncthing to a phone, an editor. Tick a
    /// box to complete, add a line to add, remove a line to delete; the
    /// text after 📅 is a WHEN. iCloud stays where the reminders are kept.
    #[command(after_help = AFTER_HELP)]
    Folder {
        #[command(subcommand)]
        action: FolderCommand,
    },
}

#[derive(Subcommand)]
enum FolderCommand {
    /// Turn the folder on and write the files (default ~/Documents/Reminders).
    ///
    /// Files already there are kept: lines iCloud lacks are added to it.
    ///
    /// JSON: {on, dir, files: [{list, file}], added, edited, deleted, written, held, warnings}
    Start {
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Turn the folder off. The files stay as they are, no longer synced.
    ///
    /// JSON: {on, dir}
    Stop,
    /// Whether the folder is on, where, and which files are held.
    ///
    /// JSON: {on, dir, files: [{list, file, held}]}
    Status,
    /// Sync with iCloud, then bring the files and iCloud in step now.
    ///
    /// A file that would delete most of its list is held, not applied
    /// (exit 2), until this runs with --yes.
    ///
    /// JSON: {on, dir, files: [{list, file}], added, edited, deleted, written, held: [{list, file, removing}], warnings}
    Sync {
        /// Apply held files too: delete what they no longer list.
        #[arg(long, short)]
        yes: bool,
    },
}

/// A failure: the contract's error object and exit code.
type Failure = CliError;

type Outcome = Result<Response, Failure>;

fn usage(msg: impl Into<String>) -> Failure {
    CliError::new("usage", EXIT_USAGE, msg)
}

fn other(msg: impl Into<String>) -> Failure {
    CliError::new("error", EXIT_ERROR, msg)
}

/// The shared sign-in hint, plus the app's own button.
fn sign_in_hint() -> String {
    format!("{SIGN_IN_HINT}, or Sign In in the app")
}

/// A CloudKit failure as the contract's code, exit code, message and hint:
/// the shared wording and mapping of `icloud_session::output`.
fn cloud_error(e: cloudkit::Error) -> Failure {
    use cloudkit::Error as C;
    let msg = e.to_string();
    match e {
        C::SignInRequired => output::sign_in_required().hint(sign_in_hint()),
        C::Offline(_) => CliError::new("offline", EXIT_UNAVAILABLE, msg).hint(NETWORK_HINT),
        C::KeyringUnavailable(_) => CliError::new("keyring_unavailable", EXIT_UNAVAILABLE, msg).hint(KEYRING_HINT),
        C::BadResponse(_) => CliError::new("bad_response", EXIT_UNAVAILABLE, msg).hint(SERVICE_TROUBLE_HINT),
        C::Service(_) => CliError::new("session_service", EXIT_UNAVAILABLE, msg).hint(SESSION_SERVICE_HINT),
        C::Http { status, .. } => {
            let e = output::http_failure(status, msg);
            if e.hint.as_deref() == Some(SIGN_IN_HINT) {
                e.hint(sign_in_hint())
            } else {
                e
            }
        }
        C::CloudKit { .. } | C::Other(_) => other(msg),
    }
}

/// A problem that did not stop the command, on stderr.
fn warning(msg: &str) {
    eprintln!("{}", output::terminal_safe(&format!("warning: {msg}")));
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
        let dir = args
            .data_dir
            .or_else(Store::default_dir)
            .ok_or_else(|| other("no data directory (no $XDG_DATA_HOME or $HOME)").hint("pass --data-dir DIR"))?;
        let cli = Cli {
            mode,
            cached: args.cached,
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
    cached: bool,
    svc: Service<'a>,
    local: TimeZone,
}

impl Cli<'_> {
    fn dispatch(&self, command: Command) -> Outcome {
        match command {
            Command::Lists => self.lists(),
            Command::List { list, completed, all } => self.list(list.as_deref(), completed, all),
            Command::Show { reminder } => self.show(&reminder),
            Command::Add {
                title,
                list,
                notes,
                due,
            } => self.add(&title, list.as_deref(), notes, due.as_deref()),
            Command::Edit {
                reminder,
                title,
                notes,
                due,
                no_due,
            } => self.edit(&reminder, title, notes, due.as_deref(), no_due),
            Command::Complete { reminder } => self.set_completed(&reminder, true),
            Command::Uncomplete { reminder } => self.set_completed(&reminder, false),
            Command::Delete { reminder, yes } => self.delete(&reminder, yes),
            Command::Sync { full } => self.sync(full),
            Command::Background { sync } => self.background(sync),
            Command::Folder { action } => match action {
                FolderCommand::Start { dir } => self.folder_start(dir),
                FolderCommand::Stop => self.folder_stop(),
                FolderCommand::Status => self.folder_status(),
                FolderCommand::Sync { yes } => self.folder_sync(yes),
            },
        }
    }

    /// One folder run under its lock: the files' changes to iCloud and
    /// iCloud's state to the files, with the state saved whatever happened.
    /// `start` gives the folder to turn on when it is off.
    fn folder_run(
        &self,
        start: Option<PathBuf>,
        force: bool,
    ) -> Result<Option<(folder::State, folder::Report)>, Failure> {
        let store = &self.svc.store;
        let file = |e: std::io::Error| other(e.to_string());
        let _lock = store.folder_lock().map_err(file)?;
        let mut state = match (store.folder().map_err(file)?, start) {
            (Some(on), Some(dir)) if on.dir != dir => {
                return Err(usage(format!("the folder is already on, in {}", on.dir.display()))
                    .hint("run `icloud-reminders folder stop` first to move it"));
            }
            (Some(on), _) => on,
            (None, Some(dir)) => folder::State {
                dir,
                ..folder::State::default()
            },
            (None, None) => return Ok(None),
        };
        let now = jiff::Timestamp::now().to_zoned(self.local.clone());
        let result = folder::sync(&self.svc, &mut state, &now, force);
        store.save_folder(&state).map_err(file)?;
        Ok(Some((state, result.map_err(cloud_error)?)))
    }

    fn folder_start(&self, dir: Option<PathBuf>) -> Outcome {
        let dir = match dir {
            Some(d) if d.is_absolute() => d,
            Some(d) => std::env::current_dir().map_err(|e| other(e.to_string()))?.join(d),
            None => std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Documents/Reminders"))
                .ok_or_else(|| usage("no $HOME to put the folder in").hint("name a directory: folder start DIR"))?,
        };
        self.fresh()?;
        let (state, report) = self.folder_run(Some(dir), false)?.expect("started above");
        let text = format!(
            "Reminders folder on: {} ({} lists). The background timer keeps it in step.",
            state.dir.display(),
            state.lists.len()
        );
        Ok(folder_response(&state, &report, text))
    }

    fn folder_stop(&self) -> Outcome {
        let store = &self.svc.store;
        let file = |e: std::io::Error| other(e.to_string());
        let _lock = store.folder_lock().map_err(file)?;
        let was = store.folder().map_err(file)?;
        store.remove_folder().map_err(file)?;
        let dir = was.map(|s| s.dir);
        let text = match &dir {
            Some(d) => format!(
                "Reminders folder off. The files in {} stay as they are, no longer synced.",
                d.display()
            ),
            None => "The reminders folder was not on.".to_owned(),
        };
        Ok(Response::new(json!({ "on": false, "dir": dir }), text))
    }

    fn folder_status(&self) -> Outcome {
        let Some(state) = self.svc.store.folder().map_err(|e| other(e.to_string()))? else {
            return Ok(Response::new(
                json!({ "on": false, "dir": null, "files": [] }),
                "The reminders folder is off. Turn it on with: icloud-reminders folder start",
            ));
        };
        let cache = self.cache()?;
        let files: Vec<Value> = state
            .lists
            .iter()
            .map(|(id, ls)| json!({ "list": cache.list(id).map(|l| l.name.as_str()), "file": ls.file, "held": ls.held.is_some() }))
            .collect();
        let held = state.lists.values().filter(|ls| ls.held.is_some()).count();
        let mut text = format!("Reminders folder on: {} ({} files)", state.dir.display(), files.len());
        if held > 0 {
            text.push_str(&format!("; {held} held, see: icloud-reminders folder sync"));
        }
        Ok(Response::new(json!({ "on": true, "dir": state.dir, "files": files }), text).count(files.len()))
    }

    fn folder_sync(&self, yes: bool) -> Outcome {
        if self.svc.store.folder().map_err(|e| other(e.to_string()))?.is_none() {
            return Err(
                CliError::new("not_set_up", cli::EXIT_SIGN_IN, "the reminders folder is off")
                    .hint("turn it on with: icloud-reminders folder start"),
            );
        }
        self.fresh()?;
        let (state, report) = self.folder_run(None, yes)?.expect("on, checked above");
        let mut text = folder_summary(&report);
        let held = !report.held.is_empty();
        for h in &report.held {
            text.push_str(&format!(
                "\nHeld: {} would delete {} of the list's reminders. To do that: icloud-reminders folder sync --yes",
                h.file, h.removing
            ));
        }
        let resp = folder_response(&state, &report, text);
        Ok(if held { resp.exit(EXIT_USAGE) } else { resp })
    }

    /// The cache after a sync, or as the last sync left it with `--cached`.
    fn fresh(&self) -> Result<Cache, Failure> {
        if !self.cached
            && let Some(w) = self.svc.sync(false).map_err(cloud_error)?.warning()
        {
            warning(&w);
        }
        self.cache()
    }

    fn cache(&self) -> Result<Cache, Failure> {
        self.svc.cache().map_err(cloud_error)
    }

    fn lists(&self) -> Outcome {
        let cache = self.fresh()?;
        let open = open_counts(&cache);
        let count = |l: &List| open.get(l.id.as_str()).copied().unwrap_or(0);
        let ids: Vec<String> = cache.lists.iter().map(|l| bare(&l.id).to_owned()).collect();
        let human = if cache.lists.is_empty() {
            "No lists.".to_owned()
        } else {
            cache
                .lists
                .iter()
                .map(|l| format!("{}  ({} open)", l.name, count(l)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let summary = match cache.lists.len() {
            1 => "1 list".to_owned(),
            n => format!("{n} lists"),
        };
        Ok(Response::new(lists_json(&cache, &open), summary).human(human).ids(ids))
    }

    fn list(&self, list: Option<&str>, completed: bool, all: bool) -> Outcome {
        let cache = self.fresh()?;
        let list = list.map(|q| resolve_list(&cache, q)).transpose()?;
        let rows = cache.sorted(&self.local, |r| {
            list.is_none_or(|l| r.list_id == l.id) && (all || r.completed == completed)
        });
        let data: Vec<Value> = rows.iter().map(|r| reminder_json(r, &cache, &self.local)).collect();
        let ids: Vec<String> = rows.iter().map(|r| r.uuid().to_owned()).collect();
        let human = if rows.is_empty() {
            "No reminders.".to_owned()
        } else {
            rows.iter()
                .map(|r| line(r, &cache, &self.local, list.is_none()))
                .collect::<String>()
                .trim_end()
                .to_owned()
        };
        let summary = match rows.len() {
            1 => "1 reminder".to_owned(),
            n => format!("{n} reminders"),
        };
        Ok(Response::new(data, summary).human(human).ids(ids))
    }

    fn show(&self, query: &str) -> Outcome {
        let cache = self.fresh()?;
        let r = resolve(&cache, query)?;
        Ok(Response::new(reminder_json(r, &cache, &self.local), r.title.clone())
            .human(detail(r, &cache, &self.local).trim_end().to_owned())
            .ids(vec![r.uuid().to_owned()]))
    }

    fn add(&self, title: &str, list: Option<&str>, notes: Option<String>, due: Option<&str>) -> Outcome {
        if title.trim().is_empty() {
            return Err(usage("add needs a title"));
        }
        let due = due.map(|w| self.when(w)).transpose()?;
        let cache = self.fresh()?;
        let list = match list {
            Some(q) => resolve_list(&cache, q)?,
            None => cache.only_list().ok_or_else(|| {
                let names: String = cache.lists.iter().map(|l| format!("\n  {}", l.name)).collect();
                usage("add needs --list LIST (iCloud records no default list)")
                    .hint(format!("name one of these lists:{names}"))
            })?,
        };
        let saved = self
            .svc
            .add(&list.id, title.trim(), notes.as_deref().unwrap_or(""), due.as_ref())
            .map_err(cloud_error)?;
        let r = saved.value;
        self.done(
            "add",
            &r,
            &format!("Added \"{}\" to {}", r.title, list.name),
            saved.warning,
        )
    }

    fn when(&self, w: &str) -> Result<Due, Failure> {
        due::parse_when(w, &jiff::Zoned::now().with_time_zone(self.local.clone())).map_err(usage)
    }

    fn edit(
        &self,
        query: &str,
        title: Option<String>,
        notes: Option<String>,
        due: Option<&str>,
        no_due: bool,
    ) -> Outcome {
        let mut changes = Vec::new();
        if let Some(t) = title {
            if t.trim().is_empty() {
                return Err(usage("--title cannot be empty"));
            }
            changes.push(Change::Title(t.trim().to_owned()));
        }
        if let Some(n) = notes {
            changes.push(Change::Notes(n));
        }
        if let Some(w) = due {
            changes.push(Change::Due(Some(self.when(w)?)));
        }
        if no_due {
            changes.push(Change::Due(None));
        }
        if changes.is_empty() {
            return Err(usage("edit needs --title, --notes, --due or --no-due"));
        }
        let cache = self.fresh()?;
        let r = resolve(&cache, query)?.clone();
        if r.alarms > 0 && changes.iter().any(|c| matches!(c, Change::Due(_))) {
            warning(&format!(
                "\"{}\" has {} alert(s) set on an Apple device; they keep their own time",
                r.title, r.alarms
            ));
        }
        let saved = self.svc.update(&r, &changes).map_err(cloud_error)?;
        let after = saved.value.ok_or_else(|| gone(&r))?;
        self.done("edit", &after, &format!("Changed \"{}\"", after.title), saved.warning)
    }

    fn set_completed(&self, query: &str, completed: bool) -> Outcome {
        let cache = self.fresh()?;
        // complete prefers an open match, uncomplete a completed one.
        let r = resolve_prefer(&cache, query, completed)?.clone();
        let action = if completed { "complete" } else { "uncomplete" };
        let changed = r.completed != completed;
        let (after, warning) = if changed {
            let saved = self
                .svc
                .update(&r, &[Change::Completed(completed)])
                .map_err(cloud_error)?;
            (saved.value.ok_or_else(|| gone(&r))?, saved.warning)
        } else {
            (r, None)
        };
        let text = match (completed, changed) {
            (true, true) => format!("Completed \"{}\"", after.title),
            (false, true) => format!("Reopened \"{}\"", after.title),
            (true, false) => format!("\"{}\" was already completed", after.title),
            (false, false) => format!("\"{}\" was already open", after.title),
        };
        let cache = self.cache()?;
        let mut out =
            json!({ "action": action, "changed": changed, "reminder": reminder_json(&after, &cache, &self.local) });
        warn(&mut out, warning);
        Ok(Response::new(out, text).ids(vec![after.uuid().to_owned()]))
    }

    fn delete(&self, query: &str, yes: bool) -> Outcome {
        let cache = self.fresh()?;
        let r = resolve(&cache, query)?.clone();
        confirm(self.mode, "delete", yes, &format!("Delete \"{}\"?", r.title))?;
        let saved = self.svc.update(&r, &[Change::Deleted]).map_err(cloud_error)?;
        let mut out = json!({ "action": "delete", "reminder": { "id": bare(&r.id), "title": r.title } });
        warn(&mut out, saved.warning);
        Ok(Response::new(out, format!("Deleted \"{}\"", r.title)).ids(vec![r.uuid().to_owned()]))
    }

    fn done(&self, action: &str, r: &Reminder, text: &str, warning: Option<String>) -> Outcome {
        let cache = self.cache()?;
        let mut out = json!({ "action": action, "reminder": reminder_json(r, &cache, &self.local) });
        warn(&mut out, warning);
        Ok(Response::new(out, text.to_owned()).ids(vec![r.uuid().to_owned()]))
    }

    fn sync(&self, full: bool) -> Outcome {
        let report = self.svc.sync(full).map_err(cloud_error)?;
        let mut out = json!({ "lists": report.lists, "reminders": report.reminders, "changed": report.changed, "full": report.full });
        warn(&mut out, report.warning());
        let text = format!(
            "Synced: {} lists, {} reminders ({} changed{})",
            report.lists,
            report.reminders,
            report.changed,
            if report.full { ", full" } else { "" }
        );
        Ok(Response::new(out, text).count(report.changed))
    }

    fn background(&self, force_sync: bool) -> Outcome {
        let store = &self.svc.store;
        let file = |e: std::io::Error| other(e.to_string());
        // The last sync, or the last attempt when that failed.
        let last = self
            .cache()?
            .synced_ms
            .max(store.notified().map_err(file)?.sync_attempt_ms);
        let stale = last.is_none_or(|ms| service::now_ms() - ms >= service::SYNC_EVERY.as_millis() as i64);
        // A changed file in the folder is sent now, not at the next sync,
        // unless the last sync failed: that waits its turn.
        let last_failed = store.notified().map_err(file)?.sync_attempt_ms > self.cache()?.synced_ms;
        let folder_on = store.folder().map_err(file)?;
        let pending = folder_on.as_ref().is_some_and(folder::pending);
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
        let mut folder_report = None;
        let mut folder_error = None;
        if folder_on.is_some() && (synced || !pending) {
            match self.folder_run(None, false) {
                Ok(done) => folder_report = done.map(|(_, report)| report),
                Err(e) => {
                    warning(&format!("folder not synced: {}", e.message));
                    folder_error = Some(e.message);
                }
            }
        }
        for h in folder_report.iter().flat_map(|r| &r.held).filter(|h| h.new) {
            let body = format!(
                "It would delete {} of the list's reminders, so nothing was changed. Check the file, or run: icloud-reminders folder sync --yes",
                h.removing
            );
            if let Err(e) = notify::send(&format!("Reminders folder: {} not applied", h.file), &body) {
                warning(&format!("could not notify about {}: {e}", h.file));
            }
        }
        let _lock = store.lock().map_err(file)?;
        let cache = store.cache().map_err(file)?;
        let previous = store.notified().map_err(file)?;
        let (fire, mut state) = notify::due_now(&cache, &previous, jiff::Timestamp::now(), &self.local);
        state.sync_attempt_ms = attempted.or(state.sync_attempt_ms);
        let mut notified = Vec::new();
        let mut ids = Vec::new();
        for r in fire {
            let (headline, body) = notify::text(r, &cache, &self.local);
            match notify::send(&headline, &body) {
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
        let mut quiet = count == 0 && folder_error.is_none();
        if folder_on.is_some() {
            out["folder"] = folder_report.as_ref().map_or(Value::Null, folder_json);
            out["folder_error"] = json!(folder_error);
        }
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

/// A folder run as `{added, edited, deleted, written, held, warnings}`.
fn folder_json(report: &folder::Report) -> Value {
    let held: Vec<Value> = report
        .held
        .iter()
        .map(|h| json!({ "list": h.list, "file": h.file, "removing": h.removing }))
        .collect();
    json!({
        "added": report.added,
        "edited": report.edited,
        "deleted": report.deleted,
        "written": report.written,
        "held": held,
        "warnings": report.warnings,
    })
}

fn folder_summary(report: &folder::Report) -> String {
    format!(
        "Folder: {} added, {} edited, {} deleted in iCloud; {} file(s) written",
        report.added,
        report.edited,
        report.deleted,
        report.written.len()
    )
}

/// A folder command's answer: where it is, its files, and what the run did.
fn folder_response(state: &folder::State, report: &folder::Report, text: String) -> Response {
    for w in &report.warnings {
        warning(&format!("folder: {w}"));
    }
    let mut out = folder_json(report);
    out["on"] = json!(true);
    out["dir"] = json!(state.dir);
    out["files"] = state.lists.values().map(|ls| json!({ "file": ls.file })).collect();
    Response::new(out, text).count(report.added + report.edited + report.deleted)
}

/// Asks unless `yes`, and only when someone can answer
/// ([`output::interactive`]: text mode, stdin and stdout a terminal);
/// otherwise refuses (exit 2) without `--yes`.
fn confirm(mode: Mode, cmd: &str, yes: bool, question: &str) -> Result<(), Failure> {
    if yes {
        return Ok(());
    }
    if !output::interactive(mode) {
        return Err(
            usage(format!("{cmd} asks before it acts, and nobody can be asked here"))
                .hint(format!("pass --yes to {cmd} without asking")),
        );
    }
    if cli::confirm(question) {
        Ok(())
    } else {
        Err(CliError::new("cancelled", EXIT_ERROR, "cancelled"))
    }
}

/// How many reminders are open in each list, by list ID, in one pass.
fn open_counts(cache: &Cache) -> HashMap<&str, usize> {
    let mut open = HashMap::new();
    for r in cache.reminders.values().filter(|r| !r.completed) {
        *open.entry(r.list_id.as_str()).or_insert(0) += 1;
    }
    open
}

/// The lists as `[{id, name, color, open}]`.
fn lists_json(cache: &Cache, open: &HashMap<&str, usize>) -> Vec<Value> {
    cache
        .lists
        .iter()
        .map(|l| {
            json!({
                "id": bare(&l.id),
                "name": l.name,
                "color": l.color,
                "open": open.get(l.id.as_str()).copied().unwrap_or(0),
            })
        })
        .collect()
}

/// `List/ABC` and `Reminder/ABC` as `ABC`.
fn bare(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, rest)| rest)
}

/// A write or sync that happened but left a problem behind: on stderr, and
/// as `warning` in the JSON. The command still succeeds.
fn warn(out: &mut Value, w: Option<String>) {
    if let Some(w) = w {
        warning(&w);
        out["warning"] = Value::String(w);
    }
}

/// The reminder was deleted elsewhere before the write could land.
fn gone(r: &Reminder) -> Failure {
    CliError::new("not_found", EXIT_NOT_FOUND, format!("\"{}\" has been deleted", r.title))
}

/// Lowercase, with typographic apostrophes as plain ones.
fn fold(s: &str) -> String {
    s.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

fn resolve<'c>(cache: &'c Cache, query: &str) -> Result<&'c Reminder, Failure> {
    resolve_prefer(cache, query, true)
}

/// Finds a reminder by ID, else by title (ignoring case), else by a unique
/// part of the title; among the open ones first (`open`), or the completed.
fn resolve_prefer<'c>(cache: &'c Cache, query: &str, open: bool) -> Result<&'c Reminder, Failure> {
    let q = query.trim();
    if let Some(r) = cache.reminders.values().find(|r| r.id == q || r.uuid() == q) {
        return Ok(r);
    }
    let f = fold(q);
    let all: Vec<&Reminder> = cache.reminders.values().collect();
    let preferred = |rs: Vec<&'c Reminder>| -> Vec<&'c Reminder> {
        let first: Vec<_> = rs.iter().copied().filter(|r| r.completed != open).collect();
        if first.is_empty() { rs } else { first }
    };
    let exact = preferred(all.iter().copied().filter(|r| fold(&r.title) == f).collect());
    let matches = if exact.is_empty() && !f.is_empty() {
        preferred(all.iter().copied().filter(|r| fold(&r.title).contains(&f)).collect())
    } else {
        exact
    };
    let listing = |rs: &[&Reminder]| {
        rs.iter()
            .take(20)
            .map(|r| format!("\n  {}  ({})", r.title, r.uuid()))
            .collect::<String>()
    };
    match matches.as_slice() {
        [r] => Ok(r),
        [] => Err(CliError::new(
            "not_found",
            EXIT_NOT_FOUND,
            format!("no reminder matches \"{query}\""),
        )),
        many => Err(CliError::new(
            "ambiguous",
            EXIT_USAGE,
            format!("\"{query}\" matches {} reminders", many.len()),
        )
        .hint(format!("name one exactly or use its ID:{}", listing(many)))),
    }
}

fn resolve_list<'c>(cache: &'c Cache, query: &str) -> Result<&'c List, Failure> {
    let q = query.trim();
    if let Some(l) = cache.lists.iter().find(|l| l.id == q || bare(&l.id) == q) {
        return Ok(l);
    }
    let f = fold(q);
    let exact: Vec<&List> = cache.lists.iter().filter(|l| fold(&l.name) == f).collect();
    let matches = if exact.is_empty() && !f.is_empty() {
        cache.lists.iter().filter(|l| fold(&l.name).contains(&f)).collect()
    } else {
        exact
    };
    let names = |ls: &[&List]| ls.iter().map(|l| format!("\n  {}", l.name)).collect::<String>();
    match matches.as_slice() {
        [l] => Ok(l),
        [] => Err(
            CliError::new("not_found", EXIT_NOT_FOUND, format!("no list matches \"{query}\"")).hint(format!(
                "name one of these lists:{}",
                names(&cache.lists.iter().collect::<Vec<_>>())
            )),
        ),
        many => Err(CliError::new(
            "ambiguous",
            EXIT_USAGE,
            format!("\"{query}\" matches {} lists", many.len()),
        )
        .hint(format!("name one exactly or use its ID:{}", names(many)))),
    }
}

pub fn due_json(d: &Due, local: &TimeZone) -> Value {
    let wall = d.wall();
    json!({
        "date": wall.date().to_string(),
        "time": (!d.all_day).then(|| wall.time().strftime("%H:%M").to_string()),
        "all_day": d.all_day,
        "time_zone": d.time_zone,
        "at": rfc3339(d.instant(local).as_second()),
    })
}

fn reminder_json(r: &Reminder, cache: &Cache, local: &TimeZone) -> Value {
    let list = cache.list(&r.list_id);
    json!({
        "id": r.uuid(),
        "list": { "id": bare(&r.list_id), "name": list.map(|l| l.name.as_str()) },
        "title": r.title,
        "notes": r.notes,
        "completed": r.completed,
        "completed_at": r.completed_ms.map(|ms| rfc3339(ms / 1000)),
        "due": r.due.as_ref().map(|d| due_json(d, local)),
        "flagged": r.flagged,
        "priority": r.priority,
        "alerts": r.alarms,
    })
}

fn line(r: &Reminder, cache: &Cache, local: &TimeZone, with_list: bool) -> String {
    let check = if r.completed { "[x]" } else { "[ ]" };
    let due = r
        .due
        .as_ref()
        .map(|d| format!("  due {}", d.display(local)))
        .unwrap_or_default();
    let list = if with_list {
        cache
            .list(&r.list_id)
            .map(|l| format!("  ({})", l.name))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let flag = if r.flagged { "  ⚑" } else { "" };
    format!("{check} {}{due}{list}{flag}\n", r.title)
}

fn detail(r: &Reminder, cache: &Cache, local: &TimeZone) -> String {
    let mut out = format!("{}\n", r.title);
    let list = cache.list(&r.list_id).map(|l| l.name.as_str()).unwrap_or("?");
    out += &format!("  list:      {list}\n");
    out += &format!("  id:        {}\n", r.uuid());
    let status = match r.completed_ms {
        _ if !r.completed => "open".to_owned(),
        Some(ms) => format!("completed {}", rfc3339(ms / 1000)),
        None => "completed".to_owned(),
    };
    out += &format!("  status:    {status}\n");
    if let Some(d) = &r.due {
        out += &format!("  due:       {}\n", d.display(local));
    }
    if r.alarms > 0 {
        out += &format!("  alerts:    {} (set on an Apple device)\n", r.alarms);
    }
    if !r.notes.is_empty() {
        out += "\n";
        for l in r.notes.lines() {
            out += &format!("  {l}\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reminder(uuid: &str, title: &str, completed: bool) -> Reminder {
        Reminder {
            id: format!("Reminder/{uuid}"),
            list_id: "List/A".into(),
            title: title.into(),
            notes: String::new(),
            completed,
            completed_ms: None,
            due: None,
            priority: 0,
            flagged: false,
            parent_id: None,
            alarms: 0,
            created_ms: None,
            modified_ms: None,
            change_tag: None,
            tokens: None,
        }
    }

    #[test]
    fn resolves_ids_titles_and_parts_open_first() {
        let mut cache = Cache::default();
        for r in [
            reminder("1", "Buy milk", false),
            reminder("2", "Buy milk", true),
            reminder("3", "Call Ann’s office", false),
            reminder("4", "Call Bob", false),
        ] {
            cache.reminders.insert(r.id.clone(), r);
        }
        assert_eq!(resolve(&cache, "Reminder/2").ok().unwrap().uuid(), "2");
        assert_eq!(resolve(&cache, "4").ok().unwrap().uuid(), "4");
        assert_eq!(resolve(&cache, "buy MILK").ok().unwrap().uuid(), "1");
        assert_eq!(resolve_prefer(&cache, "buy milk", false).ok().unwrap().uuid(), "2");
        assert_eq!(resolve(&cache, "ann's").ok().unwrap().uuid(), "3");
        let Err(e) = resolve(&cache, "call") else {
            panic!("ambiguous")
        };
        assert_eq!((e.code.as_str(), e.exit), ("ambiguous", 2));
        let hint = e.hint.unwrap();
        assert!(hint.contains("Call Bob") && hint.contains("(3)"), "{hint}");
        let Err(e) = resolve(&cache, "eggs") else {
            panic!("not found")
        };
        assert_eq!((e.code.as_str(), e.exit), ("not_found", 4));
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn failures_use_the_shared_codes_exits_and_hints() {
        use cloudkit::Error as C;
        let http = |status| C::Http {
            status,
            body: String::new(),
        };
        let cases = [
            (http(421), "session_rejected", 3),
            (http(401), "session_rejected", 3),
            (http(503), "http", 5),
            (http(404), "http", 1),
            (C::Offline("x".into()), "offline", 5),
            (C::KeyringUnavailable("x".into()), "keyring_unavailable", 5),
            (C::BadResponse("x".into()), "bad_response", 5),
            (C::Service("x".into()), "session_service", 5),
            (
                C::CloudKit {
                    code: "x".into(),
                    reason: "y".into(),
                },
                "error",
                1,
            ),
            (C::Other("x".into()), "error", 1),
        ];
        for (e, code, exit) in cases {
            let what = format!("{e:?}");
            let f = cloud_error(e);
            assert_eq!((f.code.as_str(), f.exit), (code, exit), "{what}");
            assert!(exit != 5 || f.hint.is_some(), "{what}");
        }
        assert_eq!(cloud_error(http(421)).hint.as_deref(), Some(sign_in_hint().as_str()));
    }

    #[test]
    fn open_reminders_are_counted_per_list_in_one_pass() {
        let mut cache = Cache::default();
        let mut other_list = reminder("3", "c", false);
        other_list.list_id = "List/B".into();
        for r in [reminder("1", "a", false), reminder("2", "b", true), other_list] {
            cache.reminders.insert(r.id.clone(), r);
        }
        let open = open_counts(&cache);
        assert_eq!((open.get("List/A"), open.get("List/B")), (Some(&1), Some(&1)));
    }
}
