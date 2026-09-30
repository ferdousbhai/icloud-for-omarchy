//! The command line: `icloud-findmy <command>` runs one Find My action
//! without GTK, through the same [`FindMe`] client and [`History`] the app
//! uses. Human-readable output by default, `--json` for machines.
//!
//! Exit codes are the table every iCloud tool shares (docs/CLI.md,
//! `icloud_session::cli`): 0 ok, 1 error, 2 sign-in required, 4 Find My
//! needs the Apple password (after `icloud-session`'s own re-authorization
//! with a stored password failed), 64 usage. With `--json` an error is one
//! JSON line on stderr, `{"error":{"code","message","exit_code"}}`.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use icloud_session::cli::{self, EXIT_ERROR, EXIT_FIND_MY_AUTH, EXIT_OK, EXIT_SIGN_IN, EXIT_USAGE};
use serde_json::{Value, json};

use crate::findme::{self, FindMe, SessionTransport, Transport};
use crate::history::{self, History, Point};
use crate::models::{self, Device, Fix};

const TOOL: &str = "icloud-findmy";

/// How long `locate` waits for a fresh fix unless `--wait` says otherwise.
pub const DEFAULT_WAIT_SECS: u64 = 30;
/// How often `locate` asks again while it waits.
const POLL: Duration = Duration::from_secs(3);

const AFTER_HELP: &str = "\
NAME|ID matches a device name case-insensitively (a unique part of it is
enough); an ambiguous NAME lists the matches, use the ID then.

With --json, stdout is only the JSON result and an error is one JSON line on
stderr: {\"error\":{\"code\",\"message\",\"exit_code\"}}; codes: usage,
sign_in_required, find_my_auth_required, not_found, ambiguous, cancelled,
unsupported, no_fix, error.

Exit codes: 0 ok, 1 error, 2 sign-in required (icloud-session sign-in),
4 Find My needs the Apple password (icloud-session authorize-find-my),
64 usage.";

#[derive(Parser)]
#[command(
    name = "icloud-findmy",
    version,
    about = "Find My from the command line. With no command, opens the app.",
    after_help = AFTER_HELP
)]
struct Args {
    /// Machine-readable output on stdout (errors as JSON on stderr).
    #[arg(long, global = true)]
    json: bool,
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
    /// Plays a sound on a device. Asks on a terminal; without one, --yes is
    /// required (a usage error otherwise).
    ///
    /// JSON: {ok: true, action: "play_sound", device: {id, name}}
    #[command(after_help = AFTER_HELP)]
    PlaySound {
        #[arg(value_name = "NAME|ID")]
        device: String,
        /// Do not ask; required when stdin is not a terminal.
        #[arg(long, short)]
        yes: bool,
    },
    /// Turn on Lost Mode (locks the device).
    ///
    /// Turns on Lost Mode: the device locks and shows the message with a
    /// button to call the number. Asks on a terminal; without one, --yes is
    /// required.
    ///
    /// JSON: {ok: true, action: "lost_mode", device: {id, name}}
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
        /// Do not ask; required when stdin is not a terminal.
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

enum Failure {
    Usage(String),
    Find(findme::Error),
    /// A machine-readable code (`not_found`, `ambiguous`, `no_fix`,
    /// `cancelled`, ...) and its message.
    Coded(&'static str, String),
    Other(String),
}

impl From<findme::Error> for Failure {
    fn from(e: findme::Error) -> Self {
        Failure::Find(e)
    }
}

impl Failure {
    fn code(&self) -> u8 {
        match self {
            Failure::Usage(_) => EXIT_USAGE,
            Failure::Find(findme::Error::SignInRequired) => EXIT_SIGN_IN,
            Failure::Find(findme::Error::FindMyAuthRequired) => EXIT_FIND_MY_AUTH,
            _ => EXIT_ERROR,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Failure::Usage(_) => "usage",
            Failure::Find(findme::Error::SignInRequired) => "sign_in_required",
            Failure::Find(findme::Error::FindMyAuthRequired) => "find_my_auth_required",
            Failure::Find(findme::Error::Unsupported(_)) => "unsupported",
            Failure::Coded(code, _) => code,
            _ => "error",
        }
    }

