//! `icloud-notes <command>`: every feature of the Notes window from a
//! terminal (or an agent), without a window: list, read, search, create,
//! edit, rename, move and delete notes and folders, resolve conflicts, and
//! sync. Commands go through [`Backend`] and `syncmodel`, the window's own
//! rules, and take the vault's lock before they change the vault or run
//! icloud-notes-sync, as the window and the background sync do.
//!
//! Output follows the contract every iCloud tool shares
//! (`icloud_session::output`): text on a terminal, the JSON envelope
//! `{"ok":true,"data","summary"}` when piped or with `--json`; `--quiet`
//! (data only), `--styled` (text), `--ids-only`, `--count`, combinable
//! (ids-only > count > quiet > json > styled). A failure is
//! `{"ok":false,"error":{"code","message","hint"?}}` on stdout in those
//! machine modes, `error:`/`hint:` on stderr in text mode. The JSON is laid
//! out as the Qt app's (`json.rs`), which this command line replaces, so a
//! caller sees the same bytes from both.
//!
//! Exit codes, as in every iCloud tool: 0 ok, 1 error, 2 usage or invalid
//! request, 3 not signed in or not cloned, 4 not found, 5 offline or a
//! service (icloud-session, the engine) unavailable. `push --dry-run` and
//! `diff` report their changes and exit 0; a sync, pull, push or clone that
//! ran but failed prints its result (with `error` in it: the engine's own
//! code, message, hint and exit when it gave one, sync_failed/1 only when
//! it did not) and exits with the failure's code.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use icloud_session::cli::{EXIT_ERROR, EXIT_NOT_FOUND, EXIT_OK, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE};
use icloud_session::output::{
    CliError, Flags, Mode, NETWORK_HINT, SESSION_SERVICE_HINT, SIGN_IN_HINT, interactive, sign_in_message,
    terminal_safe,
};
use serde_json::{Map, Value, json};

use crate::backend::{Backend, Refusal, Role};
use crate::files::os_error_text;
use crate::files::utf8;
use crate::help::{self, Spec};
use crate::lock::Taken;
use crate::sync::Kind;
use crate::syncmodel;
use crate::text::{eq_ignoring_case, join_path, split_path, to_int, utf16_len};
use crate::vault::Vault;
use crate::{files, json as layout};

/// How long a command waits for the vault's lock when a background sync or
/// another command holds it (the window holds it while open: no waiting
/// for that unless --wait says so).
const DEFAULT_WAIT: Duration = Duration::from_secs(30);
/// How often a command waiting for the lock looks again.
const LOCK_POLL: Duration = Duration::from_millis(200);
/// How often the background step syncs when nothing changed here.
const BACKGROUND_EVERY_SECS: u32 = 900;

/// How a run of the command line ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// With this exit code.
    Exit(u8),
    /// Not here: the command (`export-pdf`) lays out a page, which only the
    /// app's binary can. Its arguments are fine.
    NeedsApp,
}

fn failure(code: &str, exit: u8, message: impl Into<String>) -> CliError {
    CliError::new(code, exit, message)
}

/// Every usage error points at the help.
fn usage_error(message: impl Into<String>) -> CliError {
    failure("usage", EXIT_USAGE, message).hint("run `icloud-notes --help`")
}

fn error(message: impl Into<String>) -> CliError {
    failure("error", EXIT_ERROR, message)
}

// ---- arguments ---------------------------------------------------------------

/// The five global output flags, strongest first: they combine, and the
/// strongest wins (then text on a terminal, the envelope when piped).
const OUTPUT_FLAGS: [&str; 5] = ["--ids-only", "--count", "--quiet", "--json", "--styled"];
const VALUED: [&str; 9] = [
    "--folder",
    "--flag",
    "--body",
    "--file",
    "--in",
    "--all",
    "--choices",
    "--vault",
    "--wait",
];
/// The switches besides the output flags.
const SWITCHES: [&str; 12] = [
    "--yes",
    "--push",
    "--append",
    "--force",
    "--stdin",
    "--raw",
    "--dry-run",
    "--strip",
    "--synced",
    "--records",
    "--help",
    "--version",
];

/// The output flags given, in the order given, as icloud-notes-sync takes
/// them (the same five).
#[derive(Debug, Default, Clone)]
struct OutputFlags {
    given: Vec<String>,
}

impl OutputFlags {
    /// Takes `arg` when it is one of the five.
    fn set(&mut self, arg: &str) -> bool {
        if !OUTPUT_FLAGS.contains(&arg) {
            return false;
        }
        if !self.given.iter().any(|flag| flag == arg) {
            self.given.push(arg.to_string());
        }
        true
    }

    fn mode(&self) -> Mode {
        let has = |flag: &str| self.given.iter().any(|given| given == flag);
        Flags {
            json: has("--json"),
            styled: has("--styled"),
            quiet: has("--quiet"),
            ids_only: has("--ids-only"),
            count: has("--count"),
        }
        .mode()
    }
}

/// The output flags among the arguments (up to `--`), for a usage error
/// found before the arguments are fully parsed.
fn scan_output_flags(argv: &[String]) -> OutputFlags {
    let mut flags = OutputFlags::default();
    for arg in argv.iter().take_while(|arg| *arg != "--") {
        flags.set(arg);
    }
    flags
}

struct Args {
    command: String,
    positional: Vec<String>,
    /// The valued options and the switches, in the order first given.
    values: Vec<(String, String)>,
    switches: Vec<String>,
    flags: OutputFlags,
    mode: Mode,
}

impl Args {
    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|switch| switch == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn parse_args(argv: &[String]) -> Result<Args, CliError> {
    let mut a = Args {
        command: String::new(),
        positional: Vec::new(),
        values: Vec::new(),
        switches: Vec::new(),
        flags: OutputFlags::default(),
        mode: Mode::Json,
    };
    let mut rest = argv.iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            a.positional.extend(rest.cloned());
            break;
        }
        let arg = match arg.as_str() {
            "-h" => "--help",
            "-V" => "--version",
            other => other,
        };
        if !arg.starts_with("--") {
            if arg.starts_with('-') && arg.len() > 1 {
                return Err(usage_error(format!("unknown option {arg}")));
            }
            if a.command.is_empty() {
                a.command = arg.to_string();
            } else {
                a.positional.push(arg.to_string());
            }
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg, None),
        };
        if VALUED.contains(&name) {
            let value = match inline {
                Some(value) => value.to_string(),
                None => rest
                    .next()
                    .ok_or_else(|| usage_error(format!("{name} needs a value")))?
                    .clone(),
            };
            // Given again, the last one counts.
            match a.values.iter_mut().find(|(key, _)| key == name) {
                Some(entry) => entry.1 = value,
                None => a.values.push((name.to_string(), value)),
            }
        } else if SWITCHES.contains(&name) || OUTPUT_FLAGS.contains(&name) {
            if inline.is_some() {
                return Err(usage_error(format!("{name} takes no value")));
            }
            if !a.flags.set(name) && !a.has(name) {
                a.switches.push(name.to_string());
            }
        } else {
            return Err(usage_error(format!("unknown option {name}")));
        }
    }
    a.mode = a.flags.mode();
    Ok(a)
}

fn check_args(spec: &Spec, a: &Args) -> Result<(), CliError> {
    const GLOBAL: [&str; 3] = ["--vault", "--wait", "--help"];
    let given = a.values.iter().map(|(name, _)| name).chain(&a.switches);
    for name in given {
        if !GLOBAL.contains(&name.as_str()) && !spec.options.contains(&name.as_str()) {
            return Err(usage_error(format!("{} does not take {name}", spec.name)));
        }
    }
    if !(spec.min_args..=spec.max_args).contains(&a.positional.len()) {
        return Err(usage_error(format!("usage: icloud-notes {}", spec.synopsis)));
    }
    Ok(())
}

// ---- output ------------------------------------------------------------------

