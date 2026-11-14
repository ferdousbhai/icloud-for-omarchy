//! The built binary's command line against the fake CloudKit server
//! (examples/fake_reminders.rs) through `icloud-session`'s mock mode: no
//! D-Bus, no Apple account, a temporary data directory, and a stand-in
//! `omarchy-notification-send` that records what it was asked to show.

#[path = "../examples/fake_reminders.rs"]
mod fake;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use icloud_app::testing::{
    assert_cli_links_no_gtk, assert_no_command_runs_the_app, code, command, envelope, stderr, stdout, write_script,
};
use icloud_session::time::now_ms;
use serde_json::{Value, json};

const MINUTE: i64 = 60_000;

struct Env {
    base: String,
    state: Arc<fake::State>,
    dir: tempfile::TempDir,
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
        let v = envelope(&out.stdout);
        assert_eq!(v["ok"], true, "{args:?}");
        assert!(v["summary"].is_string(), "{args:?}");
        v["data"].clone()
    }

    /// A failed run: its exit code and the `error` of the envelope on stdout.
    fn fails(&self, args: &[&str]) -> (i32, Value) {
        let out = self.run(args);
        let v = envelope(&out.stdout);
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

    /// Starts `background` with the fake holding back the answer to its
    /// `nth` write (which it applies), and returns once that write is in.
    fn background_held_at_write(&self, nth: usize) -> std::process::Child {
        let number = self.sent("records/modify").len() + nth;
        *self.state.hold_modify.lock().unwrap() = Some(number);
        let mut child = command_at(&self.base, self.dir.path(), &["background"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Applied: the record carries a tag newer than the one sent.
        let written = || {
            let sent = self.sent("records/modify");
            let record = &sent.get(number - 1)?["operations"][0]["record"];
            let stored = self.state.record(record["recordName"].as_str()?)?;
            (stored["recordChangeTag"] != record["recordChangeTag"]).then_some(())
        };
        let mut tries = 0;
        while written().is_none() && tries < 2000 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            tries += 1;
        }
        if written().is_none() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("write {number} never arrived: {:?}", self.ops());
        }
        child
    }

    fn release_writes(&self) {
        *self.state.hold_modify.lock().unwrap() = None;
    }

    fn creates(&self) -> usize {
        self.sent("records/modify")
            .iter()
            .filter(|m| m["operations"][0]["operationType"] == "create")
            .count()
    }

    fn notifications(&self) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(self.dir.path().join("notifications.log")).unwrap_or_default();
        log.split("---\n")
            .filter(|n| !n.is_empty())
            .map(|n| n.lines().map(str::to_owned).collect())
            .collect()
    }
}

/// The binary against the fake server (see [`command`]), with `TZ=UTC` and
/// a `PATH` of the stand-in notifier alone (Omarchy installs the real one
/// in /usr/bin, and it must never run here).
fn run_at(base: &str, tmp: &Path, args: &[&str]) -> Output {
    command_at(base, tmp, args).output().unwrap()
}

