//! Runs of the sync engine through the backend, with the vault's lock held:
//! what was run, what each half answered, the log, and what a refused
//! sign-in leads to. The engine's whole stdout is JSON (every run is in a
//! machine mode), parsed and never logged as it is; its stderr is progress
//! and warnings, which are logged.

use std::time::Duration;

use icloud_session::Status;
use serde_json::{Map, Value, json};

use crate::backend::Backend;
use crate::engine::{self, Finished, Running, TOOL};
use crate::lock::Taken;
use crate::session;

pub const NOT_INSTALLED: &str = "The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes to sync.";
/// The log is a readout, not history: its last so many characters are kept.
const LOG_LIMIT: usize = 200_000;
/// How often a run that waits for the lock looks again.
const LOCK_RETRY: Duration = Duration::from_millis(500);

/// What to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Push what changed here, then pull, in one engine run. `delete_missing`
    /// lets the push delete most of the vault (`--yes`).
    Sync {
        delete_missing: bool,
    },
    /// The engine's step: a sync when one is due (every `every_secs`, or soon
    /// after a local change), else nothing.
    Background {
        every_secs: u32,
    },
    Pull,
    Push {
        delete_missing: bool,
    },
    /// Download every note into the (empty) vault, for the account
    /// icloud-session is signed in to.
    Clone,
}

/// How an engine run's output is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    /// A `--json` run: the envelope, or the error document.
    #[default]
    Plain,
    /// `--quiet sync`: both halves in one answer.
    Sync,
    /// `--quiet background`: a sync's answer, or that nothing was due.
    Step,
}

/// What [`Backend::start`] started.
#[derive(Debug)]
pub enum Started {
    /// Nothing: a run is under way already, the vault left iCloud, a clone
    /// has no account to clone, or the background step has no engine.
    Nothing,
    /// A run that ended before it began (no engine, no lock): its runs, failed.
    Ended(Vec<Run>),
    /// The engine: [`Backend::finish`] wants what it leaves.
    Running(Running),
}

/// One finished run, or one half of a sync: what the engine was asked
/// ("push", "pull", "clone"), whether it worked, its exit code, and its
/// result (`{error: {...}}` when it failed with one, null when it said nothing).
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub command: &'static str,
    pub ok: bool,
    pub exit_code: i32,
    pub result: Value,
    /// The engine's error `{code, message, hint?}`; empty when it worked or
    /// said nothing (a crash, an engine that printed only text).
    pub error: Map<String, Value>,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    /// The engine runs: saves wait for it to end.
    pub(crate) running: bool,
    mode: Mode,
    label: &'static str,
    log: String,
    /// The engine's step, logged only once it syncs.
    step_log: Vec<String>,
    last_step: String,
    state_stamp_before: String,
    /// Refused sign-ins to tell icloud-session about, once the run is over.
    reports: usize,
    /// One retry after icloud-session vouched for the sign-in, until a pull
    /// works.
    retried_after_report: bool,
    auth_expired: bool,
    sign_in: Option<Status>,
    finished: Vec<Run>,
}

fn object(bytes: &[u8]) -> Map<String, Value> {
    match serde_json::from_slice(bytes) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn child<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    map.get(key).and_then(Value::as_object)
}