/// Prints to stdout; a reader that went away (`… | head`) is not an error.
fn out(text: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// Prints a line to stderr. `eprintln!` panics when nothing reads it any
/// more, and the run would end with the panic's exit code, not its own.
pub fn say(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}

/// What a command produced: the result (`data`), a one-line summary (the
/// text output when there is no `text`), and for the machine modes its ids
/// and count; `exit` non-zero for a result that still failed (a sync).
struct Done {
    json: Value,
    text: String,
    summary: String,
    ids: Option<Vec<String>>,
    count: Option<usize>,
    exit: u8,
}

impl Done {
    fn new(json: Value, text: impl Into<String>, summary: impl Into<String>) -> Done {
        Done {
            json,
            text: text.into(),
            summary: summary.into(),
            ids: None,
            count: None,
            exit: EXIT_OK,
        }
    }

    /// A change's result: its one line is also its summary.
    fn change(json: Value, line: impl Into<String>) -> Done {
        Done::new(json, line, "")
    }
}

/// The result in `mode`; returns its exit code. The envelope is written key
/// by key so `ok` comes first.
fn print_result(d: &Done, mode: Mode) -> u8 {
    match mode {
        Mode::Quiet => out(&format!("{}\n", layout::text(&d.json, 0))),
        Mode::Count => {
            let n = match (&d.count, &d.ids, &d.json) {
                (Some(count), _, _) => *count,
                (None, Some(ids), _) => ids.len(),
                (None, None, Value::Array(items)) => items.len(),
                (None, None, Value::Null) => 0,
                _ => 1,
            };
            out(&format!("{n}\n"));
        }
        Mode::Ids => match &d.ids {
            Some(ids) => {
                let lines: String = ids
                    .iter()
                    .map(|id| terminal_safe(id).replace(['\n', '\t'], " ") + "\n")
                    .collect();
                out(&lines);
            }
            None => {
                say("note: this command has no ids to list; printing its result instead");
                out(&format!("{}\n", layout::text(&d.json, 0)));
            }
        },
        Mode::Human => {
            let text = if d.text.trim().is_empty() { &d.summary } else { &d.text };
            if !text.is_empty() {
                let newline = if text.ends_with('\n') { "" } else { "\n" };
                out(&format!("{}{newline}", terminal_safe(text)));
            }
        }
        // Mode::Json, and any mode added to the shared contract later.
        _ => out(&format!(
            "{{\n    \"ok\": true,\n    \"data\": {},\n    \"summary\": {}\n}}\n",
            layout::text(&d.json, 1),
            layout::text(&json!(d.summary), 0)
        )),
    }
    d.exit
}

/// `{code, message, hint?}`: the error object inside a result that failed
/// (`data.error`).
fn failure_json(f: &CliError) -> Value {
    let mut error = json!({"code": f.code, "message": f.message});
    if let Some(hint) = &f.hint {
        error["hint"] = json!(hint);
    }
    error
}

/// The failure's `error:`/`hint:` lines on stderr (text mode).
fn print_error_text(f: &CliError) {
    let mut text = format!("error: {}", f.message);
    if let Some(hint) = &f.hint {
        text += &format!("\nhint: {hint}");
    }
    say(&terminal_safe(&text));
}

/// The failure: in a machine mode `{"ok":false,"error":{"code","message",
/// "hint"?}}` on stdout (`ok` first; nothing on stderr), in text mode
/// `error:`/`hint:` lines on stderr. Returns its exit code.
fn report(f: &CliError, mode: Mode) -> u8 {
    if mode.is_machine() {
        let quoted = |text: &str| layout::text(&json!(text), 0);
        let mut document = format!(
            "{{\n    \"ok\": false,\n    \"error\": {{\n        \"code\": {},\n        \"message\": {}",
            quoted(&f.code),
            quoted(&f.message)
        );
        if let Some(hint) = &f.hint {
            document += &format!(",\n        \"hint\": {}", quoted(hint));
        }
        out(&(document + "\n    }\n}\n"));
    } else {
        print_error_text(f);
    }
    f.exit
}

/// A result that carries a sync's failure: the result, then in text mode
/// the failure's `error:`/`hint:` lines on stderr too.
fn print_result_with(d: &Done, failure: Option<&CliError>, mode: Mode) -> u8 {
    let code = print_result(d, mode);
    if let Some(failure) = failure.filter(|_| !mode.is_machine()) {
        print_error_text(failure);
    }
    code
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn iso(ms: i64) -> String {
    icloud_session::time::rfc3339(ms.div_euclid(1000))
}

fn or_null(text: &str) -> Value {
    if text.is_empty() { Value::Null } else { json!(text) }
}

/// A hint, when there is one to give.
fn hinted(f: CliError, hint: &str) -> CliError {
    if hint.is_empty() { f } else { f.hint(hint) }
}

// ---- notes -------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Note {
    folder: String,
    file: String,
    title: String,
    snippet: String,
    id: String,
    modified_ms: i64,
    flags: Vec<&'static str>,
}

impl Note {
    fn path(&self) -> String {
        join_path(&self.folder, &self.file)
    }

    fn stem(&self) -> &str {
        &self.file[..self.file.len().saturating_sub(3)]
    }

    fn json(&self) -> Value {
        json!({
            "path": self.path(),
            "folder": self.folder,
            "file": self.file,
            "title": self.title,
            "snippet": self.snippet,
            "modified": iso(self.modified_ms),
            "modified_ms": self.modified_ms,
            "id": or_null(&self.id),
            "flags": self.flags,
        })
    }
}

/// Every note in the given folder (all of them for ""), as the window
/// lists them: its note list's order, details and badges.
fn scan_notes(b: &mut Backend, folder: &str) -> Vec<Note> {
    b.refresh();
    b.set_current_folder(folder);
    b.notes()
        .iter()
        .map(|file| {
            let details = b.note_details(file);
            let path = join_path(folder, file);
            let (folder, name) = split_path(&path);
            Note {
                folder: folder.to_string(),
                file: name.to_string(),
                title: details.title,
                snippet: details.snippet,
                id: details.id,
                modified_ms: details.modified_ms,
                flags: b.note_flags(file).to_vec(),
            }
        })
        .collect()
}

/// `to` as a path from the directory `from` (both absolute and clean).
fn relative_path(from: &Path, to: &Path) -> String {
    let (from, to): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); from.len() - shared];
    parts.extend(
        to[shared..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// A note by path, id or unique title (see the usage text).
fn resolve_note(b: &mut Backend, arg: &str) -> Result<Note, CliError> {
    let mut want = arg.trim().to_string();
    if Path::new(&want).is_absolute() {
        want = relative_path(&b.vault().absolute(), &files::absolute_clean(Path::new(&want)));
    }
    if let Some(rest) = want.strip_prefix("./") {
        want = rest.to_string();
    }
    let notes = scan_notes(b, "");
    if let Some(note) = notes
        .iter()
        .find(|n| n.path() == want || n.path() == format!("{want}.md"))
    {
        return Ok(note.clone());
    }
    let mut matches: Vec<&Note> = notes.iter().filter(|n| !n.id.is_empty() && n.id == want).collect();
    if matches.is_empty() {
        let named = |n: &&Note| eq_ignoring_case(&n.title, &want) || eq_ignoring_case(n.stem(), &want);
        matches = notes.iter().filter(named).collect();
    }
    match matches.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(failure("not_found", EXIT_NOT_FOUND, format!("no note \"{arg}\""))
            .hint("`icloud-notes list` shows every note's path; a title must match exactly.")),
        several => {
            let paths: Vec<String> = several.iter().map(|n| n.path()).collect();
            Err(failure(
                "ambiguous",
                EXIT_USAGE,
                format!("\"{arg}\" names {} notes: {}", several.len(), paths.join(", ")),
            )
            .hint("Name the note by its path."))
        }
    }
}

/// Makes the note the backend's open one, as a click in the list does.
fn open_note(b: &mut Backend, n: &Note) -> Result<(), CliError> {
    b.set_current_folder(&n.folder);
    b.refresh();
    b.open_note(&n.file);
    if b.current_note() != n.file {
        return Err(failure(
            "not_found",
            EXIT_NOT_FOUND,
            format!("could not open {}", n.path()),
        ));
    }
    Ok(())
}

/// [`resolve_note`], then [`open_note`]: the note named by arg, open in the backend.
fn resolve_and_open(b: &mut Backend, arg: &str) -> Result<Note, CliError> {
    let note = resolve_note(b, arg)?;
    open_note(b, &note)?;
    Ok(note)
}

/// A folder path as given ("" and "/" are the vault root), if it exists.
fn resolve_folder(b: &Backend, arg: &str) -> Result<String, CliError> {
    let folder = arg.trim().trim_end_matches('/').trim_start_matches('/');
    let found = b.folders().iter().find(|f| *f == folder);
    let found = found.or_else(|| b.folders().iter().find(|f| eq_ignoring_case(f, folder)));
    found.cloned().ok_or_else(|| {
        failure("not_found", EXIT_NOT_FOUND, format!("no folder \"{arg}\""))
            .hint("`icloud-notes folders` lists them; `icloud-notes new-folder` makes one.")
    })
}

// ---- the lock, the sign-in, syncing --------------------------------------------

/// The lock's holder is the Notes window, which keeps it for as long as it
/// is open.
fn is_app_holder(holder: &str) -> bool {
    holder.starts_with("Notes (pid")
}

/// The vault's lock, as the window and the background sync take it. The
/// window holds it for as long as it is open: refused at once then (unless
/// --wait), since it may be syncing this very vault. A background sync or
/// another command is waited for (--wait SECS, 30 by default).
fn take_lock(b: &mut Backend, a: &Args) -> Result<(), CliError> {
    let wait = match a.value("--wait") {
        Some(given) => {
            let seconds = to_int(given).and_then(|seconds| u64::try_from(seconds).ok());
            let seconds = seconds.ok_or_else(|| usage_error(format!("--wait wants whole seconds, not \"{given}\"")))?;
            Some(Duration::from_secs(seconds))
        }
        None => None,
    };
    let started = Instant::now();
    loop {
        match b.lock_vault() {
            Taken::Locked => return Ok(()),
            Taken::Failed => {
                let lock = b.vault().lock_path();
                return Err(failure(
                    "vault_lock",
                    EXIT_ERROR,
                    format!("could not open the vault's lock {}", lock.display()),
                ));
            }
            Taken::Busy => {}
        }
        let holder = b.lock_holder();
        let app = is_app_holder(&holder);
        let limit = wait.unwrap_or(if app { Duration::ZERO } else { DEFAULT_WAIT });
        if started.elapsed() >= limit {
            return Err(if app {
                failure(
                    "vault_busy",
                    EXIT_ERROR,
                    format!("{holder} is open and owns the notes vault while it is open."),
                )
                .hint("Make the change in Notes, or quit it and retry (or --wait SECS for it to close).")
            } else {
                failure("vault_busy", EXIT_ERROR, format!("{holder} holds the notes vault."))
                    .hint("Retry later, or pass --wait SECS.")
            });
        }
        std::thread::sleep(LOCK_POLL);
    }
}

fn session_unknown() -> CliError {
    failure(
        "session_service",
        EXIT_UNAVAILABLE,
        "icloud-session is not available, so the sign-in is unknown",
    )
    .hint(SESSION_SERVICE_HINT)
}

fn signed_out() -> CliError {
    failure("sign_in_required", EXIT_SIGN_IN, sign_in_message(None)).hint(SIGN_IN_HINT)
}

/// What icloud-session says, for commands that talk to iCloud.
fn need_sign_in(b: &mut Backend) -> Result<(), CliError> {
    b.read_sign_in();
    if !b.sign_in_known() {
        return Err(session_unknown());
    }
    if !b.signed_in() || b.auth_expired() {
        return Err(signed_out());
    }
    Ok(())
}

/// The sync engine, which a command that syncs cannot do without.
fn need_sync_tool(b: &Backend) -> Result<PathBuf, CliError> {
    b.engine().map(Path::to_path_buf).ok_or_else(|| {
        failure(
            "sync_tool_missing",
            EXIT_UNAVAILABLE,
            "the sync engine (icloud-notes-sync) is missing.",
        )
        .hint("Reinstall icloud-notes: sudo pacman -S icloud-notes")
    })
}

/// A vault that left iCloud (`leave`): what would reach iCloud refuses, as
/// the engine does.
fn left_failure(vault: &Vault) -> Result<(), CliError> {
    if !vault.left_icloud() {
        return Ok(());
    }
    let root = vault.root();
    Err(failure(
        "left_icloud",
        EXIT_SIGN_IN,
        format!("Notes left iCloud on this computer; Omacloud syncs {root} now"),
    )
    .hint(format!("change the files in {root}")))
}

/// The vault's notes can be read and changed: cloned, or left iCloud for
/// Omacloud (plain files then). `not_cloned` is this tool's own code (exit
/// 3) for a vault with nothing cloned into it, found before the engine runs.
fn need_cloned(vault: &Vault) -> Result<(), CliError> {
    if vault.cloned() || vault.left_icloud() {
        return Ok(());
    }
    Err(failure(
        "not_cloned",
        EXIT_SIGN_IN,
        format!("no notes cloned at {}", vault.root()),
    )
    .hint("Run `icloud-notes clone` (signed in to icloud-session) first."))
}

#[derive(Default)]
struct SyncOutcome {
    /// `{ok, runs, log}`; `None` when nothing ran.
    json: Option<Map<String, Value>>,
    text: String,
    failure: Option<CliError>,
}

/// What a sync command runs: "sync" (push, then pull, in one engine run),
/// "pull", "push" or "clone", or "background", the engine's step.
fn kind_of(what: &str, delete_missing: bool) -> Kind {
    match what {
        "sync" => Kind::Sync { delete_missing },
        "background" => Kind::Background {
            every_secs: BACKGROUND_EVERY_SECS,
        },
        "pull" => Kind::Pull,
        "push" => Kind::Push { delete_missing },
        _ => Kind::Clone,
    }
}

/// Runs `what` through the backend with its lock held, as the window runs
/// it. A sync still reports two runs, one per half. For "background", a
/// sync when one is due: when none is, the outcome is empty (no json, no
/// failure), or the sign-in's failure when the engine found nobody to sync
/// as (it asks icloud-session, only when a sync is due).
/// `delete_missing` is push's and sync's --yes.
fn run_sync(b: &mut Backend, what: &str, delete_missing: bool) -> SyncOutcome {
    let mut result = SyncOutcome::default();
    let ready = left_failure(b.vault())
        .and_then(|()| need_sync_tool(b).map(drop))
        .and_then(|()| if what == "background" { Ok(()) } else { need_sign_in(b) });
    if let Err(f) = ready {
        result.failure = Some(f);
        return result;
    }
    b.clear_log();
    let mut runs = Vec::new();
    let mut last: HashMap<&str, bool> = HashMap::new();
    // The failure that decided the outcome: of the latest engine run (an
    // answer from icloud-session may retry, and the retry's failure is the
    // one that counts), its worse half by the engine's own rule (exit 3
    // beats everything, then the higher code), so the exit matches the
    // engine's; on a tie the later half. {code, message, hint?} and exit.
    let mut engine_error: Map<String, Value> = Map::new();
    let mut engine_exit = i32::from(EXIT_ERROR);
    // The last run refused for the sign-in: sign_in_required or
    // session_rejected, as the engine said it.
    let mut sign_in_error: Map<String, Value> = Map::new();
    let sign_in = i32::from(EXIT_SIGN_IN);
    let worse = |a: i32, b: i32| {
        if b == sign_in {
            a == sign_in
        } else {
            a == sign_in || a >= b
        }
    };
    let said =
        |error: &Map<String, Value>, key: &str| error.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    for engine_run in b.run(kind_of(what, delete_missing)) {
        engine_error = Map::new(); // an earlier run's failure no longer decides
        for run in engine_run {
            runs.push(json!({"command": run.command, "ok": run.ok, "exit_code": run.exit_code, "result": run.result}));
            last.insert(run.command, run.ok);
            let code = said(&run.error, "code");
            if run.ok || code.is_empty() {
                continue;
            }
            if code == "sign_in_required" || code == "session_rejected" {
                sign_in_error = run.error.clone();
            }
            let exit = if run.exit_code > 0 {
                run.exit_code
            } else {
                i32::from(EXIT_ERROR)
            };
            if engine_error.is_empty() || worse(exit, engine_exit) {
                engine_error = run.error;
                engine_exit = exit;
            }
        }
    }
    let halves = what == "sync" || what == "background";
    if what == "background" && runs.is_empty() {
        result.failure = match b.last_step() {
            "signed_out" => Some(signed_out()),
            "session_unavailable" => Some(session_unknown()),
            _ => None,
        };
        return result;
    }

    let wanted: &[&str] = if halves { &["push", "pull"] } else { &[what] };
    let ok = !last.is_empty() && wanted.iter().all(|half| last.get(half) == Some(&true));
    let log = b.sync_log().to_string();
    result.json = json!({"ok": ok, "runs": runs, "log": log}).as_object().cloned();
    result.text = log;
    // Which failure the run reports: a refused sign-in the backend paused on
    // as the engine said it when it said session_rejected (icloud-session
    // was told by the session library), else in this tool's words
    // (sign_in_required: the backend told icloud-session); iCloud out of
    // reach in this tool's words; any other engine error as the engine said
    // it, code, message, hint and exit (account_mismatch,
    // not_cloned_directory, cloudkit_request_failed, ...); sync_failed only
    // for a run that failed without saying why (a crash, no engine output).
    let engine_code = said(&engine_error, "code");
    result.failure = if b.auth_expired() && said(&sign_in_error, "code") == "session_rejected" {
        Some(hinted(
            failure("session_rejected", EXIT_SIGN_IN, said(&sign_in_error, "message")),
            &said(&sign_in_error, "hint"),
        ))
    } else if b.auth_expired() {
        let refused = sign_in_message(Some("iCloud refused the sign-in; icloud-session was told"));
        Some(failure("sign_in_required", EXIT_SIGN_IN, refused).hint(SIGN_IN_HINT))
    } else if !ok && (engine_code == "offline" || engine_code == "network") {
        let unreachable = format!("iCloud could not be reached ({})", said(&engine_error, "message"));
        Some(failure(&engine_code, EXIT_UNAVAILABLE, unreachable).hint(NETWORK_HINT))
    } else if !ok && !engine_error.is_empty() {
        let exit = u8::try_from(engine_exit).unwrap_or(EXIT_ERROR);
        Some(hinted(
            failure(&engine_code, exit, said(&engine_error, "message")),
            &said(&engine_error, "hint"),
        ))
    } else if !ok {
        let what = if halves { "sync" } else { what };
        Some(failure(
            "sync_failed",
            EXIT_ERROR,
            format!("{what} failed; see the log"),
        ))
    } else {
        None
    };
    result
}

/// Becomes icloud-notes-sync itself (exec) for push --dry-run, history,
/// diff, restore and leave, in the vault: its output, errors and exit codes
/// are this tool's. It speaks the same output contract, so the output flags
/// are passed on as given (it resolves them the same way); with none,
/// --styled on a terminal and --json otherwise, which is what this tool
/// would pick. The engine takes the vault's lock where it needs it (restore;
/// the others only read), honouring --wait.
fn exec_engine(b: &Backend, a: &Args, tool_args: &[&str]) -> u8 {
    let tool = match need_sync_tool(b) {
        Ok(tool) => tool,
        Err(f) => return report(&f, a.mode),
    };
    let mut command = Command::new(tool);
    if a.flags.given.is_empty() {
        command.arg(if io::stdout().is_terminal() {
            "--styled"
        } else {
            "--json"
        });
    } else {
        command.args(&a.flags.given);
    }
    if let Some(wait) = a.value("--wait") {
        command.args(["--wait", wait]);
    }
    command.args(tool_args);
    let root = b.vault().root();
    if std::env::set_current_dir(root).is_err() {
        return report(&error(format!("cannot enter {root}")), a.mode);
    }
    // Only returns on failure; on success the engine replaces this process.
    let failed = command.exec();
    report(
        &error(format!("icloud-notes-sync did not run: {}", os_error_text(&failed))),
        a.mode,
    )
}

// ---- input -------------------------------------------------------------------

/// The text a command was given: --body, --file or --stdin, once. `None`
/// when it was given none.
fn read_body(a: &Args) -> Result<Option<String>, CliError> {
    let sources = [
        a.value("--body").is_some(),
        a.value("--file").is_some(),
        a.has("--stdin"),
    ];
    if sources.iter().filter(|given| **given).count() > 1 {
        return Err(usage_error("give the text once: --body, --file or --stdin"));
    }
    if let Some(text) = a.value("--body") {
        return Ok(Some(text.to_string()));
    }
    if let Some(path) = a.value("--file") {
        let unreadable = |why: String| error(format!("cannot read {path}: {why}"));
        if Path::new(path).is_dir() {
            return Err(unreadable("file to open is a directory".into()));
        }
        return match std::fs::read(path) {
            Ok(bytes) => Ok(Some(utf8(&bytes))),
            Err(e) => Err(unreadable(os_error_text(&e))),
        };
    }
    if a.has("--stdin") {
        let mut bytes = Vec::new();
        if io::stdin().lock().read_to_end(&mut bytes).is_err() {
            return Err(error("cannot read stdin"));
        }
        return Ok(Some(utf8(&bytes)));
    }
    Ok(None)
}

/// Asks unless --yes, only in text mode with stdin and stdout a terminal;
/// anywhere else it refuses (exit 2).
fn confirm(a: &Args, question: &str) -> Result<(), CliError> {
    if a.has("--yes") {
        return Ok(());
    }
    if !interactive(a.mode) {
        return Err(failure(
            "usage",
            EXIT_USAGE,
            format!(
                "{} asks before it acts, and cannot ask here (no terminal, or a machine output mode)",
                a.command
            ),
        )
        .hint("Pass --yes to go ahead."));
    }
    if icloud_session::cli::confirm(question) {
        return Ok(());
    }
    Err(failure("cancelled", EXIT_ERROR, "cancelled"))
}

/// [`confirm`] for a write that costs something in iCloud: where it cannot
/// ask, it says what the write would do, and how to go ahead.
fn confirm_write(a: &Args, question: &str, message: String, hint: String) -> Result<(), CliError> {
    confirm(a, question).map_err(|f| {
        if f.code == "usage" {
            failure("usage", EXIT_USAGE, message).hint(hint)
        } else {
            f
        }
    })
}

// ---- the commands --------------------------------------------------------------

/// A sync (or a pull, push, clone) that ran but failed keeps its result,
/// with the failure in it as `error`; one that never ran is the failure.
fn with_error(json: Option<Map<String, Value>>, f: &CliError) -> Value {
    let mut json = json.unwrap_or_default();
    json.insert("ok".into(), json!(false));
    json.insert("error".into(), failure_json(f));
    Value::Object(json)
}

/// A refusal of the backend's (its message, and the class it gave it) as a
/// failure with a code a caller can branch on.
fn refused(r: Refusal) -> CliError {
    match r.code {
        "usage" => usage_error(r.message),
        "not_found" => hinted(failure(r.code, EXIT_NOT_FOUND, r.message), &r.hint),
        "exists" => hinted(failure(r.code, EXIT_USAGE, r.message), &r.hint),
        code => hinted(failure(code, EXIT_ERROR, r.message), &r.hint),
    }
}

/// A change's result, and with --push the sync after it: in `sync` (with
/// `error` when it failed, the change itself done all the same, and the exit
/// code the sync's).
fn finish(b: &mut Backend, a: &Args, result: Result<Done, CliError>) -> u8 {
    let mut done = match result {
        Ok(done) => done,
        Err(f) => return report(&f, a.mode),
    };
    // A change's one line is its summary; its id the file it made (a PDF)
    // or the note or folder it touched.
    if done.summary.is_empty() {
        done.summary = done.text.clone();
    }
    if done.ids.is_none() {
        let id = ["pdf", "path", "folder"].iter().find_map(|key| done.json.get(key));
        done.ids = id.map(|id| vec![id.as_str().unwrap_or_default().to_string()]);
    }
    let mut sync_failure = None;
    if a.has("--push") {
        let sync = run_sync(b, "sync", false);
        sync_failure = sync.failure.map(|mut f| {
            f.message = format!("{} done here, but not synced: {}", a.command, f.message);
            f
        });
        done.json["sync"] = match &sync_failure {
            Some(f) => with_error(sync.json, f),
            None => Value::Object(sync.json.unwrap_or_default()),
        };
        if !sync.text.is_empty() {
            done.text += &format!("\n{}", sync.text);
        }
        done.summary += if sync_failure.is_some() {
            "; not synced"
        } else {
            "; synced"
        };
        if let Some(f) = &sync_failure {
            done.exit = f.exit;
        }
    }
    print_result_with(&done, sync_failure.as_ref(), a.mode)
}

fn status(b: &mut Backend) -> Done {
    b.read_sign_in();
    let holder = match b.lock_vault() {
        Taken::Locked => {
            b.unlock_vault();
            String::new()
        }
        Taken::Busy => b.lock_holder(),
        Taken::Failed => String::new(),
    };
    let (cloned, left) = (b.vault().cloned(), b.vault().left_icloud());
    let mut flagged: Map<String, Value> = Map::new();
    let mut count = 0;
    if cloned || left {
        let notes = scan_notes(b, "");
        count = notes.len();
        for note in &notes {
            for flag in &note.flags {
                let paths = flagged.entry(*flag).or_insert_with(|| json!([]));
                paths.as_array_mut().expect("an array").push(json!(note.path()));
            }
        }
    }
    let folders = if cloned || left { b.folders().len() - 1 } else { 0 };
    let known = b.sign_in_known();
    let root = b.vault().root().to_string();
    let tool = b.engine().map(Path::to_path_buf);
    let data = json!({
        "vault": root,
        "cloned": cloned,
        "left_icloud": left,
        "title_mode": b.vault_title_mode(),
        "sync_tool": tool.is_some(),
        "sign_in_known": known,
        "signed_in": if known { json!(b.signed_in()) } else { Value::Null },
        "apple_id": or_null(b.apple_id()),
        "sign_in_days_left": b.sign_in_days_left(),
        "lock": {"held_by": or_null(&holder), "app_open": is_app_holder(&holder)},
        "notes": count,
        "folders": folders,
        "flagged": flagged,
    });
    let mut lines = vec![
        format!(
            "Vault:       {root}{}",
            if left {
                " (left iCloud: Omacloud syncs it)"
            } else if cloned {
                ""
            } else {
                " (not cloned)"
            }
        ),
        format!(
            "Signed in:   {}",
            if !known {
                "unknown (icloud-session is not available)".to_string()
            } else if b.signed_in() {
                format!("yes, as {}", b.apple_id())
            } else {
                "no (icloud-session sign-in)".to_string()
            }
        ),
        format!(
            "Sync tool:   {}",
            tool.map_or("missing".to_string(), |tool| tool.display().to_string())
        ),
        format!("Lock:        {}", if holder.is_empty() { "free" } else { &holder }),
        format!("Notes:       {count} in {folders} folders"),
    ];
    let mut flags: Vec<(&String, &Value)> = flagged.iter().collect();
    flags.sort_by_key(|(flag, _)| *flag);
    lines.extend(
        flags
            .iter()
            .map(|(flag, paths)| format!("  {flag}: {}", paths.as_array().map_or(0, Vec::len))),
    );
    let sign_in = if !known {
        "sign-in unknown".to_string()
    } else if b.signed_in() {
        format!("signed in as {}", b.apple_id())
    } else {
        "signed out".to_string()
    };
    let notes = plural(count, "note", "notes");
    let summary = if left {
        format!("{notes}, left iCloud")
    } else if cloned {
        format!("{notes}, {sign_in}")
    } else {
        format!("not cloned, {sign_in}")
    };
    let mut done = Done::new(data, lines.join("\n"), summary);
    done.count = Some(count);
    // Signed out is reported, and exits 3 (the result printed all the same);
    // a vault that left iCloud needs no sign-in.
    done.exit = if !left && known && !b.signed_in() {
        EXIT_SIGN_IN
    } else {
        EXIT_OK
    };
    done
}

fn folders(b: &mut Backend) -> Result<Done, CliError> {
    let default = b.default_folder();
    let mut list = Vec::new();
    let mut lines = Vec::new();
    let mut ids = Vec::new();
    for folder in b.folders() {
        if !folder.is_empty() {
            ids.push(folder.clone());
        }
        let notes = b.folder_note_counts().get(folder).copied().unwrap_or(0);
        list.push(json!({
            "folder": folder,
            "name": if folder.is_empty() { "All Notes" } else { split_path(folder).1 },
            "notes": notes,
            "default": !folder.is_empty() && *folder == default,
        }));
        lines.push(format!(
            "{notes}\t{}",
            if folder.is_empty() { "(All Notes)" } else { folder }
        ));
    }
    // The ids and the count are the folders, not the vault root (All Notes).
    let mut done = Done::new(json!(list), lines.join("\n"), plural(ids.len(), "folder", "folders"));
    done.ids = Some(ids);
    Ok(done)
}

fn list(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let folder = match a.value("--folder") {
        Some(folder) => resolve_folder(b, folder)?,
        None => String::new(),
    };
    let flag = a.value("--flag");
    let mut items = Vec::new();
    let mut lines = Vec::new();
    let mut ids = Vec::new();
    for note in scan_notes(b, &folder) {
        if flag.is_some_and(|flag| !note.flags.contains(&flag)) {
            continue;
        }
        items.push(note.json());
        ids.push(note.path());
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            note.path(),
            note.title,
            iso(note.modified_ms),
            note.flags.join(",")
        ));
    }
    let mut done = Done::new(json!(items), lines.join("\n"), plural(ids.len(), "note", "notes"));
    done.ids = Some(ids);
    Ok(done)
}

