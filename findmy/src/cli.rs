//! The command line: `icloud-findmy <command>` runs one Find My action
//! without GTK, through the same [`FindMe`] client and [`History`] the app
//! uses. Output follows the shared product contract
//! ([`icloud_session::output`]): text on a terminal, the JSON envelope when
//! piped, `--json`/`--styled`/`--quiet`/`--ids-only`/`--count` to choose.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok; 1 error; 2 usage, an ambiguous name, or an
//! action without `--yes` when nobody can be asked; 3 not signed in, Find My
//! needs the Apple password (after `icloud-session`'s own re-authorization
//! with a stored password failed), Find My not enabled, or iCloud still
//! refusing the session (`session_rejected`, HTTP 401/421); 4 not found; 5 offline, no network,
//! `icloud-sessiond` unreachable, the keyring locked, an unreadable answer or
//! an iCloud server error (HTTP 5xx). The JSON `code` tells them apart. A
//! failure in a machine mode is `{"ok":false,"error":{"code","message","hint"?}}`
//! on stdout; in text mode `error:`/`hint:` lines on stderr.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use icloud_session::cli::{
    self, EXIT_ERROR, EXIT_FIND_MY_AUTH, EXIT_NOT_FOUND, EXIT_SIGN_IN, EXIT_UNAVAILABLE, EXIT_USAGE,
};
use icloud_session::output::{
    self, CliError, FIND_MY_AUTH_HINT, Mode, OutputFlags, Response, SERVICE_TROUBLE_HINT, SIGN_IN_HINT,
};
use icloud_session::time::rfc3339;
use serde_json::{Value, json};

use crate::findme::{self, FindMe, SessionTransport, Transport};
use crate::history::{self, History, Point};
use crate::models::{self, Device, Fix};

/// How long `locate` waits for a fresh fix unless `--wait` says otherwise.
const DEFAULT_WAIT_SECS: u64 = 30;
/// How often `locate` asks again while it waits.
const POLL: Duration = Duration::from_secs(3);

const AFTER_HELP: &str = "\
NAME|ID matches a device name case-insensitively (a unique part of it is
enough); an ambiguous NAME lists the matches, use the ID then.

