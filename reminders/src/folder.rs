//! The reminders as Markdown checklists in a folder, one file per list, kept
//! in step with iCloud both ways by the background timer. Nothing turns it
//! on: it is `~/Documents/Reminders`, written after the first sync.
//!
//! The folder is a working copy: iCloud holds the reminders, and this is the
//! only code that reads the files. It exists so that something that only
//! carries files (Syncthing to a phone, an editor, an agent) can read and
//! change reminders. A file looks like this:
//!
//! ```text
//! # Work
//!
//! - [ ] Send the quote 📅 2026-11-18 09:00
//!     Notes are the indented lines under a reminder.
//!     - [ ] A subtask
//! - [x] Submit the sitemap
//! ```
//!
//! Ticking a box completes the reminder, a new line adds one (a bare line of
//! text will do), a removed line deletes one (to Apple's Recently Deleted),
//! and the text after 📅 is anything `--due` takes. Open reminders come in
//! the list's own order, then the ones completed in the last week; a
//! reorder made in the file is not kept.
//!
//! A line carries no ID. What ties a line to its reminder is [`ListState`]:
//! the items as the file last had them, each with its reminder's ID. A
//! changed file is compared with that ([`diff`]): the same title is the same
//! reminder, then the closest remaining titles pair up (a retitle), and what
//! is left over was added or removed. A line rewritten beyond recognition
//! therefore reads as one removed and one added.
//!
//! What keeps a bad file from costing reminders:
//!
//! - More than a few removed lines that are also most of the list ([`risky`])
//!   delete nothing: the lines are written back and a notification says so.
//!   A cut-off or emptied file looks like this. To delete that many, use the
//!   app, or remove a few lines at a time.
//! - A file seen for the first time (not yet [`ListState::adopted`]) only
//!   ever adds: its other lines are written over from iCloud.
//! - Syncthing's conflict copies (`Work.sync-conflict-….md`) are made from an
//!   older file, so only what cannot undo anything is taken from one: a tick
//!   on a reminder that is still open, and lines unlike any there is.
//! - A write that fails stops that list. Each write (to iCloud, of a file,
//!   a file's rename) is recorded in `folder.json` as soon as it is made, so
//!   the next run goes on from there instead of adding twice or reading
//!   iCloud's changes as the file's, also after a run that was killed. A line
//!   to add is recorded with the ID its reminder will have before it is sent
//!   ([`ListState::pending`]), so one that reached iCloud without the run
//!   hearing so (killed, or the answer lost) is that line's reminder at the
//!   next run, and one that did not is sent again. Nothing else ties a new
//!   line to a reminder: one made on the phone that reads the same is
//!   another.
//! - What a conflict copy added is in iCloud before it is in the file. Until
//!   the file is written with it ([`ListState::unwritten`]), a changed file
//!   that lacks it does not delete it, and a tick taken from a copy is not
//!   undone by a file that still shows the box empty.
//! - A list whose run failed and changed nothing is tried again after the
//!   next sync or when its file changes, not every minute.
//! - What a line cannot say (alerts, priority, flags, the due time's zone)
//!   is never written, so it stays as it is in iCloud.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Zoned;
use serde::{Deserialize, Serialize};

use crate::cloudkit::{Error, Result};
use crate::due;
use crate::model::{Change, List, Reminder};
use crate::service::Service;
use crate::store::Cache;

/// How long a completed reminder stays in its file.
pub const KEEP_DONE_MS: i64 = 7 * 24 * 3600 * 1000;
const DUE_MARK: &str = "📅";
const UNTITLED: &str = "(untitled)";
/// Titles at least this alike are one reminder, retitled.
const ALIKE: f64 = 0.5;

/// One line of a file, with the indented lines under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub done: bool,
    pub title: String,
    /// As [`due::Due::display`] writes it: a date, or a date and time.
    pub due: Option<String>,
    pub notes: String,
}

/// An item of a file and the reminder it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub id: String,
    #[serde(flatten)]
    pub item: Item,
}

/// A list's file as this computer last left it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListState {
    pub file: String,
    /// The file's text when it was last written or read in full.
    pub text: String,
    pub items: Vec<Known>,
    /// False until a run has finished on this file: until then it may be a
    /// file from elsewhere, compared with iCloud as it is, and only adds.
    #[serde(default)]
    pub adopted: bool,
    /// Reminders among `items` that `text` never held: added from a conflict
    /// copy, and waiting for the file to be written.
    #[serde(default)]
    pub unwritten: Vec<String>,
    /// Set by a run on this list that failed and changed nothing.
    #[serde(default)]
    pub stuck: Option<Stuck>,
    /// Lines being added: saved before each is sent, removed once its
    /// reminder is among `items`.
    #[serde(default)]
    pub pending: Vec<Pending>,
}

/// A line sent to iCloud as `Reminder/<uuid>`, whether it got there or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub uuid: String,
    #[serde(flatten)]
    pub item: Item,
    /// From a conflict copy, so not yet in the file.
    #[serde(default)]
    pub copy: bool,
}

/// A list's file and the sync it was read against when its run failed: the
/// same again would fail the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stuck {
    /// The file's text; `None` when it was missing or unreadable.
    pub text: Option<String>,
    pub synced_ms: Option<i64>,
}

impl ListState {
    /// Still as it was when its run failed.
    fn stuck_on(&self, text: &Option<String>) -> bool {
        self.stuck.as_ref().is_some_and(|s| s.text == *text)
    }
}

