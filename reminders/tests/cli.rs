//! The built binary's command line against the fake CloudKit server
//! (examples/fake_reminders.rs) through `icloud-session`'s mock mode: no
//! D-Bus, no Apple account, a temporary data directory, and a stand-in
//! `omarchy-notification-send` that records what it was asked to show.

#[path = "../examples/fake_reminders.rs"]
mod fake;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use serde_json::{Value, json};

const MINUTE: i64 = 60_000;

struct Env {
    base: String,
    state: Arc<fake::State>,
    dir: tempfile::TempDir,
}

fn now_ms() -> i64 {
    icloud_session::time::now_ms()
}

/// A seeded fake server of its own per test; the binary runs with TZ=UTC.
fn start() -> Env {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(fake::State::seeded(now_ms(), 0));
    let s = state.clone();
    std::thread::spawn(move || fake::serve_with(listener, s));
    let dir = tempfile::tempdir().unwrap();
    // The stand-in for Omarchy's notifier: one argument per line, then ---.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("notifications.log");
    write_script(
        &bin.join("omarchy-notification-send"),
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> '{0}'\necho --- >> '{0}'\n",
            log.display()
        ),
    );
    Env { base, state, dir }
}

fn write_script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Env {
    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn run(&self, args: &[&str]) -> Output {
        run_at(&self.base, self.dir.path(), args)
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        stdout(&out)
    }

    /// The `data` of the JSON envelope on stdout (the default when piped).
    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        let v = envelope(&out);
        assert_eq!(v["ok"], true, "{args:?}");
        assert!(v["summary"].is_string(), "{args:?}");
        v["data"].clone()
    }

    /// A failed run: its exit code and the `error` of the envelope on stdout.
    fn fails(&self, args: &[&str]) -> (i32, Value) {
        let out = self.run(args);
        let v = envelope(&out);
        assert_eq!(v["ok"], false, "{args:?}");
        (code(&out), v["error"].clone())
    }

    /// The `op`s sent, in order.
    fn ops(&self) -> Vec<String> {
        self.state
            .requests()
            .iter()
            .map(|r| r["op"].as_str().unwrap().to_owned())
            .collect()
    }

    fn sent(&self, op: &str) -> Vec<Value> {
        self.state
            .requests()
            .into_iter()
            .filter(|r| r["op"] == op)
            .map(|r| r["body"].clone())
            .collect()
    }

    fn notifications(&self) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(self.dir.path().join("notifications.log")).unwrap_or_default();
        log.split("---\n")
            .filter(|n| !n.is_empty())
            .map(|n| n.lines().map(str::to_owned).collect())
            .collect()
    }
}

