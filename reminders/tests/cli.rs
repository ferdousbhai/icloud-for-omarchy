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
use icloud_reminders::cloudkit::Record;
use icloud_reminders::model::{List, Parsed, Reminder};
use icloud_reminders::store::Cache;
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

/// The live reminders as iCloud (the fake) holds them now.
fn in_icloud(env: &Env) -> (Vec<List>, Vec<Reminder>) {
    let (mut lists, mut reminders) = (Vec::new(), Vec::new());
    for v in env.state.records() {
        let r = Record::from_value(&v).unwrap();
        if r.is_type("List") {
            lists.extend(List::from_record(&r).unwrap());
        } else if let Parsed::Live(rem) = Reminder::from_record(&r).unwrap() {
            reminders.push(*rem);
        }
    }
    (lists, reminders)
}

/// The titles of a list's live reminders in iCloud, open and completed;
/// the list by name (ignoring case) or ID.
fn all_titles(env: &Env, list: &str) -> Vec<String> {
    let (lists, reminders) = in_icloud(env);
    let list = lists
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case(list) || l.id == format!("List/{list}"))
        .unwrap_or_else(|| panic!("no list {list}"));
    reminders
        .into_iter()
        .filter(|r| r.list_id == list.id)
        .map(|r| r.title)
        .collect()
}

/// The live reminder in iCloud of this ID or title (ignoring case).
fn reminder(env: &Env, query: &str) -> Reminder {
    in_icloud(env)
        .1
        .into_iter()
        .find(|r| r.uuid() == query || r.title.eq_ignore_ascii_case(query))
        .unwrap_or_else(|| panic!("no reminder {query}"))
}

/// The titles the cache holds.
fn cached_titles(env: &Env) -> Vec<String> {
    let cache: Cache = serde_json::from_slice(&std::fs::read(env.data().join("cache.json")).unwrap()).unwrap();
    cache.reminders.into_values().map(|r| r.title).collect()
}

/// A change the phone made after this computer's last sync, which the next
/// sync does not fetch: this computer still holds the record's old tag.
fn unseen(env: &Env, record: Value) {
    env.state.put(record.clone());
    let tag = env.state.record(record["recordName"].as_str().unwrap()).unwrap()["recordChangeTag"]
        .as_str()
        .unwrap()
        .replace("tag-", "tok-");
    let path = env.data().join("cache.json");
    let mut c: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    c["sync_token"] = json!(tag);
    std::fs::write(&path, c.to_string()).unwrap();
}

fn file_of(env: &Env, name: &str) -> PathBuf {
    env.dir.path().join("home/Documents/Reminders").join(name)
}

