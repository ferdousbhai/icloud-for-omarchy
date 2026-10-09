//! The command line: `icloud-reminders <command>` reads and changes iCloud
//! Reminders without GTK, through the same [`Service`] the app uses, and
//! `icloud-reminders background` is what the systemd timer runs.
//! Human-readable output by default, `--json` for machines.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok, 1 error, 2 sign-in required, 64 usage.
//! With `--json` an error is one JSON line on stderr,
//! `{"error":{"code","message","exit_code"}}`.

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_OK, EXIT_SIGN_IN, EXIT_USAGE};
use icloud_session::time::rfc3339;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::cloudkit::{self, SessionTransport};
use crate::due::{self, Due};
use crate::model::{Change, List, Reminder};
use crate::notify;
use crate::service::{self, Service};
use crate::store::{Cache, Store};

const TOOL: &str = "icloud-reminders";

const AFTER_HELP: &str = "\
REMINDER is a reminder's ID, its exact title (ignoring case), or a unique
part of it; open reminders are matched before completed ones. LIST is a
list's ID or name, or a unique part of the name.

WHEN is 2026-10-10 (all day), \"2026-10-10 17:30\", today, tomorrow, either
followed by a time (\"tomorrow 9:00\"), a time alone (17:30, today), or
+30m, +2h, +3d.

Reading commands sync first; --cached reads what the last sync left
(also used, with a warning, when offline).

With --json, stdout is only the JSON result and an error is one JSON line
on stderr: {\"error\":{\"code\",\"message\",\"exit_code\"}}; codes: usage,
sign_in_required, offline, not_found, ambiguous, cancelled, error.

Exit codes: 0 ok, 1 error, 2 sign-in required (icloud-session sign-in),
64 usage.";

#[derive(Parser)]
#[command(
    name = "icloud-reminders",
    version,
    about = "iCloud Reminders from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    /// Machine-readable output on stdout (errors as JSON on stderr).
    #[arg(long, global = true)]
    json: bool,
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
    /// To --list (default: the list named Reminders, else the first).
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
    /// JSON: {action: "complete", reminder}
    #[command(after_help = AFTER_HELP)]
    Complete {
        #[arg(value_name = "REMINDER")]
        reminder: String,
    },
    /// Mark a completed reminder open again.
    ///
    /// JSON: {action: "uncomplete", reminder}
    #[command(after_help = AFTER_HELP)]
    Uncomplete {
        #[arg(value_name = "REMINDER")]
        reminder: String,
    },
    /// Delete a reminder (to Recently Deleted on your Apple devices).
    ///
    /// Asks on a terminal; without one, --yes is required.
    ///
    /// JSON: {action: "delete", reminder: {id, title}}
    #[command(after_help = AFTER_HELP)]
    Delete {
        #[arg(value_name = "REMINDER")]
        reminder: String,
        /// Do not ask; required when stdin is not a terminal.
        #[arg(long, short)]
        yes: bool,
    },
    /// Fetch what changed in iCloud.
    ///
    /// JSON: {lists, reminders, changed, full}
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
    /// JSON: {synced, sync_error, notified: [{id, title}]}
    #[command(after_help = AFTER_HELP)]
    Background {
        /// Sync even if the last sync is recent.
        #[arg(long)]
        sync: bool,
    },
}

enum Failure {
    Usage(String),
    Cloud(cloudkit::Error),
    /// A machine-readable code (`not_found`, `ambiguous`, `cancelled`) and
    /// its message.
    Coded(&'static str, String),
    Other(String),
}

impl From<cloudkit::Error> for Failure {
    fn from(e: cloudkit::Error) -> Self {
        Failure::Cloud(e)
    }
}

impl Failure {
    fn code(&self) -> u8 {
        match self {
            Failure::Usage(_) => EXIT_USAGE,
            Failure::Cloud(cloudkit::Error::SignInRequired) => EXIT_SIGN_IN,
            _ => EXIT_ERROR,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Failure::Usage(_) => "usage",
            Failure::Cloud(cloudkit::Error::SignInRequired) => "sign_in_required",
            Failure::Cloud(cloudkit::Error::Offline(_)) => "offline",
            Failure::Coded(code, _) => code,
            _ => "error",
        }
    }

    fn message(&self) -> String {
        match self {
            Failure::Usage(m) | Failure::Other(m) | Failure::Coded(_, m) => m.clone(),
            Failure::Cloud(cloudkit::Error::SignInRequired) => {
                "sign in to iCloud required: run `icloud-session sign-in` (or Sign In in the app)".into()
            }
            Failure::Cloud(e) => e.to_string(),
        }
    }
}

type Outcome = Result<(), Failure>;

fn usage(msg: impl Into<String>) -> Failure {
    Failure::Usage(msg.into())
}

/// Runs the command line on the process arguments and returns the exit code.
pub fn run() -> u8 {
    let args = match cli::parse::<Args>(TOOL) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let json = args.json;
    let result = match args.data_dir.or_else(Store::default_dir) {
        None => Err(Failure::Other("no data directory: pass --data-dir".into())),
        Some(dir) => {
            let transport = SessionTransport::default();
            let cli = Cli {
                json,
                cached: args.cached,
                svc: Service::new(&transport, Store::new(dir)),
                local: due::local_zone(),
            };
            cli.dispatch(args.command)
        }
    };
    match result {
        Ok(()) => EXIT_OK,
        Err(f) => cli::report(TOOL, json, f.kind(), f.code(), &f.message(), None),
    }
}

struct Cli<'a> {
    json: bool,
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
            Command::Add { title, list, notes, due } => self.add(&title, list.as_deref(), notes, due.as_deref()),
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
        }
    }

    /// The cache after a sync (unless `--cached`). Offline with a cache
    /// that has been synced before: that cache, and a warning.
    fn fresh(&self) -> Result<Cache, Failure> {
        if self.cached {
            return Ok(self.svc.cache());
        }
        match self.svc.sync(false) {
            Ok(_) => Ok(self.svc.cache()),
            Err(cloudkit::Error::Offline(e)) if self.svc.cache().synced_ms.is_some() => {
                let cache = self.svc.cache();
                let when = cache.synced_ms.map(|ms| rfc3339(ms / 1000)).unwrap_or_default();
                eprintln!("{TOOL}: offline ({e}); showing the reminders as of {when}");
                Ok(cache)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn print(&self, v: Value, text: impl FnOnce() -> String) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(&v).expect("JSON values serialize"));
        } else {
            print!("{}", text());
        }
    }

    fn lists(&self) -> Outcome {
        let cache = self.fresh()?;
        let open = |l: &List| {
            cache
                .reminders
                .values()
                .filter(|r| r.list_id == l.id && !r.completed)
                .count()
        };
        let rows: Vec<Value> = cache
            .lists
            .iter()
            .map(|l| json!({ "id": bare(&l.id), "name": l.name, "color": l.hex_color(), "open": open(l) }))
            .collect();
        self.print(json!(rows), || {
            if cache.lists.is_empty() {
                return "No lists.\n".into();
            }
            cache
                .lists
                .iter()
                .map(|l| format!("{}  ({} open)\n", l.name, open(l)))
                .collect()
        });
        Ok(())
    }

    fn list(&self, list: Option<&str>, completed: bool, all: bool) -> Outcome {
        let cache = self.fresh()?;
        let list = list.map(|q| resolve_list(&cache, q)).transpose()?;
        let rows = cache.sorted(&self.local, |r| {
            list.is_none_or(|l| r.list_id == l.id) && (all || r.completed == completed)
        });
        let json_rows: Vec<Value> = rows.iter().map(|r| reminder_json(r, &cache, &self.local)).collect();
        self.print(json!(json_rows), || {
            if rows.is_empty() {
                return "No reminders.\n".into();
            }
            rows.iter().map(|r| line(r, &cache, &self.local, list.is_none())).collect()
        });
        Ok(())
    }

    fn show(&self, query: &str) -> Outcome {
        let cache = self.fresh()?;
        let r = resolve(&cache, query)?;
        self.print(reminder_json(r, &cache, &self.local), || detail(r, &cache, &self.local));
        Ok(())
    }

    fn add(&self, title: &str, list: Option<&str>, notes: Option<String>, due: Option<&str>) -> Outcome {
        if title.trim().is_empty() {
            return Err(usage("add needs a title"));
        }
        let due = due.map(|w| self.when(w)).transpose()?;
        let cache = self.fresh()?;
        let list = match list {
            Some(q) => resolve_list(&cache, q)?,
            None => cache
                .default_list()
                .ok_or_else(|| Failure::Coded("not_found", "this account has no reminder lists".into()))?,
        };
        let r = self
            .svc
            .add(&list.id, title.trim(), notes.as_deref().unwrap_or(""), due.as_ref())?;
        self.done("add", &r, &format!("Added \"{}\" to {}", r.title, list.name));
        Ok(())
    }

    fn when(&self, w: &str) -> Result<Due, Failure> {
        due::parse_when(w, &jiff::Zoned::now().with_time_zone(self.local.clone())).map_err(Failure::Usage)
    }

    fn edit(&self, query: &str, title: Option<String>, notes: Option<String>, due: Option<&str>, no_due: bool) -> Outcome {
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
            eprintln!(
                "{TOOL}: \"{}\" has {} alert(s) set on an Apple device; they keep their own time",
                r.title, r.alarms
            );
        }
        let after = self.svc.update(&r, &changes)?.expect("not a delete");
        self.done("edit", &after, &format!("Changed \"{}\"", after.title));
        Ok(())
    }

    fn set_completed(&self, query: &str, completed: bool) -> Outcome {
        let cache = self.fresh()?;
        let r = resolve_prefer(&cache, query, !completed)?.clone();
        let action = if completed { "complete" } else { "uncomplete" };
        let after = if r.completed == completed {
            r
        } else {
            self.svc.update(&r, &[Change::Completed(completed)])?.expect("not a delete")
        };
        let text = if completed {
            format!("Completed \"{}\"", after.title)
        } else {
            format!("Reopened \"{}\"", after.title)
        };
        self.done(action, &after, &text);
        Ok(())
    }

    fn delete(&self, query: &str, yes: bool) -> Outcome {
        let cache = self.fresh()?;
        let r = resolve(&cache, query)?.clone();
        confirm(yes, &format!("Delete \"{}\"?", r.title))?;
        self.svc.update(&r, &[Change::Deleted])?;
        self.print(
            json!({ "action": "delete", "reminder": { "id": bare(&r.id), "title": r.title } }),
            || format!("Deleted \"{}\"\n", r.title),
        );
        Ok(())
    }

    fn done(&self, action: &str, r: &Reminder, text: &str) {
        let cache = self.svc.cache();
        self.print(
            json!({ "action": action, "reminder": reminder_json(r, &cache, &self.local) }),
            || format!("{text}\n"),
        );
    }

    fn sync(&self, full: bool) -> Outcome {
        let report = self.svc.sync(full)?;
        self.print(
            json!({ "lists": report.lists, "reminders": report.reminders, "changed": report.changed, "full": report.full }),
            || {
                format!(
                    "Synced: {} lists, {} reminders ({} changed{})\n",
                    report.lists,
                    report.reminders,
                    report.changed,
                    if report.full { ", full" } else { "" }
                )
            },
        );
        Ok(())
    }

    fn background(&self, force_sync: bool) -> Outcome {
        let store = &self.svc.store;
        // The last sync, or the last attempt when that failed.
        let last = self.svc.cache().synced_ms.max(store.notified().sync_attempt_ms);
        let stale = last.is_none_or(|ms| service::now_ms() - ms >= service::SYNC_EVERY.as_millis() as i64);
        let attempted = (force_sync || stale).then(service::now_ms);
        let mut synced = false;
        let mut sync_error = None;
        if attempted.is_some() {
            match self.svc.sync(false) {
                Ok(_) => synced = true,
                Err(e) => {
                    eprintln!("{TOOL}: not synced: {e}");
                    sync_error = Some(e.to_string());
                }
            }
        }
        let _lock = store.lock().map_err(|e| Failure::Other(e.to_string()))?;
        let cache = store.cache();
        let (fire, mut state) = notify::due_now(&cache, &store.notified(), jiff::Timestamp::now(), &self.local);
        state.sync_attempt_ms = attempted.or(state.sync_attempt_ms);
        let mut notified = Vec::new();
        for r in fire {
            let (headline, body) = notify::text(r, &cache, &self.local);
            match notify::send(&headline, &body) {
                Ok(()) => notified.push(json!({ "id": bare(&r.id), "title": r.title })),
                Err(e) => {
                    eprintln!("{TOOL}: could not notify about \"{}\": {e}", r.title);
                    // Try again next minute.
                    state.fired.remove(&r.id);
                }
            }
        }
        store
            .save_notified(&state)
            .map_err(|e| Failure::Other(format!("cannot save notified.json: {e}")))?;
        let count = notified.len();
        self.print(
            json!({ "synced": synced, "sync_error": sync_error, "notified": notified }),
            || if count > 0 { format!("Notified about {count} reminder(s)\n") } else { String::new() },
        );
        Ok(())
    }
}