/// Stdin not a terminal, `--data-dir <tmp>/data`, a home and XDG
/// directories in the temp dir, a `PATH` of the stand-in notifier alone
/// (Omarchy installs the real one in /usr/bin, and it must never run
/// here), and no session bus.
fn run_at(base: &str, tmp: &Path, args: &[&str]) -> Output {
    let path = tmp.join("bin");
    Command::new(env!("CARGO_BIN_EXE_icloud-reminders"))
        .args(args)
        .arg("--data-dir")
        .arg(tmp.join("data"))
        .env("ICLOUD_SESSION_MOCK", "1")
        .env("ICLOUD_SESSION_MOCK_URL", base)
        .env("HOME", tmp.join("home"))
        .env("XDG_DATA_HOME", tmp.join("xdg"))
        .env("TZ", "UTC")
        .env("PATH", path)
        .env_remove("OMARCHY_PATH")
        .env("FAKE_REMINDERS_QUIET", "1")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn envelope(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {} / {}", stdout(out), stderr(out)))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn titles(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn lists_and_reminders_soonest_first() {
    let env = start();
    let lists = env.json(&["lists"]);
    assert_eq!(
        lists,
        json!([
            {"id": "LIST-GROCERIES", "name": "Groceries", "color": "#FF9500", "open": 2},
            {"id": "LIST-REMINDERS", "name": "Reminders", "color": "#007AFF", "open": 2},
        ])
    );
    let open = env.json(&["list"]);
    assert_eq!(titles(&open), ["Milk", "Call the dentist", "Renew passport", "Eggs"]);
    let milk = &open[0];
    assert_eq!(milk["id"], "REM-MILK");
    assert_eq!(milk["list"], json!({"id": "LIST-GROCERIES", "name": "Groceries"}));
    assert_eq!(milk["notes"], "Oat, 2 cartons");
    assert_eq!(milk["due"]["all_day"], false);
    assert!(milk["due"]["at"].as_str().unwrap().ends_with('Z'));
    assert!(open[3]["due"].is_null());

    assert_eq!(titles(&env.json(&["list", "groceries"])), ["Milk", "Eggs"]);
    assert_eq!(titles(&env.json(&["list", "--completed"])), ["Buy paint"]);
    assert_eq!(env.json(&["list", "--all"]).as_array().unwrap().len(), 5);

    // `--styled` forces text even when piped.
    let text = env.ok(&["list", "--styled"]);
    assert!(text.starts_with("[ ] Milk  due "), "{text}");
    assert!(text.contains("(Groceries)"), "{text}");
    let shown = env.ok(&["show", "passport", "--styled"]);
    assert!(
        shown.contains("Renew passport\n") && shown.contains("  Photos first"),
        "{shown}"
    );

    // Only the first read fetched every reminder; the rest asked for changes.
    let syncs = env.sent("changes/zone");
    let reminder_syncs: Vec<&Value> = syncs
        .iter()
        .filter(|b| b["zones"][0]["desiredRecordTypes"] == json!(["Reminder"]))
        .collect();
    assert!(reminder_syncs[0]["zones"][0].get("syncToken").is_none());
    assert!(
        reminder_syncs[1..]
            .iter()
            .all(|b| b["zones"][0]["syncToken"].is_string())
    );
}

#[test]
fn add_writes_a_reminder_apple_can_read() {
    let env = start();
    let out = env.json(&[
        "add",
        "Buy bread",
        "--list",
        "Groceries",
        "--notes",
        "rye",
        "--due",
        "2030-01-02 08:15",
    ]);
    assert_eq!(out["action"], "add");
    let r = &out["reminder"];
    assert_eq!(r["title"], "Buy bread");
    assert_eq!(r["notes"], "rye");
    assert_eq!(r["list"]["name"], "Groceries");
    assert_eq!(r["due"]["date"], "2030-01-02");
    assert_eq!(r["due"]["time"], "08:15");
    assert_eq!(r["due"]["time_zone"], "UTC");

    let modify = &env.sent("records/modify")[0];
    assert_eq!(modify["atomic"], true);
    assert_eq!(modify["zoneID"]["zoneName"], "Reminders");
    let rec = &modify["operations"][0]["record"];
    assert_eq!(modify["operations"][0]["operationType"], "create");
    assert_eq!(rec["recordType"], "Reminder");
    assert_eq!(rec["parent"]["recordName"], "List/LIST-GROCERIES");
    assert_eq!(rec["fields"]["DueDate"]["value"], 1_893_572_100_000i64); // 2030-01-02T08:15Z
    let stored = env.state.record(rec["recordName"].as_str().unwrap()).unwrap();
    let title = stored["fields"]["TitleDocument"]["value"].as_str().unwrap();
    assert_eq!(icloud_reminders::topotext::decode(title).unwrap(), "Buy bread");

    // An all-day due date.
    let out = env.json(&["add", "Pay rent", "--list", "Reminders", "--due", "2030-02-01"]);
    assert_eq!(
        out["reminder"]["due"],
        json!({"date": "2030-02-01", "time": null, "all_day": true, "time_zone": null, "at": "2030-02-01T09:00:00Z"})
    );
}

/// iCloud records no default list: with several, `add` names one, even
/// when one of them is called "Reminders".
#[test]
fn add_without_a_list_needs_one_and_names_them() {
    let env = start();
    let (exit, err) = env.fails(&["add", "Pay rent"]);
    assert_eq!((exit, &err["code"]), (2, &json!("usage")), "{err}");
    assert!(err["message"].as_str().unwrap().contains("--list"), "{err}");
    let hint = err["hint"].as_str().unwrap();
    assert!(
        hint.contains("\n  Groceries") && hint.contains("\n  Reminders"),
        "{hint}"
    );
    // In text mode: error:/hint: lines on stderr, nothing on stdout.
    let out = env.run(&["add", "Pay rent", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (2, ""));
    let err = stderr(&out);
    assert!(
        err.starts_with("error: add needs --list") && err.contains("\nhint: "),
        "{err}"
    );
    assert!(env.sent("records/modify").is_empty());
}

#[test]
fn complete_uncomplete_edit_and_delete() {
    let env = start();
    let out = env.json(&["complete", "milk"]);
    assert_eq!(out["reminder"]["completed"], true);
    assert!(out["reminder"]["completed_at"].is_string());
    let update = &env.sent("records/modify")[0]["operations"][0];
    assert_eq!(update["operationType"], "update");
    assert_eq!(update["record"]["recordName"], "Reminder/REM-MILK");
    assert!(
        update["record"]["recordChangeTag"]
            .as_str()
            .unwrap()
            .starts_with("tag-")
    );
    let fields = update["record"]["fields"].as_object().unwrap();
    let mut names: Vec<&String> = fields.keys().collect();
    names.sort();
    assert_eq!(
        names,
        ["Completed", "CompletionDate", "LastModifiedDate", "ResolutionTokenMap"]
    );
    // The phone's counter for `completed` was 3.
    let tokens: Value = serde_json::from_str(fields["ResolutionTokenMap"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(tokens["map"]["completed"]["counter"], 4);

    // "milk" now finds the completed one only for uncomplete.
    assert_eq!(env.json(&["uncomplete", "milk"])["reminder"]["completed"], false);
    let out = env.json(&["edit", "milk", "--title", "Oat milk", "--due", "2031-05-06 07:00"]);
    assert_eq!(out["reminder"]["title"], "Oat milk");
    assert_eq!(out["reminder"]["due"]["date"], "2031-05-06");
    let out = env.json(&["edit", "oat milk", "--no-due", "--notes", ""]);
    assert!(out["reminder"]["due"].is_null());
    assert_eq!(out["reminder"]["notes"], "");

    // Delete asks; without a terminal it needs --yes.
    for args in [&["delete", "eggs"][..], &["delete", "eggs", "--styled"]] {
        let out = env.run(args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
    }
    let (exit, err) = env.fails(&["delete", "eggs"]);
    assert_eq!((exit, &err["code"]), (2, &json!("usage")), "{err}");
    assert!(err["hint"].as_str().unwrap().contains("--yes"), "{err}");
    assert!(
        env.sent("records/modify")
            .iter()
            .all(|m| m["operations"][0]["record"]["recordName"] != "Reminder/REM-EGGS")
    );
    let out = env.json(&["delete", "eggs", "--yes"]);
    assert_eq!(
        out,
        json!({"action": "delete", "reminder": {"id": "REM-EGGS", "title": "Eggs"}})
    );
    let eggs = env.state.record("Reminder/REM-EGGS").unwrap();
    assert_eq!(eggs["fields"]["Deleted"]["value"], 1);
    assert!(!titles(&env.json(&["list", "--all"])).contains(&"Eggs".to_owned()));
}

#[test]
fn complete_and_uncomplete_pick_the_one_they_can_change() {
    // An open and a completed "Buy milk".
    let two = || {
        let env = start();
        let now = now_ms();
        env.state.put(fake::reminder(
            "REM-OPEN",
            "List/LIST-GROCERIES",
            "Buy milk",
            "",
            None,
            now - MINUTE,
        ));
        let mut done = fake::reminder("REM-DONE", "List/LIST-GROCERIES", "Buy milk", "", None, now - MINUTE);
        done["fields"]["Completed"]["value"] = json!(1);
        env.state.put(done);
        env
    };
    let env = two();
    let out = env.json(&["complete", "buy milk"]);
    assert_eq!(
        (&out["changed"], &out["reminder"]["id"]),
        (&json!(true), &json!("REM-OPEN"))
    );
    let env = two();
    let out = env.json(&["uncomplete", "buy milk"]);
    assert_eq!(
        (&out["changed"], &out["reminder"]["id"]),
        (&json!(true), &json!("REM-DONE"))
    );
    // Already open: said so, nothing written.
    let writes = env.sent("records/modify").len();
    let out = env.run(&["uncomplete", "REM-OPEN", "--styled"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "\"Buy milk\" was already open");
    assert_eq!(env.sent("records/modify").len(), writes);
}

#[test]
fn a_change_made_elsewhere_meanwhile_is_written_over_once() {
    let env = start();
    env.json(&["list"]);
    // The phone renames it after our last sync: our change tag is stale.
    let mut phone = env.state.record("Reminder/REM-CALL").unwrap();
    phone["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Call the dentist at 9"));
    env.state.put(phone);

    let out = env.json(&["--cached", "complete", "dentist"]);
    assert_eq!(out["reminder"]["completed"], true);
    // The phone's title survived; one CONFLICT, a lookup, the retry.
    assert_eq!(out["reminder"]["title"], "Call the dentist at 9");
    assert_eq!(
        env.ops()[env.ops().len() - 3..],
        ["records/modify", "records/lookup", "records/modify"]
    );

    // The phone already completed it: after the CONFLICT nothing is left
    // to write.
    let mut phone = env.state.record("Reminder/REM-EGGS").unwrap();
    phone["fields"]["Completed"]["value"] = json!(1);
    env.state.put(phone);
    let out = env.json(&["--cached", "complete", "eggs"]);
    assert_eq!(out["reminder"]["completed"], true);
    assert_eq!(env.ops()[env.ops().len() - 2..], ["records/modify", "records/lookup"]);
}

#[test]
fn sync_pages_and_recovers_from_an_expired_token() {
    let env = start();
    *env.state.page.lock().unwrap() = 1;
    let out = env.json(&["sync"]);
    assert_eq!(out, json!({"lists": 2, "reminders": 5, "changed": 5, "full": true}));
    assert!(env.ops().len() >= 7, "one page per record: {:?}", env.ops());

    let out = env.json(&["sync"]);
    assert_eq!(out["changed"], 0);
    assert_eq!(out["full"], false);

    // A token the server no longer knows, or can't read: everything again.
    let cache = env.data().join("cache.json");
    for token in ["stale", "garbled"] {
        let mut c: Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
        c["sync_token"] = json!(token);
        std::fs::write(&cache, c.to_string()).unwrap();
        assert_eq!(env.json(&["sync"])["full"], true, "{token}");
    }
    assert_eq!(env.json(&["sync", "--full"])["reminders"], 5);
}

#[test]
fn an_unreadable_reminder_is_skipped_kept_and_named() {
    let env = start();
    env.json(&["sync"]);
    // A phone writes a title this version can't decode, and renames another.
    let mut bad = env.state.record("Reminder/REM-CALL").unwrap();
    bad["fields"]["TitleDocument"]["value"] = json!("bm90IHpsaWI=");
    env.state.put(bad);
    let mut eggs = env.state.record("Reminder/REM-EGGS").unwrap();
    eggs["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("A dozen eggs"));
    env.state.put(eggs);

    // A delta, then a full fetch, which drops whatever it lacks: either way
    // the good change lands and the bad reminder stays as last synced.
    for args in [&["sync"][..], &["sync", "--full"]] {
        let out = env.run(args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        let report = envelope(&out)["data"].clone();
        assert_eq!(report["reminders"], 5, "{args:?}");
        let warning = report["warning"].as_str().unwrap();
        assert!(warning.contains("Reminder/REM-CALL: TitleDocument"), "{warning}");
        assert!(stderr(&out).contains("Reminder/REM-CALL"), "{}", stderr(&out));
        let open = titles(&env.json(&["--cached", "list"]));
        assert!(
            open.contains(&"Call the dentist".into()) && open.contains(&"A dozen eggs".into()),
            "{open:?}"
        );
    }
    // The token stayed put, so a plain sync fetches the bad record again.
    assert!(env.json(&["sync"])["warning"].is_string());
    assert!(env.sent("records/modify").is_empty());
}

#[test]
fn signed_out_exits_3() {
    let env = start();
    *env.state.signed_out.lock().unwrap() = true;
    for args in [&["list"][..], &["lists"], &["show", "milk"], &["add", "x"], &["sync"]] {
        let (exit, err) = env.fails(args);
        assert_eq!((exit, &err["code"]), (3, &json!("sign_in_required")), "{args:?}: {err}");
        assert!(
            err["hint"].as_str().unwrap().contains("icloud-session sign-in"),
            "{err}"
        );
    }
    let out = env.run(&["list", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (3, ""));
    let err = stderr(&out);
    assert!(
        err.starts_with("error: sign in to iCloud required") && err.contains("hint: run `icloud-session sign-in`"),
        "{err}"
    );
}

#[test]
fn offline_is_unavailable_and_cached_reads_need_no_network() {
    let env = start();
    env.json(&["sync"]);
    // Nothing listens on port 9.
    let out = run_at("http://127.0.0.1:9", env.dir.path(), &["list"]);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    let err = &envelope(&out)["error"];
    assert_eq!(err["code"], "offline");
    assert!(err["hint"].is_string(), "{err}");
    let out = run_at("http://127.0.0.1:9", env.dir.path(), &["--cached", "list"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = run_at("http://127.0.0.1:9", env.dir.path(), &["--cached", "complete", "milk"]);
    assert_eq!(code(&out), 5, "writes need the network");
}

#[test]
fn ids_only_count_and_quiet() {
    let env = start();
    let open = env.json(&["list"]);
    let ids: Vec<&str> = open
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 4);
    assert_eq!(&ids[..2], ["REM-MILK", "REM-CALL"]);
    assert_eq!(env.ok(&["list", "--ids-only"]).lines().collect::<Vec<_>>(), ids);
    assert_eq!(env.ok(&["list", "--count"]).trim(), "4");
    assert_eq!(env.ok(&["list", "--all", "--count"]).trim(), "5");
    assert_eq!(env.ok(&["list", "groceries", "--count", "--json"]).trim(), "2");
    // ids-only wins over count.
    assert_eq!(
        env.ok(&["list", "--completed", "--count", "--ids-only"])
            .lines()
            .count(),
        1
    );
    // --quiet is the data alone.
    let quiet: Value = serde_json::from_str(&env.ok(&["list", "--quiet"])).unwrap();
    assert_eq!(quiet, open);

    assert_eq!(env.ok(&["lists", "--ids-only"]), "LIST-GROCERIES\nLIST-REMINDERS\n");
    assert_eq!(env.ok(&["lists", "--count"]).trim(), "2");
    assert_eq!(env.ok(&["show", "milk", "--ids-only"]).trim(), "REM-MILK");
    assert_eq!(env.ok(&["complete", "milk", "--ids-only"]).trim(), "REM-MILK");
    assert_eq!(env.ok(&["delete", "eggs", "--yes", "--ids-only"]).trim(), "REM-EGGS");
    // The phone renames one: sync counts what changed since the last one.
    env.json(&["sync"]);
    let mut call = env.state.record("Reminder/REM-CALL").unwrap();
    call["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Call the dentist at 9"));
    env.state.put(call);
    assert_eq!(env.ok(&["sync", "--count"]).trim(), "1");
    assert_eq!(env.ok(&["background", "--count"]).trim(), "0");
    // A failure in a machine mode is still the error object on stdout.
    let (exit, err) = env.fails(&["list", "nowhere", "--ids-only"]);
    assert_eq!((exit, &err["code"]), (4, &json!("not_found")), "{err}");
}

#[test]
fn background_notifies_once_when_due() {
    let env = start();
    // Installed before signing in: that run syncs nothing, so it can't mark
    // the backlog; the first synced run after sign-in does.
    *env.state.signed_out.lock().unwrap() = true;
    assert_eq!(env.json(&["background"])["synced"], false);
    *env.state.signed_out.lock().unwrap() = false;
    // First synced run: Milk (due a minute ago) is the backlog, marked silently.
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["synced"], true);
    assert_eq!(out["notified"], json!([]));
    assert!(env.notifications().is_empty());

    // The phone adds one due right now, set ahead of time.
    let now = now_ms();
    env.state.put(fake::reminder(
        "REM-NOW",
        "List/LIST-GROCERIES",
        "Bread",
        "",
        Some(now - 5_000),
        now - 10 * MINUTE,
    ));
    let out = env.json(&["background"]);
    assert_eq!(out["synced"], false, "synced a moment ago");
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["notified"], json!([{"id": "REM-NOW", "title": "Bread"}]));
    let shown = env.notifications();
    assert_eq!(shown.len(), 1);
    let args = &shown[0];
    assert_eq!(args[..6], ["--app-name", "Reminders", "-g", "󰢌", "-u", "normal"]);
    assert_eq!(args[6], "Bread");
    assert!(args[7].starts_with("Groceries · "), "{args:?}");
    assert_eq!(args[8..], ["--exec", "icloud-reminders-app"]);

    // Once.
    assert_eq!(env.json(&["background", "--sync"])["notified"], json!([]));
    assert_eq!(env.notifications().len(), 1);
    // As the timer runs it: text, and nothing when there is nothing to say.
    assert_eq!(env.ok(&["background", "--styled"]), "");
}

#[test]
fn background_without_a_session_still_notifies_from_the_cache() {
    let env = start();
    env.json(&["background"]);
    let now = now_ms();
    env.state.put(fake::reminder(
        "REM-NOW",
        "List/LIST-REMINDERS",
        "Stretch",
        "",
        Some(now - 5_000),
        now - 10 * MINUTE,
    ));
    env.json(&["sync"]);
    *env.state.signed_out.lock().unwrap() = true;
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["synced"], false);
    assert!(out["sync_error"].as_str().unwrap().contains("sign in"), "{out}");
    assert_eq!(out["notified"][0]["title"], "Stretch");
    // A failed attempt counts: the next minute does not try again.
    let out = env.json(&["background"]);
    assert_eq!(
        (out["synced"].clone(), out["sync_error"].clone()),
        (json!(false), Value::Null)
    );
}

#[test]
fn usage_not_found_and_ambiguous_are_coded() {
    let env = start();
    for args in [
        &["frobnicate"][..],
        &["show"],
        &["add"],
        &["edit", "milk"],
        &["edit", "milk", "--due", "someday"],
        &["edit", "milk", "--due", "today", "--no-due"],
        &["list", "--all", "--completed"],
        &["add", "  "],
    ] {
        let (exit, err) = env.fails(args);
        assert_eq!((exit, &err["code"]), (2, &json!("usage")), "{args:?}: {err}");
        // Text mode: clap's or our message on stderr, the same exit code.
        let out = env.run(&[args, &["--styled"][..]].concat());
        assert_eq!((code(&out), stdout(&out).as_str()), (2, ""), "{args:?}");
        assert!(!stderr(&out).is_empty(), "{args:?}");
    }
    let (_, err) = env.fails(&["show"]);
    assert_eq!(err["hint"], "run `icloud-reminders --help`");

    // A missing reminder or list: exit 4.
    for args in [
        &["show", "zzz"][..],
        &["complete", "zzz"],
        &["edit", "zzz", "--title", "x"],
        &["delete", "zzz", "--yes"],
        &["list", "zzz"],
        &["add", "x", "--list", "zzz"],
    ] {
        let (exit, err) = env.fails(args);
        assert_eq!((exit, &err["code"]), (4, &json!("not_found")), "{args:?}: {err}");
    }
    let (_, err) = env.fails(&["list", "zzz"]);
    assert!(err["hint"].as_str().unwrap().contains("\n  Groceries"), "{err}");

    // An ambiguous name: exit 2, the matches in the hint.
    let (exit, err) = env.fails(&["--cached", "show", "e"]);
    assert_eq!((exit, &err["code"]), (2, &json!("ambiguous")), "{err}");
    let hint = err["hint"].as_str().unwrap();
    assert!(hint.contains("(REM-CALL)") && hint.contains("(REM-EGGS)"), "{hint}");
    let (exit, err) = env.fails(&["--cached", "list", "r"]);
    assert_eq!((exit, &err["code"]), (2, &json!("ambiguous")), "{err}");
    let out = env.run(&["--cached", "show", "e", "--styled"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).starts_with("error: \"e\" matches "), "{}", stderr(&out));
    assert!(env.sent("records/modify").is_empty());
}

/// The shared libraries a 64-bit little-endian ELF executable needs (its
/// `DT_NEEDED` entries).
fn needed_libs(path: &str) -> Vec<String> {
    let f = std::fs::read(path).unwrap();
    let u16_at = |o: usize| u16::from_le_bytes(f[o..o + 2].try_into().unwrap()) as usize;
    let u64_at = |o: usize| u64::from_le_bytes(f[o..o + 8].try_into().unwrap()) as usize;
    assert_eq!(&f[..5], b"\x7fELF\x02", "a 64-bit ELF file");
    let (phoff, phentsize, phnum) = (u64_at(0x20), u16_at(0x36), u16_at(0x38));
    let headers: Vec<usize> = (0..phnum).map(|i| phoff + i * phentsize).collect();
    let p_type = |h: usize| u32::from_le_bytes(f[h..h + 4].try_into().unwrap());
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

/// The timer runs `icloud-reminders` every minute: it must not load GTK.
#[test]
fn the_command_line_binary_does_not_link_gtk() {
    let ui = |libs: Vec<String>| {
        libs.into_iter()
            .filter(|l| ["libgtk-4.", "libadwaita-1."].iter().any(|p| l.starts_with(p)))
            .count()
    };
    let cli = needed_libs(env!("CARGO_BIN_EXE_icloud-reminders"));
    assert!(cli.iter().any(|l| l.starts_with("libc.")), "{cli:?}");
    assert_eq!(ui(cli.clone()), 0, "{cli:?}");
    // Named but not built without the `ui` feature.
    if let Some(app) = option_env!("CARGO_BIN_EXE_icloud-reminders-app").filter(|p| Path::new(p).exists()) {
        assert_eq!(ui(needed_libs(app)), 2);
    }
}

/// With no command, `icloud-reminders` becomes `icloud-reminders-app` from
/// its own directory (the same process: its exit code is the app's).
#[test]
fn no_command_runs_the_app_beside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("icloud-reminders");
    std::fs::copy(env!("CARGO_BIN_EXE_icloud-reminders"), &cli).unwrap();
    let marker = tmp.path().join("ran");
    write_script(
        &tmp.path().join("icloud-reminders-app"),
        &format!("#!/bin/sh\necho \"$#\" > '{}'\nexit 7\n", marker.display()),
    );
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
    assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "0");
}

/// The folder of a test, turned on: `<tmp>/Reminders`.
fn folder(env: &Env) -> PathBuf {
    let dir = env.dir.path().join("Reminders");
    env.json(&["folder", "start", dir.to_str().unwrap()]);
    dir
}

fn modifies(env: &Env) -> usize {
    env.sent("records/modify").len()
}

fn all_titles(env: &Env, list: &str) -> Vec<String> {
    titles(&env.json(&["list", list, "--all"]))
}

#[test]
fn folder_start_writes_one_checklist_per_list() {
    let env = start();
    let dir = folder(&env);
    let groceries = std::fs::read_to_string(dir.join("Groceries.md")).unwrap();
    let lines: Vec<&str> = groceries.lines().collect();
    assert_eq!(lines[0], "# Groceries");
    assert!(lines[2].starts_with("- [ ] Milk 📅 20"), "{groceries}");
    assert_eq!(lines[3], "    Oat, 2 cartons");
    assert_eq!(lines[4], "- [ ] Eggs");
    let reminders = std::fs::read_to_string(dir.join("Reminders.md")).unwrap();
    assert!(reminders.contains("- [ ] Call the dentist 📅 "), "{reminders}");
    // Completed in the last week: still listed, after the open ones.
    assert!(reminders.trim_end().ends_with("- [x] Buy paint"), "{reminders}");
    assert_eq!(modifies(&env), 0);

    let status = env.json(&["folder", "status"]);
    assert_eq!(status["on"], true);
    assert_eq!(status["files"].as_array().unwrap().len(), 2);
    // Off: the files stay, and nothing reads them any more.
    env.json(&["folder", "stop"]);
    assert!(dir.join("Groceries.md").exists());
    assert_eq!(env.json(&["folder", "status"])["on"], false);
    let (exit, error) = env.fails(&["folder", "sync"]);
    assert_eq!((exit, error["code"].as_str()), (3, Some("not_set_up")));
}

#[test]
fn a_file_s_changes_reach_icloud_once_and_come_back() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    // On the phone: tick Eggs, add a line as plain text, another with a date.
    let edited = text.replace("- [ ] Eggs", "- [x] Eggs") + "Bread\n- [ ] Butter 📅 2030-01-02 09:30\n    Salted\n";
    std::fs::write(&path, edited).unwrap();

    let out = env.json(&["background"]);
    assert_eq!(out["synced"], true, "a changed file syncs at once");
    assert_eq!(out["folder"]["added"], 2);
    assert_eq!(out["folder"]["edited"], 1);
    assert_eq!(out["folder"]["deleted"], 0);
    let eggs = env.state.record("Reminder/REM-EGGS").unwrap();
    assert_eq!(eggs["fields"]["Completed"]["value"], 1);
    let shown = env.json(&["show", "butter"]);
    assert_eq!(shown["notes"], "Salted");
    assert_eq!(shown["due"]["date"], "2030-01-02");
    assert_eq!(shown["list"]["name"], "Groceries");
    // Written back as this program writes it.
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("- [ ] Bread\n"), "{after}");
    assert!(
        after.contains("- [ ] Butter 📅 2030-01-02 09:30\n    Salted\n"),
        "{after}"
    );
    assert!(after.trim_end().ends_with("- [x] Eggs"), "{after}");

    // Nothing left to send: no further writes, and nothing said.
    let sent = modifies(&env);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(modifies(&env), sent);
    assert_eq!(out["folder"]["written"], json!([]));
    assert_eq!(
        all_titles(&env, "Groceries").iter().filter(|t| *t == "Bread").count(),
        1
    );
}

#[test]
fn icloud_s_changes_reach_the_file_and_a_removed_line_deletes() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    // The iPhone adds one and retitles another.
    env.state.put(fake::reminder(
        "REM-TEA",
        "List/LIST-GROCERIES",
        "Tea",
        "",
        None,
        now_ms(),
    ));
    env.ok(&["edit", "eggs", "--title", "Eggs, a dozen"]);
    env.json(&["background", "--sync"]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("- [ ] Tea\n") && text.contains("- [ ] Eggs, a dozen\n"),
        "{text}"
    );

    // A line removed in the file is deleted, the Apple way: Deleted = 1.
    let sent = modifies(&env);
    std::fs::write(&path, text.replace("- [ ] Tea\n", "")).unwrap();
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["deleted"], 1);
    assert_eq!(modifies(&env), sent + 1);
    let tea = env.state.record("Reminder/REM-TEA").unwrap();
    assert_eq!(tea["fields"]["Deleted"]["value"], 1);
    assert!(!all_titles(&env, "Groceries").contains(&"Tea".to_owned()));
    // A light rewrite is an edit of the same reminder, not a new one.
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("Eggs, a dozen", "Eggs, two dozen")).unwrap();
    let out = env.json(&["background"]);
    assert_eq!(
        (out["folder"]["edited"].as_i64(), out["folder"]["added"].as_i64()),
        (Some(1), Some(0))
    );
    assert_eq!(env.json(&["show", "REM-EGGS"])["title"], "Eggs, two dozen");
}