#[test]
fn sync_applies_an_edited_file_at_once() {
    let env = start();
    // The first sync writes the folder.
    let out = env.json(&["sync"]);
    assert_eq!(
        out["folder"]["written"],
        json!(["Groceries.md", "Reminders.md"]),
        "{out}"
    );
    let path = file_of(&env, "Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    // An agent ticks Eggs, adds one with notes and a due date, and removes Milk.
    let milk = text.lines().find(|l| l.starts_with("- [ ] Milk")).unwrap().to_owned();
    let edited = text
        .replace(&format!("{milk}\n    Oat, 2 cartons\n"), "")
        .replace("- [ ] Eggs", "- [x] Eggs")
        + "- [ ] Bread 📅 2030-01-02 09:30\n    Rye\n";
    std::fs::write(&path, edited).unwrap();

    let out = env.run(&["sync"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = envelope(&out.stdout);
    assert_eq!(
        v["summary"],
        "Synced: 2 lists, 5 reminders (0 changed); Folder: 1 added, 1 edited, 1 deleted in iCloud; 1 file(s) written"
    );
    assert_eq!(
        v["data"]["folder"],
        json!({"added": 1, "edited": 1, "deleted": 1, "written": ["Groceries.md"], "restored": [], "warnings": []})
    );
    let mut titles = all_titles(&env, "Groceries");
    titles.sort();
    assert_eq!(titles, ["Bread", "Eggs"]);
    assert!(reminder(&env, "REM-EGGS").completed);
    let bread = reminder(&env, "bread");
    assert_eq!(bread.notes, "Rye");
    assert_eq!(bread.due.unwrap().date().to_string(), "2030-01-02");
    let deleted = env.state.record("Reminder/REM-MILK").unwrap();
    assert_eq!(deleted["fields"]["Deleted"]["value"], 1);

    // Applied once: the next sync has nothing to send.
    let sent = modifies(&env);
    let out = env.json(&["sync"]);
    assert_eq!(out["folder"]["written"], json!([]));
    assert_eq!(modifies(&env), sent);
    // In text mode the summary is the output.
    let text = env.ok(&["sync", "--styled"]);
    assert!(
        text.starts_with("Synced: 2 lists, 5 reminders (0 changed); Folder: 0 added"),
        "{text}"
    );
}

#[test]
fn sync_reports_a_refused_removal_with_its_data_and_exit_1() {
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
    env.json(&["sync"]);
    let path = file_of(&env, "Groceries.md");
    std::fs::write(&path, "# Groceries\n\n- [ ] Bread\n").unwrap();

    let out = env.run(&["sync"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let v = envelope(&out.stdout);
    assert_eq!(v["ok"], true, "the data comes with the exit code");
    let folder = &v["data"]["folder"];
    assert_eq!(
        folder["restored"],
        json!([{"list": "Groceries", "file": "Groceries.md", "kept": 8}])
    );
    assert_eq!(
        (folder["deleted"].as_i64(), folder["added"].as_i64()),
        (Some(0), Some(1))
    );
    assert_eq!(all_titles(&env, "Groceries").len(), 9);
    assert!(
        stderr(&out).contains("Groceries.md no longer listed 8 reminders"),
        "{}",
        stderr(&out)
    );
    assert!(std::fs::read_to_string(&path).unwrap().contains("- [ ] Extra number 5"));
    // The phone's owner is told too, as when the timer refuses it.
    let said = env.notifications();
    assert!(said[0].iter().any(|a| a.contains("Groceries.md restored")), "{said:?}");
    // Put back: the next sync is clean.
    let out = env.run(&["sync"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn a_file_no_list_has_is_named_by_sync_and_read_once_the_list_is_made() {
    let env = start();
    env.json(&["sync"]);
    std::fs::write(file_of(&env, "Hardware.md"), "# Hardware\n\n- [ ] Screws\n").unwrap();
    let out = env.run(&["sync"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let warnings = envelope(&out.stdout)["data"]["folder"]["warnings"].clone();
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .starts_with("Hardware.md: not read, as no list has this name"),
        "{warnings}"
    );
    assert_eq!(env.creates(), 0);
    // The timer is quiet about it: it would say so every minute.
    assert_eq!(env.json(&["background", "--sync"])["folder"]["warnings"], json!([]));
    // A list of that name made in the app: the file is its own, and adds.
    env.state
        .put(fake::list("List/LIST-HARDWARE", "Hardware", &[], "#34C759"));
    let out = env.json(&["sync"]);
    assert_eq!(out["folder"]["added"], 1, "{out}");
    assert_eq!(all_titles(&env, "Hardware"), ["Screws"]);
}

#[test]
fn a_change_made_elsewhere_meanwhile_is_written_over_once() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Reminders.md");
    // The phone renames it after our last sync: our change tag is stale.
    let mut phone = env.state.record("Reminder/REM-CALL").unwrap();
    phone["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Call the dentist at 9"));
    unseen(&env, phone);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("- [ ] Call the dentist", "- [x] Call the dentist")).unwrap();
    let out = env.json(&["sync"]);
    assert_eq!(out["folder"]["edited"], 1, "{out}");
    // The phone's title survived; one CONFLICT, a lookup, the retry.
    let call = reminder(&env, "REM-CALL");
    assert_eq!((call.title.as_str(), call.completed), ("Call the dentist at 9", true));
    let ops = env.ops();
    assert_eq!(
        ops[ops.len() - 3..],
        ["records/modify", "records/lookup", "records/modify"]
    );

    // The phone already completed it: after the CONFLICT nothing is left
    // to write.
    let path = dir.join("Groceries.md");
    let mut phone = env.state.record("Reminder/REM-EGGS").unwrap();
    phone["fields"]["Completed"]["value"] = json!(1);
    unseen(&env, phone);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("- [ ] Eggs", "- [x] Eggs")).unwrap();
    env.json(&["sync"]);
    let ops = env.ops();
    assert_eq!(ops[ops.len() - 2..], ["records/modify", "records/lookup"]);
}

#[test]
fn a_reminder_deleted_or_removed_for_good_elsewhere_meanwhile_is_not_written_over() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Reminders.md");
    // The phone deletes it after our last sync: our change tag is stale.
    let mut phone = env.state.record("Reminder/REM-CALL").unwrap();
    phone["fields"]["Deleted"]["value"] = json!(1);
    unseen(&env, phone);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("- [ ] Call the dentist", "- [x] Call the dentist")).unwrap();
    let out = env.run(&["sync"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let warnings = envelope(&out.stdout)["data"]["folder"]["warnings"].clone();
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .contains("\"Call the dentist\" has been deleted"),
        "{warnings}"
    );
    // One CONFLICT and a lookup; nothing is written over a deleted reminder.
    let ops = env.ops();
    assert_eq!(ops[ops.len() - 2..], ["records/modify", "records/lookup"]);
    let call = env.state.record("Reminder/REM-CALL").unwrap();
    assert_eq!(call["fields"]["Completed"]["value"], 0);
    // The next sync takes its line out.
    env.json(&["sync"]);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("Call the dentist"));

    // Recently Deleted emptied on the phone: CloudKit has no such record
    // any more, and removing its line is done.
    env.state.purge("Reminder/REM-PASSPORT");
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: String = text
        .lines()
        .filter(|l| !l.contains("Renew passport") && !l.contains("Photos first"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&path, kept).unwrap();
    let out = env.json(&["sync"]);
    assert_eq!(out["folder"]["warnings"], json!([]), "{out}");
    assert!(cached_titles(&env).iter().all(|t| t != "Renew passport"));
}

#[test]
fn sync_pages_and_recovers_from_an_expired_token() {
    let env = start();
    *env.state.page.lock().unwrap() = 1;
    let out = env.json(&["sync"]);
    assert_eq!(
        (&out["lists"], &out["reminders"], &out["changed"], &out["full"]),
        (&json!(2), &json!(5), &json!(5), &json!(true))
    );
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
        let cached = cached_titles(&env);
        assert!(
            cached.contains(&"Call the dentist".into()) && cached.contains(&"A dozen eggs".into()),
            "{cached:?}"
        );
    }
    // The token stayed put, so a plain sync fetches the bad record again.
    assert!(env.json(&["sync"])["warning"].is_string());
    assert!(env.sent("records/modify").is_empty());
}

#[test]
fn signed_out_or_offline_sync_fails_with_its_code_and_leaves_the_files() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap() + "- [ ] Bread\n";
    std::fs::write(&path, &text).unwrap();
    *env.state.signed_out.lock().unwrap() = true;
    let (exit, err) = env.fails(&["sync"]);
    assert_eq!((exit, &err["code"]), (3, &json!("sign_in_required")), "{err}");
    assert!(
        err["hint"].as_str().unwrap().contains("icloud-session sign-in"),
        "{err}"
    );
    let out = env.run(&["sync", "--styled"]);
    assert_eq!((code(&out), stdout(&out).as_str()), (3, ""));
    let err = stderr(&out);
    assert!(
        err.starts_with("error: sign in to iCloud required") && err.contains("hint: run `icloud-session sign-in`"),
        "{err}"
    );
    *env.state.signed_out.lock().unwrap() = false;
    // Nothing listens on port 9.
    let out = run_at("http://127.0.0.1:9", env.dir.path(), &["sync"]);
    assert_eq!(code(&out), 5, "{}", stderr(&out));
    let err = &envelope(&out.stdout)["error"];
    assert_eq!(err["code"], "offline");
    assert!(err["hint"].is_string(), "{err}");
    // The edit waits in the file, and the next sync that works sends it.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(env.creates(), 0);
    assert_eq!(env.json(&["sync"])["folder"]["added"], 1);
}

#[test]
fn ids_only_count_and_quiet() {
    let env = start();
    let quiet: Value = serde_json::from_str(&env.ok(&["sync", "--quiet"])).unwrap();
    assert_eq!(quiet["changed"], 5);
    // The phone renames one: sync counts what changed since the last one.
    let mut call = env.state.record("Reminder/REM-CALL").unwrap();
    call["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Call the dentist at 9"));
    env.state.put(call);
    assert_eq!(env.ok(&["sync", "--count"]).trim(), "1");
    assert_eq!(env.ok(&["background", "--count"]).trim(), "0");
    assert_eq!(env.ok(&["background", "--sync", "--ids-only"]), "");
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

/// The commands that edited reminders are gone: the files do that.
#[test]
fn usage_is_coded() {
    let env = start();
    for args in [&["frobnicate"][..], &["list"], &["add", "x"], &["sync", "--bogus"]] {
        let (exit, err) = env.fails(args);
        assert_eq!((exit, &err["code"]), (2, &json!("usage")), "{args:?}: {err}");
        // Text mode: clap's message on stderr, the same exit code.
        let out = env.run(&[args, &["--styled"][..]].concat());
        assert_eq!((code(&out), stdout(&out).as_str()), (2, ""), "{args:?}");
        assert!(!stderr(&out).is_empty(), "{args:?}");
    }
    let (_, err) = env.fails(&["frobnicate"]);
    assert_eq!(err["hint"], "run `icloud-reminders --help`");
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
    let butter = reminder(&env, "butter");
    assert_eq!(butter.notes, "Salted");
    assert_eq!(butter.due.unwrap().date().to_string(), "2030-01-02");
    assert_eq!(butter.list_id, "List/LIST-GROCERIES");
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
    let mut eggs = env.state.record("Reminder/REM-EGGS").unwrap();
    eggs["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Eggs, a dozen"));
    env.state.put(eggs);
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
    assert_eq!(reminder(&env, "REM-EGGS").title, "Eggs, two dozen");
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
    let milk = reminder(&env, "REM-MILK");
    assert_eq!((milk.notes.as_str(), milk.completed), ("Oat, 2 cartons", false));
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

#[test]
fn a_conflict_copy_left_beside_a_deleted_list_s_file_is_not_read_by_a_new_list_once_the_file_is_removed() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.clone() + "- [ ] Bread\n").unwrap();
    let copy = "Groceries.sync-conflict-20261114-093000-ABCDEFG.md";
    std::fs::write(dir.join(copy), text + "- [ ] Paprika\n").unwrap();
    let mut list = delete_groceries(&env);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["warnings"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(file_names(&dir), ["Groceries.md", copy, "Reminders.md"]);
    // The leftover file is removed by hand, and a list of that name made:
    // the copy beside it is the deleted list's, not the new one's.
    std::fs::remove_file(&path).unwrap();
    list["recordName"] = json!("List/LIST-NEW");
    list["fields"]["Deleted"]["value"] = json!(0);
    list["fields"]["ReminderIDs"]["value"] = json!("[]");
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(file_names(&dir), ["Groceries 2.md", copy, "Reminders.md"]);
    std::fs::remove_file(dir.join(copy)).unwrap();
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
    let butter = reminder(&env, "butter");
    assert_eq!((butter.completed, butter.notes.as_str()), (true, "Salted"));
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
    assert!(reminder(&env, "REM-EGGS").completed, "the copy's tick stays");
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

impl Env {
    /// `background --sync`, ended at a folder crash point (`folder::CRASH_AT`).
    fn background_crashing_at(&self, point: &str) {
        let out = command_at(&self.base, self.dir.path(), &["background", "--sync"])
            .env(icloud_reminders::folder::CRASH_AT, point)
            .output()
            .unwrap();
        assert_eq!(code(&out), 86, "never reached {point}: {}", stderr(&out));
    }
}

#[test]
fn a_file_edited_after_a_run_ended_before_its_write_landed_is_read_against_what_it_last_held() {
    // Seed 1874 of the model, which failed only when the reminders' random
    // IDs fell in one order: two "Tea", the second completed.
    let env = start();
    let now = now_ms();
    env.state.put(fake::reminder(
        "REM-TEA-2",
        "List/LIST-GROCERIES",
        "Tea",
        "",
        None,
        now - MINUTE,
    ));
    let mut done = fake::reminder("REM-TEA-1", "List/LIST-GROCERIES", "Tea", "", None, now - MINUTE);
    done["fields"]["Completed"]["value"] = json!(1);
    done["fields"]["CompletionDate"] = json!({ "type": "TIMESTAMP", "value": now - MINUTE });
    env.state.put(done);
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("- [ ] Tea\n") && text.contains("- [x] Tea\n"), "{text}");
    // Reopened in iCloud; the run writing that ends before its rename.
    let mut tea = env.state.record("Reminder/REM-TEA-1").unwrap();
    tea["fields"]["Completed"]["value"] = json!(0);
    tea["fields"]["CompletionDate"]["value"] = Value::Null;
    env.state.put(tea);
    env.background_crashing_at("tmp-written");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let tmp = dir.join(".syncthing.Groceries.md.icloud-reminders.tmp");
    // The file as it was, with its two Tea lines swapped: read against what
    // it held, not against the write that never landed (to which it would
    // be one tick, of whichever Tea came first).
    let swapped = text
        .replace("- [ ] Tea\n", "OPEN\n")
        .replace("- [x] Tea\n", "- [ ] Tea\n")
        .replace("OPEN\n", "- [x] Tea\n");
    std::fs::write(&path, swapped).unwrap();
    env.json(&["background", "--sync"]);
    let completed = |id: &str| env.state.record(id).unwrap()["fields"]["Completed"]["value"] == json!(1);
    assert!(completed("Reminder/REM-TEA-2") && !completed("Reminder/REM-TEA-1"));
    assert!(!tmp.exists());
}

#[test]
fn a_run_ended_between_writing_a_file_and_recording_it_adds_and_deletes_nothing() {
    let env = start();
    let dir = folder(&env);
    // The phone renames Milk; Groceries.md is written with it, and the run
    // ends before folder.json says so.
    let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
    milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
    env.state.put(milk);
    env.background_crashing_at("file-written");
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
    assert_eq!(env.creates(), 0);
    let milk = env.state.record("Reminder/REM-MILK").unwrap();
    assert_eq!(milk["fields"]["Deleted"]["value"], 0);
}

#[test]
fn a_write_that_landed_is_not_undone_by_syncthing_s_own_temp_file_and_that_is_left() {
    let env = start();
    let dir = folder(&env);
    let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
    milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
    env.state.put(milk);
    env.background_crashing_at("file-written");
    // Syncthing starts a download of the file: an empty temp file of its
    // own, with the write already in.
    let syncthing = dir.join(".syncthing.Groceries.md.tmp");
    std::fs::write(&syncthing, "").unwrap();

    let out = env.json(&["background"]);
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["deleted"].as_i64()),
        (Some(0), Some(0)),
        "{out}"
    );
    assert_eq!(env.creates(), 0);
    let milk = env.state.record("Reminder/REM-MILK").unwrap();
    assert_eq!(milk["fields"]["Deleted"]["value"], 0);
    assert!(syncthing.exists());
}

