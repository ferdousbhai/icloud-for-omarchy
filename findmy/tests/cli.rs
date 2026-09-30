//! The built binary's command line against the fake Find My server
//! (examples/fake_findme.rs) through `icloud-session`'s mock mode: no D-Bus,
//! no Apple account, and a temporary data directory for the history.

#[path = "../examples/fake_findme.rs"]
mod fake;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use icloud_findmy::history::{History, RETENTION_SECS};
use icloud_findmy::models::{self, Fix};
use serde_json::Value;

const IPHONE: &str = "aVBob25lMTUtZml4dHVyZS1kZXZpY2UtaWQ=";
const MAC: &str = "TWFjQm9va0Fpci1maXh0dXJlLWRldmljZS1pZA==";

struct Env {
    base: String,
    state: Arc<fake::State>,
    dir: tempfile::TempDir,
}

/// A fake server of its own (its walk and recorded actions) per test.
fn start() -> Env {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(fake::State::default());
    let s = state.clone();
    std::thread::spawn(move || fake::serve_with(listener, s));
    Env {
        base,
        state,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Env {
    fn data(&self) -> std::path::PathBuf {
        self.dir.path().join("data")
    }

    fn run(&self, args: &[&str]) -> Output {
        run_at(&self.base, self.dir.path(), args)
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn actions(&self) -> Vec<Value> {
        self.state.actions()
    }
}

/// Runs the binary with stdin not a terminal, `--data-dir <tmp>/data`, and
/// `XDG_DATA_HOME` in the temp dir too, so nothing reaches the real history.
fn run_at(base: &str, tmp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_icloud-findmy"))
        .args(args)
        .arg("--data-dir")
        .arg(tmp.join("data"))
        .env("ICLOUD_SESSION_MOCK", "1")
        .env("ICLOUD_SESSION_MOCK_URL", base)
        .env("XDG_DATA_HOME", tmp.join("xdg"))
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn devices_lists_every_field_and_coords_only_when_asked() {
    let env = start();
    let out = env.run(&["devices"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    for want in [
        "Test's iPhone",
        "iPhone 15 Pro (iphone)",
        IPHONE,
        "82%",
        "54%, charging",
        "online",
        "offline",
        "±5 m",
        "last fix: none",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    assert!(!text.contains("coords:"), "{text}");
    assert!(
        env.run(&["devices", "--coords"])
            .stdout
            .windows(7)
            .any(|w| w == b"coords:")
    );

    let list = env.json(&["devices", "--json"]);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 4);
    let phone = &list[0];
    assert_eq!(phone["id"], IPHONE);
    assert_eq!(phone["class"], "iphone");
    assert_eq!(phone["battery_percent"], 82);
    assert_eq!(phone["online"], true);
    assert_eq!(phone["last_fix"]["accuracy_m"], 4.7);
    assert!(phone["last_fix"]["time"].as_str().unwrap().ends_with('Z'));
    assert!(phone["last_fix"].get("lat").is_none());
    assert!(list[3]["last_fix"].is_null());

    let list = env.json(&["devices", "--json", "--coords"]);
    assert!(list[0]["last_fix"]["lat"].is_f64());

    // Recorded in --data-dir, as the app records after a refresh.
    assert!(env.data().join("history.db").exists());
    assert!(!env.dir.path().join("xdg").join("icloud-findmy").exists());
}

#[test]
fn devices_locate_asks_for_a_fresh_fix() {
    let (a, b) = (start(), start());
    let last = a.json(&["devices", "--json", "--coords"]);
    let fresh = b.json(&["devices", "--json", "--coords", "--locate"]);
    // Only the locating refreshClient walks the fake iPhone.
    assert_ne!(last[0]["last_fix"]["lat"], fresh[0]["last_fix"]["lat"]);
}

#[test]
fn locate_prints_a_fresh_fix_with_coordinates() {
    let env = start();
    let fix = env.json(&["locate", "test's iphone", "--json"]);
    assert_eq!(fix["id"], IPHONE);
    assert!(fix["last_fix"]["lat"].is_f64() && fix["last_fix"]["lon"].is_f64());
    assert!(fix["last_fix"]["age_secs"].as_i64().unwrap() < 60);

    let out = env.run(&["locate", IPHONE]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("coords:"), "{}", stdout(&out));
}

#[test]
fn locate_gives_up_on_a_device_with_only_an_old_fix() {
    let env = start();
    let out = env.run(&["locate", "watch", "--wait", "1"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("no fresh fix within 1 s"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn play_sound_needs_yes_without_a_terminal() {
    let env = start();
    let out = env.run(&["play-sound", "Test's iPhone"]);
    assert_eq!(code(&out), 64);
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert!(env.actions().is_empty());

    let out = env.run(&["play-sound", "TEST'S IPHONE", "--yes"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Playing a sound on Test's iPhone");
    let actions = env.actions();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["endpoint"], "playSound");
    assert_eq!(actions[0]["body"]["device"], IPHONE);

    let done = env.json(&["play-sound", MAC, "--yes", "--json"]);
    assert_eq!(done["ok"], true);
    assert_eq!(done["action"], "play_sound");
    assert_eq!(env.actions()[1]["body"]["device"], MAC);
}

#[test]
fn ambiguous_or_unknown_names_list_the_devices_and_act_on_none() {
    let env = start();
    let out = env.run(&["play-sound", "test's", "--yes"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.contains("matches 4 devices") && err.contains(IPHONE),
        "{err}"
    );

    let out = env.run(&["play-sound", "nokia", "--yes"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("no device matches"),
        "{}",
        stderr(&out)
    );
    // The same as machine-readable codes.
    for (name, want) in [("nokia", "not_found"), ("test's", "ambiguous")] {
        let out = env.run(&["play-sound", name, "--yes", "--json"]);
        let err: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(
            (code(&out), err["error"]["code"].as_str()),
            (1, Some(want)),
            "{name}"
        );
    }

    // The AirPods cannot do Lost Mode in the fixture.
    let out = env.run(&[
        "lost-mode",
        "airpods",
        "--phone",
        "1",
        "--message",
        "m",
        "--yes",
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("does not support Lost Mode"),
        "{}",
        stderr(&out)
    );
    assert!(env.actions().is_empty());
}

#[test]
fn lost_mode_sends_phone_and_message() {
    let env = start();
    let out = env.run(&[
        "lost-mode",
        "macbook",
        "--phone",
        "+1555",
        "--message",
        "Lost",
    ]);
    assert_eq!(code(&out), 64, "refused without --yes on a non-terminal");
    let out = env.run(&["lost-mode", "macbook", "--phone", "+1555", "--yes"]);
    assert_eq!(code(&out), 64, "--message is required");
    assert!(env.actions().is_empty());

    let done = env.json(&[
        "lost-mode",
        "macbook",
        "--phone=+1 555 0100",
        "--message",
        " Please call me ",
        "--yes",
        "--json",
    ]);
    assert_eq!(done["action"], "lost_mode");
    let actions = env.actions();
    assert_eq!(actions.len(), 1);
    let body = &actions[0]["body"];
    assert_eq!(actions[0]["endpoint"], "lostDevice");
    assert_eq!(body["device"], MAC);
    assert_eq!(body["ownerNbr"], "+1 555 0100");
    assert_eq!(body["text"], "Please call me");
    assert_eq!(body["lostModeEnabled"], true);
}

#[test]
fn history_shows_the_trail_by_name_or_offline_by_id() {
    let env = start();
    env.json(&["devices", "--json"]);
    // History keeps whole seconds and skips a fix no newer than the last.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    env.json(&["devices", "--json", "--locate"]);
    let trail = env.json(&["history", "test's iphone", "--json"]);
    assert_eq!(trail["device"]["id"], IPHONE);
    let points = trail["points"].as_array().unwrap();
    assert!(points.len() >= 2, "{trail}");
    assert!(points[0]["lat"].is_f64() && points[0]["time"].is_string());

    // A known ID needs no server at all.
    let dead = "http://127.0.0.1:9";
    let out = run_at(dead, env.dir.path(), &["history", IPHONE, "--since", "1h"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("positions since"), "{}", stdout(&out));

    let out = env.run(&["history", "iphone", "--since", "soon"]);
    assert_eq!(code(&out), 64);
}

#[test]
fn prune_history_deletes_rows_past_retention() {
    let env = start();
    let now = models::now_ms() / 1000;
    let fix = |lat, ts: i64| Fix {
        lat,
        lon: 25.0,
        accuracy: 5.0,
        ts_ms: ts * 1000,
        is_old: false,
    };
    {
        let h = History::open(&env.data().join("history.db")).unwrap();
        h.record("old", &fix(10.0, now - RETENTION_SECS - 60), None)
            .unwrap();
        h.record("new", &fix(20.0, now - 60), None).unwrap();
    }
    let out = env.json(&["prune-history", "--json"]);
    assert_eq!(out["deleted"], 1);
    assert_eq!(out["remaining"], 1);
    assert_eq!(out["retention_days"], 30);
    let out = env.run(&["prune-history"]);
    assert_eq!(
        stdout(&out).trim(),
        "Deleted 0 positions older than 30 days; 1 remain."
    );
}

#[test]
fn usage_errors_exit_64() {
    let env = start();
    for args in [
        &["frobnicate"][..],
        &["devices", "--wait", "3"],
        &["locate"],
        &["locate", "a", "b"],
        &["devices", "--bogus"],
    ] {
        let out = env.run(args);
        assert_eq!(code(&out), 64, "{args:?}: {}", stderr(&out));
    }
    // Help never touches the network or the history.
    let help = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_icloud-findmy"))
            .args(args)
            .env("ICLOUD_SESSION_MOCK", "1")
            .env("ICLOUD_SESSION_MOCK_URL", "http://127.0.0.1:9")
            .output()
            .unwrap()
    };
    let out = help(&["help"]);
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("play-sound"), "{}", stdout(&out));
    // Every command has its own --help (and `help COMMAND`), with its JSON
    // shape and the exit codes.
    for cmd in [
        "devices",
        "locate",
        "play-sound",
        "lost-mode",
        "history",
        "prune-history",
    ] {
        for args in [&[cmd, "--help"][..], &["help", cmd]] {
            let out = help(args);
            assert_eq!(code(&out), 0, "{args:?}");
            let text = stdout(&out);
            assert!(
                text.contains(&format!("Usage: icloud-findmy {cmd}"))
                    && text.contains("JSON:")
                    && text.contains("Exit codes"),
                "{text}"
            );
        }
    }
    assert!(env.actions().is_empty());
    // A usage error with --json is the JSON error object; --json may come
    // before the command.
    let out = env.run(&["--json", "locate"]);
    assert_eq!(code(&out), 64);
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["code"], "usage");
    assert_eq!(err["error"]["exit_code"], 64);
    let listed: Value = serde_json::from_slice(&env.run(&["--json", "devices"]).stdout).unwrap();
    assert!(listed.as_array().is_some_and(|a| !a.is_empty()));
}

/// A server answering every request with `status`.
fn answering(status: u16) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            let _ = reader.read_exact(&mut body);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} \r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    base
}

#[test]
fn find_my_password_needed_exits_4() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_at(&answering(450), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 4, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("authorize-find-my"),
        "{}",
        stderr(&out)
    );

    let out = run_at(&answering(450), tmp.path(), &["devices", "--json"]);
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["error"]["code"], "find_my_auth_required");
    assert_eq!(err["error"]["exit_code"], 4);
}

#[test]
fn other_http_errors_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_at(&answering(503), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
}

#[test]
fn signed_out_exits_2() {
    // Mock mode has no daemon to confirm a 421 with: it means signed out.
    let tmp = tempfile::tempdir().unwrap();
    let out = run_at(&answering(421), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("icloud-session sign-in"),
        "{}",
        stderr(&out)
    );
}