fn read(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let note = resolve_and_open(b, &a.positional[0])?;
    let attachments: Vec<Value> = b
        .note_attachments()
        .iter()
        .map(|attachment| json!({"name": attachment.name, "path": attachment.path.display().to_string(), "image": attachment.image}))
        .collect();
    let conflicts: Vec<Value> = b
        .note_conflicts()
        .iter()
        .map(|conflict| {
            let lines =
                |side: &[crate::backend::ConflictLine]| side.iter().map(|line| line.text.clone()).collect::<Vec<_>>();
            json!({
                "local": lines(&conflict.local),
                "remote": lines(&conflict.remote),
                "before": conflict.before,
                "after": conflict.after,
            })
        })
        .collect();
    let raw = a.has("--raw");
    let mut data = note.json();
    let fields = data.as_object_mut().expect("an object");
    fields.remove("snippet");
    fields.insert("read_only".into(), or_null(b.read_only_reason()));
    fields.insert("body".into(), json!(b.note_body()));
    if raw {
        fields.insert("text".into(), json!(b.note_content()));
    }
    fields.insert("attachments".into(), json!(attachments));
    fields.insert("conflicts".into(), json!(conflicts));
    fields.insert("conflicts_unreadable".into(), json!(b.note_conflicts_unreadable()));
    fields.insert("has_synced_copy".into(), json!(b.note_has_synced_copy()));
    let text = if raw { b.note_content() } else { b.note_body() };
    let mut done = Done::new(data, text, format!("{} ({})", note.title, note.path()));
    done.ids = Some(vec![note.path()]);
    Ok(done)
}