#[test]
fn a_run_ended_between_renaming_a_file_and_recording_it_keeps_the_file_and_its_edits() {
    let env = start();
    let dir = folder(&env);
    // Groceries becomes Food while its file has a tick not yet sent; the
    // file is renamed, and the run ends before folder.json says so.
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Name"]["value"] = json!("Food");
    env.state.put(list);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("- [ ] Eggs", "- [x] Eggs")).unwrap();
    env.background_crashing_at("renamed");
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);

    env.json(&["background", "--sync"]);
    let eggs = env.state.record("Reminder/REM-EGGS").unwrap();
    assert_eq!(eggs["fields"]["Completed"]["value"], 1);
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
    let food = std::fs::read_to_string(dir.join("Food.md")).unwrap();
    assert!(food.starts_with("# Food\n") && food.contains("- [x] Eggs"), "{food}");
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
    assert!(!reminder(&env, "REM-PAINT").completed);
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

#[test]
fn a_line_whose_reminder_was_deleted_before_the_run_heard_back_is_not_made_again() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Bread\n").unwrap();
    // The add reaches iCloud; the run is killed before it hears so.
    let mut child = env.background_held_at_write(1);
    child.kill().unwrap();
    child.wait().unwrap();
    env.release_writes();
    // Deleted on the phone before the next run.
    let made = env.sent("records/modify")[0]["operations"][0]["record"]["recordName"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut bread = env.state.record(&made).unwrap();
    bread["fields"]["Deleted"] = json!({ "type": "INT64", "value": 1 });
    env.state.put(bread);

    let out = env.json(&["background"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(env.creates(), 1, "not made again");
    assert!(!all_titles(&env, "Groceries").contains(&"Bread".to_owned()));
    assert!(!std::fs::read_to_string(&path).unwrap().contains("Bread"));
}

#[test]
fn a_renamed_list_s_old_file_brought_back_is_read_as_a_conflict_copy() {
    let env = start();
    let dir = folder(&env);
    let old = std::fs::read_to_string(dir.join("Groceries.md")).unwrap();
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Name"]["value"] = json!("Food");
    env.state.put(list);
    env.json(&["background", "--sync"]);
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
    // The phone had ticked Eggs and added Bread in Groceries.md meanwhile:
    // Syncthing keeps a changed file over its removal, so it comes back.
    let back = old.replace("- [ ] Eggs", "- [x] Eggs") + "- [ ] Bread\n";
    std::fs::write(dir.join("Groceries.md"), back).unwrap();

    let out = env.json(&["background"]);
    assert_eq!(out["synced"], true, "an old name back syncs at once");
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["edited"].as_i64()),
        (Some(1), Some(1)),
        "{out}"
    );
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with("Groceries.md: came back"),
        "{out}"
    );
    assert!(reminder(&env, "REM-EGGS").completed);
    assert_eq!(count(&all_titles(&env, "Food"), "Bread"), 1);
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
    assert!(
        std::fs::read_to_string(dir.join("Food.md"))
            .unwrap()
            .contains("- [ ] Bread\n")
    );
}

