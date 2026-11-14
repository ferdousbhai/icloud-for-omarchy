//! Model-based tests: seeded random contacts (every field kind, duplicates,
//! values with padding and punctuation, properties this app does not read)
//! edited by random sequences through both edit paths, the command line's
//! `edit` (the built binary) and the window's dialog ([`Draft::changes`]
//! through [`Service::update`]), some of them over a change another device
//! made meanwhile (iCloud's 412, then the retry). After every edit the card
//! iCloud holds must be the card the edit started from (the other device's,
//! after a conflict) with exactly the named changes made, computed here by
//! a reference model on the raw JSON: every field and every entry the edit
//! did not name is byte for byte as it was. Every generated contact is also
//! written as a vCard and read back.
//!
//! A failure prints the seed and the steps so far; `MODEL_SEED=<n>` runs
//! that one, `MODEL_CASES=<n>` the first n (40 by default).

#[path = "../examples/fake_contacts.rs"]
#[allow(dead_code)]
mod fake;

use std::net::TcpListener;
use std::sync::{Arc, OnceLock};

use icloud_app::testing::{code, command, envelope, stderr};
use icloud_contacts::api::SessionTransport;
use icloud_contacts::draft::{Draft, Row, typed};
use icloud_contacts::model::{Contact, Field, Multi};
use icloud_contacts::service::Service;
use icloud_contacts::store::Store;
use icloud_contacts::vcard;
use serde_json::{Map, Value, json};

/// SplitMix64: small, seeded, deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const TEXTS: &[&str] = &[
    "Ann",
    "Zoë",
    "O’Brien",
    "Lee, Jr",
    "semi;colon",
    "back\\slash",
    "quote\"d",
    "日本語",
    "🙂 smile",
    " padded ",
    "x:y",
    "Ünïcødé-name",
];
const NOTES: &[&str] = &[
    "Met at the conference",
    "line 1\nline 2",
    "crlf 1\r\ncrlf 2",
    "old mac\rline",
    "a;b,c\\d \"q\" 🙂",
    " leading and trailing \n",
    "tab\there",
];
/// Stored labels; `None` is an entry with no `label` key.
const LABELS: &[Option<&str>] = &[
    Some("MOBILE"),
    Some("IPHONE"),
    Some("HOME"),
    Some("WORK"),
    Some("MAIN"),
    Some("HOME FAX"),
    Some("WhatsApp"),
    Some("Ponsel \"x\""),
    Some("my, label"),
    Some(""),
    None,
];
const PHONES: &[&str] = &[
    "+1 (555) 010-0100",
    "555-010-0199",
    "+1 555 010 0199",
    "+47 555 01 234",
    " +44 20 7946 0000 ",
    "1-800-FLOWERS",
    "ext. office",
];
const EMAILS: &[&str] = &[
    "ann@acme.example",
    "Ann@Acme.Example",
    " bob@x.org ",
    "a,b;c@x.org",
    "nope",
];
const URLS: &[&str] = &[
    "https://x.org/?a=b,c",
    "acme.example",
    " https://p.example/ann ",
    "x.org/a;b",
];
/// Valid values an edit types.
const NEW_PHONES: &[&str] = &["+1 555 010 0177", "+62 812 555", "555-010-0100", "(555) 010 0199"];
const NEW_EMAILS: &[&str] = &["new@x.org", "ANN@acme.example", "c.d@e.example"];
const NEW_URLS: &[&str] = &["https://new.example/?q=a=b", "acme.example/x", "https://x.org/?a=b,c"];
/// What an added value is typed as, and the label and value it becomes.
const TYPED_LABELS: &[(&str, &str)] = &[
    ("work", "WORK"),
    ("home", "HOME"),
    ("WhatsApp", "WhatsApp"),
    ("iphone", "IPHONE"),
    ("Home Fax", "HOME FAX"),
];
/// A birthday as typed and as stored.
const BIRTHDAYS: &[(&str, &str)] = &[
    ("1990-5-1", "1990-05-01"),
    ("--02-29", "1604-02-29"),
    ("12-31", "1604-12-31"),
    ("2001-01-09", "2001-01-09"),
];
const MULTIS: [Multi; 3] = [Multi::Phone, Multi::Email, Multi::Url];

