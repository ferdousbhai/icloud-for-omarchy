//! A model-based test of the Markdown folder (`src/folder.rs`): seeded random
//! sequences of edits to the list files as a phone would make them, edits in
//! iCloud (straight into the fake server), Syncthing conflict copies, and
//! `background` runs: some killed once a write has reached iCloud but
//! before its answer has (the fake's `hold_modify`), some ended at one of
//! the folder's crash points (`folder::CRASH_POINTS`, between each write and
//! what records it), and some two at once. The edits include subtasks, due
//! dates changed on both sides, a list renamed or a reminder moved while a
//! file has unsent edits, a file removed or emptied, and the whole folder
//! deleted, a renamed list's old file or a deleted list's file brought back
//! by Syncthing, a file line like one a conflict copy adds, two copies at
//! once, notes changed in iCloud, and iCloud changing again after a run
//! ended at a crash point, before the next run settles it. A run ended
//! before the user's edits can leave a reminder in two
//! files (moved in iCloud), and the edits are then made to both. After each
//! run:
//!
//! - every reminder iCloud had before it is as the model says: kept (with
//!   the edits its line asked for, and none meant for another line) or
//!   deleted because its line was removed;
//! - the reminders it made are exactly the lines added, none twice;
//! - every file reads as iCloud now has it, and the folder holds no other;
//! - a run after it sends and writes nothing.
//!
//! After the last step, a few more runs settle it: each is quiet, and
//! `folder.json` holds no add, write, rename or failure still open, no old
//! name or removed file in the folder, and no temp file.
//!
//! What a line asks for is the library's own [`folder::diff`] (the pairing
//! heuristics are unit-tested there); the model checks that it pairs each
//! line that could only be one reminder with that reminder, and then that
//! the run carries it out, killed or not.
//!
//! On failure the seed and every operation are printed. One seed alone:
//! `ICLOUD_REMINDERS_MODEL_SEED=<n>`; more of them:
//! `ICLOUD_REMINDERS_MODEL_CASES=<n>`; longer ones:
//! `ICLOUD_REMINDERS_MODEL_STEPS=<n>`.

#[path = "../examples/fake_reminders.rs"]
#[allow(dead_code)] // what tests/cli.rs alone uses
mod fake;

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use icloud_app::testing::{code, command, envelope, stderr, write_script};
use icloud_reminders::cloudkit::Record;
use icloud_reminders::folder::{self, Item, Known};
use icloud_reminders::model::{Change, List, Parsed, Reminder};
use icloud_reminders::store::Cache;
use icloud_reminders::topotext;
use icloud_session::time::now_ms;
use jiff::Zoned;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

const CASES: u64 = 6;
const STEPS: usize = 10;
const MINUTE: i64 = 60_000;

// --- a seeded generator ---

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn word(&mut self) -> String {
        let len = 4 + self.below(3);
        (0..len).map(|_| (b'a' + self.below(26) as u8) as char).collect()
    }
}

fn capital(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
        .unwrap_or_default()
}

/// `folder.rs`'s likeness of two titles, which decides what pairs up.
fn alike(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    if short.len() >= 3 && long.windows(short.len()).any(|w| w == short.as_slice()) {
        return 1.0 - (long.len() - short.len()) as f64 / (2 * long.len()) as f64;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = cost.min(diagonal + 1).min(row[j] + 1);
        }
    }
    1.0 - row[b.len()] as f64 / a.len().max(b.len()) as f64
}

const ALIKE: f64 = 0.5;

// --- the fake server and the binary ---

struct Env {
    base: String,
    state: Arc<fake::State>,
    dir: tempfile::TempDir,
}

fn start() -> Env {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(fake::State::seeded(now_ms(), 0));
    let s = state.clone();
    std::thread::spawn(move || fake::serve_with(listener, s));
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_script(&bin.join("omarchy-notification-send"), "#!/bin/sh\nexit 0\n");
    Env { base, state, dir }
}

impl Env {
    fn folder(&self) -> PathBuf {
        self.dir.path().join("home/Documents/Reminders")
    }

    fn background_command(&self) -> Command {
        let mut c = command(env!("CARGO_BIN_EXE_icloud-reminders"), &self.base, self.dir.path());
        c.args(["background", "--sync", "--json"])
            .env("TZ", "UTC")
            .env("PATH", self.dir.path().join("bin"))
            .env_remove("OMARCHY_PATH");
        c
    }

    fn modifies(&self) -> usize {
        self.state
            .requests()
            .iter()
            .filter(|r| r["op"] == "records/modify")
            .count()
    }

    /// A whole `background` run: its `data`.
    fn background(&self) -> Result<Value, String> {
        let out = self.background_command().output().unwrap();
        if code(&out) != 0 {
            return Err(format!("background exited {}: {}", code(&out), stderr(&out)));
        }
        Ok(envelope(&out.stdout)["data"].clone())
    }

    /// A `background` run killed once its `nth` write is in iCloud, before
    /// it hears so; false when the run finished first.
    fn killed_at_write(&self, nth: usize) -> bool {
        let number = self.modifies() + nth;
        *self.state.hold_modify.lock().unwrap() = Some(number);
        let mut child = self
            .background_command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let landed = || {
            let sent: Vec<Value> = self
                .state
                .requests()
                .into_iter()
                .filter(|r| r["op"] == "records/modify")
                .collect();
            let record = &sent.get(number - 1)?["body"]["operations"][0]["record"];
            let stored = self.state.record(record["recordName"].as_str()?)?;
            (stored["recordChangeTag"] != record["recordChangeTag"]).then_some(())
        };
        let mut tries = 0;
        let killed = loop {
            if landed().is_some() {
                child.kill().unwrap();
                child.wait().unwrap();
                break true;
            }
            if child.try_wait().unwrap().is_some() {
                break false;
            }
            tries += 1;
            assert!(tries < 5000, "the held run neither wrote nor finished");
            std::thread::sleep(Duration::from_millis(2));
        };
        *self.state.hold_modify.lock().unwrap() = None;
        killed
    }

    fn edit(&self, name: &str, f: impl FnOnce(&mut Value)) {
        let mut record = self.state.record(name).unwrap();
        f(&mut record);
        self.state.put(record);
    }
}