fn text<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The contract error object `{code, message, hint?}` when `output` (the
/// engine's whole stdout) is the document `{"ok":false,"error":{...}}`.
/// Parsed as JSON: key order and layout are not part of the contract.
fn contract_error(output: &[u8]) -> Option<Map<String, Value>> {
    let document = object(output);
    (document.get("ok") == Some(&Value::Bool(false))).then(|| child(&document, "error").cloned().unwrap_or_default())
}

/// The engine's error for a failed run: in its machine modes it prints it
/// on stdout as the contract error document; a half of a sync carries it as
/// `{"error":{...}}`. Empty when neither is there (a crash).
fn engine_error(output: &[u8]) -> Map<String, Value> {
    match contract_error(output) {
        Some(error) if !error.is_empty() => error,
        _ => child(&object(output), "error").cloned().unwrap_or_default(),
    }
}

/// The JSON envelope `{"ok":true,"data","summary","meta"?}` a `--json` run
/// prints on success.
fn success_envelope(output: &[u8]) -> Option<Map<String, Value>> {
    let document = object(output);
    (document.get("ok") == Some(&Value::Bool(true))).then_some(document)
}

/// The strings of a JSON array (a report's lines).
fn strings(array: Option<&Value>) -> Vec<&str> {
    let items = array.and_then(Value::as_array).into_iter().flatten();
    items.map(|line| line.as_str().unwrap_or_default()).collect()
}

/// The error of a push whose result holds a change that failed (iCloud
/// refused it, or something it depends on failed): `push_rejected`, with the
/// first one's message. Empty when the result has none.
fn refused_change(push_result: &Map<String, Value>) -> Map<String, Value> {
    let entries = push_result
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let messages: Vec<&str> = entries
        .filter_map(|entry| entry.get("outcome")?.as_object())
        .filter(|outcome| outcome.get("succeeded").and_then(Value::as_bool) != Some(true))
        .map(|outcome| text(outcome, "message"))
        .collect();
    let message = match messages.as_slice() {
        [] => return Map::new(),
        [only] => only.to_string(),
        [first, more @ ..] => format!("{first} (and {} more; see the log)", more.len()),
    };
    object(
        json!({"code": "push_rejected", "message": message})
            .to_string()
            .as_bytes(),
    )
}

/// Only a sign-in fixes it: the engine's error code says so. Exit 3 also
/// means "not cloned", "another account's vault" and the like, so the exit
/// code counts only when no error code came through at all.
fn is_sign_in_required(code: &str, exit_code: i32) -> bool {
    if code.is_empty() {
        exit_code == 3
    } else {
        code == "sign_in_required"
    }
}

/// Apple refused the session icloud-session still holds: only a sign-in
/// fixes it too, but icloud-session was told already, by the session library.
fn is_session_rejected(code: &str) -> bool {
    code == "session_rejected"
}

/// Either: the engine's sync skips its pull after it, and syncing pauses.
fn needs_sign_in(code: &str, exit_code: i32) -> bool {
    is_sign_in_required(code, exit_code) || is_session_rejected(code)
}

/// iCloud out of reach: the engine's network errors.
fn is_network_error(code: &str) -> bool {
    code == "offline" || code == "network"
}

/// The log's lines for a failed run or half (`name`: "push", "sync"): the
/// error's message, then its hint, when it has one, on a line of its own.
fn failed_log(name: &str, error: &Map<String, Value>) -> String {
    let hint = text(error, "hint");
    let hint = if hint.is_empty() {
        String::new()
    } else {
        format!("\n{hint}")
    };
    format!("{name} failed: {}{hint}", text(error, "message"))
}

/// The same, but iCloud out of reach is one line and no failure to act on.
fn run_failed_log(name: &str, error: &Map<String, Value>) -> String {
    if is_network_error(text(error, "code")) {
        return format!(
            "{name}: iCloud could not be reached ({}); the next sync tries again.",
            text(error, "message")
        );
    }
    failed_log(name, error)
}

/// A line of the engine's stderr worth the log: not blank, and not one of
/// the progress lines (one per fetched page and processed note in a machine
/// mode: thousands in a clone).
fn is_logged_stderr_line(line: &[u8]) -> bool {
    !line.trim_ascii().is_empty() && !line.starts_with(b"icloud-notes:progress:")
}

fn compact(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

impl Backend {
    // ---- the log -------------------------------------------------------------------

    pub(crate) fn append_log(&mut self, entry: &str) {
        let log = &mut self.sync.log;
        if !log.is_empty() {
            log.push('\n');
        }
        log.push_str(entry);
        // No text has fewer bytes than UTF-16 units.
        let units = if log.len() > LOG_LIMIT {
            log.encode_utf16().count()
        } else {
            0
        };
        if units > LOG_LIMIT {
            let kept: Vec<u16> = log.encode_utf16().skip(units - LOG_LIMIT).collect();
            *log = String::from_utf16_lossy(&kept);
        }
    }

    /// Adds the window's own `line` to the log (a lock it waits for).
    pub fn log(&mut self, line: &str) {
        self.append_log(line);
    }

    /// What the runs since [`Backend::clear_log`] did, a line or more each.
    pub fn sync_log(&self) -> &str {
        &self.sync.log
    }

    pub fn clear_log(&mut self) {
        self.sync.log.clear();
    }

    // ---- the sign-in ---------------------------------------------------------------

    /// Asks icloud-session who is signed in, and waits for its answer.
    pub fn read_sign_in(&mut self) {
        self.sync.sign_in = session::read();
        // Signed out pauses syncing without trying first.
        if self.sync.sign_in.as_ref().is_some_and(|status| !status.signed_in) && self.vault.cloned() {
            self.sync.auth_expired = true;
        }
    }

    /// False while icloud-session is missing or has not answered.
    pub fn sign_in_known(&self) -> bool {
        self.sync.sign_in.is_some()
    }

    pub fn signed_in(&self) -> bool {
        self.sync.sign_in.as_ref().is_some_and(|status| status.signed_in)
    }

    pub fn apple_id(&self) -> &str {
        let id = self.sync.sign_in.as_ref().and_then(|status| status.apple_id.as_deref());
        id.unwrap_or_default()
    }

    /// Whole days until a "Keep me signed in" sign-in lapses, -1 when there
    /// is none (a phone QR sign-in, say, lasts only hours), or -2 while
    /// nobody is known to be signed in.
    pub fn sign_in_days_left(&self) -> i64 {
        if !self.signed_in() {
            return -2;
        }
        let now = icloud_session::time::now_secs();
        match self.sync.sign_in.as_ref().and_then(|status| status.expires_at) {
            Some(expires) if expires > now => ((expires - now) / 86400) as i64,
            _ => -1,
        }
    }

    /// iCloud refused the sign-in (or nobody is signed in): syncing is
    /// paused until icloud-session reports one.
    pub fn auth_expired(&self) -> bool {
        self.sync.auth_expired
    }

    // ---- running the engine --------------------------------------------------------

    /// What the engine's last step said it did: "due", "changes", "idle",
    /// "signed_out", "session_unavailable", ...
    pub fn last_step(&self) -> &str {
        &self.sync.last_step
    }

    /// Runs `kind` and waits for it, with what follows from it: a refused
    /// sign-in is reported to icloud-session, which checks it with Apple,
    /// and when it vouches for the sign-in the sync is tried once more (a
    /// whole sync, whatever was asked). Returns each engine run's finished
    /// runs, in order: a sync's two halves are two, push then pull. Nothing
    /// is run (and nothing returned) for a vault that left iCloud, or a
    /// background step without an engine.
    pub fn run(&mut self, kind: Kind) -> Vec<Vec<Run>> {
        let mut all = Vec::new();
        let mut next = Some(kind);
        while let Some(kind) = next.take() {
            let Some(runs) = self.run_once(kind) else { break };
            all.push(runs);
            for _ in 0..std::mem::take(&mut self.sync.reports) {
                match session::report_sign_in_required() {
                    Ok(still_good) => {
                        if still_good && !self.sync.retried_after_report {
                            self.sync.retried_after_report = true;
                            self.sync.auth_expired = false;
                            if self.vault.cloned() {
                                next = Some(Kind::Sync { delete_missing: false });
                            }
                        }
                    }
                    Err(why) => {
                        self.append_log(&why);
                        // Nobody could say whether the sign-in still works:
                        // unknown, which never pauses; the next sync finds out again.
                        if !(self.sign_in_known() && !self.signed_in()) {
                            self.sync.auth_expired = false;
                        }
                    }
                }
            }
        }
        all
    }

    /// One engine run, from its start to its last consequence here.
    fn run_once(&mut self, kind: Kind) -> Option<Vec<Run>> {
        match self.start(kind) {
            Started::Nothing => None,
            Started::Ended(runs) => Some(runs),
            Started::Running(running) => Some(self.finish(running.wait())),
        }
    }

    /// Starts a run of `kind`: logs its command line and starts the engine
    /// with the vault's lock, waiting for the lock while someone else has
    /// it. Until [`Backend::finish`] is given what the run left, a save
    /// waits ([`Backend::save_note`]).
    pub fn start(&mut self, kind: Kind) -> Started {
        // left iCloud: the engine would refuse every run (left_icloud)
        if self.sync.running || self.vault.left_icloud() {
            return Started::Nothing;
        }
        let (mode, label, args): (Mode, &'static str, Vec<String>) = match kind {
            Kind::Sync { delete_missing } => (Mode::Sync, "Sync", with_yes(&["--quiet", "sync"], delete_missing)),
            Kind::Background { every_secs } => (
                Mode::Step,
                "Sync",
                vec![
                    "--quiet".into(),
                    "background".into(),
                    "--every".into(),
                    every_secs.max(1).to_string(),
                ],
            ),
            Kind::Pull => (Mode::Plain, "Pull", with_yes(&["--json", "pull"], false)),
            Kind::Push { delete_missing } => (Mode::Plain, "Push", with_yes(&["--json", "push"], delete_missing)),
            Kind::Clone => {
                // The engine clones the daemon's account (named by dsid) with
                // the session it takes from the daemon, and never opens a
                // window itself. The vault root doubles as the clone's directory.
                let account = self.sync.sign_in.as_ref().filter(|s| s.signed_in);
                let Some(dsid) = account.and_then(|s| s.dsid.clone()) else {
                    return Started::Nothing;
                };
                let vault = self.vault.root().to_string();
                (
                    Mode::Plain,
                    "Clone",
                    vec![
                        "--json".into(),
                        "clone".into(),
                        "--account".into(),
                        dsid,
                        "--non-interactive".into(),
                        vault,
                    ],
                )
            }
        };
        let tool = self.engine.clone();
        if mode == Mode::Step && tool.is_none() {
            return Started::Nothing; // nothing to decide with (a sync asked for says so)
        }
        self.sync.mode = mode;
        self.sync.label = label;
        self.sync.finished.clear();
        if mode != Mode::Plain {
            self.sync.state_stamp_before = self.vault.state_stamp();
        }
        self.sync.step_log.clear();
        self.sync.running = true;
        let command = format!("$ {TOOL} {}", args.join(" "));
        // A step that does not sync is not worth a line: it is logged once it does.
        if mode == Mode::Step {
            self.sync.step_log.push(command);
        } else {
            self.append_log(&command);
        }
        let Some(tool) = tool else {
            self.append_log(NOT_INSTALLED);
            return Started::Ended(self.finish(Finished {
                exit_code: -1,
                ..Default::default()
            }));
        };
        // The engine runs with the vault's lock held here. Someone else
        // holding it (a background sync) means waiting for as long as that
        // takes; a lock that cannot be opened at all means no sync.
        let fd = loop {
            match self.lock_vault() {
                Taken::Locked => break self.lock.fd(),
                Taken::Busy => std::thread::sleep(LOCK_RETRY),
                Taken::Failed => break None,
            }
        };
        let Some(fd) = fd else {
            self.append_log(&format!(
                "Could not open the sync lock {}; not running icloud-notes-sync without it.",
                self.lock.path().display()
            ));
            return Started::Ended(self.finish(Finished {
                exit_code: -1,
                ..Default::default()
            }));
        };
        match engine::start(&tool, &args, self.vault.path(), fd) {
            Ok(running) => Started::Running(running),
            Err(e) => {
                self.append_log(&format!("Failed to start {TOOL}: {}", crate::files::os_error_text(&e)));
                Started::Ended(self.finish(Finished {
                    exit_code: -1,
                    ..Default::default()
                }))
            }
        }
    }

    /// The run ended: logs what it wrote and how it exited, reads its
    /// answer, re-reads the vault where the run may have changed it, and
    /// writes a save that waited for it. Returns its finished runs: a sync's
    /// two halves are two, push then pull.
    pub fn finish(&mut self, finished: Finished) -> Vec<Run> {
        self.sync.running = false;
        let exit_code = finished.exit_code;
        let mut skipped = false;
        if self.sync.mode == Mode::Step {
            // Not synced: nothing to log, nothing to report. Anything else
            // (it synced, or failed before it could say) is a sync's run.
            let answer = object(&finished.stdout);
            self.sync.last_step = text(&answer, "step").to_string();
            skipped = exit_code == 0 && answer.get("synced") == Some(&Value::Bool(false));
            let step_log = std::mem::take(&mut self.sync.step_log);
            if !skipped {
                for line in step_log {
                    self.append_log(&line);
                }
                self.sync.mode = Mode::Sync;
            }
        }
        if !skipped {
            for line in finished
                .stderr
                .split(|&b| b == b'\n')
                .filter(|line| is_logged_stderr_line(line))
            {
                self.append_log(&String::from_utf8_lossy(line));
            }
            self.append_log(&format!("(exit {exit_code})"));
            if self.sync.mode == Mode::Sync {
                self.finish_sync_halves(exit_code, &finished.stdout);
            } else {
                self.finish_run(self.sync.label, exit_code, &finished.stdout);
            }
        }
        self.write_queued_save();
        std::mem::take(&mut self.sync.finished)
    }

    /// One run, or one half of a sync, is over.
    fn finish_run(&mut self, label: &'static str, exit_code: i32, output: &[u8]) {
        // push --dry-run and diff report their changes in the result and
        // exit 0; only a non-zero exit is a failure.
        let ok = exit_code == 0;
        let mut captured = output.to_vec();
        // A --json run (pull, push, clone): the envelope's data is the
        // result, its meta the text report, logged here. A push that had a
        // change refused exits 1 with its result all the same.
        let enveloped = self.sync.mode == Mode::Plain;
        let envelope = enveloped.then(|| success_envelope(output)).flatten();
        let refused = !ok && envelope.is_some();
        if enveloped && (ok || refused) {
            let envelope = envelope.unwrap_or_default();
            captured = match envelope.get("data") {
                Some(data @ (Value::Object(_) | Value::Array(_))) => compact(data),
                _ => Vec::new(),
            };
            let lines = strings(child(&envelope, "meta").and_then(|meta| meta.get("lines")));
            let report = if lines.is_empty() {
                text(&envelope, "summary").to_string()
            } else {
                lines.join("\n")
            };
            if !report.is_empty() {
                self.append_log(&report);
            }
        }
        let error = if ok {
            Map::new()
        } else if refused {
            refused_change(&object(&captured))
        } else {
            engine_error(output)
        };
        // A failure's output is its error, as a failed half's is: {"error":{...}}.
        if !error.is_empty() {
            captured = compact(&json!({"error": error}));
        }
        let code = text(&error, "code").to_string();
        // A run's error is only on stdout: log it as a sync half's is
        // (finish_sync_halves logs the halves').
        if self.sync.mode != Mode::Sync && !error.is_empty() {
            self.append_log(&run_failed_log(&label.to_lowercase(), &error));
        }
        // A failure that only a sign-in fixes says `sign_in_required`. Notes
        // never signs in by itself: icloud-session owns the sign-in, so it
        // is told (it confirms with Apple before signing everyone out) and
        // syncing pauses until it reports a sign-in, since every further
        // sync would fail the same way. `session_rejected` pauses the same
        // way, but is not reported: the session library reported it already
        // when it got the refusal.
        let session_expired = !ok && is_sign_in_required(&code, exit_code);
        let session_rejected = !ok && is_session_rejected(&code);
        if session_expired {
            self.sync.reports += 1;
        }
        // A pull or clone always talks to iCloud, so one that worked proves
        // the session; a push with nothing to send never checks it. Only
        // that re-arms the retry: a push that sent nothing proves nothing,
        // and clearing it there let push-ok, pull-refused retry forever.
        let session_works = ok && (label == "Pull" || label == "Clone");
        if session_works {
            self.sync.retried_after_report = false;
        }
        self.sync.auth_expired = session_expired || session_rejected || (self.sync.auth_expired && !session_works);
        self.sync.finished.push(Run {
            command: match label {
                "Push" => "push",
                "Pull" => "pull",
                _ => "clone",
            },
            ok,
            exit_code,
            result: match serde_json::from_slice(&captured) {
                Ok(result @ Value::Object(_)) => result,
                _ => Value::Null,
            },
            error,
        });
        if self.sync.mode == Mode::Plain {
            self.refresh(); // a pull or clone changes files behind our back
        }
    }

    /// `icloud-notes-sync --quiet sync` answers `{push, pull, vault_info}`,
    /// each half `{ok, exit_code, result, lines}` or `{ok, exit_code,
    /// error}`, pull null when it did not run (the push found the sign-in
    /// gone or the session rejected, or iCloud out of reach). Each half is
    /// logged and finished as a run of its own was; then the vault is
    /// re-read, once, and only when the run may have changed it.
    fn finish_sync_halves(&mut self, exit_code: i32, output: &[u8]) {
        let answer = object(output);
        let mut halves: Vec<(&'static str, i32, Vec<u8>)> = Vec::new();
        // Nothing a re-read would show: no half failed but for a sign-in or the network.
        let mut quiet = true;
        let mut pulled_changes = false;
        let mut take = |backend: &mut Backend, label: &'static str, half: &Map<String, Value>| {
            let name = label.to_lowercase();
            let ok = half.get("ok").and_then(Value::as_bool).unwrap_or(false);
            // A failed push that ran but had a change refused has lines too:
            // what it did do, which goes to the log ahead of the failure.
            let lines = strings(half.get("lines"));
            if ok || !lines.is_empty() {
                let report = if lines.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", lines.join("\n"))
                };
                backend.append_log(&format!("{name}:{report}"));
            }
            let result = if ok {
                child(half, "result").cloned().unwrap_or_default()
            } else {
                let error = child(half, "error").cloned().unwrap_or_default();
                backend.append_log(&run_failed_log(&name, &error));
                let code = text(&error, "code");
                if !is_network_error(code) {
                    quiet = quiet && (code == "sign_in_required" || is_session_rejected(code));
                }
                Map::from_iter([("error".to_string(), Value::Object(error))])
            };
            let whole = |value: &Value| {
                value
                    .as_i64()
                    .or_else(|| value.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            };
            let code = half
                .get("exit_code")
                .and_then(whole)
                .map_or(i32::from(!ok), |code| code as i32);
            halves.push((label, code, compact(&Value::Object(result.clone()))));
            result
        };

        match child(&answer, "push").filter(|push| !push.is_empty()) {
            None => {
                // No answer (no engine, no lock, the vault busy, a crash):
                // both halves failed as the run did, as two runs would have;
                // a sign-in skips the pull. The engine's error object, when
                // it printed one, is on stdout.
                let code = if exit_code == 0 { -1 } else { exit_code };
                let error = engine_error(output);
                let mut failed = Vec::new();
                if !error.is_empty() {
                    self.append_log(&failed_log("sync", &error));
                    failed = compact(&json!({"error": error}));
                }
                halves.push(("Push", code, failed.clone()));
                if !needs_sign_in(text(&error, "code"), code) {
                    halves.push(("Pull", code, failed));
                }
                quiet = false;
            }
            Some(push) => {
                take(self, "Push", push);
                if let Some(pull) = child(&answer, "pull") {
                    let pulled = take(self, "Pull", pull);
                    let count = |key: &str| pulled.get(key).and_then(Value::as_i64).unwrap_or(0) > 0;
                    let listed = |key: &str| {
                        pulled
                            .get(key)
                            .and_then(Value::as_array)
                            .is_some_and(|list| !list.is_empty())
                    };
                    pulled_changes = ["added", "updated", "merged", "removed"].into_iter().any(count)
                        || listed("changes")
                        || listed("conflicts");
                } else {
                    let (_, push_exit, push_output) = halves[0].clone();
                    let push_code = child(push, "error")
                        .map(|error| text(error, "code"))
                        .unwrap_or_default();
                    if !needs_sign_in(push_code, push_exit) {
                        // Not run, and not for a sign-in (which pauses
                        // instead): the push found iCloud out of reach. It
                        // failed as the push did.
                        halves.push(("Pull", push_exit, push_output));
                    }
                }
            }
        }
        for (label, code, output) in halves {
            self.finish_run(label, code, &output);
        }
        // The answer carries what vault-info would say now: no second engine run.
        if let Some(info) = child(&answer, "vault_info") {
            self.adopt_vault_info(info);
        }
        if !quiet || pulled_changes || self.vault.state_stamp() != self.sync.state_stamp_before {
            self.refresh(); // the pull (or something beside it) changed files
        } else if self.open_note_changed() {
            self.refresh(); // the open note changed under the sync, from elsewhere
        }
    }
}

fn with_yes(args: &[&str], yes: bool) -> Vec<String> {
    let mut args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    if yes {
        args.push("--yes".into());
    }
    args
}