    fn message(&self) -> String {
        match self {
            Failure::Usage(m) | Failure::Other(m) | Failure::Coded(_, m) => m.clone(),
            Failure::Find(findme::Error::SignInRequired) => {
                "sign in to iCloud required: run `icloud-session sign-in` (or Sign In in the app)".into()
            }
            Failure::Find(findme::Error::FindMyAuthRequired) => {
                "Find My needs your Apple password: run `icloud-session authorize-find-my` \
                 (to stop being asked: `icloud-session set-password`)"
                    .into()
            }
            Failure::Find(e) => e.to_string(),
        }
    }
}

type Outcome = Result<(), Failure>;

fn usage(msg: impl Into<String>) -> Failure {
    Failure::Usage(msg.into())
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
    let args = match cli::parse::<Args>(TOOL) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let json = args.json;
    let mut cli = Cli {
        json,
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
        Ok(()) => EXIT_OK,
        Err(f) => cli::report(TOOL, json, f.kind(), f.code(), &f.message(), None),
    }
}

struct Cli<T: Transport> {
    json: bool,
    data_dir: Option<PathBuf>,
    fm: FindMe<T>,
    history: Option<History>,
}

impl<T: Transport> Cli<T> {
    fn history_path(&self) -> Result<PathBuf, Failure> {
        match &self.data_dir {
            Some(dir) => Ok(dir.join("history.db")),
            None => history::default_path().ok_or_else(|| Failure::Other("no data directory: pass --data-dir".into())),
        }
    }

    fn open_history(&mut self) -> Result<&History, Failure> {
        if self.history.is_none() {
            let path = self.history_path()?;
            let h = History::open(&path).map_err(|e| Failure::Other(format!("cannot open {}: {e}", path.display())))?;
            self.history = Some(h);
        }
        Ok(self.history.as_ref().expect("just opened"))
    }

    /// Stores moved positions, as the app does after every refresh. A
    /// history that cannot be written is a warning, not a failure.
    fn record(&mut self, devices: &[Device]) {
        let result = self.open_history().and_then(|h| {
            h.record_devices(devices, models::now_ms() / 1000)
                .map_err(|e| Failure::Other(e.to_string()))
        });
        if let Err(e) = result {
            eprintln!("icloud-findmy: could not save history: {}", e.message());
        }
    }

    /// The device list: `initClient`, then with `locate` one `refreshClient`
    /// asking every device to report (a fresh session's `initClient` cannot
    /// ask). Recorded in the history either way.
    fn fetch(&mut self, locate: bool) -> Result<Vec<Device>, Failure> {
        let mut devices = self.fm.refresh(false)?;
        if locate {
            self.record(&devices);
            devices = self.fm.refresh(true)?;
        }
        self.record(&devices);
        Ok(devices)
    }

    fn devices(&mut self, locate: bool, coords: bool) -> Outcome {
        let devices = self.fetch(locate)?;
        let now = models::now_ms();
        if self.json {
            let list: Vec<Value> = devices.iter().map(|d| device_json(d, coords, now)).collect();
            println!("{}", pretty(&json!(list)));
        } else if devices.is_empty() {
            println!("No devices.");
        } else {
            for (i, d) in devices.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                print!("{}", device_text(d, coords, now));
            }
        }
        Ok(())
    }

    fn locate(&mut self, query: &str, wait: Duration) -> Outcome {
        let devices = self.fetch(false)?;
        let device = resolve(&devices, query)?.clone();
        let before = device.location.map_or(0, |f| f.ts_ms);
        let deadline = Instant::now() + wait;
        let mut devices = self.fm.refresh(true)?;
        loop {
            self.record(&devices);
            let Some(now) = devices.iter().find(|d| d.id == device.id) else {
                return Err(Failure::Other(format!("{} is no longer on this Apple ID", device.name)));
            };
            // Newer than before, and not one Apple itself marks as old.
            if let Some(fix) = now.location.filter(|f| f.ts_ms > before && !f.is_old) {
                return self.print_fix(now, &fix);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let last = match now.location {
                    Some(f) if f.ts_ms > 0 => format!(
                        "last fix {} ({})",
                        utc(f.ts_ms / 1000),
                        models::last_seen(models::now_ms(), f.ts_ms)
                    ),
                    _ => "no fix at all".into(),
                };
                return Err(Failure::Coded(
                    "no_fix",
                    format!("{} sent no fresh fix within {} s; {last}", now.name, wait.as_secs()),
                ));
            }
            std::thread::sleep(POLL.min(left));
            devices = self.fm.refresh(false)?;
        }
    }

    fn print_fix(&self, d: &Device, fix: &Fix) -> Outcome {
        let now = models::now_ms();
        if self.json {
            println!("{}", pretty(&device_json(d, true, now)));
        } else {
            println!("{}", d.name);
            println!(
                "  fix:      {} ({}), ±{:.0} m",
                utc(fix.ts_ms / 1000),
                models::last_seen(now, fix.ts_ms),
                fix.accuracy
            );
            println!("  coords:   {:.6}, {:.6}", fix.lat, fix.lon);
        }
        Ok(())
    }

    fn play_sound(&mut self, query: &str, yes: bool) -> Outcome {
        let devices = self.fetch(false)?;
        let device = resolve(&devices, query)?.clone();
        if !device.can_play_sound {
            return Err(findme::Error::Unsupported("This device cannot play a sound.").into());
        }
        confirm("play-sound", yes, &format!("Play a sound on {}?", device.name))?;
        self.fm.play_sound(&device)?;
        self.done("play_sound", &device, &format!("Playing a sound on {}", device.name))
    }