/// `folder.json` in the data directory: the folder's files. The folder
/// itself is always [`default_dir`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// By list ID.
    #[serde(default)]
    pub lists: BTreeMap<String, ListState>,
    /// Files of lists gone from iCloud that were left in the folder because
    /// they held changes never sent. No list's file takes such a name while
    /// the file is there: a new list of that name would read it as its own.
    #[serde(default)]
    pub left: Vec<String>,
}

/// A file that would have removed most of its list: its lines were put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub list: String,
    pub file: String,
    /// How many reminders it no longer listed.
    pub kept: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub added: usize,
    pub edited: usize,
    pub deleted: usize,
    /// Files written, by name.
    pub written: Vec<String>,
    pub restored: Vec<Restored>,
    pub warnings: Vec<String>,
}

impl Report {
    /// Nothing was written either way.
    pub fn quiet(&self) -> bool {
        self.added + self.edited + self.deleted == 0 && self.written.is_empty() && self.warnings.is_empty()
    }
}

/// What a file asks for, against what it last held.
#[derive(Debug, Default, PartialEq)]
pub struct Delta {
    pub adds: Vec<Item>,
    /// The reminder, the item as the file has it now, and what changed.
    pub updates: Vec<(String, Item, Vec<Change>)>,
    pub deletes: Vec<String>,
}

/// `[ ] text` or `[x] text`, with or without a bullet before it.
fn checkbox(body: &str) -> Option<(bool, &str)> {
    let rest = bullet(body).unwrap_or(body);
    let done = match rest.get(..3)? {
        "[ ]" => false,
        "[x]" | "[X]" => true,
        _ => return None,
    };
    Some((done, rest[3..].trim()))
}

/// The text after a `- `, `* ` or `+ `.
fn bullet(body: &str) -> Option<&str> {
    let rest = body.strip_prefix(['-', '*', '+'])?;
    rest.starts_with([' ', '\t']).then(|| rest.trim_start())
}

/// The title and the due date after the last 📅 that reads as one. A
/// backslash before that 📅 makes it the title's own ([`escaped`]).
fn split_due(text: &str, now: &Zoned) -> (String, Option<String>) {
    if let Some(at) = text.rfind(DUE_MARK) {
        if let Some(before) = text[..at].strip_suffix('\\') {
            return (format!("{before}{}", &text[at..]), None);
        }
        if let Ok(d) = due::parse_when(&text[at + DUE_MARK.len()..], now) {
            return (text[..at].trim_end().to_owned(), Some(d.display(now.time_zone())));
        }
    }
    (text.to_owned(), None)
}

/// A title as a line with no due date holds it: with a backslash before its
/// last 📅 when the line would otherwise read as another title ("Call 📅
/// tomorrow" as "Call", due tomorrow).
fn escaped(title: &str, now: &Zoned) -> String {
    match title.rfind(DUE_MARK) {
        Some(at) if split_due(title, now).0 != title => format!("{}\\{}", &title[..at], &title[at..]),
        _ => title.to_owned(),
    }
}

/// A Markdown heading: hashes, then a space or nothing. `#hashtag` is text.
fn heading(body: &str) -> bool {
    let rest = body.trim_start_matches('#');
    rest.len() < body.len() && (rest.is_empty() || rest.starts_with([' ', '\t']))
}

fn item(done: bool, text: &str, now: &Zoned) -> Option<Item> {
    let (title, due) = split_due(text.trim(), now);
    (!title.is_empty()).then_some(Item {
        done,
        title,
        due,
        notes: String::new(),
    })
}

/// The items of a file. Headings and blank lines are layout; any other line
/// at the margin is a reminder, checkbox or not; an indented checkbox is a
/// subtask and any other indented line is a line of notes.
pub fn parse(text: &str, now: &Zoned) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut blanks = 0;
    // Some editors start a file with a byte order mark.
    for line in text.trim_start_matches('\u{feff}').lines() {
        let body = line.trim_start();
        if body.trim_end().is_empty() {
            blanks += 1;
            continue;
        }
        let body = body.trim_end();
        let indent: usize = line
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        if indent < 2 || items.is_empty() {
            blanks = 0;
            if heading(body) {
                continue;
            }
            let (done, text) = checkbox(body).unwrap_or((false, bullet(body).unwrap_or(body)));
            items.extend(item(done, text, now));
        } else if let Some((done, text)) = checkbox(body) {
            blanks = 0;
            items.extend(item(done, text, now));
        } else {
            let last = items.last_mut().expect("checked above");
            // A note that reads as a checkbox is written with a backslash.
            let text = body
                .strip_prefix('\\')
                .filter(|r| checkbox(r).is_some())
                .unwrap_or(body);
            if !last.notes.is_empty() {
                last.notes.push_str(&"\n".repeat(blanks + 1));
            }
            last.notes.push_str(text);
            blanks = 0;
        }
    }
    items
}

fn write_item(out: &mut String, r: &Reminder, sub: bool, now: &Zoned) {
    let pad = if sub { "    " } else { "" };
    let title = r.title.replace(['\n', '\r'], " ");
    let title = match title.trim() {
        "" => UNTITLED,
        t => t,
    };
    let done = if r.completed { 'x' } else { ' ' };
    match &r.due {
        Some(d) => out.push_str(&format!(
            "{pad}- [{done}] {title} {DUE_MARK} {}",
            d.display(now.time_zone())
        )),
        None => out.push_str(&format!("{pad}- [{done}] {}", escaped(title, now))),
    }
    out.push('\n');
    for line in r.notes.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            out.push('\n');
        } else {
            let escape = if checkbox(line).is_some() { "\\" } else { "" };
            out.push_str(&format!("{pad}    {escape}{line}\n"));
        }
    }
}