fn command_at(base: &str, tmp: &Path, args: &[&str]) -> std::process::Command {
    let mut c = command(env!("CARGO_BIN_EXE_icloud-reminders"), base, tmp);
    c.args(args)
        .env("TZ", "UTC")
        .env("PATH", tmp.join("bin"))
        .env_remove("OMARCHY_PATH");
    c
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
fn a_change_to_a_reminder_deleted_elsewhere_meanwhile_is_not_found() {
    let env = start();
    env.json(&["list"]);
    // The phone deletes it after our last sync: our change tag is stale.
    let mut phone = env.state.record("Reminder/REM-CALL").unwrap();
    phone["fields"]["Deleted"]["value"] = json!(1);
    env.state.put(phone);

    let (exit, err) = env.fails(&["--cached", "complete", "dentist"]);
    assert_eq!((exit, &err["code"]), (4, &json!("not_found")), "{err}");
    assert_eq!(err["message"], "\"Call the dentist\" has been deleted");
    // One CONFLICT and a lookup; nothing is written over a deleted reminder.
    assert_eq!(env.ops()[env.ops().len() - 2..], ["records/modify", "records/lookup"]);
    // The cache no longer has it.
    let (exit, err) = env.fails(&["--cached", "show", "dentist"]);
    assert_eq!((exit, &err["code"]), (4, &json!("not_found")), "{err}");
}

#[test]
fn a_reminder_removed_for_good_elsewhere_meanwhile_is_gone_not_an_error() {
    let env = start();
    env.json(&["list"]);
    // Recently Deleted was emptied on the phone after our last sync: CloudKit
    // has no such record any more.
    env.state.purge("Reminder/REM-CALL");
    let (exit, err) = env.fails(&["--cached", "complete", "dentist"]);
    assert_eq!((exit, &err["code"]), (4, &json!("not_found")), "{err}");
    assert_eq!(err["message"], "\"Call the dentist\" has been deleted");
    // The cache no longer has it.
    let (exit, _) = env.fails(&["--cached", "show", "dentist"]);
    assert_eq!(exit, 4);
    // Deleting one that is already gone is done.
    env.state.purge("Reminder/REM-EGGS");
    let out = env.json(&["--cached", "delete", "eggs", "--yes"]);
    assert_eq!(out["reminder"]["id"], "REM-EGGS");
    assert!(titles(&env.json(&["--cached", "list"])).iter().all(|t| t != "Eggs"));
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
        let report = envelope(&out.stdout)["data"].clone();
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
    let err = &envelope(&out.stdout)["error"];
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

/// The timer runs `icloud-reminders` every minute: it must not load GTK.
#[test]
fn the_command_line_binary_does_not_link_gtk() {
    assert_cli_links_no_gtk(
        env!("CARGO_BIN_EXE_icloud-reminders"),
        option_env!("CARGO_BIN_EXE_icloud-reminders-app"),
        &["libgtk-4.", "libadwaita-1."],
    );
}

#[test]
fn no_command_runs_the_app_beside_it() {
    assert_no_command_runs_the_app(env!("CARGO_BIN_EXE_icloud-reminders"), "icloud-reminders");
}

/// The folder of a test, written by a first background run:
/// `<home>/Documents/Reminders`.
fn folder(env: &Env) -> PathBuf {
    env.json(&["background", "--sync"]);
    env.dir.path().join("home/Documents/Reminders")
}

fn modifies(env: &Env) -> usize {
    env.sent("records/modify").len()
}

fn all_titles(env: &Env, list: &str) -> Vec<String> {
    titles(&env.json(&["list", list, "--all"]))
}

#[test]
fn the_timer_writes_one_checklist_per_list() {
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
fn a_gutted_file_deletes_nothing_and_is_put_back() {
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
    let whole = std::fs::read_to_string(&path).unwrap();
    assert_eq!(all_titles(&env, "Groceries").len(), 8);
    // A sync gone wrong leaves only the heading, and one new line.
    std::fs::write(&path, "# Groceries\n\n- [ ] Bread\n").unwrap();
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["restored"][0]["file"], "Groceries.md");
    assert_eq!(out["folder"]["restored"][0]["kept"], 8);
    assert_eq!(out["folder"]["deleted"], 0);
    assert_eq!(out["folder"]["added"], 1, "the rest of the file still counts");
    assert_eq!(all_titles(&env, "Groceries").len(), 9);
    let back = std::fs::read_to_string(&path).unwrap();
    assert!(
        back.starts_with(whole.trim_end()) && back.contains("- [ ] Bread"),
        "{back}"
    );
    let said = env.notifications();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].iter().any(|a| a.contains("Groceries.md restored")), "{said:?}");
    // Put back, so there is nothing more to say or send.
    let sent = modifies(&env);
    env.json(&["background", "--sync"]);
    assert_eq!((env.notifications().len(), modifies(&env)), (1, sent));
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
fn a_file_found_already_there_only_adds_what_is_new() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    // As on a computer that never wrote this folder: no state, but a file
    // with a line iCloud lacks and without one it has; iCloud gained one.
    std::fs::remove_file(env.data().join("folder.json")).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    // It is also older than iCloud on Milk: no notes, another title, ticked.
    assert!(
        text.contains("- [ ] Milk ") && text.contains("    Oat, 2 cartons\n"),
        "{text}"
    );
    let stale = text
        .replace("- [ ] Eggs\n", "")
        .replace("    Oat, 2 cartons\n", "")
        .replace("- [ ] Milk ", "- [x] Milk, 1 l ");
    std::fs::write(&path, stale + "- [ ] Bread\n").unwrap();
    env.state.put(fake::reminder(
        "REM-TEA",
        "List/LIST-GROCERIES",
        "Tea",
        "",
        None,
        now_ms(),
    ));
    let out = env.json(&["background", "--sync"]);
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["deleted"].as_i64()),
        (Some(1), Some(0))
    );
    assert_eq!(
        out["folder"]["edited"], 0,
        "what iCloud has is not changed from such a file"
    );
    let mut titles = all_titles(&env, "Groceries");
    titles.sort();
    assert_eq!(titles, ["Bread", "Eggs", "Milk", "Tea"]);
    let milk = env.json(&["show", "REM-MILK"]);
    assert_eq!(
        (&milk["notes"], &milk["completed"]),
        (&json!("Oat, 2 cartons"), &json!(false))
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("- [ ] Eggs\n") && text.contains("- [ ] Tea\n"), "{text}");
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_renamed_list_takes_its_conflict_copies_along() {
    let env = start();
    let dir = folder(&env);
    std::fs::write(
        dir.join("Groceries.sync-conflict-20261114-093000-ABCDEFG.md"),
        "# Groceries\n\n- [ ] Butter\n",
    )
    .unwrap();
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Name"]["value"] = json!("Food");
    env.state.put(list);
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["added"], 1, "{out}");
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
    assert!(all_titles(&env, "Food").contains(&"Butter".to_owned()));
}