Output: human-readable text on a terminal, a JSON envelope
{\"ok\":true,\"data\",\"summary\",\"meta\"?} when piped. --json forces the
envelope, --styled text, --quiet prints only the data, --ids-only the device
ids (devices, locate, play-sound, lost-mode), --count the number of results
(devices; positions for history; deleted for prune-history). Flags combine:
ids-only > count > quiet > json > styled. A failure is
{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on stdout, or
error:/hint: lines on stderr in text mode.

Exit codes:
  0 ok
  1 error (unsupported, no_fix, cancelled, io, error, http 4xx)
  2 usage, ambiguous, or play-sound/lost-mode without --yes when nobody can
    be asked
  3 sign_in_required (icloud-session sign-in), find_my_auth_required
    (icloud-session authorize-find-my), find_my_unavailable (Find My is not
    on for this Apple ID), session_rejected (iCloud keeps refusing the
    session although icloud-sessiond still holds it: sign in again)
  4 not_found
  5 offline, network, session_service (icloud-sessiond unreachable),
    keyring_unavailable, bad_response, http 5xx";

#[derive(Parser)]
#[command(
    name = "icloud-findmy",
    version,
    about = "Find My from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    #[command(flatten)]
    out: OutputFlags,
    /// Keep history.db in DIR (default ~/.local/share/icloud-findmy).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List devices: model, battery, online, last fix.
    ///
    /// Every device on the Apple ID, as Apple last heard from it: name,
    /// model, battery, online, Lost Mode, what it can do, and the last fix
    /// (time, age, accuracy; coordinates only with --coords). Moved
    /// positions are stored in the history.
    ///
    /// JSON: [{id, name, model, class, battery_percent, charging, online,
    /// lost_mode, can_play_sound, can_lost_mode, last_fix: {time,
    /// timestamp_ms, age_secs, accuracy_m, is_old, lat?, lon?} | null}]
    #[command(after_help = AFTER_HELP)]
    Devices {
        /// Ask every device for a fresh fix first (wakes them, like Refresh in the app).
        #[arg(long)]
        locate: bool,
        /// Include coordinates.
        #[arg(long)]
        coords: bool,
    },
    /// Ask one device for a fresh fix and print it.
    ///
    /// Asks one device for a fresh fix and waits for one newer than the
    /// last; prints it with its coordinates. No fresh fix in time is an
    /// error (code no_fix).
    ///
    /// JSON: the device, as in `devices --coords`.
    #[command(after_help = AFTER_HELP)]
    Locate {
        #[arg(value_name = "NAME|ID")]
        device: String,
        /// How long to wait for the fix, in seconds.
        #[arg(long, value_name = "SECS", default_value_t = DEFAULT_WAIT_SECS)]
        wait: u64,
    },
    /// Play a sound on a device.
    ///
    /// Plays a sound on a device. Asks in text mode on a terminal (stdin and
    /// stdout); anywhere else --yes is required (a usage error, exit 2,
    /// otherwise).
    ///
    /// JSON: {action: "play_sound", device: {id, name}}
    #[command(after_help = AFTER_HELP)]
    PlaySound {
        #[arg(value_name = "NAME|ID")]
        device: String,
        /// Do not ask; required when nobody can be asked (not a terminal, or a machine output mode).
        #[arg(long, short)]
        yes: bool,
    },
    /// Turn on Lost Mode (locks the device).
    ///
    /// Turns on Lost Mode: the device locks and shows the message with a
    /// button to call the number. Asks in text mode on a terminal (stdin and
    /// stdout); anywhere else --yes is required.
    ///
    /// JSON: {action: "lost_mode", device: {id, name}}
    #[command(after_help = AFTER_HELP)]
    LostMode {
        #[arg(value_name = "NAME|ID")]
        device: String,
        /// The number to call.
        #[arg(long, value_name = "P")]
        phone: String,
        /// The text shown.
        #[arg(long, value_name = "M")]
        message: String,
        /// Do not ask; required when nobody can be asked (not a terminal, or a machine output mode).
        #[arg(long, short)]
        yes: bool,
    },
    /// Stored positions of one device, oldest first.
    ///
    /// The stored positions of one device, oldest first: the app's map
    /// trail. A device ID already in the history needs no network.
    ///
    /// JSON: {device: {id, name}, since, points: [{time, timestamp, lat,
    /// lon, accuracy_m, battery_percent}]}
    #[command(after_help = AFTER_HELP)]
    History {
        #[arg(value_name = "NAME|ID")]
        device: String,
        /// How far back: e.g. 90m, 24h (the map's), 7d, 1w2d.
        #[arg(long, value_name = "DURATION", value_parser = parse_duration, default_value = "24h")]
        since: i64,
    },
    /// Delete positions older than 30 days.
    ///
    /// Deletes stored positions older than 30 days (opening the history
    /// does too).
    ///
    /// JSON: {deleted, remaining, retention_days}
    #[command(after_help = AFTER_HELP)]
    PruneHistory,
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

/// A Find My failure as the contract's code, exit code, message and hint:
/// the shared mapping of `icloud_session::output` (`session_failure`,
/// `http_failure`), plus Find My's own `find_my_unavailable`, `bad_response`
/// for an answer it cannot read, and `unsupported`.
fn find_error(e: findme::Error) -> Failure {
    use findme::Error as F;
    let msg = e.to_string();
    let with_app_hint = |e: CliError| {
        if e.hint.as_deref() == Some(SIGN_IN_HINT) {
            e.hint(sign_in_hint())
        } else {
            e
        }
    };
    match e {
        F::SignInRequired => output::sign_in_required().hint(sign_in_hint()),
        F::FindMyAuthRequired => CliError::new("find_my_auth_required", EXIT_FIND_MY_AUTH, msg).hint(FIND_MY_AUTH_HINT),
        F::NoService => CliError::new("find_my_unavailable", EXIT_SIGN_IN, msg)
            .hint("turn on Find My on one of this Apple ID's devices, or sign in with another Apple ID"),
        F::Http(status) => with_app_hint(output::http_failure(status, msg)),
        F::Parse(_) => CliError::new("bad_response", EXIT_UNAVAILABLE, msg).hint(SERVICE_TROUBLE_HINT),
        F::Unsupported(m) => CliError::new("unsupported", EXIT_ERROR, m),
        F::Session(s) => with_app_hint(output::session_failure(&s)),
    }
}

/// "90m", "24h", "7d", "1w2d", "30s": whole units, summed. Seconds.
fn parse_duration(s: &str) -> Result<i64, String> {
    let bad = || format!("wants a duration like 30m, 24h or 7d, not \"{s}\"");
    let (mut total, mut num) = (0i64, String::new());
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            _ => return Err(bad()),
        };
        let n: i64 = num.parse().map_err(|_| bad())?;
        total = n.checked_mul(unit).and_then(|v| total.checked_add(v)).ok_or_else(bad)?;
        num.clear();
    }
    if !num.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(total)
}