/// The reminder `r` is written under: its parent, when that is in the file.
fn under<'r>(r: &'r Reminder, all: &[&Reminder]) -> Option<&'r str> {
    r.parent_id.as_deref().filter(|p| all.iter().any(|o| o.id == *p))
}

/// A list's file, and the reminder each of its items is, in order.
pub fn render(list: &List, cache: &Cache, now: &Zoned) -> (String, Vec<String>) {
    let now_ms = now.timestamp().as_millisecond();
    let shown = |r: &Reminder| !r.completed || r.completed_ms.is_some_and(|ms| now_ms - ms < KEEP_DONE_MS);
    let position = |r: &Reminder| list.order.iter().position(|id| id == r.uuid()).unwrap_or(usize::MAX);
    // Open ones in the list's order, then completed ones, newest first.
    let key = |r: &Reminder| {
        (
            r.completed,
            if r.completed { -r.completed_ms.unwrap_or(0) } else { 0 },
            position(r),
            r.created_ms,
            r.id.clone(),
        )
    };
    let mut all: Vec<&Reminder> = cache
        .reminders
        .values()
        .filter(|r| r.list_id == list.id && shown(r))
        .collect();
    all.sort_by_key(|r| key(r));

    let mut text = format!("# {}\n", list.name.replace(['\n', '\r'], " "));
    let mut ids = Vec::new();
    for r in all.iter().filter(|r| under(r, &all).is_none()) {
        if ids.is_empty() {
            text.push('\n');
        }
        write_item(&mut text, r, false, now);
        ids.push(r.id.clone());
        for child in all.iter().filter(|c| under(c, &all) == Some(r.id.as_str())) {
            write_item(&mut text, child, true, now);
            ids.push(child.id.clone());
        }
    }
    (text, ids)
}

/// The items of a file this program wrote, each with its reminder.
fn known(text: &str, ids: Vec<String>, now: &Zoned) -> std::result::Result<Vec<Known>, String> {
    let items = parse(text, now);
    if items.len() != ids.len() {
        return Err(format!(
            "wrote {} reminders and read back {}; the file is left as it is",
            ids.len(),
            items.len()
        ));
    }
    Ok(ids
        .into_iter()
        .zip(items)
        .map(|(id, item)| Known { id, item })
        .collect())
}

fn fold(s: &str) -> Vec<char> {
    s.to_lowercase().chars().collect()
}