// --- iCloud as the fake holds it ---

struct Truth {
    lists: Vec<List>,
    live: BTreeMap<String, Reminder>,
    /// Every reminder record, live or not.
    ids: BTreeSet<String>,
}

fn truth(state: &fake::State) -> Truth {
    let mut t = Truth {
        lists: Vec::new(),
        live: BTreeMap::new(),
        ids: BTreeSet::new(),
    };
    for v in state.records() {
        let r = Record::from_value(&v).unwrap();
        if r.is_type("List") {
            t.lists.extend(List::from_record(&r).unwrap());
        } else {
            t.ids.insert(r.name.clone());
            if let Parsed::Live(rem) = Reminder::from_record(&r).unwrap() {
                t.live.insert(rem.id.clone(), *rem);
            }
        }
    }
    t.lists.sort_by_key(|l| l.name.to_lowercase());
    t
}

impl Truth {
    fn cache(&self) -> Cache {
        Cache {
            synced_ms: Some(1),
            reminders: self.live.clone(),
            ..Cache::default()
        }
    }
}

fn utc_now() -> Zoned {
    jiff::Timestamp::now().to_zoned(TimeZone::UTC)
}

/// A reminder as its line reads (the titles and notes made here need no
/// cleaning).
fn item_of(r: &Reminder) -> Item {
    Item {
        done: r.completed,
        title: r.title.clone(),
        due: r.due.as_ref().map(|d| d.display(&TimeZone::UTC)),
        notes: r.notes.clone(),
    }
}

fn key(i: &Item) -> String {
    format!("{i:?}")
}

// --- the model ---

#[derive(Clone, Debug)]
struct Block {
    /// The reminder this line was made from; `None` for a line typed in.
    origin: Option<String>,
    item: Item,
    /// How the line is spelled: `- [ ]`, a bare line, `* `, `- `.
    style: usize,
    /// Indented, as a subtask is written.
    sub: bool,
}

fn block_text(b: &Block) -> String {
    let (pad, notes) = if b.sub { ("    ", "        ") } else { ("", "    ") };
    let head = match (if b.sub { 0 } else { b.style }, b.item.done) {
        (0, false) => "- [ ] ",
        (0, true) => "- [x] ",
        (1, false) => "",
        (1, true) => "* [X] ",
        (2, false) => "* ",
        (2, true) => "+ [x] ",
        (_, false) => "- ",
        (_, true) => "[x] ",
    };
    let due = b.item.due.as_ref().map(|d| format!(" 📅 {d}")).unwrap_or_default();
    let mut s = format!("{pad}{head}{}{due}\n", b.item.title);
    for line in b.item.notes.lines() {
        s.push_str(&format!("{notes}{line}\n"));
    }
    s
}

struct Copy {
    /// Reminders it ticks.
    ticks: Vec<String>,
    adds: Vec<Item>,
}

struct MList {
    id: String,
    file: String,
    /// The file as the last run wrote it, with the reminder of each line.
    written: Vec<Known>,
    blocks: Vec<Block>,
    bom: bool,
    /// Written with no heading.
    bare: bool,
    edited: bool,
    /// The file was removed, and not made again.
    removed: bool,
    copy: Option<Copy>,
    /// The file's names before its list was renamed.
    was: Vec<String>,
    /// The items and text recorded before the file was last written, when a
    /// run ended before it recorded the write: edited since, the file is
    /// read against whichever of the two it asks less of.
    before: Option<(Vec<Known>, String)>,
}

impl MList {
    /// A list as `text` was written, `written` its items.
    fn new(id: &str, file: &str, written: Vec<Known>, text: &str, was: Vec<String>) -> MList {
        // Subtasks are the indented checkboxes; a note that reads as one is
        // written with a backslash.
        let subs: Vec<bool> = text
            .lines()
            .filter(|l| l.starts_with("- [") || l.starts_with("    - ["))
            .map(|l| l.starts_with(' '))
            .collect();
        assert_eq!(subs.len(), written.len(), "{text}");
        let blocks = written
            .iter()
            .zip(subs)
            .map(|(k, sub)| Block {
                origin: Some(k.id.clone()),
                item: k.item.clone(),
                style: 0,
                sub,
            })
            .collect();
        MList {
            id: id.to_owned(),
            file: file.to_owned(),
            written,
            blocks,
            bom: false,
            bare: false,
            edited: false,
            removed: false,
            copy: None,
            was,
            before: None,
        }
    }

    fn text(&self) -> String {
        let mut s = if self.bare {
            String::new()
        } else {
            format!(
                "{}# {}\n\n",
                if self.bom { "\u{feff}" } else { "" },
                self.file.trim_end_matches(".md")
            )
        };
        for b in &self.blocks {
            s.push_str(&block_text(b));
        }
        s
    }

    /// A reminder whose line might be paired with another of its title.
    fn ambiguous(&self, id: &str) -> bool {
        let count = |title: &str| {
            self.written.iter().filter(|k| k.item.title == title).count()
                + self.blocks.iter().filter(|b| b.item.title == title).count()
        };
        let written = self.written.iter().find(|k| k.id == id).map(|k| k.item.title.as_str());
        let block = self
            .blocks
            .iter()
            .find(|b| b.origin.as_deref() == Some(id))
            .map(|b| b.item.title.as_str());
        // Another title like it (one retitled in iCloud by a word added)
        // may take its line.
        let mine = [written, block];
        let near = |t: &str| {
            let other = |o: &str| !mine.contains(&Some(o)) && alike(o, t) >= ALIKE;
            self.written.iter().any(|k| other(&k.item.title)) || self.blocks.iter().any(|b| other(&b.item.title))
        };
        if mine.iter().flatten().any(|t| near(t)) {
            return true;
        }
        match (written, block) {
            (Some(w), Some(b)) if w == b => count(w) > 2,
            (Some(w), Some(b)) => count(w) > 1 || count(b) > 1,
            (Some(w), None) => count(w) > 1,
            _ => true,
        }
    }
}