#[test]
fn a_deleted_list_s_file_brought_back_is_left_and_not_taken_by_a_new_list_of_its_name() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let old = std::fs::read_to_string(&path).unwrap();
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Deleted"]["value"] = json!(1);
    env.state.put(list.clone());
    env.json(&["background", "--sync"]);
    assert_eq!(file_names(&dir), ["Reminders.md"]);
    // The phone had added a line meanwhile, and Syncthing brings it back.
    let back = old + "- [ ] Bread\n";
    std::fs::write(&path, &back).unwrap();
    let out = env.json(&["background", "--sync"]);
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with("Groceries.md: came back"),
        "{out}"
    );
    assert_eq!(env.json(&["background", "--sync"])["folder"]["warnings"], json!([]));

    // A new list of that name gets a file of its own and takes nothing.
    list["recordName"] = json!("List/LIST-NEW");
    list["fields"]["Deleted"]["value"] = json!(0);
    list["fields"]["ReminderIDs"]["value"] = json!("[]");
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(modifies(&env), 0);
    assert_eq!(file_names(&dir), ["Groceries 2.md", "Groceries.md", "Reminders.md"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), back);
}

#[test]
fn a_file_line_like_a_conflict_copy_s_new_line_is_another_reminder() {
    let env = start();
    let dir = folder(&env);
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    // One device adds Bread, another Bread rolls: two reminders.
    std::fs::write(
        dir.join("Groceries.sync-conflict-20261114-093000-ABCDEFG.md"),
        text.clone() + "- [ ] Bread\n",
    )
    .unwrap();
    std::fs::write(&path, text + "- [ ] Bread rolls\n").unwrap();
    let out = env.json(&["background"]);
    assert_eq!(
        (out["folder"]["added"].as_i64(), out["folder"]["edited"].as_i64()),
        (Some(2), Some(0)),
        "{out}"
    );
    let titles = all_titles(&env, "Groceries");
    assert_eq!(
        (count(&titles, "Bread"), count(&titles, "Bread rolls")),
        (1, 1),
        "{titles:?}"
    );
}