/// Runs the command line on the process arguments and returns the exit code.
pub fn run() -> u8 {
    let args = match output::parse::<Args>() {
        Ok(args) => args,
        Err(code) => return code,
    };
    let mode = args.out.mode();
    let mut cli = Cli {
        mode,
        data_dir: args.data_dir,
        fm: FindMe::new(SessionTransport::default()),
        history: None,
    };
    let result = match args.command {
        Command::Devices { locate, coords } => cli.devices(locate, coords),
        Command::Locate { device, wait } => cli.locate(&device, Duration::from_secs(wait)),
        Command::PlaySound { device, yes } => cli.play_sound(&device, yes),
        Command::LostMode {
            device,
            phone,
            message,
            yes,
        } => cli.lost_mode(&device, &phone, &message, yes),
        Command::History { device, since } => cli.history(&device, since),
        Command::PruneHistory => cli.prune_history(),
    };
    match result {
        Ok(resp) => resp.print(mode),
        Err(e) => e.report(mode),
    }
}

struct Cli<T: Transport> {
    mode: Mode,
    data_dir: Option<PathBuf>,
    fm: FindMe<T>,
    history: Option<History>,
}

impl<T: Transport> Cli<T> {
    fn history_path(&self) -> Result<PathBuf, Failure> {
        match &self.data_dir {
            Some(dir) => Ok(dir.join("history.db")),
            None => history::default_path()
                .ok_or_else(|| other("no data directory (no $XDG_DATA_HOME or $HOME)").hint("pass --data-dir DIR")),
        }
    }

    fn open_history(&mut self) -> Result<&History, Failure> {
        if self.history.is_none() {
            let path = self.history_path()?;
            let h = History::open(&path).map_err(|e| other(format!("cannot open {}: {e}", path.display())))?;
            self.history = Some(h);
        }
        Ok(self.history.as_ref().expect("just opened"))
    }

    /// Stores moved positions, as the app does after every refresh. A
    /// history that cannot be written is a warning, not a failure.
    fn record(&mut self, devices: &[Device]) {
        let result = self.open_history().and_then(|h| {
            h.record_devices(devices, icloud_session::time::now_ms() / 1000)
                .map_err(|e| other(e.to_string()))
        });
        if let Err(e) = result {
            eprintln!(
                "{}",
                output::terminal_safe(&format!("warning: could not save history: {}", e.message))
            );
        }
    }

    /// The device list (with `locate`, after asking every device to
    /// report), recorded in the history.
    fn fetch(&mut self, locate: bool) -> Result<Vec<Device>, Failure> {
        let devices = self.fm.refresh(locate).map_err(find_error)?;
        self.record(&devices);
        Ok(devices)
    }