fn pool(m: Multi) -> &'static [&'static str] {
    match m {
        Multi::Phone => PHONES,
        Multi::Email => EMAILS,
        Multi::Url => URLS,
    }
}

fn new_pool(m: Multi) -> &'static [&'static str] {
    match m {
        Multi::Phone => NEW_PHONES,
        Multi::Email => NEW_EMAILS,
        Multi::Url => NEW_URLS,
    }
}

fn text(rng: &mut Rng) -> String {
    let mut s = rng.pick(TEXTS).to_string();
    if rng.chance(30) {
        s.push(' ');
        s.push_str(rng.pick(TEXTS).trim());
    }
    s
}

fn entry(rng: &mut Rng, m: Multi) -> Value {
    let mut e = Map::new();
    if let Some(l) = rng.pick(LABELS) {
        e.insert("label".into(), json!(l));
    }
    e.insert("field".into(), json!(rng.pick(pool(m))));
    if rng.chance(15) {
        e.insert("x-extra".into(), json!({ "kept": [1, 2] }));
    }
    Value::Object(e)
}

fn contact(rng: &mut Rng, id: &str) -> Value {
    let mut c = Map::new();
    c.insert("contactId".into(), json!(id));
    for k in ["firstName", "middleName", "lastName", "prefix", "suffix", "nickName"] {
        if rng.chance(45) {
            c.insert(k.into(), json!(text(rng)));
        }
    }
    let company = rng.chance(40);
    for k in ["companyName", "jobTitle", "department"] {
        if company && rng.chance(80) || rng.chance(15) {
            c.insert(k.into(), json!(text(rng)));
        }
    }
    c.insert(
        "isCompany".into(),
        json!(company && c.contains_key("companyName") && rng.chance(50)),
    );
    for m in MULTIS {
        let n = rng.below(5);
        if n == 0 {
            continue;
        }
        let mut list: Vec<Value> = Vec::new();
        for _ in 0..n {
            if !list.is_empty() && rng.chance(15) {
                let dup = rng.pick(&list).clone();
                list.push(dup);
            } else {
                list.push(entry(rng, m));
            }
        }
        if rng.chance(10) {
            // An entry this app cannot read: it must survive every edit.
            list.push(json!({ "label": "WORK", "field": { "structured": true } }));
        }
        c.insert(m.key().into(), Value::Array(list));
    }
    if rng.chance(50) {
        let addresses: Vec<Value> = (0..1 + rng.below(2))
            .map(|_| {
                json!({
                    "label": rng.pick(&["HOME", "WORK", "Cabin"]),
                    "field": {
                        "street": rng.pick(&["1 Main St\nApt 2", "Fjordveien 3", "a;b,c"]),
                        "city": text(rng), "postalCode": "0150", "country": "Norway",
                        "countryCode": "no", "x-extra": true,
                    },
                })
            })
            .collect();
        c.insert("streetAddresses".into(), Value::Array(addresses));
    }
    if rng.chance(50) {
        c.insert(
            "birthday".into(),
            json!(rng.pick(&["1990-05-01", "1604-02-29", "1604-12-31"])),
        );
    }
    if rng.chance(50) {
        c.insert("notes".into(), json!(rng.pick(NOTES)));
    }
    if rng.chance(40) {
        c.insert(
            "photo".into(),
            json!({ "url": "https://p.invalid/a.jpg", "crop": { "x": 0, "y": 0 }, "signature": "s" }),
        );
    }
    if rng.chance(40) {
        c.insert(
            "profiles".into(),
            json!([{ "field": "https://x.example/ann", "user": "ann", "userId": "1" }]),
        );
        c.insert("relatedNames".into(), json!([{ "label": "SISTER", "field": "Bea" }]));
        c.insert(
            "dates".into(),
            json!([{ "label": "ANNIVERSARY", "field": "2010-06-01" }]),
        );
    }
    if rng.chance(40) {
        c.insert("X-UNKNOWN".into(), json!({ "nested": [1, "two", null], "n": 1.5 }));
    }
    Value::Object(c)
}