#[test]
fn a_file_left_by_a_deleted_list_is_not_taken_by_a_new_list_of_that_name() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    // A line added to the file, and the list deleted on the phone: the file
    // has something that was never sent, so it stays.
    let text = std::fs::read_to_string(&path).unwrap() + "- [ ] Bread\n";
    std::fs::write(&path, &text).unwrap();
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Deleted"]["value"] = json!(1);
    env.state.put(list.clone());
    let out = env.json(&["background", "--sync"]);
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with("Groceries.md: "),
        "{out}"
    );
    // Said once.
    assert_eq!(env.json(&["background", "--sync"])["folder"]["warnings"], json!([]));

    // Another list of that name is made: it gets a file of its own.
    list["recordName"] = json!("List/LIST-NEW");
    list["fields"]["Deleted"]["value"] = json!(0);
    list["fields"]["ReminderIDs"]["value"] = json!("[]");
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(modifies(&env), 0);
    assert_eq!(file_names(&dir), ["Groceries 2.md", "Groceries.md", "Reminders.md"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(
        std::fs::read_to_string(dir.join("Groceries 2.md")).unwrap(),
        "# Groceries\n"
    );
    // Once the leftover is removed, the list's file takes the name.
    std::fs::remove_file(&path).unwrap();
    env.json(&["background", "--sync"]);
    assert_eq!(file_names(&dir), ["Groceries.md", "Reminders.md"]);
}

fn count(titles: &[String], title: &str) -> usize {
    titles.iter().filter(|t| *t == title).count()
}

#[test]
fn a_run_killed_between_writes_does_not_add_twice() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text + "- [ ] Bread\n- [x] Butter 📅 2030-01-02 09:30\n    Salted\n",
    )
    .unwrap();
    // Bread is added; Butter reaches iCloud, and the run is killed before it
    // hears so.
    let mut child = env.background_held_at_write(2);
    child.kill().unwrap();
    child.wait().unwrap();
    env.release_writes();
    assert_eq!(env.creates(), 2);

    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(env.creates(), 2, "neither is made again");
    let titles = all_titles(&env, "Groceries");
    assert_eq!(
        (count(&titles, "Bread"), count(&titles, "Butter")),
        (1, 1),
        "{titles:?}"
    );
    // What was left of the line is done: the tick.
    let butter = env.json(&["show", "butter"]);
    assert_eq!(
        (&butter["completed"], &butter["notes"]),
        (&json!(true), &json!("Salted"))
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("- [ ] Bread\n") && after.contains("- [x] Butter 📅 2030-01-02 09:30\n"),
        "{after}"
    );

    // A second line of a title is a second reminder all the same.
    std::fs::write(&path, after + "- [ ] Bread\n").unwrap();
    assert_eq!(env.json(&["background"])["folder"]["added"], 1);
    assert_eq!(count(&all_titles(&env, "Groceries"), "Bread"), 2);
}