#[test]
fn a_file_written_by_a_run_ended_before_recording_it_and_then_edited_is_read_against_that_write() {
    let env = start();
    let dir = folder(&env);
    let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
    milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
    env.state.put(milk);
    env.background_crashing_at("file-written");
    // Before the next run, the phone ticks Eggs in the file as written.
    let path = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Coffee beans"), "{text}");
    std::fs::write(&path, text.replace("- [ ] Eggs", "- [x] Eggs")).unwrap();

    let out = env.json(&["background"]);
    assert_eq!(
        (
            out["folder"]["added"].as_i64(),
            out["folder"]["edited"].as_i64(),
            out["folder"]["deleted"].as_i64()
        ),
        (Some(0), Some(1), Some(0)),
        "{out}"
    );
    assert!(reminder(&env, "REM-EGGS").completed);
    let milk = env.state.record("Reminder/REM-MILK").unwrap();
    assert_eq!(milk["fields"]["Deleted"]["value"], 0);
}

#[test]
fn a_file_written_by_a_run_ended_before_recording_it_goes_with_its_deleted_list() {
    let env = start();
    let dir = folder(&env);
    let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
    milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
    env.state.put(milk);
    env.background_crashing_at("file-written");
    assert!(
        std::fs::read_to_string(dir.join("Groceries.md"))
            .unwrap()
            .contains("Coffee beans")
    );
    // The list is deleted before the next run: the file holds nothing that
    // was not written from iCloud, so it goes, unremarked.
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Deleted"]["value"] = json!(1);
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["warnings"], json!([]), "{out}");
    assert_eq!(file_names(&dir), ["Reminders.md"]);
}

