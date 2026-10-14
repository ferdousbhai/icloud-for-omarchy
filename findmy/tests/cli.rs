//! The built binary's command line against the fake Find My server
//! (examples/fake_findme.rs) through `icloud-session`'s mock mode: no D-Bus,
//! no Apple account, and a temporary data directory for the history.

#[path = "../examples/fake_findme.rs"]
mod fake;

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use icloud_findmy::history::{History, RETENTION_SECS};
use icloud_findmy::models::Fix;
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

    /// The `data` of the JSON envelope on stdout (every caller passes `--json`).
    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        envelope(&out.stdout)["data"].clone()
    }

    /// A human-readable run: `--styled` forces text even when piped.
    fn styled(&self, args: &[&str]) -> Output {
        let mut a = args.to_vec();
        a.push("--styled");
        self.run(&a)
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

/// Parses a JSON envelope (`{ok,data,summary}` or `{ok:false,error}`) from stdout.
fn envelope(stdout: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap()
}

/// `--styled` forces human text even when piped (tests run with no terminal).
fn styled_at(base: &str, tmp: &Path, args: &[&str]) -> Output {
    let mut a = args.to_vec();
    a.push("--styled");
    run_at(base, tmp, &a)
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
    let out = env.styled(&["devices"]);
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
        env.styled(&["devices", "--coords"])
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

    let out = env.styled(&["locate", IPHONE]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("coords:"), "{}", stdout(&out));
}

#[test]
fn locate_gives_up_on_a_device_with_only_an_old_fix() {
    let env = start();
    let out = env.styled(&["locate", "watch", "--wait", "1"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("no fresh fix within 1 s"), "{}", stderr(&out));
}

#[test]
fn play_sound_needs_yes_without_a_terminal() {
    let env = start();
    let out = env.styled(&["play-sound", "Test's iPhone"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert!(env.actions().is_empty());

    let out = env.styled(&["play-sound", "TEST'S IPHONE", "--yes"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Playing a sound on Test's iPhone");
    let actions = env.actions();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["endpoint"], "playSound");
    assert_eq!(actions[0]["body"]["device"], IPHONE);

    let done = env.json(&["play-sound", MAC, "--yes", "--json"]);
    assert!(done.get("ok").is_none(), "the envelope carries ok: {done}");
    assert_eq!(done["action"], "play_sound");
    assert_eq!(done["device"]["id"], MAC);
    assert_eq!(env.actions()[1]["body"]["device"], MAC);
}

#[test]
fn ambiguous_or_unknown_names_list_the_devices_and_act_on_none() {
    let env = start();
    let out = env.styled(&["play-sound", "test's", "--yes"]);
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.starts_with("error: \"test's\" matches 4 devices\nhint: "), "{err}");
    assert!(err.contains(IPHONE), "{err}");

    let out = env.styled(&["play-sound", "nokia", "--yes"]);
    assert_eq!(code(&out), 4);
    assert!(stderr(&out).contains("no device matches"), "{}", stderr(&out));
    // The same as machine-readable codes (not found exits 4, ambiguous 2);
    // the failure envelope is on stdout in a machine mode, the device list
    // in the hint.
    for (name, want, exit) in [("nokia", "not_found", 4), ("test's", "ambiguous", 2)] {
        let out = env.run(&["play-sound", name, "--yes", "--json"]);
        let err = envelope(&out.stdout);
        assert_eq!(
            (code(&out), err["error"]["code"].as_str()),
            (exit, Some(want)),
            "{name}"
        );
        assert!(!err["error"]["message"].as_str().unwrap().contains('\n'), "{err}");
        assert!(err["error"]["hint"].as_str().unwrap().contains(MAC), "{err}");
    }

    // The AirPods cannot do Lost Mode in the fixture.
    let out = env.styled(&["lost-mode", "airpods", "--phone", "1", "--message", "m", "--yes"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("does not support Lost Mode"), "{}", stderr(&out));
    assert!(env.actions().is_empty());
}

#[test]
fn lost_mode_sends_phone_and_message() {
    let env = start();
    let out = env.run(&["lost-mode", "macbook", "--phone", "+1555", "--message", "Lost"]);
    assert_eq!(code(&out), 2, "refused without --yes on a non-terminal");
    let err = envelope(&out.stdout);
    assert_eq!(err["error"]["code"], "usage");
    assert!(err["error"]["hint"].as_str().unwrap().contains("--yes"), "{err}");
    let out = env.run(&["lost-mode", "macbook", "--phone", "+1555", "--yes"]);
    assert_eq!(code(&out), 2, "--message is required");
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
    let out = styled_at(dead, env.dir.path(), &["history", IPHONE, "--since", "1h"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("positions since"), "{}", stdout(&out));
    // --count is the number of positions, not 1 for the one object.
    let out = run_at(dead, env.dir.path(), &["history", IPHONE, "--count"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), points.len().to_string());
    let out = run_at(dead, env.dir.path(), &["history", IPHONE]);
    let summary = envelope(&out.stdout)["summary"].as_str().unwrap().to_string();
    assert_eq!(summary, format!("{} positions", points.len()));
    // History has no ids: --ids-only prints the data, with a note.
    let out = run_at(dead, env.dir.path(), &["history", IPHONE, "--ids-only"]);
    assert_eq!(code(&out), 0);
    assert_eq!(envelope(&out.stdout)["device"]["id"], IPHONE);
    assert!(stderr(&out).starts_with("note: "), "{}", stderr(&out));
    // An unknown ID needs the server, which is gone: count 0 is not faked.
    let out = run_at(dead, env.dir.path(), &["history", "nothing", "--count"]);
    assert_eq!(code(&out), 5, "{}", stdout(&out));

    let out = env.run(&["history", "iphone", "--since", "soon"]);
    assert_eq!(code(&out), 2);
}

#[test]
fn prune_history_deletes_rows_past_retention() {
    let env = start();
    let now = icloud_session::time::now_ms() / 1000;
    let fix = |lat, ts: i64| Fix {
        lat,
        lon: 25.0,
        accuracy: 5.0,
        ts_ms: ts * 1000,
        is_old: false,
    };
    {
        let h = History::open(&env.data().join("history.db")).unwrap();
        h.record("old", &fix(10.0, now - RETENTION_SECS - 60), None).unwrap();
        h.record("new", &fix(20.0, now - 60), None).unwrap();
    }
    let out = env.run(&["prune-history"]);
    let envelope = envelope(&out.stdout);
    assert_eq!(envelope["summary"], "Deleted 1 position");
    let out = &envelope["data"];
    assert_eq!(out["deleted"], 1);
    assert_eq!(out["remaining"], 1);
    assert_eq!(out["retention_days"], 30);
    let out = env.styled(&["prune-history"]);
    assert_eq!(stdout(&out).trim(), "Deleted 0 positions older than 30 days; 1 remain.");
    let out = env.run(&["prune-history", "--count"]);
    assert_eq!(stdout(&out).trim(), "0");
}

#[test]
fn usage_errors_exit_2() {
    let env = start();
    for args in [
        &["frobnicate"][..],
        &["devices", "--wait", "3"],
        &["locate"],
        &["locate", "a", "b"],
        &["devices", "--bogus"],
    ] {
        let out = env.run(args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
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
    assert_eq!(code(&out), 2);
    let env_err = envelope(&out.stdout);
    assert_eq!(env_err["ok"], false);
    assert_eq!(env_err["error"]["code"], "usage");
    assert_eq!(env_err["error"]["hint"], "run `icloud-findmy --help`");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
    // Piped with no flag is a machine too.
    let out = env.run(&["locate"]);
    assert_eq!(envelope(&out.stdout)["error"]["code"], "usage");
    // In text mode clap's own message goes to stderr, nothing to stdout.
    let out = env.run(&["locate", "--styled"]);
    assert_eq!(code(&out), 2);
    assert!(out.stdout.is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).starts_with("error: "), "{}", stderr(&out));
    let listed = env.json(&["--json", "devices"]);
    assert!(listed.as_array().is_some_and(|a| !a.is_empty()));
}

/// A server answering every request with `status`.
fn answering(status: u16) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tiny_http::Server::from_listener(listener, None).unwrap();
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let _ = request.respond(tiny_http::Response::empty(status));
        }
    });
    base
}

#[test]
fn find_my_password_needed_exits_3() {
    let tmp = tempfile::tempdir().unwrap();
    let out = styled_at(&answering(450), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    assert!(stderr(&out).contains("authorize-find-my"), "{}", stderr(&out));

    let out = run_at(&answering(450), tmp.path(), &["devices", "--json"]);
    assert_eq!(code(&out), 3);
    let err = envelope(&out.stdout);
    assert_eq!(err["ok"], false);
    assert_eq!(err["error"]["code"], "find_my_auth_required");
    // The remediation is the hint, not part of the message.
    assert!(
        !err["error"]["message"].as_str().unwrap().contains("icloud-session"),
        "{err}"
    );
    assert!(
        err["error"]["hint"].as_str().unwrap().contains("authorize-find-my"),
        "{err}"
    );
}

#[test]
fn server_errors_exit_5_and_other_http_errors_1() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_at(&answering(503), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    assert_eq!(envelope(&out.stdout)["error"]["code"], "http");
    assert!(envelope(&out.stdout)["error"]["hint"].is_string());
    let out = run_at(&answering(404), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert_eq!(envelope(&out.stdout)["error"]["code"], "http");
}

#[test]
fn offline_exits_5() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [&["devices"][..], &["devices", "--json"], &["locate", "x", "--quiet"]] {
        let out = run_at("http://127.0.0.1:9", tmp.path(), args);
        assert_eq!(code(&out), 5, "{args:?}: {}", stdout(&out));
        let err = envelope(&out.stdout);
        assert_eq!(err["error"]["code"], "offline", "{err}");
        assert!(err["error"]["hint"].is_string(), "{err}");
        assert!(out.stderr.is_empty(), "{}", stderr(&out));
    }
    let out = styled_at("http://127.0.0.1:9", tmp.path(), &["devices"]);
    assert_eq!(code(&out), 5);
    assert!(out.stdout.is_empty());
    assert!(stderr(&out).starts_with("error: offline: "), "{}", stderr(&out));
}

#[test]
fn a_bad_answer_exits_5() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tiny_http::Server::from_listener(listener, None).unwrap();
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let _ = request.respond(tiny_http::Response::from_string("<html>maintenance</html>"));
        }
    });
    let tmp = tempfile::tempdir().unwrap();
    let out = run_at(&base, tmp.path(), &["devices"]);
    assert_eq!(code(&out), 5, "{}", stdout(&out));
    assert_eq!(envelope(&out.stdout)["error"]["code"], "bad_response");
    assert!(envelope(&out.stdout)["error"]["hint"].is_string());
}