/// What iCloud should hold after a run.
#[derive(Default)]
struct Expect {
    /// Every reminder iCloud had before: its list and line, or deleted.
    known: BTreeMap<String, Option<(String, Item)>>,
    /// The reminders the run makes, by list.
    new: BTreeMap<String, Vec<Item>>,
    /// Files of lists deleted in iCloud that stay: they hold changes never sent.
    kept: Vec<String>,
    /// Files of lists deleted in iCloud that the run removes.
    removed: Vec<String>,
}

struct Case {
    seed: u64,
    rng: Rng,
    env: Env,
    log: Vec<String>,
    lists: Vec<MList>,
    /// Every title and list name used: new ones are unlike all of them.
    used: Vec<String>,
    /// Files left behind by deleted lists.
    left: BTreeSet<String>,
    /// Files the runs removed with their deleted lists.
    removed: BTreeSet<String>,
    /// Files that may be named in a warning this step: they came back.
    back: Vec<String>,
    n: usize,
}

impl Case {
    fn fail(&self, what: &str) -> ! {
        panic!(
            "seed {} (ICLOUD_REMINDERS_MODEL_SEED={}): {what}\n--- operations:\n{}",
            self.seed,
            self.seed,
            self.log.join("\n")
        );
    }

    fn note(&mut self, s: String) {
        self.log.push(s);
    }

    /// A title unlike every one used so far.
    fn fresh(&mut self) -> String {
        loop {
            let t = format!("{} {}", capital(&self.rng.word()), self.rng.word());
            if self.used.iter().all(|u| alike(u, &t) < ALIKE) {
                self.used.push(t.clone());
                return t;
            }
        }
    }