/// One intended change, as the reference model applies it to the raw card.
#[derive(Debug, Clone)]
enum Op {
    /// Set a field; `None` removes it.
    Set(&'static str, Option<String>),
    /// Remove the `n`th entry stored with exactly this label and value (the
    /// last of them when there are fewer).
    RemoveExact(Multi, String, String, usize),
    /// Remove every entry whose value `Multi::matches` names, any label.
    RemoveMatching(Multi, String),
    /// Add, unless an entry with this label already holds a matching value.
    Add(Multi, String, String),
}

fn label_of(e: &Value) -> &str {
    e["label"].as_str().unwrap_or("")
}

/// `card` with `ops` made, each at most once; one with nothing to do (a
/// value already gone, already there) is skipped.
fn reference(card: &Value, ops: &[Op]) -> Value {
    let mut c = card.as_object().unwrap().clone();
    for op in ops {
        match op {
            Op::Set(k, None) => {
                c.remove(*k);
            }
            Op::Set(k, Some(v)) => {
                c.insert((*k).into(), json!(v));
            }
            Op::RemoveExact(m, ..) | Op::RemoveMatching(m, _) => {
                let gone = |e: &Value| {
                    let Some(field) = e["field"].as_str() else {
                        return false;
                    };
                    match op {
                        Op::RemoveExact(_, l, v, _) => label_of(e) == l && field == v,
                        Op::RemoveMatching(m, q) => m.matches(field, q),
                        _ => unreachable!(),
                    }
                };
                if let Some(list) = c.get_mut(m.key()).and_then(Value::as_array_mut)
                    && list.iter().any(gone)
                {
                    if let Op::RemoveExact(.., n) = op {
                        let at: Vec<usize> = (0..list.len()).filter(|&i| gone(&list[i])).collect();
                        list.remove(*at.get(*n).unwrap_or(at.last().unwrap()));
                    } else {
                        list.retain(|e| !gone(e));
                    }
                    if list.is_empty() {
                        c.remove(m.key());
                    }
                }
            }
            Op::Add(m, l, v) => {
                let list = c.entry(m.key()).or_insert_with(|| json!([]));
                let list = list.as_array_mut().unwrap();
                let has = list
                    .iter()
                    .any(|e| e["field"].as_str().is_some_and(|f| m.matches(f, v)) && label_of(e) == l);
                if !has {
                    // No label: no `label` key.
                    list.push(if l.is_empty() {
                        json!({ "field": v })
                    } else {
                        json!({ "label": l, "field": v })
                    });
                }
            }
        }
    }
    Value::Object(c)
}

/// What the server stamps on every write.
fn without_stamps(v: &Value) -> Value {
    let mut c = v.as_object().unwrap().clone();
    c.remove("etag");
    c.remove("dateModified");
    Value::Object(c)
}

/// Another device's edit of the card as the server holds it.
fn other_device(rng: &mut Rng, card: &Value) -> Value {
    let mut c = card.as_object().unwrap().clone();
    for _ in 0..1 + rng.below(2) {
        match rng.below(6) {
            0 => {
                let k = *rng.pick(&[
                    "firstName",
                    "lastName",
                    "companyName",
                    "jobTitle",
                    "nickName",
                    "middleName",
                ]);
                if rng.chance(25) {
                    c.remove(k);
                } else {
                    c.insert(k.into(), json!(text(rng)));
                }
            }
            1 => {
                let m = *rng.pick(&MULTIS);
                let e = entry(rng, m);
                let list = c.entry(m.key()).or_insert_with(|| json!([]));
                list.as_array_mut().unwrap().push(e);
            }
            2 => {
                let m = *rng.pick(&MULTIS);
                if let Some(list) = c.get_mut(m.key()).and_then(Value::as_array_mut) {
                    let i = rng.below(list.len());
                    list.remove(i);
                    if list.is_empty() {
                        c.remove(m.key());
                    }
                }
            }
            3 => {
                c.insert(
                    "photo".into(),
                    json!({ "url": "https://p.invalid/new.jpg", "signature": "t" }),
                );
            }
            4 => {
                c.insert("X-UNKNOWN".into(), json!({ "changed": "elsewhere" }));
            }
            _ => {
                c.insert("notes".into(), json!(rng.pick(NOTES)));
            }
        }
    }
    Value::Object(c)
}

struct World {
    base: String,
    state: Arc<fake::State>,
}

/// One fake server for the whole file; icloud-session's mock mode in this
/// process points at it (the CLI runs get the same through `command`).
fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(fake::State::default());
        let s = state.clone();
        std::thread::spawn(move || fake::serve_with(listener, s));
        // SAFETY: set once, before this process's first session; std's own
        // environment lock covers its readers.
        unsafe {
            std::env::set_var("ICLOUD_SESSION_MOCK", "1");
            std::env::set_var("ICLOUD_SESSION_MOCK_URL", &base);
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
            std::env::set_var("FAKE_CONTACTS_QUIET", "1");
        }
        World { base, state }
    })
}