/// 1.0 for the same text, 0.0 for nothing in common, case aside: edit
/// distance over the longer length, and never under 0.5 when one holds the
/// other whole.
fn alike(a: &str, b: &str) -> f64 {
    let (a, b) = (fold(a), fold(b));
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    // Words added to a title, or taken from it, leave it the same reminder.
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

/// Which item of `base` each item of `local` is, if any: the same title
/// first (in order, wherever it moved), then the most alike of the rest.
/// Lines with little in common stay apart: pairing them would hand one
/// reminder's alerts to another.
fn pair(base: &[Known], local: &[Item]) -> Vec<Option<usize>> {
    let mut of_local: Vec<Option<usize>> = vec![None; local.len()];
    let mut taken = vec![false; base.len()];
    for (l, it) in local.iter().enumerate() {
        let same: Vec<usize> = (0..base.len())
            .filter(|&b| !taken[b] && base[b].item.title == it.title)
            .collect();
        // Fewer lines of a title than there were: among those this line can
        // be (leaving one for each such line after it), it is the one it
        // still reads as, so the reminder whose line was removed is the one
        // that goes.
        let later = local[l + 1..].iter().filter(|o| o.title == it.title).count();
        let choices = &same[..same.len().saturating_sub(later).max(1).min(same.len())];
        if let Some(&b) = choices.iter().find(|&&b| base[b].item == *it).or(choices.first()) {
            taken[b] = true;
            of_local[l] = Some(b);
        }
    }
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    for (l, it) in local.iter().enumerate().filter(|(l, _)| of_local[*l].is_none()) {
        for (b, k) in base.iter().enumerate().filter(|(b, _)| !taken[*b]) {
            let score = alike(&k.item.title, &it.title);
            if score >= ALIKE {
                candidates.push((score, l, b));
            }
        }
    }
    candidates.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    for (_, l, b) in candidates {
        if of_local[l].is_none() && !taken[b] {
            taken[b] = true;
            of_local[l] = Some(b);
        }
    }
    of_local
}

/// What the file's items ask for, against the items it last held.
pub fn diff(base: &[Known], local: &[Item], now: &Zoned) -> Delta {
    let pairs = pair(base, local);
    let mut delta = Delta::default();
    let mut seen = vec![false; base.len()];
    for (it, b) in local.iter().zip(pairs) {
        let Some(b) = b else {
            delta.adds.push(it.clone());
            continue;
        };
        seen[b] = true;
        let was = &base[b].item;
        let mut changes = Vec::new();
        if it.title != was.title {
            changes.push(Change::Title(it.title.clone()));
        }
        if it.notes != was.notes {
            changes.push(Change::Notes(it.notes.clone()));
        }
        if it.done != was.done {
            changes.push(Change::Completed(it.done));
        }
        if it.due != was.due {
            // The text is what `display` wrote, so it parses.
            match &it.due {
                None => changes.push(Change::Due(None)),
                Some(text) => changes.extend(due::parse_when(text, now).ok().map(|d| Change::Due(Some(d)))),
            }
        }
        if !changes.is_empty() {
            delta.updates.push((base[b].id.clone(), it.clone(), changes));
        }
    }
    delta.deletes = base
        .iter()
        .zip(seen)
        .filter(|(_, seen)| !seen)
        .map(|(k, _)| k.id.clone())
        .collect();
    delta
}

/// More than a few removed lines that are also most of the list: a cut-off
/// or emptied file looks like this, and so its removals are not applied.
pub fn risky(delta: &Delta, base_len: usize) -> bool {
    delta.deletes.len() > 3 && delta.deletes.len() * 2 > base_len
}

/// What a conflict copy may ask for. It was made from an older file, so a
/// difference from the items now is as likely stale as meant: only a tick on
/// a reminder still open, and lines unlike any there is, are taken.
fn from_conflict_copy(base: &[Known], copy: &[Item]) -> Delta {
    let mut delta = Delta::default();
    let mut taken = vec![false; base.len()];
    for it in copy {
        // Of the items of its title not yet taken, the one it reads as:
        // its twin's tick is not its own.
        let same: Vec<usize> = (0..base.len())
            .filter(|&b| !taken[b] && base[b].item.title == it.title)
            .collect();
        let found = same
            .iter()
            .find(|&&b| base[b].item == *it)
            .or_else(|| same.iter().find(|&&b| base[b].item.done == it.done))
            .or(same.first())
            .map(|&b| {
                taken[b] = true;
                &base[b]
            });
        match found {
            Some(k) if it.done && !k.item.done => {
                let mut now_done = k.item.clone();
                now_done.done = true;
                delta
                    .updates
                    .push((k.id.clone(), now_done, vec![Change::Completed(true)]));
            }
            Some(_) => {}
            None if base.iter().all(|k| alike(&k.item.title, &it.title) < ALIKE)
                && !delta.adds.iter().any(|a| a.title == it.title) =>
            {
                delta.adds.push(it.clone())
            }
            None => {}
        }
    }
    delta
}

/// A file name for a list: its name, without what a path cannot hold.
fn file_stem(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    match clean.trim().trim_start_matches('.') {
        "" => "List".to_owned(),
        s => s.to_owned(),
    }
}

/// `Work.md`, or `Work 2.md` when another list has that name.
fn free_name(stem: &str, taken: &[String]) -> String {
    let used = |name: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(name));
    let mut name = format!("{stem}.md");
    let mut n = 2;
    while used(&name) {
        name = format!("{stem} {n}.md");
        n += 1;
    }
    name
}

fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Replaces the file whole: a temp file beside it, then a rename. The temp
/// file has the name Syncthing gives its own temporaries
/// (`.syncthing.<name>.tmp`), which it does not sync. Its data is on disk
/// before the rename, as `folder.json`'s is: a power cut must not leave an
/// empty file that `folder.json` says was written, read as every line removed.
fn write(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("list");
    let tmp = path.with_file_name(format!(".syncthing.{name}.tmp"));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// What the names of `file`'s conflict copies start with.
fn conflict_prefix(file: &str) -> String {
    format!("{}.sync-conflict-", file.strip_suffix(".md").unwrap_or(file))
}

/// Syncthing's conflict copies of `file`: `Work.sync-conflict-<when>-<device>.md`.
fn conflict_copies(dir: &Path, file: &str) -> Vec<PathBuf> {
    let prefix = conflict_prefix(file);
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".md"))
        })
        .collect();
    found.sort();
    found
}

/// `~/Documents/Reminders`.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents/Reminders"))
}

/// Whether a file has something to send: it differs from what was last
/// written, or has a conflict copy. A background run syncs with iCloud first
/// when this is so. A file whose run failed is not, until it changes: one bad
/// list does not make every minute's run a sync.
pub fn pending(state: &State, dir: &Path) -> bool {
    state.lists.values().any(|ls| {
        let text = read(&dir.join(&ls.file)).ok().flatten();
        !ls.stuck_on(&text) && (text.is_some_and(|text| text != ls.text) || !conflict_copies(dir, &ls.file).is_empty())
    })
}

/// Records a list's state in `folder.json`.
type Save<'a> = &'a mut dyn FnMut(&ListState) -> Result<()>;

/// Where a delta is from: the list's file, or a conflict copy of it.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    File,
    Copy,
}

/// What a run on one list works with.
#[derive(Clone, Copy)]
struct Run<'a, 's> {
    svc: &'a Service<'s>,
    list: &'a List,
    now: &'a Zoned,
}