/// Asks on a terminal unless `yes`; refuses without a terminal.
fn confirm(yes: bool, question: &str) -> Outcome {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(usage("delete asks before it acts: pass --yes when stdin is not a terminal"));
    }
    if cli::confirm(question) {
        Ok(())
    } else {
        Err(Failure::Coded("cancelled", "cancelled".into()))
    }
}

/// `List/ABC` and `Reminder/ABC` as `ABC`.
fn bare(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, rest)| rest)
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
        [] => Err(Failure::Coded("not_found", format!("no reminder matches \"{query}\""))),
        many => Err(Failure::Coded(
            "ambiguous",
            format!(
                "\"{query}\" matches {} reminders; name one exactly or use its ID:{}",
                many.len(),
                listing(many)
            ),
        )),
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
        [] => Err(Failure::Coded(
            "not_found",
            format!(
                "no list matches \"{query}\"; lists:{}",
                names(&cache.lists.iter().collect::<Vec<_>>())
            ),
        )),
        many => Err(Failure::Coded(
            "ambiguous",
            format!("\"{query}\" matches {} lists:{}", many.len(), names(many)),
        )),
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
    let due = r.due.as_ref().map(|d| format!("  due {}", d.display(local))).unwrap_or_default();
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

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

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
        let Err(Failure::Coded("ambiguous", msg)) = resolve(&cache, "call") else {
            panic!("ambiguous")
        };
        assert!(msg.contains("Call Bob") && msg.contains("(3)"), "{msg}");
        assert!(matches!(resolve(&cache, "eggs"), Err(Failure::Coded("not_found", _))));
    }
}