    fn lost_mode(&mut self, query: &str, phone: &str, message: &str, yes: bool) -> Outcome {
        if phone.trim().is_empty() || message.trim().is_empty() {
            return Err(usage("lost-mode needs --phone P and --message M"));
        }
        let devices = self.fetch(false)?;
        let device = resolve(&devices, query)?.clone();
        if !device.can_lost_mode {
            return Err(findme::Error::Unsupported("This device does not support Lost Mode.").into());
        }
        if device.lost_mode_enabled {
            return Err(Failure::Other(format!("Lost Mode is already on for {}", device.name)));
        }
        confirm(
            "lost-mode",
            yes,
            &format!(
                "Turn on Lost Mode for {}? It locks and shows \"{}\" with a button to call {}.",
                device.name,
                message.trim(),
                phone.trim()
            ),
        )?;
        self.fm.lost_mode(&device, phone.trim(), message.trim())?;
        self.done("lost_mode", &device, &format!("Lost Mode is on for {}", device.name))
    }

    fn done(&self, action: &str, d: &Device, text: &str) -> Outcome {
        if self.json {
            println!(
                "{}",
                pretty(&json!({"ok": true, "action": action, "device": {"id": d.id, "name": d.name}}))
            );
        } else {
            println!("{text}");
        }
        Ok(())
    }

    fn history(&mut self, query: &str, since_secs: i64) -> Outcome {
        let query = query.to_string();
        let since = models::now_ms() / 1000 - since_secs;
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
        if self.json {
            let rows: Vec<Value> = points.iter().map(point_json).collect();
            println!(
                "{}",
                pretty(&json!({
                    "device": {"id": id, "name": name},
                    "since": utc(since),
                    "points": rows,
                }))
            );
            return Ok(());
        }
        let who = name.as_deref().unwrap_or(&id);
        if points.is_empty() {
            println!("No positions for {who} since {}.", utc(since));
            return Ok(());
        }
        println!("{who}: {} positions since {}", points.len(), utc(since));
        for p in &points {
            let battery = p.battery.map(|b| format!("  {:.0}%", b * 100.0)).unwrap_or_default();
            println!(
                "{}  {:.6}, {:.6}  ±{:.0} m{battery}",
                utc(p.ts),
                p.lat,
                p.lon,
                p.accuracy
            );
        }
        Ok(())
    }

    fn prune_history(&mut self) -> Outcome {
        let h = self.open_history()?;
        // Opening prunes already; prune again in case the clock moved on.
        let deleted = h.pruned_on_open() + h.prune(models::now_ms() / 1000).map_err(db)?;
        let remaining = h.count().map_err(db)?;
        let days = history::RETENTION_SECS / 86_400;
        if self.json {
            println!(
                "{}",
                pretty(&json!({"deleted": deleted, "remaining": remaining, "retention_days": days}))
            );
        } else {
            println!("Deleted {deleted} positions older than {days} days; {remaining} remain.");
        }
        Ok(())
    }
}

/// Asks on a terminal unless `yes`; refuses without a terminal.
fn confirm(cmd: &str, yes: bool, question: &str) -> Outcome {
    if yes {
        return Ok(());
    }
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(usage(format!(
            "{cmd} asks before it acts: pass --yes when stdin is not a terminal"
        )));
    }
    eprint!("{question} [y/N] ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer).ok();
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => Err(Failure::Coded("cancelled", "cancelled".into())),
    }
}

fn db(e: rusqlite::Error) -> Failure {
    Failure::Other(format!("history: {e}"))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("JSON values serialize")
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
        [] => Err(Failure::Coded(
            "not_found",
            format!(
                "no device matches \"{query}\"; devices:{}",
                list(&devices.iter().collect::<Vec<_>>())
            ),
        )),
        many => Err(Failure::Coded(
            "ambiguous",
            format!(
                "\"{query}\" matches {} devices; name one exactly or use its ID:{}",
                many.len(),
                list(many)
            ),
        )),
    }
}

fn device_json(d: &Device, coords: bool, now_ms: i64) -> Value {
    let fix = d.location.filter(|f| f.ts_ms > 0).map(|f| {
        let mut v = json!({
            "time": utc(f.ts_ms / 1000),
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
                utc(f.ts_ms / 1000),
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
        "time": utc(p.ts),
        "timestamp": p.ts,
        "lat": p.lat,
        "lon": p.lon,
        "accuracy_m": p.accuracy,
        "battery_percent": p.battery.map(|b| (b * 100.0).round() as i64),
    })
}

/// Unix seconds as `2026-09-29T12:34:56Z`.
pub fn utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
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
    fn utc_formats() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(utc(-1), "1969-12-31T23:59:59Z");
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
        let Err(Failure::Coded("ambiguous", msg)) = resolve(&ds, "dous") else {
            panic!("ambiguous")
        };
        assert!(msg.contains("a1") && msg.contains("b2"), "{msg}");
        assert!(resolve(&ds, "watch").is_err());
    }
}