#[test]
fn a_list_named_like_a_conflict_copy_is_a_list_of_its_own() {
    let env = start();
    let dir = folder(&env);
    rename_groceries(&env, "Reminders.sync-conflict-draft");
    let out = env.json(&["background", "--sync"]);
    assert_eq!(
        (&out["folder"]["added"], &out["folder"]["warnings"]),
        (&json!(0), &json!([])),
        "{out}"
    );
    assert_eq!(file_names(&dir), ["Reminders.md", "Reminders.sync-conflict-draft.md"]);
    // A line in it goes to its own list.
    let path = dir.join("Reminders.sync-conflict-draft.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Paprika\n").unwrap();
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 1, "{out}");
    assert!(all_titles(&env, "LIST-GROCERIES").contains(&"Paprika".to_owned()));
    assert!(!all_titles(&env, "Reminders").contains(&"Paprika".to_owned()));
}

#[test]
fn a_conflict_copy_of_a_deleted_list_s_file_that_comes_later_is_left_and_not_read_by_a_new_list() {
    // Under the name the file had when the list was deleted, and under one
    // it had before the list was renamed.
    for renamed in [false, true] {
        let env = start();
        let dir = folder(&env);
        if renamed {
            rename_groceries(&env, "Food");
            env.json(&["background", "--sync"]);
            assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
        }
        let mut list = delete_groceries(&env);
        env.json(&["background", "--sync"]);
        assert_eq!(file_names(&dir), ["Reminders.md"], "renamed: {renamed}");
        let copy = "Groceries.sync-conflict-20261114-093000-ABCDEFG.md";
        std::fs::write(dir.join(copy), "# Groceries\n\n- [ ] Milk\n- [ ] Paprika\n").unwrap();
        let out = env.json(&["background", "--sync"]);
        let warnings = out["folder"]["warnings"].as_array().unwrap();
        assert!(
            warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with(&format!("{copy}: ")),
            "renamed: {renamed}: {out}"
        );
        // Said once.
        assert_eq!(env.json(&["background", "--sync"])["folder"]["warnings"], json!([]));
        // A new list of the old name gets a file of its own.
        list["recordName"] = json!("List/LIST-NEW");
        list["fields"]["Name"]["value"] = json!("Groceries");
        list["fields"]["Deleted"]["value"] = json!(0);
        list["fields"]["ReminderIDs"]["value"] = json!("[]");
        env.state.put(list);
        let out = env.json(&["background", "--sync"]);
        assert_eq!(out["folder"]["added"], 0, "renamed: {renamed}: {out}");
        assert_eq!(file_names(&dir), ["Groceries 2.md", copy, "Reminders.md"]);
    }
}