#[test]
fn a_gutted_file_is_held_until_asked() {
    let env = start();
    for n in 0..6 {
        env.state.put(fake::reminder(
            &format!("REM-EXTRA-{n}"),
            "List/LIST-GROCERIES",
            &format!("Extra number {n}"),
            "",
            None,
            now_ms(),
        ));
    }
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let before = all_titles(&env, "Groceries").len();
    assert_eq!(before, 8);
    // A sync gone wrong leaves only the heading.
    std::fs::write(&path, "# Groceries\n").unwrap();
    let sent = modifies(&env);
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["held"][0]["file"], "Groceries.md");
    assert_eq!(out["folder"]["held"][0]["removing"], 8);
    assert_eq!(modifies(&env), sent, "nothing is deleted");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Groceries\n",
        "the file is left alone"
    );
    let said = env.notifications();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].iter().any(|a| a.contains("Groceries.md not applied")),
        "{said:?}"
    );
    // Said once, and no sync forced every minute while it waits.
    let ops = env.ops().len();
    env.json(&["background"]);
    assert_eq!(env.notifications().len(), 1);
    assert_eq!(env.ops().len(), ops);
    assert_eq!(env.json(&["folder", "status"])["files"][0]["held"], true);

    // Asking without --yes says so and exits 2; with it, the lines go.
    let out = env.run(&["folder", "sync"]);
    assert_eq!(code(&out), 2);
    assert_eq!(envelope(&out)["data"]["held"][0]["removing"], 8);
    assert_eq!(all_titles(&env, "Groceries").len(), before);
    let out = env.json(&["folder", "sync", "--yes"]);
    assert_eq!(out["deleted"], 8);
    assert!(all_titles(&env, "Groceries").is_empty());
}