fn search(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let query = a.positional[0].trim();
    if utf16_len(query) < 2 {
        return Err(usage_error("search wants at least 2 characters"));
    }
    let mut items = Vec::new();
    let mut lines = Vec::new();
    let mut ids = Vec::new();
    for hit in b.search_vault(query) {
        let path = join_path(&hit.folder, &hit.file);
        items.push(json!({
            "path": path,
            "folder": hit.folder,
            "file": hit.file,
            "title": hit.title,
            "snippet": hit.snippet,
        }));
        lines.push(format!("{path}\t{}\t{}", hit.title, hit.snippet));
        ids.push(path);
    }
    let summary = format!("{} matching \"{query}\"", plural(ids.len(), "note", "notes"));
    let mut done = Done::new(json!(items), lines.join("\n"), summary);
    done.ids = Some(ids);
    Ok(done)
}

/// `old`, then `text` on a line of its own.
fn appended(old: &str, text: &str) -> String {
    if old.is_empty() || old.ends_with('\n') {
        format!("{old}{text}")
    } else {
        format!("{old}\n{text}")
    }
}

fn new_note(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let mut folder = b.default_folder();
    if !b.folders().contains(&folder) {
        folder.clear();
    }
    if let Some(given) = a.value("--folder") {
        folder = resolve_folder(b, given)?;
    }
    let body = read_body(a)?;
    let title = Backend::clean_name(&a.positional[0]);
    if title.is_empty() {
        return Err(usage_error("the title is empty"));
    }
    let reason = Backend::unsynced_name_reason(&title, false);
    if !reason.is_empty() {
        return Err(usage_error(reason));
    }
    let file = if title.to_lowercase().ends_with(".md") {
        title.clone()
    } else {
        format!("{title}.md")
    };
    b.set_current_folder(&folder);
    b.refresh();
    if b.notes().contains(&file) || b.vault().folder(&folder).join(&file).exists() {
        let place = if folder.is_empty() { "the vault root" } else { &folder };
        return Err(failure(
            "exists",
            EXIT_USAGE,
            format!("a note named \"{file}\" is already in {place}"),
        )
        .hint("Edit it with `icloud-notes write`, or pick another title."));
    }
    b.new_note(&title);
    if b.current_note() != file {
        return Err(error(format!("could not create {file}")));
    }
    if let Some(body) = body {
        let mut text = appended(b.note_body(), &body);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if !b.save_note(&text, None) {
            return Err(error(format!(
                "created {} but could not write its text",
                b.current_path()
            )));
        }
    }
    let path = b.current_path();
    Ok(Done::change(
        json!({"action": "new", "path": path, "folder": b.current_folder(), "file": b.current_note(), "title": title}),
        format!("Created {path}"),
    ))
}