/// Writes a delta to iCloud, keeping `ls.items` as the file's items with
/// their reminders as far as the writes got, saved after each.
fn apply(run: Run, delta: Delta, from: Source, ls: &mut ListState, report: &mut Report, save: Save) -> Result<()> {
    let Run { svc, list, now } = run;
    // Read once: a change tag gone stale meanwhile is `Service::update`'s
    // conflict to retry.
    let cache = svc.cache()?;
    for (id, it, changes) in delta.updates {
        // Gone in iCloud meanwhile: the line goes with the next write. A
        // copy's tick already given (by a run that then failed) is not
        // given again.
        if let Some(r) = cache
            .reminders
            .get(&id)
            .filter(|r| from == Source::File || !r.completed)
        {
            if let Some(w) = svc.update(r, &changes)?.warning {
                report.warnings.push(w);
            }
            report.edited += 1;
        }
        // A copy's tick is not the file's: the item stays as the file has
        // it, so the file is not read as unticking it.
        if from == Source::File
            && let Some(k) = ls.items.iter_mut().find(|k| k.id == id)
        {
            k.item = it;
            save(ls)?;
        }
    }
    for it in delta.adds {
        // Recorded before it is sent: a run that never hears back (killed,
        // or the answer lost) leaves the next one to look the ID up.
        let uuid = crate::service::new_uuid();
        ls.pending.push(Pending {
            uuid: uuid.clone(),
            item: it.clone(),
            copy: from == Source::Copy,
        });
        save(ls)?;
        let when = it.due.as_deref().and_then(|d| due::parse_when(d, now).ok());
        let saved = svc.add_as(&uuid, &list.id, &it.title, &it.notes, when.as_ref())?;
        report.warnings.extend(saved.warning);
        report.added += 1;
        let made = saved.value;
        // Known from here on, so a failure below does not add it again.
        known_now(ls, &uuid, &made);
        save(ls)?;
        if it.done && !made.completed {
            svc.update(&made, &[Change::Completed(true)])?;
            ls.items.last_mut().expect("just pushed").item.done = true;
            save(ls)?;
        }
    }
    for id in delta.deletes {
        if let Some(r) = cache.reminders.get(&id) {
            svc.update(r, &[Change::Deleted])?;
            report.deleted += 1;
        }
        ls.items.retain(|k| k.id != id);
        save(ls)?;
    }
    Ok(())
}

/// A pending line's reminder `made`, now among the items.
fn known_now(ls: &mut ListState, uuid: &str, made: &Reminder) {
    let Some(at) = ls.pending.iter().position(|p| p.uuid == uuid) else {
        return;
    };
    let p = ls.pending.remove(at);
    ls.items.push(Known {
        id: made.id.clone(),
        item: Item {
            done: made.completed,
            ..p.item
        },
    });
    if p.copy {
        ls.unwritten.push(made.id.clone());
    }
}

/// One list: the file's changes to iCloud, then iCloud's state to the file.
fn one(run: Run, dir: &Path, ls: &mut ListState, report: &mut Report, save: Save) -> Result<()> {
    let Run { svc, list, now } = run;
    let path = dir.join(&ls.file);
    let on_disk = read(&path)?;
    // Not ours yet: a file already there (from another computer, or from
    // before the data directory was cleared) is compared with iCloud as it is, and only adds.
    let first = !ls.adopted;
    if first {
        let (text, ids) = render(list, &svc.cache()?, now);
        ls.items = known(&text, ids, now).map_err(Error::Other)?;
        ls.unwritten.clear();
        // Those that got there are among the items already.
        ls.pending.clear();
    }
    // Lines a run sent without hearing back: the reminder iCloud has is
    // that line's; one it lacks never got there, and the line is sent
    // again (the file, or the copy, still has it).
    if !ls.pending.is_empty() {
        let cache = svc.cache()?;
        for p in std::mem::take(&mut ls.pending) {
            let id = format!("Reminder/{}", p.uuid);
            if let Some(made) = cache.reminders.get(&id) {
                ls.pending.push(p.clone());
                known_now(ls, &p.uuid, made);
            }
        }
        save(ls)?;
    }

    // Before the file: a copy is compared with the file as last written, so
    // a box it shows ticked that the file has just unticked is not news.
    let mut unreadable = None;
    for copy in conflict_copies(dir, &ls.file) {
        let text = match read(&copy) {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            // The run fails once the file's own changes are in.
            Err(e) => {
                unreadable.get_or_insert(e);
                continue;
            }
        };
        let delta = from_conflict_copy(&ls.items, &parse(&text, now));
        apply(run, delta, Source::Copy, ls, report, save)?;
        fs::remove_file(&copy)?;
    }

    // What the file holds is recorded as the writes are made: the next run
    // compares against that, so a write that failed is tried again and one
    // that worked is not.
    if let Some(text) = on_disk.as_ref().filter(|text| **text != ls.text) {
        let mut delta = diff(&ls.items, &parse(text, now), now);
        if first {
            // Nobody knows how old it is: what it lacks is not removed, and
            // a line that differs from its reminder does not change it.
            delta.deletes.clear();
            delta.updates.clear();
        }
        // A conflict copy's, which the file has yet to be written with: not
        // lines removed from it.
        delta.deletes.retain(|id| !ls.unwritten.contains(id));
        // Most of the list as the file last held it: `items` has grown by
        // what a run killed after its adds made, and by a copy's adds.
        if risky(&delta, parse(&ls.text, now).len()) {
            // Too much of the list to take from a file: the lines are
            // written back below, with the rest of what it asks applied.
            report.restored.push(Restored {
                list: list.name.clone(),
                file: ls.file.clone(),
                kept: delta.deletes.len(),
            });
            delta.deletes.clear();
        }
        apply(run, delta, Source::File, ls, report, save)?;
        ls.text = text.clone();
        save(ls)?;
    }
    if let Some(e) = unreadable {
        return Err(e.into());
    }

    let cache = svc.cache()?;
    let Some(list) = cache.list(&list.id) else {
        return Ok(());
    };
    let (text, ids) = render(list, &cache, now);
    let items = known(&text, ids, now).map_err(Error::Other)?;
    let current = read(&path)?;
    let written = current.as_deref() != Some(text.as_str());
    if written {
        if current != on_disk {
            // The file changed while this ran: its changes are for the next
            // run, against the file as it was read here.
            return Ok(());
        }
        write(&path, &text)?;
        report.written.push(ls.file.clone());
    }
    ls.text = text;
    ls.items = items;
    ls.unwritten.clear();
    ls.adopted = true;
    // Recorded at once: a run killed later on would leave the next one to
    // read iCloud's changes in this file as the file's own.
    if written { save(ls) } else { Ok(()) }
}