/// One CLI `edit`: its arguments and the reference ops.
fn cli_edit(rng: &mut Rng, cached: &Contact) -> (Vec<String>, Vec<Op>) {
    let mut args = Vec::new();
    let mut removes = Vec::new();
    let mut rest = Vec::new();
    // A single-valued flag is given once.
    let mut used = std::collections::HashSet::new();
    for _ in 0..1 + rng.below(4) {
        const SETS: &[(&str, &str)] = &[
            ("first", "firstName"),
            ("middle", "middleName"),
            ("last", "lastName"),
            ("nickname", "nickName"),
            ("company", "companyName"),
            ("job-title", "jobTitle"),
            ("department", "department"),
        ];
        let kind = rng.below(4);
        let set = *rng.pick(SETS);
        let once = match kind {
            0 => set.0,
            1 => "birthday",
            2 => "notes",
            _ => "",
        };
        if !once.is_empty() && !used.insert(once) {
            continue;
        }
        match kind {
            0 => {
                let (flag, key) = set;
                let v = if rng.chance(20) { String::new() } else { text(rng) };
                args.push(format!("--{flag}={v}"));
                let v = v.trim().to_owned();
                rest.push(Op::Set(key, (!v.is_empty()).then_some(v)));
            }
            1 => {
                if rng.chance(20) {
                    args.push("--birthday=".into());
                    rest.push(Op::Set("birthday", None));
                } else {
                    let (typed, stored) = *rng.pick(BIRTHDAYS);
                    args.push(format!("--birthday={typed}"));
                    rest.push(Op::Set("birthday", Some(stored.into())));
                }
            }
            2 => {
                let v = if rng.chance(15) {
                    String::new()
                } else {
                    rng.pick(NOTES).to_string()
                };
                args.push(format!("--notes={v}"));
                rest.push(Op::Set("notes", (!v.is_empty()).then_some(v)));
            }
            _ => {
                let m = *rng.pick(&MULTIS);
                let flag = match m {
                    Multi::Phone => "phone",
                    Multi::Email => "email",
                    Multi::Url => "url",
                };
                let values = cached.values(m);
                if !values.is_empty() && rng.chance(50) {
                    // Remove a stored value, written as stored or another way.
                    let v = rng.pick(&values).value.clone();
                    let q = match m {
                        Multi::Phone if rng.chance(50) => v.chars().filter(char::is_ascii_digit).collect(),
                        Multi::Email if rng.chance(50) => v.to_uppercase(),
                        _ => v,
                    };
                    if q.trim().is_empty() {
                        continue;
                    }
                    args.push(format!("--remove-{flag}={q}"));
                    removes.push(Op::RemoveMatching(m, q));
                } else {
                    let v = *rng.pick(new_pool(m));
                    if rng.chance(50) {
                        let (typed, stored) = *rng.pick(TYPED_LABELS);
                        args.push(format!("--{flag}={typed}={v}"));
                        rest.push(Op::Add(m, stored.into(), v.into()));
                    } else {
                        args.push(format!("--{flag}={v}"));
                        rest.push(Op::Add(m, m.default_label().into(), v.into()));
                    }
                }
            }
        }
    }
    if args.is_empty() {
        let v = rng.pick(NOTES).to_string();
        args.push(format!("--notes={v}"));
        rest.push(Op::Set("notes", Some(v)));
    }
    // The command line makes the removals first.
    removes.extend(rest);
    (args, removes)
}