fn write(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let Some(text) = read_body(a)? else {
        return Err(usage_error("write needs the text: --body TEXT, --file PATH or --stdin"));
    };
    let note = resolve_and_open(b, &a.positional[0])?;
    let path = note.path();
    if !b.read_only_reason().is_empty() {
        return Err(failure(
            "read_only",
            EXIT_ERROR,
            format!("{path} is read-only here: this note {}", b.read_only_reason()),
        )
        .hint("Edit it in Apple Notes; the changes still sync here."));
    }
    let mut body = if a.has("--append") {
        appended(b.note_body(), &text)
    } else {
        text
    };
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    // Notes discards a note left empty, and so does the next push: emptying
    // one deletes it in iCloud, which is `delete`'s to do (or --yes). Blank
    // lines are no text either: the engine drops them after the ID block.
    if body.chars().all(|c| c == '\n') && b.vault_title_mode() != "filename" {
        confirm_write(
            a,
            &format!("Empty {path}? The next push moves it to Recently Deleted in iCloud."),
            format!("the text is empty, and the next push would delete {path} in iCloud"),
            "To delete it, use `icloud-notes delete NOTE --yes`; to empty it anyway, pass --yes.".into(),
        )?;
    }
    // A link gone from the text removes its attachment in iCloud at the
    // next push: never without --yes (or a yes on a terminal).
    let removed = b.removed_attachments(&body);
    if !removed.is_empty() {
        let names = removed.join(", ");
        let (what, it) = if removed.len() == 1 {
            ("attachment", "it")
        } else {
            ("attachments", "them")
        };
        confirm_write(
            a,
            &format!("Remove the {what} {names} from {path}? The next push removes {it} on all your devices."),
            format!(
                "the text no longer links the {what} {names} of {path}, and the next push would remove {it} on all \
                 your devices"
            ),
            format!("Keep the link(s) as `icloud-notes read` shows them, or pass --yes to remove the {what}."),
        )?;
    }
    let warning = b.guardrail_warning(&body);
    if !warning.is_empty() && !a.has("--force") {
        return Err(failure("guardrail", EXIT_ERROR, warning)
            .hint("Nothing was written. Pass --force to write it anyway (as Ctrl+S does in Notes)."));
    }
    let before = b.note_content().to_string();
    if !b.save_note(&body, None) {
        return Err(error(format!("could not write {path}")));
    }
    let changed = b.note_content() != before;
    let path = b.current_path();
    Ok(Done::change(
        json!({"action": "write", "path": path, "changed": changed}),
        if changed {
            format!("Wrote {path}")
        } else {
            format!("{path} unchanged")
        },
    ))
}