fn rename_groceries(env: &Env, to: &str) {
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Name"]["value"] = json!(to);
    env.state.put(list);
}

fn delete_groceries(env: &Env) -> Value {
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["fields"]["Deleted"]["value"] = json!(1);
    env.state.put(list.clone());
    list
}

#[test]
fn a_file_renamed_by_a_run_ended_before_recording_it_goes_with_its_deleted_list() {
    let env = start();
    let dir = folder(&env);
    rename_groceries(&env, "Food");
    env.background_crashing_at("renamed");
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
    delete_groceries(&env);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["warnings"], json!([]), "{out}");
    assert_eq!(file_names(&dir), ["Reminders.md"]);
}

#[test]
fn a_rename_a_run_ended_before_making_is_made_by_the_next() {
    let env = start();
    let dir = folder(&env);
    rename_groceries(&env, "Food");
    env.background_crashing_at("rename-pending");
    assert_eq!(file_names(&dir), ["Groceries.md", "Reminders.md"]);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["warnings"], json!([]), "{out}");
    assert_eq!(modifies(&env), 0);
    assert_eq!(file_names(&dir), ["Food.md", "Reminders.md"]);
    let food = std::fs::read_to_string(dir.join("Food.md")).unwrap();
    assert!(food.starts_with("# Food\n") && food.contains("Eggs"), "{food}");
}

#[test]
fn a_deleted_list_s_old_file_brought_back_is_left_and_not_taken_by_a_new_list_of_its_name() {
    let env = start();
    let dir = folder(&env);
    let old = std::fs::read_to_string(dir.join("Groceries.md")).unwrap();
    rename_groceries(&env, "Food");
    env.json(&["background", "--sync"]);
    let mut list = delete_groceries(&env);
    env.json(&["background", "--sync"]);
    assert_eq!(file_names(&dir), ["Reminders.md"]);
    // The phone had added a line to Groceries.md, and Syncthing brings it back.
    let back = old + "- [ ] Bread\n";
    std::fs::write(dir.join("Groceries.md"), &back).unwrap();
    let out = env.json(&["background", "--sync"]);
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with("Groceries.md: came back"),
        "{out}"
    );
    // A new list of that name takes nothing from it.
    list["recordName"] = json!("List/LIST-NEW");
    list["fields"]["Name"]["value"] = json!("Groceries");
    list["fields"]["Deleted"]["value"] = json!(0);
    list["fields"]["ReminderIDs"]["value"] = json!("[]");
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(file_names(&dir), ["Groceries 2.md", "Groceries.md", "Reminders.md"]);
    assert_eq!(std::fs::read_to_string(dir.join("Groceries.md")).unwrap(), back);
}

#[test]
fn a_write_a_run_ended_before_finishing_leaves_no_temp_file() {
    let env = start();
    let dir = folder(&env);
    let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
    let title = milk["fields"]["TitleDocument"].clone();
    milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
    env.state.put(milk.clone());
    env.background_crashing_at("tmp-written");
    let tmp = dir.join(".syncthing.Groceries.md.icloud-reminders.tmp");
    assert!(tmp.exists());
    // Retitled back on the phone: the next run has nothing to write.
    milk["fields"]["TitleDocument"] = title;
    env.state.put(milk);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["written"], json!([]), "{out}");
    assert!(!tmp.exists());
}

#[test]
fn a_conflict_copy_s_lines_deleted_in_icloud_do_not_make_a_removal_risky() {
    let env = start();
    let dir = folder(&env);
    // Four lines of a conflict copy were sent and deleted on the phone
    // before the run heard back.
    let path = env.data().join("folder.json");
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for n in 1..=4 {
        let uuid = format!("GONE-{n}");
        let mut r = fake::reminder(
            &uuid,
            "List/LIST-GROCERIES",
            &format!("Copy line {n}"),
            "",
            None,
            now_ms(),
        );
        r["fields"]["Deleted"] = json!({ "type": "INT64", "value": 1 });
        env.state.put(r);
        state["lists"]["List/LIST-GROCERIES"]["pending"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "uuid": uuid, "done": false, "title": format!("Copy line {n}"), "due": null, "notes": "", "copy": true }));
    }
    std::fs::write(&path, state.to_string()).unwrap();
    // One line removed from the file: not most of the list.
    let file = dir.join("Groceries.md");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("- [ ] Eggs\n", "")).unwrap();
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["restored"], json!([]), "{out}");
    assert_eq!(out["folder"]["deleted"], 1, "{out}");
    assert_eq!(
        env.state.record("Reminder/REM-EGGS").unwrap()["fields"]["Deleted"]["value"],
        1
    );
}

