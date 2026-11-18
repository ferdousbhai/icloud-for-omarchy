//! `icloud-session leave [notes|reminders|drive ...] [--yes]`: every app's
//! `leave` at once (see `icloud_app::leave`). Optional, like each app's:
//! nothing offers it, a person types `leave` once for all of them, and
//! `--yes` is for when they asked.
//!
//! Omacloud is checked for every folder before anything changes; then each
//! app's own `leave --yes` runs in turn (its last sync, its checks), and the
//! first that fails stops the rest. What left stays left.

use std::path::PathBuf;
use std::process::Command;

use icloud_app::leave;
use icloud_session::cli::{EXIT_ERROR, EXIT_NOT_FOUND};
use icloud_session::output::{CliError, Mode, Response};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Debug)]
pub enum App {
    Notes,
    Reminders,
    Drive,
}

impl App {
    const ALL: [App; 3] = [App::Notes, App::Reminders, App::Drive];

    fn name(self) -> &'static str {
        match self {
            App::Notes => "Notes",
            App::Reminders => "Reminders",
            App::Drive => "iCloud Drive",
        }
    }

    fn word(self) -> &'static str {
        match self {
            App::Notes => "notes",
            App::Reminders => "reminders",
            App::Drive => "drive",
        }
    }

    /// The command, and the data directory its left record is in.
    fn bin(self) -> &'static str {
        match self {
            App::Notes => "icloud-notes",
            App::Reminders => "icloud-reminders",
            App::Drive => "icloud-drive",
        }
    }

    fn folder(self, home: &std::path::Path) -> PathBuf {
        match self {
            App::Notes => home.join("Documents/Notes"),
            App::Reminders => home.join("Documents/Reminders"),
            App::Drive => home.join("iCloud Drive"),
        }
    }

    /// Why it has nothing to leave here, if it hasn't.
    fn not_in_use(self, home: &std::path::Path) -> Option<&'static str> {
        if !on_path(self.bin()) {
            return Some("not installed");
        }
        let used = match self {
            // a vault the engine cloned
            App::Notes => self.folder(home).join(".icloud-notes").is_dir(),
            App::Reminders => icloud_app::store::data_dir(self.bin()).is_some_and(|d| d.is_dir()),
            App::Drive => self.folder(home).is_dir(),
        };
        (!used).then_some("not used on this computer")
    }
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

#[derive(Serialize)]
struct Skipped {
    app: &'static str,
    why: String,
}

#[derive(Serialize)]
struct Failed {
    app: &'static str,
    code: String,
    message: String,
}

pub fn run(mode: Mode, apps: Vec<App>, yes: bool) -> Result<Response, CliError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::new("error", EXIT_ERROR, "no home directory ($HOME)"))?;
    let named = !apps.is_empty();
    let mut apps = if named { apps } else { App::ALL.to_vec() };
    apps.sort();
    apps.dedup();

    let mut skipped = Vec::new();
    let mut todo = Vec::new();
    for app in apps {
        let why = match leave::left(app.bin()) {
            Ok(Some(_)) | Err(_) => Some("left iCloud already"),
            Ok(None) => app.not_in_use(&home),
        };
        match why {
            Some(why) => skipped.push(Skipped {
                app: app.word(),
                why: why.into(),
            }),
            None => todo.push(app),
        }
    }
    if todo.is_empty() {
        if named && skipped.iter().any(|s| s.why != "left iCloud already") {
            let list: Vec<String> = skipped.iter().map(|s| format!("{}: {}", s.app, s.why)).collect();
            return Err(CliError::new(
                "not_found",
                EXIT_NOT_FOUND,
                format!("nothing to leave ({})", list.join("; ")),
            ));
        }
        let summary = "Nothing left iCloud: every app had left already or isn't used here".to_string();
        return Ok(Response::new(
            json!({ "left": [], "skipped": skipped, "failed": null }),
            summary,
        ));
    }

    // Omacloud first, for every folder: nothing changes until all are ready
    let mut problems: Vec<(App, CliError)> = Vec::new();
    for &app in &todo {
        if let Err(e) = leave::omacloud_ready(&app.folder(&home)) {
            problems.push((app, e));
        }
    }
    if let Some((_, first)) = problems.first() {
        let mut e = CliError::new(
            &first.code,
            first.exit,
            problems
                .iter()
                .map(|(a, e)| format!("{}: {}", a.name(), e.message))
                .collect::<Vec<_>>()
                .join("; "),
        );
        let mut hints: Vec<String> = problems.iter().filter_map(|(_, e)| e.hint.clone()).collect();
        hints.dedup();
        if !hints.is_empty() {
            e = e.hint(format!("{}; nothing left iCloud", hints.join("; ")));
        }
        return Err(e);
    }

    let names: Vec<&str> = todo.iter().map(|a| a.name()).collect();
    let list = names.join(", ");
    let folders: Vec<String> = todo.iter().map(|a| a.folder(&home).display().to_string()).collect();
    leave::confirm(
        mode,
        yes,
        &list,
        &format!(
            "Leave iCloud for {list} on this computer? Each syncs with iCloud a last time, then Omacloud syncs {} and nothing of {list} talks to iCloud here again. iCloud keeps its copy as it is; nothing there is deleted.",
            folders.join(", ")
        ),
    )?;

    let mut left: Vec<Value> = Vec::new();
    let mut failed = None;
    let mut exit = 0;
    for &app in &todo {
        if mode == Mode::Human {
            eprintln!("Leaving iCloud for {}...", app.name());
        }
        match run_one(app) {
            Ok(data) => left.push(json!({ "app": app.word(), "result": data })),
            Err(e) => {
                exit = e.exit;
                failed = Some(Failed {
                    app: app.word(),
                    code: e.code,
                    message: e.message,
                });
                break;
            }
        }
    }
    let done: Vec<&str> = left.iter().filter_map(|l| l["app"].as_str()).collect();
    let mut summary = if done.is_empty() {
        "Nothing left iCloud".to_string()
    } else {
        format!("Left iCloud for {}; Omacloud syncs them now", done.join(", "))
    };
    if let Some(f) = &failed {
        summary.push_str(&format!(". {} did not: {} ({})", f.app, f.message, f.code));
        let rest: Vec<&str> = todo.iter().skip(done.len() + 1).map(|a| a.word()).collect();
        if !rest.is_empty() {
            summary.push_str(&format!("; not tried: {}", rest.join(", ")));
        }
    }
    Ok(Response::new(json!({ "left": left, "skipped": skipped, "failed": failed }), summary).exit(exit))
}

/// `icloud-<app> leave --yes --json`, its envelope's data or error.
fn run_one(app: App) -> Result<Value, CliError> {
    let out = Command::new(app.bin())
        .args(["leave", "--yes", "--json"])
        .output()
        .map_err(|e| CliError::new("error", EXIT_ERROR, format!("{} did not run: {e}", app.bin())))?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        CliError::new(
            "error",
            EXIT_ERROR,
            format!(
                "{} leave gave no answer: {}",
                app.bin(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )
    })?;
    if v["ok"].as_bool() == Some(true) {
        return Ok(v["data"].clone());
    }
    let exit = out
        .status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(EXIT_ERROR);
    Err(CliError::new(
        v["error"]["code"].as_str().unwrap_or("error"),
        exit,
        v["error"]["message"].as_str().unwrap_or("failed").to_string(),
    ))
}