fn rename(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let note = resolve_and_open(b, &a.positional[0])?;
    b.rename_current_note(&a.positional[1]).map_err(refused)?;
    let title = a.positional[1].trim();
    Ok(Done::change(
        json!({"action": "rename", "path": b.current_path(), "from": note.path(), "title": title}),
        format!("Renamed {} to \"{title}\"", note.path()),
    ))
}

fn move_note(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let note = resolve_and_open(b, &a.positional[0])?;
    let folder = resolve_folder(b, &a.positional[1])?;
    b.move_current_note(&folder).map_err(refused)?;
    let path = b.current_path();
    Ok(Done::change(
        json!({"action": "move", "path": path, "from": note.path()}),
        format!("Moved {} to {path}", note.path()),
    ))
}

fn delete_note(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let note = resolve_note(b, &a.positional[0])?;
    let path = note.path();
    confirm(
        a,
        &format!("Move \"{path}\" to the trash? The next push moves it to Recently Deleted in iCloud."),
    )?;
    open_note(b, &note)?;
    b.delete_current_note().map_err(refused)?;
    Ok(Done::change(
        json!({"action": "delete", "path": path}),
        format!("Moved {path} to the trash"),
    ))
}

fn toggle(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let given = &a.positional[1];
    let Some(line) = to_int(given).filter(|line| *line >= 1) else {
        return Err(usage_error(format!(
            "LINE is a line number, 1 or more, not \"{given}\""
        )));
    };
    let note = resolve_and_open(b, &a.positional[0])?;
    if !b.read_only_reason().is_empty() {
        return Err(failure(
            "read_only",
            EXIT_ERROR,
            format!("This note is read-only here: it {}", b.read_only_reason()),
        ));
    }
    let body = b.note_body().to_string();
    let toggled = syncmodel::toggle_checkbox(&body, i64::from(line) - 1);
    if toggled == body {
        return Err(failure(
            "not_a_list_item",
            EXIT_ERROR,
            format!("line {line} of {} is not a list item", note.path()),
        ));
    }
    if !b.save_note(&toggled, None) {
        return Err(error(format!("could not write {}", note.path())));
    }
    let text = b
        .note_body()
        .split('\n')
        .nth(line as usize - 1)
        .unwrap_or_default()
        .to_string();
    let path = b.current_path();
    let mut done = Done::change(
        json!({"action": "toggle", "path": path, "line": line, "text": text}),
        text.clone(),
    );
    done.summary = format!("Line {line} of {path}: {text}");
    Ok(done)
}

/// The vault-relative path of an attachment of the open note's folder.
fn attachment_path(b: &Backend, name: &str) -> String {
    join_path(b.current_folder(), &format!("attachments/{name}"))
}

fn attach(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let note = resolve_and_open(b, &a.positional[0])?;
    let added = b.add_attachment(&a.positional[1]).map_err(refused)?;
    let old = b.note_body();
    let newline = if old.is_empty() || old.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let body = format!("{old}{newline}{}\n", added.link);
    if !b.save_note(&body, None) {
        let _ = std::fs::remove_file(&added.path);
        return Err(error(format!("could not write {}", note.path())));
    }
    let path = b.current_path();
    Ok(Done::change(
        json!({"action": "attach", "path": path, "file": attachment_path(b, &added.name), "link": added.link}),
        format!("Attached {} to {path}", added.name),
    ))
}

fn detach(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let note = resolve_and_open(b, &a.positional[0])?;
    let path = note.path();
    if !b.read_only_reason().is_empty() {
        return Err(failure(
            "read_only",
            EXIT_ERROR,
            format!("{path} is read-only here: this note {}", b.read_only_reason()),
        )
        .hint("Remove the attachment in Apple Notes; the change still syncs here."));
    }
    let name = &a.positional[1];
    let names: Vec<String> = b
        .note_attachments()
        .iter()
        .map(|attachment| attachment.name.clone())
        .collect();
    let old = b.note_body().to_string();
    let body = crate::attachments::without_attachment_link(&old, name);
    if !names.contains(name) || body == old || b.removed_attachments(&body) != [name.as_str()] {
        return Err(failure(
            "not_found",
            EXIT_NOT_FOUND,
            format!("{path} has no attachment named \"{name}\""),
        )
        .hint(if names.is_empty() {
            "The note has no attachments.".to_string()
        } else {
            format!("Its attachments: {}.", names.join(", "))
        }));
    }
    confirm(
        a,
        &format!("Remove the attachment {name} from {path}? The next push removes it on all your devices."),
    )?;
    if !b.save_note(&body, None) {
        return Err(error(format!("could not write {path}")));
    }
    let path = b.current_path();
    Ok(Done::change(
        json!({"action": "detach", "path": path, "file": attachment_path(b, name)}),
        format!("Removed the attachment {name} from {path}"),
    ))
}