#[test]
fn a_conflict_copy_ticks_and_adds_and_is_removed() {
    let env = start();
    let dir = folder(&env);
    // Syncthing's copy of an older file: Milk ticked there, one new line,
    // and Eggs missing, which must not delete it.
    let copy = dir.join("Groceries.sync-conflict-20261114-093000-ABCDEFG.md");
    std::fs::write(&copy, "# Groceries\n\n- [x] Milk\n- [ ] Butter\n").unwrap();
    let out = env.json(&["background"]);
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["edited"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(out["folder"]["deleted"], 0);
    assert!(!copy.exists());
    let titles = all_titles(&env, "Groceries");
    assert!(
        titles.contains(&"Eggs".to_owned()) && titles.contains(&"Butter".to_owned()),
        "{titles:?}"
    );
    let milk = env.state.record("Reminder/REM-MILK").unwrap();
    assert_eq!(milk["fields"]["Completed"]["value"], 1);
}

#[test]
fn a_change_made_offline_waits_and_is_sent_once() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Bread\n").unwrap();
    *env.state.signed_out.lock().unwrap() = true;
    let out = env.json(&["background"]);
    assert_eq!(out["synced"], false);
    assert_eq!(out["folder"], Value::Null, "the file waits for a sync");
    assert!(std::fs::read_to_string(&path).unwrap().contains("- [ ] Bread"));
    *env.state.signed_out.lock().unwrap() = false;
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 1);
    env.json(&["background", "--sync"]);
    assert_eq!(
        all_titles(&env, "Groceries").iter().filter(|t| *t == "Bread").count(),
        1
    );
}

#[test]
fn starting_again_over_existing_files_only_adds_what_is_new() {
    let env = start();
    let dir = folder(&env);
    env.json(&["folder", "stop"]);
    let path = dir.join("Groceries.md");
    // While it was off: a line added in the file, one removed, and iCloud gained one.
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("- [ ] Eggs\n", "") + "- [ ] Bread\n").unwrap();
    env.state.put(fake::reminder(
        "REM-TEA",
        "List/LIST-GROCERIES",
        "Tea",
        "",
        None,
        now_ms(),
    ));
    let out = env.json(&["folder", "start", dir.to_str().unwrap()]);
    assert_eq!((out["added"].as_i64(), out["deleted"].as_i64()), (Some(1), Some(0)));
    let mut titles = all_titles(&env, "Groceries");
    titles.sort();
    assert_eq!(titles, ["Bread", "Eggs", "Milk", "Tea"]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("- [ ] Eggs\n") && text.contains("- [ ] Tea\n"), "{text}");
    // Elsewhere is refused while it is on.
    let (exit, error) = env.fails(&["folder", "start", "/tmp/elsewhere"]);
    assert_eq!((exit, error["code"].as_str()), (2, Some("usage")));
}