/// A row of the dialog for one phone, email or URL as shown: the stored
/// value it was filled with (`origin`, its index), its label, its text.
#[derive(Debug, Clone)]
struct Shown {
    origin: Option<usize>,
    label: String,
    text: String,
}

/// One dialog edit: the draft as the window reads it back from its rows,
/// and the ops the person meant.
fn dialog_edit(rng: &mut Rng, before: &Draft) -> (Draft, Vec<Op>) {
    let mut now = before.clone();
    let mut rows: Vec<Vec<Shown>> = before
        .multis
        .iter()
        .map(|(_, values)| {
            values
                .iter()
                .map(|r| Shown {
                    origin: r.origin,
                    label: r.label.clone(),
                    text: r.value.clone(),
                })
                .collect()
        })
        .collect();
    for _ in 0..1 + rng.below(4) {
        match rng.below(3) {
            0 => {
                let i = rng.below(now.scalars.len());
                now.scalars[i].1 = if rng.chance(20) {
                    String::new()
                } else if now.scalars[i].0 == Field::Birthday {
                    rng.pick(BIRTHDAYS).0.to_owned()
                } else {
                    text(rng)
                };
            }
            1 => now.notes = rng.pick(NOTES).to_string(),
            _ => {
                let mi = rng.below(3);
                let m = before.multis[mi].0;
                let rows = &mut rows[mi];
                match rng.below(3) {
                    0 if !rows.is_empty() => {
                        rows.remove(rng.below(rows.len()));
                    }
                    1 if !rows.is_empty() => {
                        let i = rng.below(rows.len());
                        rows[i].text = rng.pick(new_pool(m)).to_string();
                    }
                    _ => {
                        let v = *rng.pick(new_pool(m));
                        let text = if rng.chance(50) {
                            format!("{}={v}", rng.pick(TYPED_LABELS).0)
                        } else {
                            v.to_owned()
                        };
                        let (label, _) = typed(m, None, &text);
                        rows.push(Shown {
                            origin: None,
                            label,
                            text,
                        });
                    }
                }
            }
        }
    }
    let mut ops = Vec::new();
    // Scalars and notes: a field whose text differs (ignoring padding) is set.
    for ((f, v), (_, was)) in now.scalars.iter().zip(&before.scalars) {
        if v.trim() == was.trim() {
            continue;
        }
        let stored = if *f == Field::Birthday {
            BIRTHDAYS
                .iter()
                .find(|(t, _)| *t == v.trim())
                .map_or(String::new(), |(_, s)| s.to_string())
        } else {
            v.trim().to_owned()
        };
        ops.push(Op::Set(f.key(), (!stored.is_empty()).then_some(stored)));
    }
    if now.notes != before.notes {
        ops.push(Op::Set("notes", (!now.notes.is_empty()).then(|| now.notes.clone())));
    }
    // Rows: the stored value of a row removed or retyped goes (that entry,
    // the how-manyth of its exact copies it is; the last first); a retyped
    // or new one is added; an untouched row is no change at all.
    for (mi, rows) in rows.iter().enumerate() {
        let (m, was) = (before.multis[mi].0, &before.multis[mi].1);
        let still = |i: usize| {
            rows.iter()
                .any(|r| r.origin == Some(i) && r.text.trim() == was[i].value.trim())
        };
        for i in (0..was.len()).rev() {
            if !still(i) {
                let w = &was[i];
                let n = was[..i]
                    .iter()
                    .filter(|o| o.label == w.label && o.value == w.value)
                    .count();
                ops.push(Op::RemoveExact(m, w.label.clone(), w.value.clone(), n));
            }
        }
        for r in rows {
            match r.origin {
                Some(i) if r.text.trim() == was[i].value.trim() => {}
                Some(_) => ops.push(Op::Add(m, r.label.clone(), r.text.trim().to_owned())),
                None => {
                    let (l, v) = typed(m, None, &r.text);
                    ops.push(Op::Add(m, l, v));
                }
            }
        }
    }
    // What the window reads back: each row through `typed`, empty ones out.
    for (mi, rows) in rows.iter().enumerate() {
        let m = before.multis[mi].0;
        now.multis[mi].1 = rows
            .iter()
            .map(|r| {
                let (label, value) = match r.origin {
                    Some(_) => typed(m, Some(&r.label), &r.text),
                    None => typed(m, None, &r.text),
                };
                Row {
                    origin: r.origin,
                    label,
                    value,
                }
            })
            .filter(|r| !r.value.is_empty())
            .collect();
    }
    (now, ops)
}