fn resolve(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let (all, choices) = (a.value("--all"), a.value("--choices"));
    if all.is_some() == choices.is_some() {
        return Err(usage_error(
            "resolve needs --all local|remote|both or --choices C1,C2,...",
        ));
    }
    let note = resolve_and_open(b, &a.positional[0])?;
    let path = note.path();
    if b.note_conflicts_unreadable() {
        return Err(failure(
            "conflicts_unreadable",
            EXIT_ERROR,
            format!("{path} has conflict markers the version picker cannot read"),
        )
        .hint("Use `icloud-notes recover NOTE --strip` (keep every line) or --synced (the last synced text)."));
    }
    let blocks = b.note_conflicts().len();
    if blocks == 0 {
        return Err(failure(
            "no_conflicts",
            EXIT_ERROR,
            format!("{path} has no conflict to resolve"),
        ));
    }
    let picked: Vec<String> = match (all, choices) {
        (Some(all), _) => vec![all.trim().to_string(); blocks],
        (None, choices) => {
            let given = choices
                .unwrap_or_default()
                .split(',')
                .filter(|choice| !choice.is_empty());
            given.map(|choice| choice.trim().to_string()).collect()
        }
    };
    if let Some(choice) = picked
        .iter()
        .find(|c| !matches!(c.as_str(), "local" | "remote" | "both"))
    {
        return Err(usage_error(format!(
            "a choice is local, remote or both, not \"{choice}\""
        )));
    }
    if picked.len() != blocks {
        return Err(failure(
            "choices_mismatch",
            EXIT_USAGE,
            format!(
                "{path} has {blocks} conflict blocks, and {} choices were given",
                picked.len()
            ),
        )
        .hint("`icloud-notes read NOTE --json` lists the blocks."));
    }
    b.resolve_conflicts(&picked).map_err(refused)?;
    let path = b.current_path();
    Ok(Done::change(
        json!({"action": "resolve", "path": path, "choices": picked}),
        format!("Resolved {blocks} conflict block(s) in {path}"),
    ))
}

fn recover(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    if a.has("--strip") == a.has("--synced") {
        return Err(usage_error("recover needs --strip or --synced"));
    }
    let how = if a.has("--strip") { "strip" } else { "synced" };
    resolve_and_open(b, &a.positional[0])?;
    let recovered = b.recover_conflicted_note(how).map_err(refused)?;
    Ok(Done::change(
        json!({
            "action": "recover",
            "path": b.current_path(),
            "how": how,
            "backup": recovered.backup.display().to_string(),
            "message": recovered.message,
        }),
        recovered.message,
    ))
}

fn new_folder(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let parent = match a.value("--in") {
        Some(parent) => resolve_folder(b, parent)?,
        None => String::new(),
    };
    let name = Backend::clean_name(&a.positional[0]);
    if name.is_empty() {
        return Err(usage_error("the folder name is empty"));
    }
    let reason = Backend::unsynced_name_reason(&name, true);
    if !reason.is_empty() {
        return Err(usage_error(reason));
    }
    let folder = join_path(&parent, &name);
    if b.folders().contains(&folder) {
        return Err(failure(
            "exists",
            EXIT_USAGE,
            format!("folder \"{folder}\" already exists"),
        ));
    }
    b.set_current_folder(&parent);
    b.new_folder(&name);
    if !b.folders().contains(&folder) {
        return Err(error(format!("could not create folder \"{folder}\"")));
    }
    Ok(Done::change(
        json!({"action": "new-folder", "folder": folder}),
        format!("Created folder {folder}"),
    ))
}

fn rename_folder(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let folder = resolve_folder(b, &a.positional[0])?;
    b.set_current_folder(&folder);
    b.rename_current_folder(&a.positional[1]).map_err(refused)?;
    let renamed = b.current_folder();
    Ok(Done::change(
        json!({"action": "rename-folder", "folder": renamed, "from": folder}),
        format!("Renamed folder {folder} to {renamed}"),
    ))
}

fn delete_folder(b: &mut Backend, a: &Args) -> Result<Done, CliError> {
    let folder = resolve_folder(b, &a.positional[0])?;
    if folder.is_empty() {
        return Err(error("All Notes cannot be deleted."));
    }
    let count = b.folder_note_counts().get(&folder).copied().unwrap_or(0);
    confirm(
        a,
        &format!(
            "Move \"{folder}\" and its {count} note(s) to the trash? The next push moves the notes to Recently \
             Deleted in iCloud."
        ),
    )?;
    b.set_current_folder(&folder);
    b.delete_current_folder().map_err(refused)?;
    Ok(Done::change(
        json!({"action": "delete-folder", "folder": folder}),
        format!("Moved folder {folder} to the trash"),
    ))
}

fn sync_command(b: &mut Backend, a: &Args, what: &str) -> Result<u8, CliError> {
    left_failure(b.vault())?;
    if what == "clone" {
        if b.vault().cloned() {
            return Err(failure(
                "already_cloned",
                EXIT_USAGE,
                format!("{} is already cloned", b.vault().root()),
            )
            .hint("Run `icloud-notes pull` to fetch what changed."));
        }
        let _ = std::fs::create_dir_all(b.vault().path());
    } else {
        need_cloned(b.vault())?;
    }
    need_sync_tool(b)?;
    take_lock(b, a)?;
    let sync = run_sync(b, what, a.has("--yes"));
    let (json, failure) = match (sync.json, sync.failure) {
        (None, Some(f)) => return Err(f),
        (json, failure) => (json, failure),
    };
    // It ran: its result (the runs and the log) whether or not it worked,
    // with the failure in it and its exit code.
    let runs = json
        .as_ref()
        .and_then(|json| json.get("runs")?.as_array())
        .map_or(0, Vec::len);
    let mut done = match &failure {
        Some(f) => Done::new(with_error(json, f), sync.text, format!("{what} failed: {}", f.message)),
        None => Done::new(
            Value::Object(json.unwrap_or_default()),
            sync.text,
            format!("{what} done"),
        ),
    };
    done.count = Some(runs);
    if let Some(f) = &failure {
        done.exit = f.exit;
    }
    Ok(print_result_with(&done, failure.as_ref(), a.mode))
}