#[test]
fn signed_out_exits_3() {
    // Mock mode has no daemon to confirm a 421 with: it means signed out.
    let tmp = tempfile::tempdir().unwrap();
    let out = styled_at(&answering(421), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.starts_with("error: sign in to iCloud required"), "{err}");
    assert!(err.contains("\nhint: run `icloud-session sign-in`"), "{err}");
    let out = run_at(&answering(421), tmp.path(), &["devices"]);
    assert_eq!(code(&out), 3);
    let err = envelope(&out.stdout);
    assert_eq!(err["error"]["code"], "sign_in_required");
    assert!(
        err["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("icloud-session sign-in"),
        "{err}"
    );
}

/// The output modes: the envelope when piped (no flag), `--quiet` the data,
/// `--ids-only` the ids, `--count` the number; flags combine by precedence.
#[test]
fn output_modes() {
    let env = start();
    let out = env.run(&["devices"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    let whole = envelope(&out.stdout);
    assert_eq!(whole["ok"], true);
    assert_eq!(whole["summary"], "4 devices");
    assert_eq!(whole["data"].as_array().unwrap().len(), 4);
    assert!(
        stdout(&out).trim_start().starts_with("{\n  \"ok\": true,"),
        "ok first: {}",
        stdout(&out)
    );

    // The fake stamps fixes with "now": compare without the fix times.
    let timeless = |mut v: Value| {
        for d in v.as_array_mut().unwrap() {
            d["last_fix"].as_object_mut().map(|f| f.remove("timestamp_ms"));
            d["last_fix"].as_object_mut().map(|f| f.remove("time"));
            d["last_fix"].as_object_mut().map(|f| f.remove("age_secs"));
        }
        v
    };
    let out = env.run(&["devices", "--quiet"]);
    assert_eq!(timeless(envelope(&out.stdout)), timeless(whole["data"].clone()));

    let ids: Vec<String> = whole["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    let out = env.run(&["devices", "--ids-only"]);
    assert_eq!(stdout(&out).lines().collect::<Vec<_>>(), ids);
    let out = env.run(&["devices", "--count"]);
    assert_eq!(stdout(&out).trim(), "4");

    // Combinations are not usage errors: ids-only > count > quiet > json > styled.
    for (args, want) in [
        (&["--json", "devices", "--quiet"][..], "quiet"),
        (&["devices", "--ids-only", "--count"], "ids"),
        (&["--styled", "--json", "devices"], "json"),
        (&["devices", "--quiet", "--quiet"], "quiet"),
        (&["--count", "devices", "--json", "--styled"], "count"),
    ] {
        let out = env.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stdout(&out));
        let text = stdout(&out);
        match want {
            "quiet" => assert_eq!(
                timeless(envelope(&out.stdout)),
                timeless(whole["data"].clone()),
                "{args:?}"
            ),
            "json" => assert_eq!(envelope(&out.stdout)["ok"], true, "{args:?}"),
            "ids" => assert_eq!(text.lines().count(), 4, "{args:?}"),
            _ => assert_eq!(text.trim(), "4", "{args:?}"),
        }
    }

    // play-sound has the device's id.
    let out = env.run(&["play-sound", MAC, "--yes", "--ids-only"]);
    assert_eq!(stdout(&out).trim(), MAC);
    let out = env.run(&["play-sound", MAC, "--yes"]);
    assert_eq!(
        envelope(&out.stdout)["summary"],
        "Playing a sound on Test's MacBook Air"
    );
}

/// A machine-mode failure is one JSON document on stdout and nothing on
/// stderr; a text-mode one is `error:` (and `hint:`) on stderr only.
#[test]
fn errors_go_where_the_mode_says() {
    let env = start();
    for args in [
        &["locate", "nokia"][..],
        &["locate", "nokia", "--quiet"],
        &["locate", "nokia", "--count"],
    ] {
        let out = env.run(args);
        assert_eq!(code(&out), 4, "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {}", stderr(&out));
        let err = envelope(&out.stdout);
        // Parsed map order depends on serde_json's preserve_order (feature
        // unification); the byte check below pins `ok` first.
        let mut keys: Vec<&String> = err.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["error", "ok"]);
        let mut fields: Vec<&String> = err["error"].as_object().unwrap().keys().collect();
        fields.sort();
        assert_eq!(fields, ["code", "hint", "message"]);
        assert!(
            stdout(&out).trim_start().starts_with("{\n  \"ok\": false,"),
            "{}",
            stdout(&out)
        );
    }
    let out = env.styled(&["locate", "nokia"]);
    assert_eq!(code(&out), 4);
    assert!(out.stdout.is_empty());
    let err = stderr(&out);
    assert!(err.starts_with("error: no device matches \"nokia\"\nhint: "), "{err}");
}

/// The shared libraries an x86-64 ELF executable needs (its `DT_NEEDED`
/// entries), as `ldd` would start from.
fn needed_libs(path: &str) -> Vec<String> {
    let f = std::fs::read(path).unwrap();
    let u16_at = |o: usize| u16::from_le_bytes(f[o..o + 2].try_into().unwrap()) as usize;
    let u64_at = |o: usize| u64::from_le_bytes(f[o..o + 8].try_into().unwrap()) as usize;
    assert_eq!(&f[..5], b"\x7fELF\x02", "a 64-bit ELF file");
    let (phoff, phentsize, phnum) = (u64_at(0x20), u16_at(0x36), u16_at(0x38));
    let headers: Vec<usize> = (0..phnum).map(|i| phoff + i * phentsize).collect();
    let p_type = |h: usize| u32::from_le_bytes(f[h..h + 4].try_into().unwrap());
    // A virtual address to its file offset, through the PT_LOAD segments.
    let file_offset = |vaddr: usize| {
        headers
            .iter()
            .filter(|&&h| p_type(h) == 1)
            .find(|&&h| (u64_at(h + 0x10)..u64_at(h + 0x10) + u64_at(h + 0x20)).contains(&vaddr))
            .map(|&h| vaddr - u64_at(h + 0x10) + u64_at(h + 0x08))
            .expect("address in a loaded segment")
    };
    let dynamic = headers.iter().find(|&&h| p_type(h) == 2).expect("dynamically linked");
    let entries: Vec<(usize, usize)> = (u64_at(dynamic + 0x08)..u64_at(dynamic + 0x08) + u64_at(dynamic + 0x20))
        .step_by(16)
        .map(|e| (u64_at(e), u64_at(e + 8)))
        .take_while(|&(tag, _)| tag != 0)
        .collect();
    let strtab = file_offset(entries.iter().find(|e| e.0 == 5).expect("DT_STRTAB").1);
    entries
        .iter()
        .filter(|e| e.0 == 1)
        .map(|&(_, name)| {
            let start = strtab + name;
            let end = start + f[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8_lossy(&f[start..end]).into_owned()
        })
        .collect()
}

/// `icloud-findmy` is the command line only: GTK, libadwaita and libshumate
/// are not among the libraries it needs, so commands do not load them. The
/// app binary does need them.
#[test]
fn the_command_line_binary_does_not_link_gtk() {
    let ui = |libs: Vec<String>| {
        libs.into_iter()
            .filter(|l| {
                ["libgtk-4.", "libadwaita-1.", "libshumate-1.0."]
                    .iter()
                    .any(|p| l.starts_with(p))
            })
            .count()
    };
    let cli = needed_libs(env!("CARGO_BIN_EXE_icloud-findmy"));
    assert!(cli.iter().any(|l| l.starts_with("libc.")), "{cli:?}");
    assert_eq!(ui(cli.clone()), 0, "{cli:?}");
    if let Some(app) = option_env!("CARGO_BIN_EXE_icloud-findmy-app") {
        assert_eq!(ui(needed_libs(app)), 3);
    }
}

/// With no command, `icloud-findmy` becomes `icloud-findmy-app` from its
/// own directory (the same process: its exit code is the app's).
#[test]
fn no_command_runs_the_app_beside_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("icloud-findmy");
    std::fs::copy(env!("CARGO_BIN_EXE_icloud-findmy"), &cli).unwrap();
    let app = tmp.path().join("icloud-findmy-app");
    let marker = tmp.path().join("ran");
    let script = format!("#!/bin/sh\necho \"$#\" > '{}'\nexit 7\n", marker.display());
    std::fs::write(&app, script).unwrap();
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();

    // A test thread forking while the copy was open for writing holds its
    // fd until that child execs: "Text file busy" (ETXTBSY) for a moment.
    let mut tries = 0;
    let out = loop {
        match Command::new(&cli).stdin(Stdio::null()).output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => break r.unwrap(),
        }
    };
    assert_eq!(code(&out), 7, "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "0");

    // Without it (and none on PATH) it says so.
    std::fs::remove_file(&app).unwrap();
    let out = Command::new(&cli)
        .env("PATH", tmp.path().join("nowhere"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("cannot open the app"), "{}", stderr(&out));
}