    fn devices(&mut self, locate: bool, coords: bool) -> Outcome {
        let devices = self.fetch(locate)?;
        let now = icloud_session::time::now_ms();
        let data: Vec<Value> = devices.iter().map(|d| device_json(d, coords, now)).collect();
        let ids: Vec<String> = devices.iter().map(|d| d.id.clone()).collect();
        let human = if devices.is_empty() {
            "No devices.".to_string()
        } else {
            devices
                .iter()
                .map(|d| device_text(d, coords, now).trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        let summary = match devices.len() {
            1 => "1 device".to_string(),
            n => format!("{n} devices"),
        };
        Ok(Response::new(data, summary).human(human).ids(ids))
    }

    fn locate(&mut self, query: &str, wait: Duration) -> Outcome {
        let devices = self.fetch(false)?;
        let device = resolve(&devices, query)?.clone();
        let before = device.location.map_or(0, |f| f.ts_ms);
        let deadline = Instant::now() + wait;
        let mut devices = self.fm.refresh(true).map_err(find_error)?;
        loop {
            self.record(&devices);
            let Some(now) = devices.iter().find(|d| d.id == device.id) else {
                return Err(CliError::new(
                    "not_found",
                    EXIT_NOT_FOUND,
                    format!("{} is no longer on this Apple ID", device.name),
                ));
            };
            // Newer than before, and not one Apple itself marks as old.
            if let Some(fix) = now.location.filter(|f| f.ts_ms > before && !f.is_old) {
                return Ok(self.fix_response(now, &fix));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let last = match now.location {
                    Some(f) if f.ts_ms > 0 => format!(
                        "last fix {} ({})",
                        rfc3339(f.ts_ms / 1000),
                        models::last_seen(icloud_session::time::now_ms(), f.ts_ms)
                    ),
                    _ => "no fix at all".into(),
                };
                return Err(CliError::new(
                    "no_fix",
                    EXIT_ERROR,
                    format!("{} sent no fresh fix within {} s; {last}", now.name, wait.as_secs()),
                )
                .hint("try a longer --wait; the device may be off or out of reach"));
            }
            std::thread::sleep(POLL.min(left));
            devices = self.fm.refresh(false).map_err(find_error)?;
        }
    }

    fn fix_response(&self, d: &Device, fix: &Fix) -> Response {
        let now = icloud_session::time::now_ms();
        let human = format!(
            "{}\n  fix:      {} ({}), ±{:.0} m\n  coords:   {:.6}, {:.6}",
            d.name,
            rfc3339(fix.ts_ms / 1000),
            models::last_seen(now, fix.ts_ms),
            fix.accuracy,
            fix.lat,
            fix.lon
        );
        Response::new(device_json(d, true, now), format!("Located {}", d.name))
            .human(human)
            .ids(vec![d.id.clone()])
    }

    fn play_sound(&mut self, query: &str, yes: bool) -> Outcome {
        let devices = self.fetch(false)?;
        let device = resolve(&devices, query)?.clone();
        if !device.can_play_sound {
            return Err(find_error(findme::Error::Unsupported(
                "This device cannot play a sound.",
            )));
        }
        confirm(
            self.mode,
            "play-sound",
            yes,
            &format!("Play a sound on {}?", device.name),
        )?;
        self.fm.play_sound(&device).map_err(find_error)?;
        self.done("play_sound", &device, &format!("Playing a sound on {}", device.name))
    }

    fn lost_mode(&mut self, query: &str, phone: &str, message: &str, yes: bool) -> Outcome {
        if phone.trim().is_empty() || message.trim().is_empty() {
            return Err(usage("lost-mode needs --phone P and --message M"));
        }
        let devices = self.fetch(false)?;
        let device = resolve(&devices, query)?.clone();
        if !device.can_lost_mode {
            return Err(find_error(findme::Error::Unsupported(
                "This device does not support Lost Mode.",
            )));
        }
        if device.lost_mode_enabled {
            return Err(other(format!("Lost Mode is already on for {}", device.name)));
        }
        confirm(
            self.mode,
            "lost-mode",
            yes,
            &format!(
                "Turn on Lost Mode for {}? It locks and shows \"{}\" with a button to call {}.",
                device.name,
                message.trim(),
                phone.trim()
            ),
        )?;
        self.fm
            .lost_mode(&device, phone.trim(), message.trim())
            .map_err(find_error)?;
        self.done("lost_mode", &device, &format!("Lost Mode is on for {}", device.name))
    }

    fn done(&self, action: &str, d: &Device, text: &str) -> Outcome {
        let data = json!({"action": action, "device": {"id": d.id, "name": d.name}});
        Ok(Response::new(data, text.to_string())
            .human(text.to_string())
            .ids(vec![d.id.clone()]))
    }

    fn history(&mut self, query: &str, since_secs: i64) -> Outcome {
        let query = query.to_string();
        let since = icloud_session::time::now_ms() / 1000 - since_secs;
        // A device ID already in the history needs no network; a NAME is
        // resolved against the live list (which is recorded first).
        let (id, name) = if self.open_history()?.last(&query).map_err(db)?.is_some() {
            (query, None)
        } else {
            let devices = self.fetch(false)?;
            let d = resolve(&devices, &query)?;
            (d.id.clone(), Some(d.name.clone()))
        };
        let points = self.open_history()?.trail(&id, since).map_err(db)?;
        let rows: Vec<Value> = points.iter().map(point_json).collect();
        let data = json!({
            "device": {"id": id, "name": name},
            "since": rfc3339(since),
            "points": rows,
        });
        let who = name.as_deref().unwrap_or(&id);
        let human = if points.is_empty() {
            format!("No positions for {who} since {}.", rfc3339(since))
        } else {
            let mut lines = vec![format!("{who}: {} since {}", positions(points.len()), rfc3339(since))];
            for p in &points {
                let battery = p.battery.map(|b| format!("  {:.0}%", b * 100.0)).unwrap_or_default();
                lines.push(format!(
                    "{}  {:.6}, {:.6}  ±{:.0} m{battery}",
                    rfc3339(p.ts),
                    p.lat,
                    p.lon,
                    p.accuracy
                ));
            }
            lines.join("\n")
        };
        Ok(Response::new(data, positions(points.len()))
            .human(human)
            .count(points.len()))
    }

    fn prune_history(&mut self) -> Outcome {
        let h = self.open_history()?;
        // Opening prunes already; prune again in case the clock moved on.
        let deleted = h.pruned_on_open() + h.prune(icloud_session::time::now_ms() / 1000).map_err(db)?;
        let remaining = h.count().map_err(db)?;
        let days = history::RETENTION_SECS / 86_400;
        let data = json!({"deleted": deleted, "remaining": remaining, "retention_days": days});
        let human = format!(
            "Deleted {} older than {days} days; {remaining} remain.",
            positions(deleted as usize)
        );
        Ok(Response::new(data, format!("Deleted {}", positions(deleted as usize)))
            .human(human)
            .count(deleted as usize))
    }
}

/// "1 position", "N positions".
fn positions(n: usize) -> String {
    match n {
        1 => "1 position".into(),
        n => format!("{n} positions"),
    }
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

fn db(e: rusqlite::Error) -> Failure {
    other(format!("history: {e}"))
}

/// Lowercase, with typographic apostrophes as plain ones ("Dous’s" = "dous's").
fn fold(s: &str) -> String {
    s.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

/// Finds a device by exact ID, else by name ignoring case, else by a unique
/// part of the name.
fn resolve<'a>(devices: &'a [Device], query: &str) -> Result<&'a Device, Failure> {
    if let Some(d) = devices.iter().find(|d| d.id == query) {
        return Ok(d);
    }
    let q = fold(query.trim());
    let exact: Vec<&Device> = devices.iter().filter(|d| fold(&d.name) == q).collect();
    let matches = if exact.is_empty() && !q.is_empty() {
        devices.iter().filter(|d| fold(&d.name).contains(&q)).collect()
    } else {
        exact
    };
    let list = |ds: &[&Device]| {
        ds.iter()
            .map(|d| format!("\n  {}  ({})", d.name, d.id))
            .collect::<String>()
    };
    match matches.as_slice() {
        [d] => Ok(d),
        [] => Err(
            CliError::new("not_found", EXIT_NOT_FOUND, format!("no device matches \"{query}\"")).hint(format!(
                "name one of these devices or use its ID:{}",
                list(&devices.iter().collect::<Vec<_>>())
            )),
        ),
        many => Err(CliError::new(
            "ambiguous",
            EXIT_USAGE,
            format!("\"{query}\" matches {} devices", many.len()),
        )
        .hint(format!("name one exactly or use its ID:{}", list(many)))),
    }
}

fn device_json(d: &Device, coords: bool, now_ms: i64) -> Value {
    let fix = d.location.filter(|f| f.ts_ms > 0).map(|f| {
        let mut v = json!({
            "time": rfc3339(f.ts_ms / 1000),
            "timestamp_ms": f.ts_ms,
            "age_secs": (now_ms - f.ts_ms).max(0) / 1000,
            "accuracy_m": f.accuracy,
            "is_old": f.is_old,
        });
        if coords {
            v["lat"] = json!(f.lat);
            v["lon"] = json!(f.lon);
        }
        v
    });
    json!({
        "id": d.id,
        "name": d.name,
        "model": d.model_name,
        "class": d.class.as_str(),
        "battery_percent": d.battery.map(|b| (b * 100.0).round() as i64),
        "charging": d.charging,
        "online": d.online,
        "lost_mode": d.lost_mode_enabled,
        "can_play_sound": d.can_play_sound,
        "can_lost_mode": d.can_lost_mode,
        "last_fix": fix,
    })
}

fn device_text(d: &Device, coords: bool, now_ms: i64) -> String {
    let mut out = format!("{}\n", d.name);
    let model = if d.model_name.is_empty() {
        d.class.as_str().to_string()
    } else {
        format!("{} ({})", d.model_name, d.class.as_str())
    };
    out += &format!("  model:    {model}\n");
    out += &format!("  id:       {}\n", d.id);
    let battery = match d.battery {
        Some(b) if d.charging => format!("{:.0}%, charging", b * 100.0),
        Some(b) => format!("{:.0}%", b * 100.0),
        None => "unknown".into(),
    };
    out += &format!("  battery:  {battery}\n");
    let mut status = vec![if d.online { "online" } else { "offline" }];
    if d.lost_mode_enabled {
        status.push("Lost Mode on");
    }
    out += &format!("  status:   {}\n", status.join(", "));
    match d.location.filter(|f| f.ts_ms > 0) {
        Some(f) => {
            let old = if f.is_old { ", old" } else { "" };
            out += &format!(
                "  last fix: {} ({}), ±{:.0} m{old}\n",
                rfc3339(f.ts_ms / 1000),
                models::last_seen(now_ms, f.ts_ms),
                f.accuracy
            );
            if coords {
                out += &format!("  coords:   {:.6}, {:.6}\n", f.lat, f.lon);
            }
        }
        None => out += "  last fix: none\n",
    }
    out
}

fn point_json(p: &Point) -> Value {
    json!({
        "time": rfc3339(p.ts),
        "timestamp": p.ts,
        "lat": p.lat,
        "lon": p.lon,
        "accuracy_m": p.accuracy,
        "battery_percent": p.battery.map(|b| (b * 100.0).round() as i64),
    })
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
        use findme::Error as F;
        use icloud_session::Error as S;
        let cases = [
            (F::Http(421), "session_rejected", 3),
            (F::Http(401), "session_rejected", 3),
            (F::Http(503), "http", 5),
            (F::Http(404), "http", 1),
            (F::NoService, "find_my_unavailable", 3),
            (F::Parse("x".into()), "bad_response", 5),
            (F::Session(S::Offline("x".into())), "offline", 5),
            (F::Session(S::Service("x".into())), "session_service", 5),
            (F::Session(S::KeyringUnavailable("x".into())), "keyring_unavailable", 5),
            (F::Session(S::Io(std::io::Error::other("x"))), "io", 1),
        ];
        for (e, code, exit) in cases {
            let what = format!("{e:?}");
            let f = find_error(e);
            assert_eq!((f.code.as_str(), f.exit), (code, exit), "{what}");
            assert!(exit != 5 || f.hint.is_some(), "{what}");
        }
        let f = find_error(F::Http(421));
        assert_eq!(f.hint.as_deref(), Some(sign_in_hint().as_str()));
        assert!(f.hint.unwrap().starts_with(SIGN_IN_HINT));
        let f = find_error(F::Session(S::Offline("x".into())));
        assert_eq!(f.hint.as_deref(), Some(output::NETWORK_HINT));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").ok(), Some(90));
        assert_eq!(parse_duration("24h").ok(), Some(86_400));
        assert_eq!(parse_duration("1w2d").ok(), Some(9 * 86_400));
        for bad in ["", "24", "h", "5y", "0m", "-1h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    fn named(id: &str, name: &str) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            model_name: String::new(),
            class: models::DeviceClass::Other,
            battery: None,
            charging: false,
            online: true,
            location: None,
            can_play_sound: true,
            can_lost_mode: true,
            lost_mode_enabled: false,
        }
    }

    #[test]
    fn resolves_id_name_and_part() {
        let ds = [
            named("a1", "Dous’s iPhone"),
            named("b2", "Dous’s iPad"),
            named("c3", "iPhone"),
        ];
        assert_eq!(resolve(&ds, "b2").ok().unwrap().id, "b2");
        assert_eq!(resolve(&ds, "IPHONE").ok().unwrap().id, "c3");
        assert_eq!(resolve(&ds, "dous's iphone").ok().unwrap().id, "a1");
        assert_eq!(resolve(&ds, "ipad").ok().unwrap().id, "b2");
        let Err(e) = resolve(&ds, "dous") else {
            panic!("ambiguous")
        };
        assert_eq!((e.code.as_str(), e.exit), ("ambiguous", 2));
        let hint = e.hint.unwrap();
        assert!(hint.contains("a1") && hint.contains("b2"), "{hint}");
        let Err(e) = resolve(&ds, "watch") else {
            panic!("not found")
        };
        assert_eq!((e.code.as_str(), e.exit), ("not_found", 4));
    }
}