fn run_command(a: &Args, spec: &Spec) -> Result<Ran, CliError> {
    let named = match a.value("--vault") {
        Some(vault) if vault.trim().is_empty() => return Err(usage_error("--vault needs a directory")),
        named => named,
    };
    let cmd = spec.name;
    // The page is laid out by the app's binary, which runs the whole command.
    if cmd == "export-pdf" {
        return Ok(Ran::NeedsApp);
    }
    let mut backend = Backend::new(Vault::locate(named), Role::Cli);
    let b = &mut backend;
    let exit = |code: u8| Ok(Ran::Exit(code));

    if cmd == "status" {
        return exit(print_result(&status(b), a.mode));
    }
    if matches!(cmd, "sync" | "pull" | "clone") || (cmd == "push" && !a.has("--dry-run")) {
        return exit(sync_command(b, a, cmd)?);
    }
    let yes: &[&str] = if a.has("--yes") { &["--yes"] } else { &[] };
    // the engine's own leave: it asks, syncs once more, and hands the vault over
    if cmd == "leave" {
        let root = b.vault().root().to_string();
        return exit(exec_engine(b, a, &[&["leave", &root], yes].concat()));
    }
    need_cloned(b.vault())?;
    if matches!(cmd, "push" | "history" | "diff" | "restore") {
        left_failure(b.vault())?;
    }

    // icloud-notes-sync's own commands, on a resolved note.
    if cmd == "push" {
        // --yes with it: the preview of what push --yes would send
        return exit(exec_engine(b, a, &[&["push", "--dry-run"], yes].concat()));
    }
    if matches!(cmd, "history" | "diff" | "restore") {
        let path = resolve_note(b, &a.positional[0])?.path();
        if cmd == "restore" {
            confirm(
                a,
                &format!("Throw away the local edits to \"{path}\" and go back to the last synced copy?"),
            )?;
        }
        let mut args = vec![cmd, path.as_str()];
        if cmd == "diff" {
            args.push(&a.positional[1]);
        }
        if cmd == "history" && a.has("--records") {
            args.push("--records");
        }
        return exit(exec_engine(b, a, &args));
    }

    // Reading needs no lock.
    let read_only = match cmd {
        "folders" => Some(folders(b)),
        "list" => Some(list(b, a)),
        "read" => Some(read(b, a)),
        "search" => Some(search(b, a)),
        _ => None,
    };
    if let Some(result) = read_only {
        return exit(print_result(&result?, a.mode));
    }

    // Changing the vault: under its lock, so no sync runs meanwhile.
    take_lock(b, a)?;
    b.refresh();
    let result = match cmd {
        "new" => new_note(b, a),
        "write" => write(b, a),
        "rename" => rename(b, a),
        "move" => move_note(b, a),
        "delete" => delete_note(b, a),
        "toggle" => toggle(b, a),
        "attach" => attach(b, a),
        "detach" => detach(b, a),
        "resolve" => resolve(b, a),
        "recover" => recover(b, a),
        "new-folder" => new_folder(b, a),
        "rename-folder" => rename_folder(b, a),
        "delete-folder" => delete_folder(b, a),
        // A command of the table that nothing above runs.
        other => Err(usage_error(format!("unknown command \"{other}\""))),
    };
    exit(finish(b, a, result))
}

/// An argument as text: each byte that is not UTF-8 as one U+FFFD.
pub fn argument(arg: &OsStr) -> String {
    files::utf8_lossy(arg.as_bytes())
}

/// Whether `arg`, the first argument, starts a command (or global options,
/// help, the version) rather than the window. `--sync`, the background
/// sync, is not one.
pub fn is_cli_invocation(arg: &str) -> bool {
    const STARTS: [&str; 7] = ["help", "-h", "--help", "-V", "--version", "--vault", "--wait"];
    arg != "--sync"
        && (STARTS.contains(&arg)
            || OUTPUT_FLAGS.contains(&arg)
            || arg.starts_with("--vault=")
            || arg.starts_with("--wait=")
            || help::find(arg).is_some())
}

/// Runs the command line on `argv` (without the program's name).
/// `export-pdf` is not run here ([`Ran::NeedsApp`]).
pub fn run(argv: &[String]) -> Ran {
    let a = match parse_args(argv) {
        Ok(a) => a,
        // The output mode, also for a usage error found before parsing ended.
        Err(f) => return Ran::Exit(report(&f, scan_output_flags(argv).mode())),
    };
    let fail = |f: CliError| Ran::Exit(report(&f, a.mode));
    if a.command == "help" || (a.command.is_empty() && a.has("--help")) {
        let text = match a.positional.first() {
            None => help::usage_text(),
            Some(name) => match help::find(name) {
                Some(spec) => help::command_help(spec),
                None => return fail(usage_error(format!("unknown command \"{name}\""))),
            },
        };
        out(&format!("{text}\n"));
        return Ran::Exit(EXIT_OK);
    }
    if a.command.is_empty() && a.has("--version") {
        // Plain `icloud-notes VERSION` in every mode, as every iCloud tool's.
        out(&format!("icloud-notes {}\n", env!("CARGO_PKG_VERSION")));
        return Ran::Exit(EXIT_OK);
    }
    if a.command.is_empty() {
        return fail(usage_error("no command given"));
    }
    let Some(spec) = help::find(&a.command) else {
        return fail(usage_error(format!("unknown command \"{}\"", a.command)));
    };
    if a.has("--help") {
        out(&format!("{}\n", help::command_help(spec)));
        return Ran::Exit(EXIT_OK);
    }
    match check_args(spec, &a).and_then(|()| run_command(&a, spec)) {
        Ok(ran) => ran,
        Err(f) => fail(f),
    }
}

/// A command the app's binary was needed for could not be handed to it:
/// the failure, in the output mode `argv` asks for.
pub fn report_no_app(argv: &[String], app: &Path, why: &io::Error) -> u8 {
    let mode = parse_args(argv).map_or_else(|_| scan_output_flags(argv).mode(), |a| a.mode);
    let f = error(format!(
        "the Notes app is needed to lay out a PDF, and {} did not run: {}",
        app.display(),
        os_error_text(why)
    ))
    .hint("Reinstall icloud-notes: sudo pacman -S icloud-notes");
    report(&f, mode)
}

/// `icloud-notes --sync`: what the systemd user timer runs every minute
/// while Notes is closed: the engine's step (`icloud-notes-sync
/// background`), which syncs every 15 minutes and within about a minute of a
/// local change (an editor's, an agent's) to push, and otherwise does
/// nothing. The decision is the engine's (notes-sync/src/cmd/background.rs);
/// the window runs the same step while it is open. Once it syncs, the `sync`
/// command's path through the backend (conflict handling, reporting a
/// refused session to icloud-session, the one retry).
///
/// It never waits for the lock and does nothing (0) when there is no vault
/// or the window or another sync holds the vault's lock; a due sync with
/// nobody signed in to icloud-session (or it unknown) is a one-line skip
/// (0). Returns 0 once both halves worked (or nothing was due), non-zero
/// when either failed or the engine is missing. What it did is printed; a
/// minute with nothing to do prints nothing.
pub fn run_background_sync() -> u8 {
    let vault = Vault::locate(None);
    // Asked every minute until there is one: nothing to say.
    if !vault.cloned() {
        return EXIT_OK;
    }
    let root = vault.root().to_string();
    // Never waits for the lock: whoever holds it (the window, which runs
    // the same step itself, or another sync) syncs this vault already.
    let mut b = Backend::new(vault, Role::Background);
    match b.lock_vault() {
        Taken::Locked => {}
        Taken::Busy => return EXIT_OK,
        Taken::Failed => {
            out(&format!(
                "Could not open the sync lock {}; not syncing.\n",
                b.vault().lock_path().display()
            ));
            return EXIT_ERROR;
        }
    }
    let sync = run_sync(&mut b, "background", false);
    match (&sync.json, &sync.failure) {
        // Nothing due: nothing to say.
        (None, None) => return EXIT_OK,
        // Nothing ran: signed out or the sign-in unknown is a skip (the
        // timer tries again), a missing engine an error.
        (None, Some(f)) => {
            let skip = f.code == "sign_in_required" || f.code == "session_service";
            let end = if skip {
                "; skipped.".to_string()
            } else {
                format!(" ({})", f.hint.as_deref().unwrap_or_default())
            };
            out(&format!("{}{end}\n", f.message));
            return if skip { EXIT_OK } else { EXIT_ERROR };
        }
        // No network: one line, and a skip like signed out (the timer tries again).
        (Some(_), Some(f)) if f.code == "offline" || f.code == "network" => {
            out(&format!("{}; skipped, the next sync tries again.\n", f.message));
            return EXIT_OK;
        }
        _ => {}
    }
    let mut said = format!("{}\n", sync.text);
    let failed = match &sync.failure {
        Some(f) if f.code == "sign_in_required" => {
            said += "iCloud refused the sign-in; icloud-session was told. Sync paused until you sign in.\n";
            true
        }
        Some(f) if f.code == "session_rejected" => {
            let hint = f.hint.as_deref().unwrap_or_default();
            said += &format!("Apple refused the iCloud session icloud-session holds; sign in again ({hint}).\n");
            true
        }
        failure => {
            said += &format!(
                "{}{root}.\n",
                if failure.is_some() {
                    "Sync failed for "
                } else {
                    "Synced "
                }
            );
            failure.is_some()
        }
    };
    out(&said);
    if failed { EXIT_ERROR } else { EXIT_OK }
}