#[test]
fn what_a_conflict_copy_added_is_not_deleted_by_a_file_that_never_held_it() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    let copy = dir.join("Groceries.sync-conflict-20261114-093000-ABCDEFG.md");
    std::fs::write(&copy, "# Groceries\n\n- [x] Eggs\n- [ ] Butter\n").unwrap();
    // While the copy's lines are sent, the phone's next edit of the file
    // arrives: the run leaves the file to the next one.
    let mut child = env.background_held_at_write(2);
    let edited = text + "- [ ] Tea\n";
    std::fs::write(&path, &edited).unwrap();
    env.release_writes();
    assert!(child.wait().unwrap().success());
    assert!(!copy.exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    let sent = modifies(&env);
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["deleted"], 0, "{out}");
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["edited"].as_i64()),
        (Some(1), Some(0)),
        "{out}"
    );
    assert_eq!(modifies(&env), sent + 1, "Tea alone");
    let titles = all_titles(&env, "Groceries");
    assert_eq!((count(&titles, "Butter"), count(&titles, "Tea")), (1, 1), "{titles:?}");
    assert_eq!(
        env.json(&["show", "REM-EGGS"])["completed"],
        true,
        "the copy's tick stays"
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("- [ ] Butter\n") && after.contains("- [ ] Tea\n"),
        "{after}"
    );
}

#[test]
fn a_list_that_keeps_failing_does_not_sync_every_minute() {
    let env = start();
    let dir = folder(&env);
    // A conflict copy that cannot be read.
    std::fs::create_dir(dir.join("Groceries.sync-conflict-20261114-093000-ABCDEFG.md")).unwrap();
    let out = env.json(&["background"]);
    assert_eq!(out["synced"], true);
    assert_eq!(out["folder"]["warnings"].as_array().unwrap().len(), 1, "{out}");
    // Nothing changed since: it waits for the next sync, and the other
    // lists' files are still kept.
    let out = env.json(&["background"]);
    assert_eq!(
        (&out["synced"], &out["folder"]["warnings"]),
        (&json!(false), &json!([])),
        "{out}"
    );
    let out = env.json(&["background", "--sync"]);
    assert_eq!(
        out["folder"]["warnings"].as_array().unwrap().len(),
        1,
        "tried again after a sync"
    );
    assert_eq!(env.json(&["background"])["synced"], false);
    // A change to its file is sent at once all the same.
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Bread\n").unwrap();
    let out = env.json(&["background"]);
    assert_eq!(
        (&out["synced"], &out["folder"]["added"]),
        (&json!(true), &json!(1)),
        "{out}"
    );
}

#[test]
fn a_run_killed_after_writing_a_file_does_not_read_it_as_the_file_s_edits() {
    let env = start();
    let dir = folder(&env);
    // The phone renames Milk; the Reminders file gets a line. Groceries.md
    // is written first, then the run is killed during Reminders' write,
    // before anything recorded the new Groceries.md.
    let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
    milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
    env.state.put(milk);
    let path = dir.join("Reminders.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Stretch\n").unwrap();
    let mut child = env.background_held_at_write(1);
    child.kill().unwrap();
    child.wait().unwrap();
    env.release_writes();
    assert!(
        std::fs::read_to_string(dir.join("Groceries.md"))
            .unwrap()
            .contains("Coffee beans")
    );

    let out = env.json(&["background"]);
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["deleted"].as_i64()),
        (Some(0), Some(0)),
        "{out}"
    );
    let milk = env.state.record("Reminder/REM-MILK").unwrap();
    assert_eq!(milk["fields"]["Deleted"]["value"], 0);
    assert_eq!(env.creates(), 1, "Stretch alone");
    assert_eq!(count(&all_titles(&env, "Groceries"), "Coffee beans"), 1);
}

#[test]
fn a_renamed_list_s_file_keeps_its_new_name_after_a_run_killed_later() {
    let env = start();
    let dir = folder(&env);
    // Groceries becomes Food, and its run fails (a conflict copy that cannot
    // be read); the run is then killed during Reminders' write.
    std::fs::create_dir(dir.join("Groceries.sync-conflict-20261114-093000-ABCDEFG.md")).unwrap();
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Name"]["value"] = json!("Food");
    env.state.put(list);
    let path = dir.join("Reminders.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Stretch\n").unwrap();
    let mut child = env.background_held_at_write(1);
    child.kill().unwrap();
    child.wait().unwrap();
    env.release_writes();

    env.json(&["background", "--sync"]);
    assert_eq!(
        file_names(&dir),
        [
            "Food.md",
            "Food.sync-conflict-20261114-093000-ABCDEFG.md",
            "Reminders.md"
        ]
    );
}