fn names_someone(d: &Draft) -> bool {
    d.scalars.iter().any(|(f, v)| {
        matches!(f, Field::First | Field::Last | Field::Company | Field::Nickname) && !v.trim().is_empty()
    }) || d
        .multis
        .iter()
        .any(|(m, rows)| matches!(m, Multi::Phone | Multi::Email) && !rows.is_empty())
}

/// Each case: a few contacts, then edits through both paths.
fn run_case(seed: u64) {
    let w = world();
    let mut rng = Rng(seed);
    let mut log = vec![format!("seed {seed}")];
    let fail = |log: &[String], msg: String| -> ! { panic!("{msg}\n--- steps ---\n{}", log.join("\n")) };
    let ids: Vec<String> = (0..3).map(|i| format!("S{seed}-C{i}")).collect();
    for id in &ids {
        let c = contact(&mut rng, id);
        log.push(format!("contact {c}"));
        w.state.put(c);
        check_vcard(&w.state.card(id).unwrap(), &log);
    }
    let dir = tempfile::tempdir().unwrap();
    let transport = SessionTransport::default();
    let svc = Service::new(&transport, Store::new(dir.path().join("data")));
    for step in 0..6 {
        svc.sync().unwrap();
        let id = rng.pick(&ids).clone();
        let cached = svc.cache().unwrap().get(&id).unwrap().clone();
        let conflict = rng.chance(35);
        let dialog = rng.chance(50);
        // What the edit will be, from the card as the editor saw it.
        enum Edit {
            Cli(Vec<String>),
            Dialog(Draft, Draft),
        }
        let (edit, ops) = if dialog {
            let before = Draft::of(&cached);
            let (now, ops) = dialog_edit(&mut rng, &before);
            (Edit::Dialog(before, now), ops)
        } else {
            let (args, ops) = cli_edit(&mut rng, &cached);
            (Edit::Cli(args), ops)
        };
        let mut base = w.state.card(&id).unwrap();
        if conflict {
            base = other_device(&mut rng, &base);
            w.state.put(base.clone());
            base = w.state.card(&id).unwrap();
        }
        log.push(format!(
            "step {step}: {} {id}{} ops {ops:?}",
            if dialog { "dialog" } else { "cli" },
            if conflict { " over another device's change" } else { "" }
        ));
        if conflict {
            log.push(format!("  other device: {base}"));
        }
        let expected = reference(&base, &ops);
        let puts = |w: &World, since: usize| {
            w.state
                .requests_since(since)
                .iter()
                .filter(|r| r["query"]["method"] == "PUT" && r["body"]["contacts"][0]["contactId"] == id.as_str())
                .count()
        };
        let since = w.state.request_count();
        let mut wrote = true;
        match edit {
            Edit::Cli(args) => {
                log.push(format!("  args {args:?}"));
                let mut full = vec!["edit".to_owned(), id.clone(), "--cached".into()];
                full.extend(args);
                let out = command(env!("CARGO_BIN_EXE_icloud-contacts"), &w.base, dir.path())
                    .args(&full)
                    .env("FAKE_CONTACTS_QUIET", "1")
                    .output()
                    .unwrap();
                if code(&out) != 0 {
                    fail(
                        &log,
                        format!("edit failed: {} {}", String::from_utf8_lossy(&out.stdout), stderr(&out)),
                    );
                }
                let v = envelope(&out.stdout);
                assert_eq!(v["ok"], true);
            }
            Edit::Dialog(before, now) => {
                log.push(format!("  draft {now:?}"));
                if !names_someone(&now) && !before.middle_name {
                    // Save is disabled: nothing to check.
                    log.push("  (names nobody: not saved)".into());
                    continue;
                }
                if let Some(p) = now.problem(&before) {
                    fail(&log, format!("Save disabled for a valid edit: {p}"));
                }
                let changes = now.changes(&before);
                log.push(format!("  changes {changes:?}"));
                wrote = !changes.is_empty();
                if !changes.is_empty()
                    && let Err(e) = svc.update(&cached, &changes)
                {
                    fail(&log, format!("update failed: {e}"));
                }
            }
        }
        let got = w.state.card(&id).unwrap();
        if without_stamps(&got) != without_stamps(&expected) {
            fail(
                &log,
                format!(
                    "the card iCloud holds is not the edit made once over what it started from\n got:      {}\n expected: {}",
                    without_stamps(&got),
                    without_stamps(&expected)
                ),
            );
        }
        // One write, and one more after a conflict; none for no change.
        let sent = puts(w, since);
        if sent > usize::from(wrote) + usize::from(wrote && conflict) {
            fail(&log, format!("{sent} PUTs"));
        }
        // An edit that wrote (or met a conflict) leaves the card as saved in
        // the cache; one with no changes sends nothing and leaves the cache.
        let cache = Store::new(dir.path().join("data")).cache().unwrap();
        if wrote && cache.get(&id).map(|c| &c.0) != got.as_object() {
            fail(&log, "the cache does not hold the card as saved".into());
        }
        check_vcard(&got, &log);
    }
    // The next cases sync without these.
    for id in &ids {
        w.state.remove(id);
    }
}