    fn new(seed: u64) -> Case {
        let mut case = Case {
            seed,
            rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1),
            env: start(),
            log: Vec::new(),
            lists: Vec::new(),
            used: [
                "Milk",
                "Eggs",
                "Call the dentist",
                "Renew passport",
                "Buy paint",
                "Groceries",
                "Reminders",
            ]
            .map(str::to_owned)
            .to_vec(),
            left: BTreeSet::new(),
            removed: BTreeSet::new(),
            back: Vec::new(),
            n: 0,
        };
        // Enough lines that removing most of a list is possible.
        for _ in 0..5 {
            let title = case.fresh();
            case.icloud_add("List/LIST-GROCERIES", &title, 30);
        }
        let data = case.env.background().unwrap_or_else(|e| case.fail(&e));
        case.note(format!("first run: {}", data["folder"]));
        case.rebuild();
        case
    }

    // --- iCloud's side ---

    fn icloud_add(&mut self, list: &str, title: &str, created_minutes_ago: i64) {
        self.n += 1;
        let uuid = format!("ICLOUD-{}", self.n);
        let notes = if self.rng.chance(20) {
            format!("Note {}", self.rng.word())
        } else {
            String::new()
        };
        let mut r = fake::reminder(
            &uuid,
            list,
            title,
            &notes,
            None,
            now_ms() - created_minutes_ago * MINUTE,
        );
        if self.rng.chance(25) {
            // An all-day date in 2030.
            let day = 1 + self.rng.below(28) as i64;
            r["fields"]["DueDate"] = json!({ "type": "TIMESTAMP", "value": 1_893_456_000_000i64 + day * 86_400_000 });
            r["fields"]["AllDay"] = json!({ "type": "INT64", "value": 1 });
        }
        self.note(format!("icloud: add {uuid} {title:?} to {list}"));
        self.env.state.put(r);
    }

    fn icloud_op(&mut self) {
        let t = truth(&self.env.state);
        let ids: Vec<&Reminder> = t.live.values().collect();
        let pick_list = |rng: &mut Rng| t.lists[rng.below(t.lists.len())].id.clone();
        match self.rng.below(13) {
            0 | 1 if !t.lists.is_empty() => {
                let list = pick_list(&mut self.rng);
                let title = self.fresh();
                let ago = self.rng.below(60) as i64;
                self.icloud_add(&list, &title, ago);
            }
            2 if !ids.is_empty() => {
                let r = ids[self.rng.below(ids.len())];
                let done = !r.completed;
                self.note(format!("icloud: {} {}", if done { "complete" } else { "reopen" }, r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["Completed"] = json!({ "type": "INT64", "value": i64::from(done) });
                    rec["fields"]["CompletionDate"] =
                        json!({ "type": "TIMESTAMP", "value": if done { json!(now_ms()) } else { Value::Null } });
                });
            }
            3 | 4 if !ids.is_empty() => {
                let r = ids[self.rng.below(ids.len())];
                let title = if self.rng.chance(50) {
                    format!("{} {}", r.title, self.rng.word())
                } else {
                    self.fresh()
                };
                self.used.push(title.clone());
                self.note(format!("icloud: retitle {} to {title:?}", r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["TitleDocument"] =
                        json!({ "type": "ENCRYPTED_BYTES", "value": topotext::encode(&title) });
                });
            }
            5 if !ids.is_empty() => {
                let r = ids[self.rng.below(ids.len())];
                self.note(format!("icloud: delete {}", r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["Deleted"] = json!({ "type": "INT64", "value": 1 })
                });
            }
            6 if !ids.is_empty() && t.lists.len() > 1 => {
                let r = ids[self.rng.below(ids.len())];
                let to = pick_list(&mut self.rng);
                if to == r.list_id {
                    return;
                }
                self.note(format!("icloud: move {} to {to}", r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["List"] =
                        json!({ "type": "REFERENCE", "value": { "recordName": to, "action": "VALIDATE" } });
                });
            }
            7 if !t.lists.is_empty() => {
                let list = pick_list(&mut self.rng);
                let name = self.fresh();
                self.note(format!("icloud: rename {list} to {name:?}"));
                self.env.edit(&list, |rec| {
                    rec["fields"]["Name"] = json!({ "type": "STRING", "value": name })
                });
            }
            8 if t.lists.len() > 2 => {
                let list = pick_list(&mut self.rng);
                self.note(format!("icloud: delete {list} and its reminders"));
                for r in t.live.values().filter(|r| r.list_id == list) {
                    self.env.edit(&r.id, |rec| {
                        rec["fields"]["Deleted"] = json!({ "type": "INT64", "value": 1 })
                    });
                }
                self.env.edit(&list, |rec| {
                    rec["fields"]["Deleted"] = json!({ "type": "INT64", "value": 1 })
                });
            }
            9 => {
                self.n += 1;
                let id = format!("List/LIST-{}", self.n);
                let name = self.fresh();
                self.note(format!("icloud: new list {id} {name:?}"));
                self.env.state.put(fake::list(&id, &name, &[], "#34C759"));
            }
            10 if !ids.is_empty() => {
                let r = ids[self.rng.below(ids.len())];
                let due = if self.rng.chance(30) {
                    Value::Null
                } else {
                    json!(1_893_456_000_000i64 + (1 + self.rng.below(300) as i64) * 86_400_000)
                };
                self.note(format!("icloud: due of {} to {due}", r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["DueDate"] = json!({ "type": "TIMESTAMP", "value": due });
                    rec["fields"]["AllDay"] = json!({ "type": "INT64", "value": 1 });
                });
            }
            11 if !ids.is_empty() => {
                // One level of subtasks, as Apple's Reminders has.
                let r = ids[self.rng.below(ids.len())];
                let parent = if r.parent_id.is_some() && self.rng.chance(40) {
                    None
                } else {
                    let parents: Vec<&&Reminder> = ids
                        .iter()
                        .filter(|p| p.list_id == r.list_id && p.id != r.id && p.parent_id.is_none())
                        .collect();
                    if parents.is_empty() || ids.iter().any(|c| c.parent_id.as_deref() == Some(&r.id)) {
                        return;
                    }
                    Some(parents[self.rng.below(parents.len())].id.clone())
                };
                self.note(format!("icloud: parent of {} to {parent:?}", r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["ParentReminder"] = match parent {
                        Some(p) => json!({ "type": "REFERENCE", "value": { "recordName": p, "action": "NONE" } }),
                        None => json!({ "type": "REFERENCE", "value": Value::Null }),
                    };
                });
            }
            12 if !ids.is_empty() => {
                let r = ids[self.rng.below(ids.len())];
                let notes = if self.rng.chance(30) {
                    String::new()
                } else {
                    format!("Note {}", self.rng.word())
                };
                self.note(format!("icloud: notes of {} to {notes:?}", r.id));
                self.env.edit(&r.id, |rec| {
                    rec["fields"]["NotesDocument"] =
                        json!({ "type": "ENCRYPTED_BYTES", "value": topotext::encode(&notes) });
                });
            }
            _ => {}
        }
    }

    // --- the phone's side ---

    /// A title for line `b` of list `li` a little different from what it
    /// was, that still pairs with it and with nothing else.
    fn light(&mut self, li: usize, b: usize) -> Option<String> {
        let ml = &self.lists[li];
        let old = ml.blocks[b].item.title.clone();
        let new = if self.rng.chance(60) || old.chars().count() < 4 {
            format!("{old} {}", self.rng.word())
        } else {
            let mut c: Vec<char> = old.chars().collect();
            let last = c.len() - 1;
            c[last] = if c[last] == 'q' { 'z' } else { 'q' };
            c.into_iter().collect()
        };
        let origin = ml.blocks[b].origin.as_deref();
        let was = origin.and_then(|id| ml.written.iter().find(|k| k.id == id));
        let ok = ml
            .written
            .iter()
            .all(|k| k.item.title != new && (Some(k.id.as_str()) == origin || alike(&k.item.title, &new) < ALIKE))
            && ml.blocks.iter().all(|o| o.item.title != new)
            && was.is_none_or(|k| alike(&k.item.title, &new) >= ALIKE);
        ok.then(|| {
            self.used.push(new.clone());
            new
        })
    }

    fn date(&mut self) -> String {
        let day = format!("2030-0{}-1{}", 1 + self.rng.below(9), self.rng.below(10));
        if self.rng.chance(50) {
            day
        } else {
            format!("{day} 09:30")
        }
    }

    fn file_op(&mut self, li: usize) {
        let n = self.lists[li].blocks.len();
        let file = self.lists[li].file.clone();
        let pick = self.rng.below(n);
        let what = match self.rng.below(15) {
            0 | 1 if n > 0 => {
                let b = &mut self.lists[li].blocks[pick];
                b.item.done = !b.item.done;
                format!("tick/untick {:?}", b.item.title)
            }
            2..=4 => {
                let title = if self.rng.chance(15) {
                    let t = format!("#{} {}", self.rng.word(), capital(&self.rng.word()));
                    self.used.push(t.clone());
                    t
                } else {
                    self.fresh()
                };
                let due = self.rng.chance(30).then(|| self.date());
                let notes = if self.rng.chance(20) {
                    format!("Note {}", self.rng.word())
                } else {
                    String::new()
                };
                let block = Block {
                    origin: None,
                    item: Item {
                        done: self.rng.chance(10),
                        title,
                        due,
                        notes,
                    },
                    style: self.rng.below(4),
                    sub: self.rng.chance(10),
                };
                let at = self.rng.below(n + 1);
                let what = format!("add {:?}", block_text(&block));
                self.lists[li].blocks.insert(at, block);
                what
            }
            5 if n > 0 => {
                let b = self.lists[li].blocks.remove(pick);
                format!("remove {:?}", b.item.title)
            }
            6 if n >= 5 => {
                let k = 4 + self.rng.below(n - 3);
                for _ in 0..k {
                    let at = self.rng.below(self.lists[li].blocks.len());
                    self.lists[li].blocks.remove(at);
                }
                // A file from elsewhere has lines of its own.
                for _ in 0..self.rng.below(4) {
                    let title = self.fresh();
                    self.lists[li].blocks.push(Block {
                        origin: None,
                        item: Item {
                            done: false,
                            title,
                            due: None,
                            notes: String::new(),
                        },
                        style: 0,
                        sub: false,
                    });
                }
                format!("remove {k} lines, add {}", self.lists[li].blocks.len() + k - n)
            }
            7 if n > 0 => {
                let Some(title) = self.light(li, pick) else {
                    return;
                };
                let b = &mut self.lists[li].blocks[pick];
                let what = format!("retitle {:?} lightly to {title:?}", b.item.title);
                b.item.title = title;
                what
            }
            8 if n > 0 => {
                let title = self.fresh();
                let b = &mut self.lists[li].blocks[pick];
                let what = format!("rewrite {:?} as {title:?}", b.item.title);
                b.item.title = title;
                b.origin = None;
                what
            }
            9 => {
                let blocks = &mut self.lists[li].blocks;
                for i in (1..blocks.len()).rev() {
                    let j = (self.rng.next() % (i as u64 + 1)) as usize;
                    blocks.swap(i, j);
                }
                "reorder".to_owned()
            }
            10 if n > 0 => {
                let word = self.rng.word();
                let b = &mut self.lists[li].blocks[pick];
                b.item.notes = match b.item.notes.as_str() {
                    "" => format!("Note {word}"),
                    notes => format!("{notes}\n{word}"),
                };
                format!("note under {:?}", b.item.title)
            }
            11 if n > 0 => {
                let mut copy = self.lists[li].blocks[pick].clone();
                copy.origin = None;
                copy.style = self.rng.below(4);
                if self.rng.chance(30) {
                    copy.item.done = !copy.item.done;
                }
                let at = self.rng.below(n + 1);
                let what = format!("duplicate {:?}", block_text(&copy));
                self.lists[li].blocks.insert(at, copy);
                what
            }
            12 if n > 0 && self.rng.chance(50) => {
                let due = if self.rng.chance(30) { None } else { Some(self.date()) };
                let b = &mut self.lists[li].blocks[pick];
                b.item.due = due;
                format!("due of {:?} to {:?}", b.item.title, b.item.due)
            }
            12 => {
                self.lists[li].bom = true;
                "byte order mark".to_owned()
            }
            13 if self.rng.chance(40) => {
                let ml = &mut self.lists[li];
                ml.blocks.clear();
                ml.edited = false;
                ml.removed = true;
                self.note(format!("file {file}: removed"));
                return;
            }
            14 if self.rng.chance(40) => {
                self.lists[li].blocks.clear();
                self.lists[li].bare = self.rng.chance(50);
                "emptied".to_owned()
            }
            _ => return,
        };
        self.lists[li].edited = true;
        self.lists[li].removed = false;
        self.note(format!("file {file}: {what}"));
    }

    /// A Syncthing conflict copy of list `li`'s file: the file as last
    /// written, a few lines short, a box or two the other way, a tick and a
    /// line or two of its own.
    fn copy_op(&mut self, li: usize, back: Option<String>) {
        let mut lines = Vec::new();
        let mut ticks = Vec::new();
        let written = self.lists[li].written.clone();
        for k in &written {
            let ml = &self.lists[li];
            let Some(b) = ml.blocks.iter().find(|b| b.origin.as_deref() == Some(&k.id)) else {
                // Removed from the file meanwhile: a copy holding it would
                // add it back, as documented.
                continue;
            };
            let unique = !ml.ambiguous(&k.id);
            if self.rng.chance(25) {
                continue;
            }
            let mut it = k.item.clone();
            if b.item.title == k.item.title && unique && !k.item.done && !b.item.done && self.rng.chance(35) {
                it.done = true;
                ticks.push(k.id.clone());
            } else if it.done && self.rng.chance(40) {
                it.done = false;
            }
            lines.push(it);
        }
        let mut adds = Vec::new();
        for _ in 0..self.rng.below(3) {
            let title = self.fresh();
            let it = Item {
                done: false,
                title,
                due: None,
                notes: String::new(),
            };
            let at = self.rng.below(lines.len() + 1);
            lines.insert(at, it.clone());
            adds.push(it);
        }
        // The file gets a line like one of them, typed on another device.
        if let Some(add) = adds.first().filter(|_| self.rng.chance(30)) {
            let title = format!("{} {}", add.title, self.rng.word());
            let ml = &self.lists[li];
            let apart = ml.written.iter().all(|k| alike(&k.item.title, &title) < ALIKE)
                && ml.blocks.iter().all(|b| alike(&b.item.title, &title) < ALIKE);
            if apart {
                self.used.push(title.clone());
                let block = Block {
                    origin: None,
                    item: Item {
                        done: false,
                        title,
                        due: None,
                        notes: String::new(),
                    },
                    style: 0,
                    sub: false,
                };
                self.note(format!("file {}: add {:?}", self.lists[li].file, block_text(&block)));
                let ml = &mut self.lists[li];
                ml.blocks.push(block);
                ml.edited = true;
                ml.removed = false;
            }
        }
        let ml = &mut self.lists[li];
        let stem = ml.file.trim_end_matches(".md").to_owned();
        self.n += 1;
        let name = match back {
            Some(name) => {
                self.back.push(name.clone());
                name
            }
            None => format!("{stem}.sync-conflict-20261114-0930{:02}-ABCDEFG.md", self.n % 100),
        };
        let mut text = format!("# {stem}\n\n");
        for it in &lines {
            text.push_str(&block_text(&Block {
                origin: None,
                item: it.clone(),
                style: 0,
                sub: false,
            }));
        }
        std::fs::write(self.env.folder().join(&name), &text).unwrap();
        // A second copy in one step adds to what the first asks for.
        match &mut ml.copy {
            Some(c) => {
                c.ticks.extend(ticks);
                c.adds.extend(adds);
            }
            None => ml.copy = Some(Copy { ticks, adds }),
        }
        self.note(format!("copy {name}: {text:?}"));
    }

    // --- prediction and checks ---

    fn predict(&self, t: &Truth) -> Expect {
        let now = utc_now();
        let mut expect = Expect::default();
        for id in &t.ids {
            let live = t.live.get(id).map(|r| (r.list_id.clone(), item_of(r)));
            expect.known.insert(id.clone(), live);
        }
        for ml in &self.lists {
            if !t.lists.iter().any(|l| l.id == ml.id) {
                // Its old names are remembered as removed too.
                expect.removed.extend(ml.was.iter().cloned());
                if ml.edited {
                    expect.kept.push(ml.file.clone());
                } else if self.env.folder().join(&ml.file).exists() {
                    expect.removed.push(ml.file.clone());
                }
                continue;
            }
            let new = expect.new.entry(ml.id.clone()).or_default();
            if ml.edited {
                let local: Vec<Item> = ml.blocks.iter().map(|b| b.item.clone()).collect();
                let mut delta = folder::diff(&ml.written, &local, &now);
                let mut base = ml.written.len();
                let size = |d: &folder::Delta| d.adds.len() + d.updates.len() + d.deletes.len();
                match &ml.before {
                    Some((items, text))
                        if *text == ml.text() || size(&folder::diff(items, &local, &now)) <= size(&delta) =>
                    {
                        delta = folder::diff(items, &local, &now);
                        base = items.len();
                    }
                    _ => self.check_pairing(ml, &delta),
                }
                for (id, it, changes) in &delta.updates {
                    if let Some(Some((_, item))) = expect.known.get_mut(id) {
                        for c in changes {
                            match c {
                                Change::Title(t) => item.title = t.clone(),
                                Change::Notes(n) => item.notes = n.clone(),
                                Change::Completed(d) => item.done = *d,
                                Change::Due(_) => item.due = it.due.clone(),
                                Change::Deleted => unreachable!(),
                            }
                        }
                    }
                }
                if !folder::risky(&delta, base) {
                    for id in &delta.deletes {
                        if let Some(e) = expect.known.get_mut(id) {
                            *e = None;
                        }
                    }
                }
                new.extend(delta.adds);
            }
            if let Some(copy) = &ml.copy {
                for id in &copy.ticks {
                    if let Some(Some((_, item))) = expect.known.get_mut(id) {
                        item.done = true;
                    }
                }
                new.extend(copy.adds.iter().cloned());
            }
        }
        expect
    }

    /// A line that can only be one reminder is paired with it.
    fn check_pairing(&self, ml: &MList, delta: &folder::Delta) {
        for b in &ml.blocks {
            let Some(id) = b.origin.as_deref().filter(|id| !ml.ambiguous(id)) else {
                continue;
            };
            let was = &ml.written.iter().find(|k| k.id == id).expect("written").item;
            let update = delta.updates.iter().find(|u| u.0 == id);
            let paired = match update {
                Some((_, it, _)) => *it == b.item,
                None => *was == b.item,
            };
            if !paired || delta.deletes.iter().any(|d| d == id) {
                self.fail(&format!(
                    "{}: the line {:?} is not paired with {id}: {delta:?}",
                    ml.file, b.item
                ));
            }
        }
        for k in &ml.written {
            let kept = ml.blocks.iter().any(|b| b.origin.as_deref() == Some(&k.id));
            if !kept && !ml.ambiguous(&k.id) && !delta.deletes.contains(&k.id) {
                self.fail(&format!(
                    "{}: {} lost its line, but is not deleted: {delta:?}",
                    ml.file, k.id
                ));
            }
        }
    }

    fn check(&mut self, before: &Truth, expect: &Expect, datas: &[Value]) {
        let after = truth(&self.env.state);
        for (id, want) in &expect.known {
            let got = after.live.get(id).map(|r| (r.list_id.clone(), item_of(r)));
            if got != *want {
                self.fail(&format!("{id}: expected {want:?}, iCloud has {got:?}"));
            }
        }
        let mut made: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for r in after.live.values().filter(|r| !before.ids.contains(&r.id)) {
            made.entry(r.list_id.clone()).or_default().push(key(&item_of(r)));
        }
        for (list, items) in &expect.new {
            let mut want: Vec<String> = items.iter().map(key).collect();
            want.sort();
            let mut got = made.remove(list).unwrap_or_default();
            got.sort();
            if got != want {
                self.fail(&format!("{list}: expected new {want:?}, iCloud made {got:?}"));
            }
        }
        if !made.is_empty() {
            self.fail(&format!("made in lists with no file: {made:?}"));
        }
        for w in datas
            .iter()
            .flat_map(|d| d["folder"]["warnings"].as_array().into_iter().flatten())
        {
            let w = w.as_str().unwrap_or("");
            let kept = self.left.iter().chain(&expect.kept);
            let back = self.back.iter().any(|f| w.starts_with(&format!("{f}: came back")));
            if !back && !kept.clone().any(|f| w.starts_with(&format!("{f}: its list is gone"))) {
                self.fail(&format!("warning: {w}"));
            }
        }
    }

    /// The folder: one file per list, as iCloud has it, and what deleted
    /// lists left.
    fn check_files(&mut self, expect: &Expect) {
        let after = truth(&self.env.state);
        let dir = self.env.folder();
        let now = utc_now();
        let cache = after.cache();
        let mut want_files: BTreeSet<String> = self.left.clone();
        for file in &expect.kept {
            self.left.insert(file.clone());
            want_files.insert(file.clone());
        }
        self.removed.extend(expect.removed.iter().cloned());
        for l in &after.lists {
            let file = format!("{}.md", l.name);
            let text = std::fs::read_to_string(dir.join(&file)).unwrap_or_default();
            let (want, _) = folder::render(l, &cache, &now);
            if text != want {
                self.fail(&format!("{file} reads\n{text}\nnot, as iCloud has it,\n{want}"));
            }
            want_files.insert(file);
        }
        let files: BTreeSet<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        if files != want_files {
            self.fail(&format!("the folder holds {files:?}, not {want_files:?}"));
        }
    }

    /// The model's lists from the files as they now are.
    fn rebuild(&mut self) {
        let t = truth(&self.env.state);
        let now = utc_now();
        let cache = t.cache();
        self.lists = t
            .lists
            .iter()
            .map(|l| {
                let (text, ids) = folder::render(l, &cache, &now);
                let written: Vec<Known> = ids
                    .into_iter()
                    .zip(folder::parse(&text, &now))
                    .map(|(id, item)| Known { id, item })
                    .collect();
                let file = format!("{}.md", l.name);
                // Renamed: the old name is remembered.
                let mut was = Vec::new();
                if let Some(old) = self.lists.iter().find(|m| m.id == l.id) {
                    was = old.was.clone();
                    if old.file != file {
                        was.retain(|w| *w != file);
                        was.push(old.file.clone());
                    }
                }
                MList::new(&l.id, &file, written, &text, was)
            })
            .collect();
    }

    /// The model's lists from `folder.json`, after a run that ended early:
    /// false (and nothing changed) unless every list's file is as its state
    /// says it was written, with nothing half done.
    fn rebuild_from_state(&mut self) -> bool {
        let Ok(text) = std::fs::read_to_string(self.env.dir.path().join("data/folder.json")) else {
            return false;
        };
        let state: folder::State = serde_json::from_str(&text).unwrap();
        let t = truth(&self.env.state);
        let dir = self.env.folder();
        let ids: BTreeSet<&String> = t.lists.iter().map(|l| &l.id).collect();
        let left: BTreeSet<String> = state.left.iter().cloned().collect();
        let removed: BTreeSet<String> = state.removed.iter().cloned().collect();
        // A file found as the run was about to write it was written.
        let as_left = |ls: &folder::ListState| {
            let disk = std::fs::read_to_string(dir.join(&ls.file)).ok()?;
            match &ls.writing {
                Some(w) if w.text == disk => Some((
                    w.text.clone(),
                    w.items.clone(),
                    Some((ls.items.clone(), ls.text.clone())),
                )),
                _ => (ls.text == disk).then(|| (ls.text.clone(), ls.items.clone(), None)),
            }
        };
        let clean = state.lists.keys().collect::<BTreeSet<_>>() == ids
            && left == self.left
            && removed == self.removed
            && state.lists.values().all(|ls| {
                ls.adopted
                    && ls.pending.is_empty()
                    && ls.unwritten.is_empty()
                    && ls.stuck.is_none()
                    && as_left(ls).is_some()
            });
        let stray = std::fs::read_dir(&dir).unwrap().any(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.contains(".sync-conflict-")
        });
        if !clean || stray {
            return false;
        }
        self.lists = t
            .lists
            .iter()
            .map(|l| {
                let ls = &state.lists[&l.id];
                let (text, items, before) = as_left(ls).expect("clean");
                let mut ml = MList::new(&l.id, &ls.file, items, &text, ls.was.clone());
                ml.before = before;
                ml
            })
            .collect();
        true
    }

    /// A run that ends at a crash point: whether it got there.
    fn crashed_at(&mut self, points: &[&str]) -> bool {
        let point = points[self.rng.below(points.len())];
        let n = 1 + self.rng.below(3);
        let out = self
            .env
            .background_command()
            .env(folder::CRASH_AT, format!("{point}:{n}"))
            .output()
            .unwrap();
        let crashed = match code(&out) {
            0 => false,
            86 => true,
            c => self.fail(&format!("the run to crash at {point}:{n} exited {c}: {}", stderr(&out))),
        };
        self.note(format!("run, crashing at {point}:{n}: {crashed}"));
        crashed
    }

    /// Two runs at once.
    fn overlapping(&mut self) -> Vec<Value> {
        let children: Vec<_> = (0..2)
            .map(|_| {
                self.env
                    .background_command()
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let mut datas = Vec::new();
        for child in children {
            let out = child.wait_with_output().unwrap();
            if code(&out) != 0 {
                self.fail(&format!("an overlapping run exited {}: {}", code(&out), stderr(&out)));
            }
            datas.push(envelope(&out.stdout)["data"].clone());
        }
        self.note(format!(
            "two runs at once: {}",
            datas
                .iter()
                .map(|d| d["folder"].to_string())
                .collect::<Vec<_>>()
                .join(" and ")
        ));
        datas
    }

    fn step(&mut self, i: usize) {
        self.note(format!("--- step {i}"));
        self.back.clear();
        for _ in 0..self.rng.below(3) {
            self.icloud_op();
        }
        // iCloud's changes half written to the files, which the user then
        // edits: a reminder moved in iCloud can be in two files.
        if self.rng.chance(20) {
            let crashed = self.crashed_at(&[
                "tmp-written",
                "file-written",
                "rename-pending",
                "renamed",
                "gone-removed",
            ]);
            if crashed && self.rebuild_from_state() {
                self.note("the files as that run left them".to_owned());
                // iCloud changes again before the next run settles them.
                for _ in 0..self.rng.below(3) {
                    self.icloud_op();
                }
            } else if crashed {
                // Finished by a run before the user edits.
                let data = self.env.background().unwrap_or_else(|e| self.fail(&e));
                self.note(format!("run: {}", data["folder"]));
                self.rebuild();
            } else {
                self.rebuild();
            }
        }
        let live = truth(&self.env.state);
        for _ in 0..self.rng.below(5) {
            if self.lists.is_empty() {
                break;
            }
            let li = self.rng.below(self.lists.len());
            self.file_op(li);
        }
        for _ in 0..usize::from(self.rng.chance(20)) + usize::from(self.rng.chance(5)) {
            if self.lists.is_empty() {
                break;
            }
            let li = self.rng.below(self.lists.len());
            // A copy is read against what the run settles the file was.
            if live.lists.iter().any(|l| l.id == self.lists[li].id) && self.lists[li].before.is_none() {
                // Syncthing brings back the file under a name it had before
                // its list was renamed, as the phone had changed it.
                let dir = self.env.folder();
                let olds: Vec<String> = self.lists[li]
                    .was
                    .iter()
                    .filter(|w| !dir.join(w).exists())
                    .cloned()
                    .collect();
                let back = (!olds.is_empty() && self.rng.chance(50)).then(|| olds[self.rng.below(olds.len())].clone());
                self.copy_op(li, back);
            }
        }
        // ... or the file of a deleted list, which the phone had changed.
        if self.rng.chance(40) && !self.removed.is_empty() {
            let names: Vec<String> = self.removed.iter().cloned().collect();
            let name = names[self.rng.below(names.len())].clone();
            let dir = self.env.folder();
            if !dir.join(&name).exists() {
                let line = self.fresh();
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(&name), format!("# Gone\n\n- [ ] {line}\n")).unwrap();
                self.note(format!("{name} back, with {line:?}"));
                self.removed.remove(&name);
                self.left.insert(name.clone());
                self.back.push(name);
            }
        }
        let now = utc_now();
        let dir = self.env.folder();
        for ml in &mut self.lists {
            if ml.edited {
                let text = ml.text();
                for b in &ml.blocks {
                    let read = folder::parse(&block_text(b), &now);
                    assert_eq!(
                        read,
                        std::slice::from_ref(&b.item),
                        "the model writes {:?}",
                        block_text(b)
                    );
                }
                // Edits that undo each other leave the file as it was.
                ml.edited = std::fs::read_to_string(dir.join(&ml.file)).ok() != Some(text.clone());
                if ml.edited {
                    std::fs::write(dir.join(&ml.file), text).unwrap();
                }
            } else if ml.removed {
                let _ = std::fs::remove_file(dir.join(&ml.file));
            }
        }
        if self.rng.chance(4) {
            std::fs::remove_dir_all(&dir).unwrap();
            self.note("the folder deleted".to_owned());
            // A deleted list's file brought back is gone again, and stays
            // remembered.
            for name in &self.back {
                if self.left.contains(name) {
                    self.removed.insert(name.clone());
                }
            }
            self.left.clear();
            for ml in &mut self.lists {
                ml.edited = false;
                ml.copy = None;
            }
        }

        let before = truth(&self.env.state);
        let expect = self.predict(&before);
        let mut datas = Vec::new();
        match self.rng.below(100) {
            0..30 => {
                let nth = 1 + self.rng.below(4);
                let killed = self.env.killed_at_write(nth);
                self.note(format!("run, killed at write {nth}: {killed}"));
            }
            30..55 => {
                self.crashed_at(folder::CRASH_POINTS);
            }
            55..65 => datas = self.overlapping(),
            _ => {}
        }
        let data = self.env.background().unwrap_or_else(|e| self.fail(&e));
        self.note(format!("run: {}", data["folder"]));
        datas.push(data);
        self.check(&before, &expect, &datas);

        // A list's run may change a reminder that another list's file,
        // written earlier in the run, shows (one moved there in iCloud):
        // that file catches up at the next run, which sends nothing.
        let sent = self.env.modifies();
        let mut again = self.env.background().unwrap_or_else(|e| self.fail(&e));
        if again["folder"]["written"] != json!([]) {
            self.note(format!("catching up: {}", again["folder"]));
            again["folder"]["written"] = json!([]);
            self.check_quiet(sent, &again);
            again = self.env.background().unwrap_or_else(|e| self.fail(&e));
        }
        // Nothing left to do: nothing sent, nothing written.
        self.check_quiet(sent, &again);
        self.check_files(&expect);
        self.rebuild();
    }

    fn check_quiet(&self, sent: usize, data: &Value) {
        let f = &data["folder"];
        let quiet = self.env.modifies() == sent
            && f["written"] == json!([])
            && f["warnings"] == json!([])
            && f["restored"] == json!([])
            && [&f["added"], &f["edited"], &f["deleted"]]
                .iter()
                .all(|v| **v == json!(0));
        if !quiet {
            self.fail(&format!("a run with nothing to do did something: {f}"));
        }
    }

    /// Runs past the case's end: nothing is waiting for anything. Every
    /// run is quiet, the files read as iCloud has them, and `folder.json`
    /// holds no marker of a write, rename, add or failure still open.
    fn settle(&mut self) {
        for i in 0..3 {
            let sent = self.env.modifies();
            let data = self.env.background().unwrap_or_else(|e| self.fail(&e));
            self.note(format!("settling run {i}: {}", data["folder"]));
            self.check_quiet(sent, &data);
            self.check_files(&Expect::default());
        }
        let text = std::fs::read_to_string(self.env.dir.path().join("data/folder.json")).unwrap();
        let state: folder::State = serde_json::from_str(&text).unwrap();
        let t = truth(&self.env.state);
        let dir = self.env.folder();
        let ids: BTreeSet<&String> = t.lists.iter().map(|l| &l.id).collect();
        if state.lists.keys().collect::<BTreeSet<_>>() != ids {
            self.fail(&format!(
                "folder.json has lists {:?}, iCloud {ids:?}",
                state.lists.keys()
            ));
        }
        for (id, ls) in &state.lists {
            let open = !ls.adopted
                || !ls.pending.is_empty()
                || ls.writing.is_some()
                || ls.renaming.is_some()
                || ls.stuck.is_some()
                || !ls.unwritten.is_empty()
                || std::fs::read_to_string(dir.join(&ls.file)).ok().as_ref() != Some(&ls.text);
            if open {
                self.fail(&format!("{id} has not settled: {ls:?}"));
            }
            if let Some(w) = ls.was.iter().find(|w| dir.join(w).exists()) {
                self.fail(&format!("{id}'s old name {w} is in the folder"));
            }
        }
        let left: BTreeSet<String> = state.left.iter().cloned().collect();
        if left != self.left || left.iter().any(|f| !dir.join(f).exists()) {
            self.fail(&format!("folder.json leaves {left:?}, the model {:?}", self.left));
        }
        if let Some(r) = state.removed.iter().find(|r| dir.join(r).exists()) {
            self.fail(&format!("{r} is back but not left"));
        }
        let hidden: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        if !hidden.is_empty() {
            self.fail(&format!("the folder still holds {hidden:?}"));
        }
    }
}

fn run_case(seed: u64) {
    let steps = std::env::var("ICLOUD_REMINDERS_MODEL_STEPS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(STEPS);
    let mut case = Case::new(seed);
    for i in 0..steps {
        case.step(i);
    }
    case.settle();
}

#[test]
fn random_edits_on_both_sides_and_killed_runs_keep_the_folder_and_icloud_in_step() {
    let seeds: Vec<u64> = match std::env::var("ICLOUD_REMINDERS_MODEL_SEED") {
        Ok(s) => vec![s.parse().expect("a seed")],
        Err(_) => {
            let cases = std::env::var("ICLOUD_REMINDERS_MODEL_CASES")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(CASES);
            (1..=cases).collect()
        }
    };
    // A few cases at a time: thousands at once would time the runs out.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..workers.min(seeds.len()) {
            s.spawn(|| {
                while let Some(&seed) = seeds.get(next.fetch_add(1, std::sync::atomic::Ordering::SeqCst)) {
                    run_case(seed);
                }
            });
        }
    });
}