#[test]
fn a_conflict_copy_does_not_tick_again_what_the_file_unticked() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Reminders.md");
    let text = std::fs::read_to_string(&path).unwrap();
    // The copy is the file as it was; the file reopens Buy paint.
    std::fs::write(dir.join("Reminders.sync-conflict-20261114-093000-ABCDEFG.md"), &text).unwrap();
    std::fs::write(&path, text.replace("- [x] Buy paint", "- [ ] Buy paint")).unwrap();
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["edited"], 1, "{out}");
    assert_eq!(env.json(&["show", "REM-PAINT"])["completed"], false);
}

#[test]
fn a_gutted_file_s_run_killed_after_its_adds_still_deletes_nothing() {
    let env = start();
    for n in 0..4 {
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
    // A file from elsewhere: none of the six lines, eight of its own.
    let fruit = [
        "Apples", "Bananas", "Cherries", "Dates", "Plums", "Grapes", "Kiwis", "Lemons",
    ];
    let lines: String = fruit.iter().map(|f| format!("- [ ] {f}\n")).collect();
    std::fs::write(dir.join("Groceries.md"), format!("# Groceries\n\n{lines}")).unwrap();
    // Killed once the last of them is in iCloud, before it hears so.
    let mut child = env.background_held_at_write(8);
    child.kill().unwrap();
    child.wait().unwrap();
    env.release_writes();

    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["deleted"], 0, "{out}");
    let titles = all_titles(&env, "Groceries");
    assert_eq!(titles.len(), 14, "{titles:?}");
    assert_eq!(env.creates(), 8);
}

#[test]
fn a_line_and_a_reminder_of_one_title_added_on_each_side_are_two() {
    let env = start();
    let dir = folder(&env);
    // Within a minute, the phone adds Bread in iCloud and a line of it in
    // the file: two reminders asked for, two kept.
    env.state.put(fake::reminder(
        "REM-BREAD",
        "List/LIST-GROCERIES",
        "Bread",
        "",
        None,
        now_ms(),
    ));
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Bread\n").unwrap();
    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["added"], 1, "{out}");
    assert_eq!(count(&all_titles(&env, "Groceries"), "Bread"), 2);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().matches("- [ ] Bread\n").count(),
        2
    );
}

#[test]
fn a_line_whose_add_never_reached_icloud_is_sent_again_once() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Bread\n").unwrap();
    // As a run killed before its add was sent leaves it: recorded, not made.
    let state_path = env.data().join("folder.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["lists"]["List/LIST-GROCERIES"]["pending"] =
        json!([{"uuid": "NEVER-SENT", "done": false, "title": "Bread", "due": null, "notes": ""}]);
    std::fs::write(&state_path, state.to_string()).unwrap();

    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["added"], 1, "{out}");
    assert_eq!(count(&all_titles(&env, "Groceries"), "Bread"), 1);
    assert!(env.state.record("Reminder/NEVER-SENT").is_none());
    let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["lists"]["List/LIST-GROCERIES"]["pending"], json!([]));
}

#[test]
fn a_deleted_list_s_conflict_copies_go_with_its_file_or_stay_with_it() {
    // Nothing unsent in the file: it goes, and its copy with it, said.
    let env = start();
    let dir = folder(&env);
    let copy = "Groceries.sync-conflict-20261114-093000-ABCDEFG.md";
    std::fs::write(dir.join(copy), "# Groceries\n\n- [x] Milk\n").unwrap();
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Deleted"]["value"] = json!(1);
    env.state.put(list.clone());
    let out = env.json(&["background", "--sync"]);
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with(&format!("{copy}: ")),
        "{out}"
    );
    assert_eq!(file_names(&dir), ["Reminders.md"]);
    assert_eq!(modifies(&env), 0);

    // A file with changes never sent stays, and its copy beside it.
    let env = start();
    let dir = folder(&env);
    std::fs::write(dir.join(copy), "# Groceries\n\n- [x] Milk\n").unwrap();
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap() + "- [ ] Bread\n";
    std::fs::write(&path, text).unwrap();
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().contains(copy),
        "{out}"
    );
    assert_eq!(file_names(&dir), ["Groceries.md", copy, "Reminders.md"]);
}