#[test]
fn a_write_a_run_ended_before_finishing_leaves_no_temp_file_after_its_list_is_renamed_or_deleted() {
    for deleted in [false, true] {
        let env = start();
        let dir = folder(&env);
        let mut milk = env.state.record("Reminder/REM-MILK").unwrap();
        milk["fields"]["TitleDocument"]["value"] = json!(icloud_reminders::topotext::encode("Coffee beans"));
        env.state.put(milk);
        env.background_crashing_at("tmp-written");
        assert!(dir.join(".syncthing.Groceries.md.icloud-reminders.tmp").exists());
        if deleted {
            delete_groceries(&env);
        } else {
            rename_groceries(&env, "Food");
        }
        env.json(&["background", "--sync"]);
        let want: &[&str] = if deleted {
            &["Reminders.md"]
        } else {
            &["Food.md", "Reminders.md"]
        };
        assert_eq!(file_names(&dir), want, "deleted: {deleted}");
    }
}

#[test]
fn a_deleted_list_s_file_that_cannot_be_removed_is_left_and_the_run_goes_on() {
    use std::os::unix::fs::PermissionsExt;
    let env = start();
    let dir = folder(&env);
    delete_groceries(&env);
    // The other list's file gets a line; the folder cannot be changed.
    let path = dir.join("Reminders.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text + "- [ ] Stretch\n").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = env.json(&["background", "--sync"]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out["folder"]["added"], 1, "the other list's line is sent: {out}");
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    // The other list's own write back fails too, as a warning of its own.
    assert!(
        warnings.len() == 2
            && warnings[1].as_str().unwrap().starts_with("Reminders.md: ")
            && warnings[0]
                .as_str()
                .unwrap()
                .starts_with("Groceries.md: its list is gone from iCloud, and the file could not be removed"),
        "{out}"
    );
    assert_eq!(file_names(&dir), ["Groceries.md", "Reminders.md"]);
    // Said once, and a new list of that name gets a file of its own.
    assert_eq!(env.json(&["background", "--sync"])["folder"]["warnings"], json!([]));
    let mut list = env.state.record("List/LIST-GROCERIES").unwrap();
    list["recordName"] = json!("List/LIST-NEW");
    list["fields"]["Deleted"]["value"] = json!(0);
    list["fields"]["ReminderIDs"]["value"] = json!("[]");
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["added"], 0, "{out}");
    assert_eq!(file_names(&dir), ["Groceries 2.md", "Groceries.md", "Reminders.md"]);
}

#[test]
fn a_deleted_list_s_conflict_copy_that_cannot_be_removed_is_left_and_not_read_by_a_new_list() {
    let env = start();
    let dir = folder(&env);
    // A directory of a copy's name, with something in it: not removable.
    let copy = "Groceries.sync-conflict-20261114-093000-ABCDEFG.md";
    std::fs::create_dir(dir.join(copy)).unwrap();
    std::fs::write(dir.join(copy).join("x"), "").unwrap();
    let mut list = delete_groceries(&env);
    let out = env.json(&["background", "--sync"]);
    let warnings = out["folder"]["warnings"].as_array().unwrap();
    assert!(
        warnings.len() == 1 && warnings[0].as_str().unwrap().starts_with(&format!("{copy}: ")),
        "{out}"
    );
    assert_eq!(file_names(&dir), [copy, "Reminders.md"]);
    // A new list of that name would read it as its own copy: it gets
    // another name while the copy is there.
    list["recordName"] = json!("List/LIST-NEW");
    list["fields"]["Deleted"]["value"] = json!(0);
    list["fields"]["ReminderIDs"]["value"] = json!("[]");
    env.state.put(list);
    let out = env.json(&["background", "--sync"]);
    assert_eq!(out["folder"]["warnings"], json!([]), "{out}");
    assert_eq!(file_names(&dir), ["Groceries 2.md", copy, "Reminders.md"]);
    std::fs::remove_dir_all(dir.join(copy)).unwrap();
    env.json(&["background", "--sync"]);
    assert_eq!(file_names(&dir), ["Groceries.md", "Reminders.md"]);
}