#[test]
fn random_edits_change_only_what_they_name() {
    let seeds: Vec<u64> = match std::env::var("MODEL_SEED") {
        Ok(s) => vec![s.parse().unwrap()],
        Err(_) => (0..std::env::var("MODEL_CASES").map_or(40, |n| n.parse().unwrap())).collect(),
    };
    // Each case has its own contacts, cache and transport: they run side by
    // side (every request to the fake takes a few milliseconds).
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                while let Some(&seed) = seeds.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                    run_case(seed);
                }
            });
        }
    });
}

// --- vCard, read back -------------------------------------------------

/// Unescapes a vCard text value.
fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits a structured value at unescaped `;`.
fn parts(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut esc) = (Vec::new(), String::new(), false);
    for c in s.chars() {
        if esc {
            cur.push('\\');
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ';' {
            out.push(unesc(&cur));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    out.push(unesc(&cur));
    out
}

/// `(name, TYPE, value)` of each property.
fn read_vcard(v: &str, log: &[String]) -> Vec<(String, String, String)> {
    let fail = |msg: String| -> ! { panic!("{msg}\n{v:?}\n--- steps ---\n{}", log.join("\n")) };
    if !v.ends_with("\r\n") {
        fail("a vCard ends with CRLF".into());
    }
    let physical: Vec<&str> = v[..v.len() - 2].split("\r\n").collect();
    for l in &physical {
        if l.contains('\r') || l.contains('\n') {
            fail(format!("a bare CR or LF inside a line: {l:?}"));
        }
        if l.len() > 75 {
            fail(format!("a line longer than 75 octets: {l:?}"));
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for l in physical {
        match l.strip_prefix(' ') {
            Some(cont) => lines.last_mut().unwrap().push_str(cont),
            None => lines.push(l.to_owned()),
        }
    }
    lines
        .into_iter()
        .map(|l| {
            let mut quoted = false;
            let colon = l
                .char_indices()
                .find(|&(_, c)| {
                    if c == '"' {
                        quoted = !quoted;
                    }
                    c == ':' && !quoted
                })
                .map(|(i, _)| i)
                .unwrap_or_else(|| fail(format!("no colon in {l:?}")));
            let (head, value) = (&l[..colon], &l[colon + 1..]);
            let mut params = head.splitn(2, ';');
            let name = params.next().unwrap().to_owned();
            let kind = params
                .next()
                .and_then(|p| p.strip_prefix("TYPE="))
                .unwrap_or("")
                .trim_start_matches("INTERNET,")
                .to_owned();
            (name, kind, value.to_owned())
        })
        .collect()
}

/// The `TYPE` a stored label is exported as.
fn kind_of(label: &str) -> String {
    match label {
        "MOBILE" | "IPHONE" => "CELL".into(),
        "" => "OTHER".into(),
        l if l.chars().all(|c| c.is_ascii_uppercase()) => l.into(),
        l => format!("\"{}\"", l.replace('"', "'")),
    }
}

fn check_vcard(card: &Value, log: &[String]) {
    let c = Contact(card.as_object().unwrap().clone());
    let v = vcard::card(&c);
    let props = read_vcard(&v, log);
    let fail = |msg: String| -> ! { panic!("{msg}\n{v}\n--- steps ---\n{}", log.join("\n")) };
    let all = |name: &str| props.iter().filter(|(n, ..)| n == name).collect::<Vec<_>>();
    let one = |name: &str| -> Option<String> {
        let found = all(name);
        if found.len() > 1 {
            fail(format!("{name} more than once"));
        }
        found.first().map(|(_, _, v)| v.clone())
    };
    let text = |s: &str| s.replace("\r\n", "\n").replace('\r', "\n");
    let n = parts(&one("N").unwrap_or_else(|| fail("no N".into())));
    let want: Vec<String> = ["lastName", "firstName", "middleName", "prefix", "suffix"]
        .iter()
        .map(|k| text(c.get(k)))
        .collect();
    if n != want {
        fail(format!("N {n:?}, not {want:?}"));
    }
    if one("FN").map(|s| unesc(&s)) != Some(text(&c.display_name())) {
        fail("FN is not the shown name".into());
    }
    for (prop, f) in [
        ("NICKNAME", Field::Nickname),
        ("TITLE", Field::JobTitle),
        ("NOTE", Field::Notes),
    ] {
        let want = Some(text(c.field(f))).filter(|s| !s.is_empty());
        if one(prop).map(|s| unesc(&s)) != want {
            fail(format!("{prop} is not {want:?}"));
        }
    }
    let (company, dept) = (c.field(Field::Company), c.field(Field::Department));
    let org = one("ORG").map(|s| parts(&s));
    let want = (!company.is_empty() || !dept.is_empty()).then(|| vec![text(company), text(dept)]);
    if org != want {
        fail(format!("ORG {org:?}, not {want:?}"));
    }
    for (prop, m) in [("TEL", Multi::Phone), ("EMAIL", Multi::Email), ("URL", Multi::Url)] {
        let got: Vec<(String, String)> = all(prop).iter().map(|(_, k, v)| (k.clone(), unesc(v))).collect();
        let want: Vec<(String, String)> = c
            .values(m)
            .iter()
            .map(|l| (kind_of(&l.label), text(&l.value)))
            .collect();
        if got != want {
            fail(format!("{prop} {got:?}, not {want:?}"));
        }
    }
    let got: Vec<Vec<String>> = all("ADR").iter().map(|(_, _, v)| parts(v)).collect();
    let want: Vec<Vec<String>> = c
        .addresses()
        .iter()
        .map(|a| {
            ["", "", &a.street, &a.city, &a.state, &a.postal_code, &a.country]
                .map(text)
                .to_vec()
        })
        .collect();
    if got != want {
        fail(format!("ADR {got:?}, not {want:?}"));
    }
    let bday = c.field(Field::Birthday);
    let want = (!bday.is_empty()).then(|| match bday.strip_prefix("1604-") {
        Some(md) => format!("--{md}"),
        None => bday.to_owned(),
    });
    if one("BDAY") != want {
        fail(format!("BDAY is not {want:?}"));
    }
    if one("UID").map(|s| unesc(&s)).as_deref() != Some(c.id()) {
        fail("UID is not the contactId".into());
    }
}