/// Brings the folder and iCloud in step. The cache must be fresh (the
/// caller syncs first): what it holds is taken as iCloud's state. One list's failure is a warning
/// and does not stop the others. `state` is saved after each write to iCloud;
/// the caller saves it once more when this returns.
pub fn sync(svc: &Service, dir: &Path, state: &mut State, now: &Zoned) -> Result<Report> {
    let mut report = Report::default();
    let cache = svc.cache()?;
    if cache.synced_ms.is_none() {
        return Ok(report);
    }
    fs::create_dir_all(dir)?;

    // A list gone from iCloud: its file goes too, unless it was changed.
    state.left.retain(|file| dir.join(file).exists());
    let gone: Vec<String> = state
        .lists
        .keys()
        .filter(|id| cache.list(id).is_none())
        .cloned()
        .collect();
    for id in gone {
        let ls = state.lists.remove(&id).expect("listed above");
        let path = dir.join(&ls.file);
        let copies = conflict_copies(dir, &ls.file);
        let names: Vec<String> = copies
            .iter()
            .filter_map(|c| c.file_name()?.to_str().map(str::to_owned))
            .collect();
        match read(&path)? {
            Some(text) if text != ls.text => {
                let beside = match names.as_slice() {
                    [] => String::new(),
                    names => format!(" (and beside it {})", names.join(", ")),
                };
                report.warnings.push(format!(
                    "{}: its list is gone from iCloud; the file has changes that were never sent, so it is left here{beside} and no longer kept in step",
                    ls.file
                ));
                state.left.push(ls.file);
            }
            file => {
                if file.is_some() {
                    fs::remove_file(&path)?;
                }
                // Nothing can take them in now: the list and its reminders
                // are gone, and a new list of its name would read them as
                // its own.
                for (copy, name) in copies.iter().zip(&names) {
                    fs::remove_file(copy)?;
                    report.warnings.push(format!(
                        "{name}: its list is gone from iCloud, so this conflict copy was removed unread"
                    ));
                }
            }
        }
    }

    for list in &cache.lists {
        let others: Vec<String> = state
            .lists
            .iter()
            .filter(|(id, _)| **id != list.id)
            .map(|(_, ls)| ls.file.clone())
            .chain(state.left.iter().cloned())
            .collect();
        let wanted = free_name(&file_stem(&list.name), &others);
        let mut ls = state.lists.get(&list.id).cloned().unwrap_or_default();
        if ls.file.is_empty() {
            ls.file = wanted;
        } else if ls.file != wanted {
            // The list was renamed: the file follows, when nothing is in the way.
            let (from, to) = (dir.join(&ls.file), dir.join(&wanted));
            if !to.exists() && (!from.exists() || fs::rename(&from, &to).is_ok()) {
                // Its conflict copies too: they are found by the file's name.
                let (old, new) = (conflict_prefix(&ls.file), conflict_prefix(&wanted));
                for copy in conflict_copies(dir, &ls.file) {
                    let rest = copy.file_name().and_then(|n| n.to_str()).map(|n| &n[old.len()..]);
                    if let Some(to) = rest.map(|rest| dir.join(format!("{new}{rest}")))
                        && !to.exists()
                    {
                        let _ = fs::rename(&copy, &to);
                    }
                }
                ls.file = wanted;
                // Recorded at once: the next run would otherwise look for
                // the file under its old name, and write it there afresh.
                state.lists.insert(list.id.clone(), ls.clone());
                svc.store.save_folder(state)?;
            }
        }
        // Failed on this file and this sync, and neither is new: not again.
        let waits = ls.stuck.as_ref().is_some_and(|s| s.synced_ms == cache.synced_ms)
            && ls.stuck_on(&read(&dir.join(&ls.file)).ok().flatten());
        if !waits {
            let before = (ls.items.clone(), ls.text.clone());
            let result = one(Run { svc, list, now }, dir, &mut ls, &mut report, &mut |ls| {
                state.lists.insert(list.id.clone(), ls.clone());
                Ok(svc.store.save_folder(state)?)
            });
            ls.stuck = match result {
                Ok(()) => None,
                Err(e) => {
                    report.warnings.push(format!("{}: {e}", ls.file));
                    // One that got somewhere is tried again the next minute.
                    (before == (ls.items.clone(), ls.text.clone())).then(|| Stuck {
                        text: read(&dir.join(&ls.file)).ok().flatten(),
                        synced_ms: cache.synced_ms,
                    })
                }
            };
        }
        state.lists.insert(list.id.clone(), ls);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn now() -> Zoned {
        "2026-11-14T08:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .to_zoned(TimeZone::UTC)
    }

    fn reminder(uuid: &str, title: &str) -> Reminder {
        Reminder {
            id: format!("Reminder/{uuid}"),
            list_id: "List/W".into(),
            title: title.into(),
            notes: String::new(),
            completed: false,
            completed_ms: None,
            due: None,
            priority: 0,
            flagged: false,
            parent_id: None,
            alarms: 0,
            created_ms: Some(1),
            modified_ms: Some(1),
            change_tag: Some("t".into()),
            tokens: None,
        }
    }

    fn list(order: &[&str]) -> List {
        List {
            id: "List/W".into(),
            name: "Work".into(),
            color: None,
            order: order.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn cache(rs: Vec<Reminder>) -> Cache {
        Cache {
            synced_ms: Some(1),
            reminders: rs.into_iter().map(|r| (r.id.clone(), r)).collect(),
            ..Cache::default()
        }
    }

    fn base(titles: &[&str]) -> Vec<Known> {
        titles
            .iter()
            .enumerate()
            .map(|(i, t)| Known {
                id: format!("Reminder/{i}"),
                item: item(false, t, &now()).unwrap(),
            })
            .collect()
    }

    fn items(text: &str) -> Vec<Item> {
        parse(text, &now())
    }

    #[test]
    fn a_file_reads_back_as_it_was_written() {
        let now = now();
        let mut quote = reminder("A", "Send the quote");
        quote.due = Some(due::parse_when("2026-11-18 09:00", &now).unwrap());
        quote.notes = "First line\n\n- [ ] not a subtask\n  indented".into();
        let mut sub = reminder("B", "A subtask");
        sub.parent_id = Some("Reminder/A".into());
        let mut done = reminder("C", "Submit the sitemap");
        done.completed = true;
        done.completed_ms = Some(now.timestamp().as_millisecond() - 1000);
        let mut old = reminder("D", "Done long ago");
        old.completed = true;
        old.completed_ms = Some(now.timestamp().as_millisecond() - KEEP_DONE_MS - 1);
        let plain = reminder("E", "Mac connector test");
        let (text, ids) = render(&list(&["E", "A"]), &cache(vec![quote, sub, done, old, plain]), &now);
        assert_eq!(
            text,
            "# Work\n\n- [ ] Mac connector test\n- [ ] Send the quote 📅 2026-11-18 09:00\n    First line\n\n    \\- [ ] not a subtask\n    indented\n    - [ ] A subtask\n- [x] Submit the sitemap\n"
        );
        assert_eq!(ids, ["Reminder/E", "Reminder/A", "Reminder/B", "Reminder/C"]);
        let read = known(&text, ids, &now).unwrap();
        assert_eq!(read[1].item.due.as_deref(), Some("2026-11-18 09:00"));
        assert_eq!(read[1].item.notes, "First line\n\n- [ ] not a subtask\nindented");
        assert!(read[3].item.done);
        // Written again from what was read, nothing asks for a change.
        assert_eq!(diff(&read, &items(&text), &now), Delta::default());
    }

    #[test]
    fn any_line_at_the_margin_is_a_reminder() {
        let got = items(
            "# Groceries\n\nMilk\n- Eggs\n* [x] Bread 📅 tomorrow 9:00\n\n## Later\n+ Tea 📅 not a date\n- [ ]\n",
        );
        let titles: Vec<&str> = got.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Milk", "Eggs", "Bread", "Tea 📅 not a date"]);
        assert!(got[2].done);
        assert_eq!(got[2].due.as_deref(), Some("2026-11-15 09:00"));
        assert_eq!(got[3].due, None);
    }

    #[test]
    fn only_hashes_and_a_space_are_a_heading_and_a_byte_order_mark_is_skipped() {
        let got = items("\u{feff}# Groceries\n#hashtag buy milk\n##\n### Later\n#1 priority\n");
        let titles: Vec<&str> = got.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["#hashtag buy milk", "#1 priority"]);
    }

    #[test]
    fn a_title_that_reads_as_a_due_date_stays_a_title() {
        let now = now();
        let mut dated = reminder("C", "Call \\📅 Ann 📅 2026-12-01");
        dated.due = Some(due::parse_when("2026-12-24", &now).unwrap());
        let all = vec![
            reminder("A", "📅 tomorrow"),
            reminder("B", "Call 📅 tomorrow"),
            dated,
            reminder("D", "Odd \\📅 one"),
        ];
        let titles: Vec<String> = all.iter().map(|r| r.title.clone()).collect();
        let (text, ids) = render(&list(&["A", "B", "C", "D"]), &cache(all), &now);
        let read = known(&text, ids, &now).unwrap();
        assert_eq!(read.iter().map(|k| k.item.title.clone()).collect::<Vec<_>>(), titles);
        let dues: Vec<Option<&str>> = read.iter().map(|k| k.item.due.as_deref()).collect();
        assert_eq!(dues, [None, None, Some("2026-12-24"), None]);
        // Read on another day, with a box ticked: only the tick is asked for.
        let later = now.checked_add(jiff::Span::new().days(3)).unwrap();
        let ticked = text.replace("- [ ] Call \\📅 tomorrow", "- [x] Call \\📅 tomorrow");
        assert_ne!(ticked, text, "{text}");
        let delta = diff(&read, &parse(&ticked, &later), &later);
        assert_eq!(delta.updates.len(), 1);
        assert_eq!(delta.updates[0].2, vec![Change::Completed(true)]);
        assert!(delta.adds.is_empty() && delta.deletes.is_empty());
    }

    #[test]
    fn ticks_edits_adds_and_removals_are_told_apart() {
        let now = now();
        let before = base(&["Call the dentist", "Buy milk", "Send the quote", "Water plants"]);
        let after = items(
            "- [x] Call the dentist\n- [ ] Send the quote to Sara 📅 2026-11-18\n    with the fees\n- [ ] Book flights\n- [ ] Water plants\n",
        );
        let delta = diff(&before, &after, &now);
        assert_eq!(
            delta.adds.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["Book flights"]
        );
        assert_eq!(delta.deletes, ["Reminder/1"]);
        let by_id: BTreeMap<&str, &Vec<Change>> = delta.updates.iter().map(|(id, _, c)| (id.as_str(), c)).collect();
        assert_eq!(by_id["Reminder/0"], &vec![Change::Completed(true)]);
        assert_eq!(
            by_id["Reminder/2"],
            &vec![
                Change::Title("Send the quote to Sara".into()),
                Change::Notes("with the fees".into()),
                Change::Due(Some(due::parse_when("2026-11-18", &now).unwrap())),
            ]
        );
        assert_eq!(delta.updates.len(), 2);
    }

    #[test]
    fn a_moved_line_and_a_light_rewrite_keep_their_reminders() {
        let now = now();
        let before = base(&["Alpha", "Beta", "Gamma"]);
        // Reordered: nothing to do.
        assert_eq!(
            diff(&before, &items("- Gamma\n- Alpha\n- Beta\n"), &now),
            Delta::default()
        );
        // A light rewrite is still that reminder; a different line is not.
        let delta = diff(&before, &items("- Alpha\n- Beta (call back)\n- Gamma\n"), &now);
        assert!(delta.adds.is_empty() && delta.deletes.is_empty());
        assert_eq!(delta.updates[0].0, "Reminder/1");
        let delta = diff(&before, &items("- Alpha\n- Something else entirely\n- Gamma\n"), &now);
        assert_eq!(
            (delta.adds.len(), delta.deletes.as_slice()),
            (1, &["Reminder/1".to_owned()][..])
        );
        // Two same titles are two reminders, in order.
        let twins = base(&["Milk", "Milk"]);
        let delta = diff(&twins, &items("- [x] Milk\n- [ ] Milk\n"), &now);
        assert_eq!(delta.updates.len(), 1);
        assert_eq!(delta.updates[0].0, "Reminder/0");
        // With one of their lines removed, the reminder that goes is the one
        // the line left does not read as, not the second of that name.
        let mut twins = base(&["Milk", "Milk"]);
        twins[1].item.done = true;
        let delta = diff(&twins, &items("- [x] Milk\n"), &now);
        assert_eq!(delta.updates, []);
        assert_eq!(delta.deletes, ["Reminder/0"]);
    }

    #[test]
    fn a_gutted_file_is_risky_and_a_few_removals_are_not() {
        let now = now();
        let before = base(&["a1", "b2", "c3", "d4", "e5", "f6", "g7", "h8"]);
        let emptied = diff(&before, &items("# Work\n"), &now);
        assert_eq!(emptied.deletes.len(), 8);
        assert!(risky(&emptied, before.len()));
        let three = diff(&before, &items("- a1\n- b2\n- c3\n- d4\n- e5\n"), &now);
        assert!(!risky(&three, before.len()));
        // Four of eight is not most of the list.
        let four = diff(&before, &items("- a1\n- b2\n- c3\n- d4\n"), &now);
        assert!(!risky(&four, before.len()));
        let five = diff(&before, &items("- a1\n- b2\n- c3\n"), &now);
        assert!(risky(&five, before.len()));
    }

    #[test]
    fn a_conflict_copy_only_ticks_and_adds() {
        let mut before = base(&["Call the dentist", "Buy milk", "Send the quote"]);
        before[1].item.done = true;
        // The copy is older: it lacks a line, has an old title, unticks one.
        let copy = items("- [x] Call the dentist\n- [ ] Buy milk\n- [ ] Send quote\n- [ ] Renew passport\n");
        let delta = from_conflict_copy(&before, &copy);
        assert_eq!(
            delta.adds.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["Renew passport"]
        );
        assert!(delta.deletes.is_empty());
        assert_eq!(delta.updates.len(), 1);
        assert_eq!(delta.updates[0].0, "Reminder/0");
        assert_eq!(delta.updates[0].2, vec![Change::Completed(true)]);
    }

    #[test]
    fn a_conflict_copy_s_line_of_a_shared_title_is_the_reminder_it_reads_as() {
        let mut twins = base(&["Milk", "Milk"]);
        twins[1].item.done = true;
        // As the file has them: nothing to tick.
        assert_eq!(
            from_conflict_copy(&twins, &items("- [ ] Milk\n- [x] Milk\n")),
            Delta::default()
        );
        assert_eq!(from_conflict_copy(&twins, &items("- [x] Milk\n")), Delta::default());
        // Both ticked: the open one is.
        let delta = from_conflict_copy(&twins, &items("- [x] Milk\n- [x] Milk\n"));
        assert_eq!(delta.updates.len(), 1);
        assert_eq!(delta.updates[0].0, "Reminder/0");
    }

    #[test]
    fn file_names_are_safe_and_distinct() {
        assert_eq!(file_stem("Work/Home"), "Work-Home");
        assert_eq!(file_stem(" .hidden "), "hidden");
        assert_eq!(file_stem("/"), "-");
        assert_eq!(file_stem(""), "List");
        assert_eq!(free_name("Work", &["work.md".into(), "Work 2.md".into()]), "Work 3.md");
    }
}
