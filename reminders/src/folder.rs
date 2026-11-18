//! The reminders as Markdown checklists in a folder, one file per list, kept
//! in step with iCloud both ways by the background timer. Nothing turns it
//! on: it is `~/Documents/Reminders`, written after the first sync.
//!
//! The folder is a working copy: iCloud holds the reminders, and this is the
//! only code that reads the files. It exists so that something that only
//! carries files (Syncthing to a phone, an editor, an agent) can read and
//! change reminders. While it does, each run puts an empty `.nosync` file in
//! the folder (`icloud_app::leave::hold`), which keeps Omacloud from syncing
//! the same files as well.
//!
//! Once the reminders left iCloud on this computer (`icloud-reminders
//! leave`), none of this runs: the files are the reminders, Omacloud
//! carries them, the marker is gone, and [`crate::files`] is all that reads
//! them (for due-time notifications). A run that waited for the folder's
//! lock while `leave` held it does nothing. A file looks like this:
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
//! and the text after 📅 is anything [`due::parse_when`] reads. Open
//! reminders come in the list's own order, then the ones completed in the
//! last week; a reorder made in the file is not kept.
//!
//! The files are the lists, as in Apple's app, and the file name is the
//! list's name; the `# Title` line is written for whoever reads the file and
//! is not read (a heading is layout). A group, a list's sections and a list
//! shared with this account by someone else have no file (the last are in
//! CloudKit's shared database, which this app does not read): those are
//! made and changed in the app. [`lists_from_files`] does what the files
//! ask before any list's lines are read:
//!
//! - A new `.md` file (not hidden, not a conflict copy or temp file, not a
//!   list's old name or a deleted list's file come back) makes a list named
//!   after the file, without `.md` and as it is (a name made safe for a file,
//!   `/` written `-`, cannot be told back), in Apple's default colour; each
//!   of its lines is added to it, as a new line is. It waits
//!   until it has not changed for [`SETTLE_MS`]. A name another list has
//!   (case aside) makes none: the file is named in a warning once, and once
//!   a list of exactly its name is made in the app the file is that list's.
//! - A new file whose lines are more than half a list's whose file is gone
//!   ([`renames`]) is that file renamed: the list is renamed, keeping its
//!   reminders and their IDs, and the file's line changes are then edits
//!   (an old name of the list is its file renamed back, and no other's).
//!   One as like two lists as either is renames nothing and makes nothing:
//!   it is named in a warning, and their files are written back. One that
//!   is the file of a list deleted in iCloud meanwhile, renamed, is left as
//!   a changed file of a deleted list is.
//! - A list's file removed deletes the list, as the app's Delete List does:
//!   each of its reminders as a removed line deletes one (to Recently
//!   Deleted), then the list (`Deleted`), and a notification says so. Only
//!   when every guard holds, else the file is written back, with a warning
//!   and a notification saying why: the file was gone on two runs in a row
//!   at least [`MISSING_MS`] apart (one that is back in between starts the
//!   count again); it is the only list file gone (two at once look like a
//!   folder being emptied or replaced, so neither is deleted; remove one at
//!   a time); the folder is there and holds another list's file; and the
//!   list is not shared with other people (its record has a `share`). Lists
//!   are made, renamed and deleted only with a sync under [`FRESH_MS`] old.
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
//!   next run (deleted since, its line goes), and one iCloud never heard of
//!   is sent again. A file about to be written is recorded first
//!   ([`ListState::writing`]): one found as it says was written, by a run
//!   killed before it could record so (its temp file, named apart from
//!   Syncthing's own, is removed; one still holding the text says it was
//!   not); a
//!   rename too ([`ListState::renaming`]): done only when the file is at
//!   its new name and not its old. Nothing else ties a new
//!   line to a reminder: one made on the phone that reads the same is
//!   another. A list made from a file is recorded with its ID before it is
//!   sent ([`State::creating`]): found in iCloud, it was made; a list's
//!   rename too ([`ListState::retitling`]); and a deletion
//!   ([`ListState::deleting`]) is recorded before the first reminder goes,
//!   and finished by the next run whatever the file does meanwhile, so a
//!   list is never left half deleted. A file refused deletion is recorded
//!   as being written back ([`ListState::restoring`]) until it is there: a
//!   run killed first does not leave it to be read as removed.
//! - What a conflict copy added is in iCloud before it is in the file. Until
//!   the file is written with it ([`ListState::unwritten`]), a changed file
//!   that lacks it does not delete it, and a tick taken from a copy is not
//!   undone by a file that still shows the box empty, and a line of the
//!   file is never read as a retitle of it.
//! - A file Syncthing brings back after this computer moved or removed it
//!   (the phone had changed it meanwhile) is never left unread: one under a
//!   renamed list's old name ([`ListState::was`]) is read as a conflict copy
//!   and removed; a deleted list's ([`State::removed`]), or a conflict
//!   copy of it, is left in the folder like one that held changes never
//!   sent. Each is named in a warning. Only names of Syncthing's exact
//!   shape are conflict copies (`copy_of`).
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
use crate::model::{Change, List, ListChange, Reminder};
use crate::service::Service;
use crate::store::Cache;

/// `ICLOUD_REMINDERS_CRASH_AT=<point>[:<n>]` (debug builds only) ends the
/// process the `n`th time (default the first) a folder run reaches
/// `<point>`, as a kill would: the points sit between each write and what
/// records it (`tests/folder_model.rs`). A release build has no hook.
pub const CRASH_AT: &str = "ICLOUD_REMINDERS_CRASH_AT";

/// Every point [`CRASH_AT`] can name.
pub const CRASH_POINTS: &[&str] = &[
    "pending-saved",
    "added",
    "add-completed",
    "updated",
    "deleted",
    "copy-applied",
    "file-applied",
    "tmp-written",
    "file-written",
    "rename-pending",
    "renamed",
    "gone-removed",
    "list-create-pending",
    "list-created",
    "list-rename-pending",
    "list-renamed",
    "list-delete-pending",
    "list-reminder-deleted",
    "list-deleted",
];

/// `ICLOUD_REMINDERS_FOLDER_CLOCK_MS=<ms>` (debug builds only) puts the
/// folder's clock that far ahead ([`clock`]): the tests' runs a minute
/// apart without waiting a minute. A release build has no hook.
pub const CLOCK: &str = "ICLOUD_REMINDERS_FOLDER_CLOCK_MS";

/// Now, as a folder run takes it.
pub fn clock() -> jiff::Timestamp {
    let now = jiff::Timestamp::now();
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var(CLOCK).ok().and_then(|v| v.parse::<i64>().ok()) {
        return now.checked_add(jiff::SignedDuration::from_millis(ms)).unwrap_or(now);
    }
    now
}

/// A new file is made into a list once it has not changed for this long:
/// an editor or Syncthing may still be writing it.
pub const SETTLE_MS: i64 = 10_000;
/// A list is deleted once two runs at least this far apart found its file
/// gone (the timer runs every minute): a file that is only away for a
/// moment (an editor saving, Syncthing replacing it) costs nothing.
pub const MISSING_MS: i64 = 50_000;
/// Lists are made, renamed or deleted from files only with a sync this
/// recent: a list deleted from a stale cache would leave alive the
/// reminders the phone added since, and one made from it may already exist.
pub const FRESH_MS: i64 = 120_000;

#[inline]
fn crash(point: &str) {
    #[cfg(debug_assertions)]
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static HITS: AtomicUsize = AtomicUsize::new(0);
        debug_assert!(CRASH_POINTS.contains(&point));
        let Some(want) = std::env::var_os(CRASH_AT) else { return };
        let want = want.to_string_lossy();
        let (at, nth) = match want.rsplit_once(':').map(|(p, n)| (p, n.parse::<usize>())) {
            Some((p, Ok(n))) => (p, n.max(1)),
            _ => (&*want, 1),
        };
        if at == point && HITS.fetch_add(1, Ordering::SeqCst) + 1 == nth {
            eprintln!("icloud-reminders: crashing at {point} ({CRASH_AT})");
            // Exited, not aborted: no core dump for a crash the tests asked for.
            std::process::exit(86);
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = point;
}

/// How long a completed reminder stays in its file.
pub const KEEP_DONE_MS: i64 = 7 * 24 * 3600 * 1000;
/// What comes before a line's due date.
pub const DUE_MARK: &str = "📅";
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
    /// The file being written, saved before it is: found so on disk, it was.
    #[serde(default)]
    pub writing: Option<Writing>,
    /// The file's names before the list was renamed. Syncthing brings one
    /// back when the phone changed it meanwhile; it is then read as a
    /// conflict copy.
    #[serde(default)]
    pub was: Vec<String>,
    /// The name the file is being renamed to, saved before it is: found
    /// there and no longer under `file`, it was.
    #[serde(default)]
    pub renaming: Option<String>,
    /// When a run found the file gone (Unix ms, by [`clock`]), every run
    /// since finding it gone too: cleared by one that finds it back. See
    /// [`MISSING_MS`].
    #[serde(default)]
    pub missing_since: Option<i64>,
    /// The list's new name being sent, saved before it is, and the file
    /// that asks for it (a list file renamed).
    #[serde(default)]
    pub retitling: Option<Retitle>,
    /// The list is being deleted (its file was removed), saved before the
    /// first of its reminders is: how many have been so far. A run that
    /// finds it set finishes the deletion.
    #[serde(default)]
    pub deleting: Option<usize>,
    /// The file was gone and the list not deleted: it is written back, and
    /// until it is (a run killed first) it is never read as removed.
    #[serde(default)]
    pub restoring: bool,
}

/// A list renamed from its file: the name, and the file now holding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retitle {
    pub name: String,
    pub file: String,
}

/// A list being made from a new file, recorded with its ID before it is
/// sent: found in iCloud, it was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewList {
    pub id: String,
    pub name: String,
    pub file: String,
}

/// A file's text and items, recorded before the file is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Writing {
    pub text: String,
    pub items: Vec<Known>,
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
    /// they held changes never sent, or could not be read or removed (a
    /// conflict copy too). No list's file takes such a name (a copy's, its
    /// file's) while the file is there: a new list of that name would read
    /// it as its own.
    #[serde(default)]
    pub left: Vec<String>,
    /// Files removed with their deleted lists. One that comes back (changed
    /// on the phone meanwhile) is left, as above.
    #[serde(default)]
    pub removed: Vec<String>,
    /// Lists being made from new files.
    #[serde(default)]
    pub creating: Vec<NewList>,
    /// New files no list is made from (a name another list has, or like
    /// two lists whose files are gone), each with the text it was declined
    /// with: warned about once, and not a reason to sync until it changes.
    #[serde(default)]
    pub declined: BTreeMap<String, String>,
}

/// A list deleted because its file was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedList {
    pub list: String,
    pub file: String,
    /// Its reminders, deleted with it.
    pub reminders: usize,
}

/// A list whose file was removed and that was not deleted: the file was
/// written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptList {
    pub list: String,
    pub file: String,
    pub why: String,
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
    /// Lists made from new files, by name.
    pub lists_created: Vec<String>,
    /// Lists renamed from their files: the name before and after.
    pub lists_renamed: Vec<(String, String)>,
    pub lists_deleted: Vec<DeletedList>,
    /// Lists whose removed file was written back (each is a warning too).
    pub lists_kept: Vec<KeptList>,
    /// New files whose list waits until they have not changed for
    /// [`SETTLE_MS`].
    pub waiting: Vec<String>,
}

impl Report {
    /// Nothing was written either way.
    pub fn quiet(&self) -> bool {
        self.added + self.edited + self.deleted == 0
            && self.written.is_empty()
            && self.warnings.is_empty()
            && self.lists_created.is_empty()
            && self.lists_renamed.is_empty()
            && self.lists_deleted.is_empty()
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

/// How much a file's lines are a list's items, as [`diff`] pairs them: the
/// lines paired, of the longer of the two; 1.0 when both are empty.
pub fn likeness(base: &[Known], local: &[Item]) -> f64 {
    match base.len().max(local.len()) {
        0 => 1.0,
        n => pair(base, local).iter().flatten().count() as f64 / n as f64,
    }
}

/// Which new file is which list whose file is gone: a renamed file. A pair
/// is more than half alike ([`likeness`]), the most alike first; a file or
/// a list that is as alike to two is neither's (`ambiguous`). `may` says
/// which pairs can be at all.
pub fn renames(
    lists: &[&[Known]],
    files: &[Vec<Item>],
    may: impl Fn(usize, usize) -> bool,
) -> (Vec<(usize, usize)>, Ambiguous) {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (f, local) in files.iter().enumerate() {
        for (l, base) in lists.iter().enumerate() {
            let score = likeness(base, local);
            if may(f, l) && score > 0.5 {
                pairs.push((score, f, l));
            }
        }
    }
    pairs.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    let mut out = Vec::new();
    let mut ambiguous = Ambiguous::default();
    let taken = |out: &[(usize, usize)], a: &Ambiguous, f: usize, l: usize| {
        out.iter().any(|&(of, ol)| of == f || ol == l) || a.files.contains(&f) || a.lists.contains(&l)
    };
    for &(score, f, l) in &pairs {
        if taken(&out, &ambiguous, f, l) {
            continue;
        }
        let tie = pairs.iter().any(|&(s, of, ol)| {
            s == score && (of, ol) != (f, l) && (of == f || ol == l) && !taken(&out, &ambiguous, of, ol)
        });
        if tie {
            ambiguous.files.push(f);
            ambiguous.lists.push(l);
            for &(s, of, ol) in &pairs {
                if s == score && (of == f || ol == l) {
                    ambiguous.files.push(of);
                    ambiguous.lists.push(ol);
                }
            }
        } else {
            out.push((f, l));
        }
    }
    ambiguous.files.sort();
    ambiguous.files.dedup();
    ambiguous.lists.sort();
    ambiguous.lists.dedup();
    (out, ambiguous)
}

/// The files and lists [`renames`] could not tell apart.
#[derive(Debug, Default, PartialEq)]
pub struct Ambiguous {
    pub files: Vec<usize>,
    pub lists: Vec<usize>,
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
/// file, `.syncthing.<name>.icloud-reminders.tmp`, is of the shape Syncthing
/// gives its own temporaries, which it does not sync, but never one of
/// them: Syncthing's download of the file is `.syncthing.<name>.tmp`, and
/// that is left alone. Its data is on disk
/// before the rename, as `folder.json`'s is: a power cut must not leave an
/// empty file that `folder.json` says was written, read as every line removed.
fn write(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let tmp = tmp_path(path);
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    crash("tmp-written");
    fs::rename(&tmp, path)
}

fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("list");
    path.with_file_name(format!(".syncthing.{name}.icloud-reminders.tmp"))
}

/// What the names of `file`'s conflict copies start with.
fn conflict_prefix(file: &str) -> String {
    format!("{}.sync-conflict-", file.strip_suffix(".md").unwrap_or(file))
}

/// The file a Syncthing conflict copy is of: `Work.md` for
/// `Work.sync-conflict-<YYYYMMDD>-<HHMMSS>-<device>.md` (the device is 7
/// letters or digits), and for a copy of that copy. `None` for any other
/// name: a list may be called `Plan.sync-conflict-draft`.
fn copy_of(name: &str) -> Option<String> {
    const MARKER: &str = ".sync-conflict-";
    let base = name.strip_suffix(".md")?;
    let at = base.len().checked_sub(23)?;
    let tail = &base.as_bytes()[at..];
    let digits = |r: std::ops::Range<usize>| tail[r].iter().all(u8::is_ascii_digit);
    let shaped = digits(0..8)
        && tail[8] == b'-'
        && digits(9..15)
        && tail[15] == b'-'
        && tail[16..].iter().all(u8::is_ascii_alphanumeric);
    if !shaped {
        return None;
    }
    // ASCII from `at` on, so `at` is a character boundary.
    let file = format!("{}.md", base[..at].strip_suffix(MARKER)?);
    Some(copy_of(&file).unwrap_or(file))
}

/// Whether `name` is a Syncthing conflict copy of a list's file.
pub fn is_conflict_copy(name: &str) -> bool {
    copy_of(name).is_some()
}

/// Syncthing's conflict copies of `file`: `Work.sync-conflict-<when>-<device>.md`.
fn conflict_copies(dir: &Path, file: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(copy_of)
                .is_some_and(|of| of == file)
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
/// written, or has a conflict copy, or an old name of it is back, or it is
/// gone, or it is new; or a list's making, renaming or deletion is under
/// way. A background run syncs with iCloud first when this is so. A file
/// whose run failed is not, until it changes, and nor is a new file
/// declined until it changes: one bad file does not make every minute's
/// run a sync.
pub fn pending(state: &State, dir: &Path) -> bool {
    let lists = state.lists.values().any(|ls| {
        let text = read(&dir.join(&ls.file)).ok().flatten();
        let back = ls
            .was
            .iter()
            .any(|w| dir.join(w).exists() || !conflict_copies(dir, w).is_empty());
        let gone = ls.adopted && text.is_none() && !dir.join(&ls.file).exists();
        ls.deleting.is_some()
            || ls.retitling.is_some()
            || (!ls.stuck_on(&text)
                && (text.is_some_and(|text| text != ls.text)
                    || gone
                    || !conflict_copies(dir, &ls.file).is_empty()
                    || back))
    });
    let new = new_files(state, dir)
        .iter()
        .any(|n| state.declined.get(n) != read(&dir.join(n)).ok().flatten().as_ref());
    lists || new || !state.creating.is_empty()
}

/// The `.md` files in the folder that are no list's: each a new list, or a
/// list's file renamed. Not a hidden or temporary file, not a conflict
/// copy, not one left or removed with a deleted list, and not one being
/// made into a list. A list's old name is among them (renamed back).
fn new_files(state: &State, dir: &Path) -> Vec<String> {
    let listed = |name: &str| {
        state.lists.values().any(|ls| {
            ls.file == name
                || ls.renaming.as_deref() == Some(name)
                || ls.retitling.as_ref().is_some_and(|r| r.file == name)
        }) || state.creating.iter().any(|c| c.file == name)
    };
    let mut found: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && !n.starts_with('.') && copy_of(n).is_none())
        .filter(|n| !listed(n) && !state.left.contains(n) && !state.removed.contains(n))
        .filter(|n| dir.join(n).is_file())
        .collect();
    found.sort();
    found
}

/// When a file was last changed (Unix ms).
fn modified_ms(path: &Path) -> Option<i64> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(d.as_millis()).ok()
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
            crash("updated");
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
        crash("pending-saved");
        let when = it.due.as_deref().and_then(|d| due::parse_when(d, now).ok());
        let saved = svc.add_as(&uuid, &list.id, &it.title, &it.notes, when.as_ref())?;
        report.warnings.extend(saved.warning);
        report.added += 1;
        let made = saved.value;
        crash("added");
        // Known from here on, so a failure below does not add it again.
        known_now(ls, &uuid, &made);
        save(ls)?;
        if it.done && !made.completed {
            svc.update(&made, &[Change::Completed(true)])?;
            crash("add-completed");
            ls.items.last_mut().expect("just pushed").item.done = true;
            save(ls)?;
        }
    }
    for id in delta.deletes {
        if let Some(r) = cache.reminders.get(&id) {
            svc.update(r, &[Change::Deleted])?;
            report.deleted += 1;
            crash("deleted");
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

/// Whether a file that is neither what was being written nor what was
/// last written (changed since, by the user) was changed from the former:
/// that is the one it asks less of. A write that never landed leaves the
/// last one, and the user's edits are then of that.
fn closer(writing: &Writing, ls: &ListState, text: &str, now: &Zoned) -> bool {
    let local = parse(text, now);
    let size = |d: Delta| d.adds.len() + d.updates.len() + d.deletes.len();
    size(diff(&writing.items, &local, now)) < size(diff(&ls.items, &local, now))
}

/// One list: the file's changes to iCloud, then iCloud's state to the file.
fn one(run: Run, dir: &Path, ls: &mut ListState, report: &mut Report, save: Save) -> Result<()> {
    let Run { svc, list, now } = run;
    let path = dir.join(&ls.file);
    let on_disk = read(&path)?;
    // Written by a run killed before it recorded so: compared with what it
    // held before, iCloud's changes in it would read as its own (a
    // reminder retitled there as one removed and one added).
    if let Some(w) = ls.writing.take()
        && on_disk
            .as_ref()
            .is_some_and(|text| *text == w.text || (*text != ls.text && closer(&w, ls, text, now)))
    {
        ls.text = w.text;
        ls.items = w.items;
        ls.unwritten.clear();
        ls.adopted = true;
    }
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
    // that line's, and one deleted since is too (its line goes with the
    // next write, not sent again); only one iCloud never heard of is sent
    // again (the file, or the copy, still has it). Without an answer the
    // entry waits, and nothing of this list is sent.
    if !ls.pending.is_empty() {
        let cache = svc.cache()?;
        let mut failed = None;
        for p in std::mem::take(&mut ls.pending) {
            let id = format!("Reminder/{}", p.uuid);
            let found = match cache.reminders.get(&id) {
                Some(r) => Ok(Some(Some(r.clone()))),
                None if failed.is_some() => Err(()),
                None => svc.look_up(&id).map_err(|e| failed = Some(e)),
            };
            match found {
                Err(()) => ls.pending.push(p),
                Ok(Some(Some(made))) => {
                    ls.pending.push(p.clone());
                    known_now(ls, &p.uuid, &made);
                }
                Ok(Some(None)) => {
                    // A copy's: not a line the file lost.
                    if p.copy {
                        ls.unwritten.push(id.clone());
                    }
                    ls.items.push(Known { id, item: p.item });
                }
                Ok(None) => {}
            }
        }
        save(ls)?;
        if let Some(e) = failed {
            return Err(e);
        }
    }

    // Before the file: a copy is compared with the file as last written, so
    // a box it shows ticked that the file has just unticked is not news.
    let mut unreadable = None;
    let mut copies = conflict_copies(dir, &ls.file);
    // Its old names come back as the phone last had them, from before the
    // rename: older than the file, so read as conflict copies.
    let old: Vec<PathBuf> = ls
        .was
        .iter()
        .flat_map(|was| std::iter::once(dir.join(was)).chain(conflict_copies(dir, was)))
        .filter(|p| p.exists())
        .collect();
    copies.extend(old.iter().cloned());
    for copy in copies {
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
        crash("copy-applied");
        fs::remove_file(&copy)?;
        if old.contains(&copy) {
            let name = copy
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            report.warnings.push(format!(
                "{name}: came back after its list was renamed (it holds changes from another device); its ticks and new lines were taken into {} and it was removed",
                ls.file
            ));
        }
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
        // lines removed from it, and not retitled by a line like it (typed
        // on another device: another reminder).
        delta.deletes.retain(|id| !ls.unwritten.contains(id));
        let (retitles, updates) = std::mem::take(&mut delta.updates)
            .into_iter()
            .partition(|(id, _, c)| ls.unwritten.contains(id) && c.iter().any(|c| matches!(c, Change::Title(_))));
        delta.updates = updates;
        delta.adds.extend(retitles.into_iter().map(|(_, it, _)| it));
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
        crash("file-applied");
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
        ls.writing = Some(Writing {
            text: text.clone(),
            items: items.clone(),
        });
        save(ls)?;
        write(&path, &text)?;
        crash("file-written");
        report.written.push(ls.file.clone());
    }
    ls.text = text;
    ls.items = items;
    ls.writing = None;
    ls.unwritten.clear();
    ls.adopted = true;
    // Recorded at once: a run killed later on would leave the next one to
    // read iCloud's changes in this file as the file's own.
    if written { save(ls) } else { Ok(()) }
}

/// The list's file now at `to`: its conflict copies follow (they are found
/// by the file's name), and its old name is remembered.
fn renamed(dir: &Path, ls: &mut ListState, to: String) {
    let (old, new) = (conflict_prefix(&ls.file), conflict_prefix(&to));
    for copy in conflict_copies(dir, &ls.file) {
        let rest = copy.file_name().and_then(|n| n.to_str()).map(|n| &n[old.len()..]);
        if let Some(to) = rest.map(|rest| dir.join(format!("{new}{rest}")))
            && !to.exists()
        {
            let _ = fs::rename(&copy, &to);
        }
    }
    ls.was.retain(|w| !w.eq_ignore_ascii_case(&to));
    ls.was.push(std::mem::replace(&mut ls.file, to));
}

/// The list, other than `except`, whose file `name` is or would be, and
/// whether it has a file yet: a new file of that name is not another list.
fn holder<'c>(state: &State, lists: &[&'c List], name: &str, except: Option<&str>) -> Option<(&'c List, bool)> {
    lists.iter().filter(|l| Some(l.id.as_str()) != except).find_map(|l| {
        let ls = state.lists.get(&l.id);
        let file = ls.map_or_else(|| format!("{}.md", file_stem(&l.name)), |ls| ls.file.clone());
        let wanted = format!("{}.md", file_stem(&l.name));
        (file.eq_ignore_ascii_case(name) || wanted.eq_ignore_ascii_case(name)).then_some((*l, ls.is_some()))
    })
}

/// The state of a list made from `file`: the list began empty, so each of
/// the file's lines is one to add, as in a file this program wrote (not as
/// in one seen for the first time, which only adds: a tick in it would be
/// lost by a run killed after its adds).
fn made_from(file: &str) -> ListState {
    ListState {
        file: file.to_owned(),
        adopted: true,
        ..ListState::default()
    }
}

fn stem(file: &str) -> &str {
    file.strip_suffix(".md").unwrap_or(file)
}

/// Deletes the list `id` its file asked to delete (`deleting` is set): each
/// of its reminders, as a removed line deletes one, then the list. Its file
/// and old names are remembered as removed, so one Syncthing brings back is
/// left, not read. `list` is `None` once iCloud no longer has it.
fn finish_delete(svc: &Service, state: &mut State, id: &str, list: Option<&List>, report: &mut Report) -> Result<()> {
    let save = |state: &State| -> Result<()> { Ok(svc.store.save_folder(state)?) };
    let mut n = state.lists[id].deleting.unwrap_or(0);
    if let Some(list) = list {
        let cache = svc.cache()?;
        let mine: Vec<&Reminder> = cache.reminders.values().filter(|r| r.list_id == list.id).collect();
        for r in mine {
            report.warnings.extend(svc.update(r, &[Change::Deleted])?.warning);
            n += 1;
            state.lists.get_mut(id).expect("being deleted").deleting = Some(n);
            save(state)?;
            crash("list-reminder-deleted");
        }
        // Fresh: the change tag a write of the list itself may have changed.
        let list = svc.cache()?.list(id).cloned().unwrap_or_else(|| list.clone());
        report
            .warnings
            .extend(svc.update_list(&list, &ListChange::Deleted)?.warning);
        crash("list-deleted");
    }
    let ls = state.lists.remove(id).expect("being deleted");
    for name in std::iter::once(ls.file.clone()).chain(ls.was) {
        if !state.removed.contains(&name) {
            state.removed.push(name);
        }
    }
    report.lists_deleted.push(DeletedList {
        list: list.map_or_else(|| stem(&ls.file).to_owned(), |l| l.name.clone()),
        file: ls.file,
        reminders: n,
    });
    save(state)
}

/// What the files ask of the lists themselves, before any list's lines are
/// read: a new file makes a list, a list's file renamed renames it, and a
/// list's file removed deletes it (see the module's notes for the guards).
/// Each write is recorded in `folder.json` before it is sent, and a run
/// finishes what a killed one began. Returns the lists whose run waits
/// this time: their file is gone, and not yet for long enough.
fn lists_from_files(
    svc: &Service,
    dir: &Path,
    existed: bool,
    state: &mut State,
    now: &Zoned,
    report: &mut Report,
) -> Result<std::collections::BTreeSet<String>> {
    let save = |state: &State| -> Result<()> { Ok(svc.store.save_folder(state)?) };
    let now_ms = now.timestamp().as_millisecond();
    let mut waits = std::collections::BTreeSet::new();

    // Lists a killed run began making: made when iCloud has them (deleted
    // since too: the file then goes as a deleted list's does).
    let mut creating = std::mem::take(&mut state.creating).into_iter();
    while let Some(p) = creating.next() {
        match svc.look_up_list(&p.id) {
            Ok(None) => {}
            Ok(Some(_)) => {
                report.lists_created.push(p.name.clone());
                state.lists.insert(p.id.clone(), made_from(&p.file));
            }
            Err(e) => {
                state.creating.push(p);
                state.creating.extend(creating);
                save(state)?;
                return Err(e);
            }
        }
        save(state)?;
    }

    let cache = svc.cache()?;
    let lists: Vec<&List> = cache.lists.iter().filter(|l| !l.group).collect();
    let list_of = |id: &str| lists.iter().find(|l| l.id == id).copied();

    // A rename a killed run sent: done when iCloud has the name and the
    // files are as it found them; else found again below.
    for (id, ls) in state.lists.iter_mut() {
        let Some(r) = ls.retitling.take() else { continue };
        if list_of(id).is_some_and(|l| l.name == r.name) && dir.join(&r.file).exists() && !dir.join(&ls.file).exists() {
            report.lists_renamed.push((stem(&ls.file).to_owned(), r.name.clone()));
            renamed(dir, ls, r.file);
            ls.missing_since = None;
        }
    }
    save(state)?;
    // A deletion a killed run began is finished, whatever the file does now.
    let deleting: Vec<String> = state
        .lists
        .iter()
        .filter(|(_, ls)| ls.deleting.is_some())
        .map(|(id, _)| id.clone())
        .collect();
    for id in deleting {
        if let Err(e) = finish_delete(svc, state, &id, list_of(&id), report) {
            report.warnings.push(format!(
                "{}: the list is not all deleted yet ({e}); the next run goes on",
                state.lists[&id].file
            ));
            waits.insert(id);
        }
    }

    for ls in state.lists.values_mut() {
        if dir.join(&ls.file).exists() {
            ls.missing_since = None;
            ls.restoring = false;
        }
    }
    // One being written back is not gone: a run killed before writing it
    // must not leave the next to count it as removed.
    let gone: Vec<String> = state
        .lists
        .iter()
        .filter(|(id, ls)| {
            list_of(id).is_some()
                && ls.adopted
                && ls.deleting.is_none()
                && !ls.restoring
                && !dir.join(&ls.file).exists()
        })
        .map(|(id, _)| id.clone())
        .collect();
    let fresh = cache
        .synced_ms
        .is_some_and(|ms| crate::service::now_ms() - ms < FRESH_MS);
    if !fresh {
        waits.extend(gone);
        return Ok(waits);
    }

    // The new files, once nothing has changed them for a while.
    let listed = |state: &State, name: &str| state.lists.values().any(|ls| ls.file == name);
    state
        .declined
        .retain(|name, _| dir.join(name).exists() && !state.lists.values().any(|ls| ls.file == *name));
    let mut files: Vec<(String, String)> = Vec::new();
    let mut unsettled = false;
    for name in new_files(state, dir) {
        let path = dir.join(&name);
        let Ok(Some(text)) = read(&path) else { continue };
        if modified_ms(&path).is_none_or(|m| now_ms - m < SETTLE_MS) {
            unsettled = true;
            report.waiting.push(name);
            continue;
        }
        files.push((name, text));
    }
    let was_of = |state: &State, name: &str| {
        state
            .lists
            .iter()
            .find(|(_, ls)| ls.was.iter().any(|w| w == name))
            .map(|(id, _)| id.clone())
    };
    let parsed: Vec<Vec<Item>> = files.iter().map(|(_, text)| parse(text, now)).collect();
    // Lists deleted in iCloud whose file is gone too: a file renamed from
    // one is that deleted list's, not a new list of its reminders.
    let dead: Vec<String> = state
        .lists
        .iter()
        .filter(|(id, ls)| list_of(id).is_none() && ls.adopted && !dir.join(&ls.file).exists())
        .map(|(id, _)| id.clone())
        .collect();
    let candidates: Vec<&String> = gone.iter().chain(&dead).collect();
    let bases: Vec<&[Known]> = candidates.iter().map(|id| state.lists[*id].items.as_slice()).collect();
    let (matched, ambiguous) = renames(&bases, &parsed, |f, l| {
        let name = &files[f].0;
        match was_of(state, name) {
            Some(of) => of == *candidates[l],
            None => holder(state, &lists, name, Some(candidates[l])).is_none(),
        }
    });

    // Renamed files rename their lists.
    for &(f, l) in &matched {
        if l >= gone.len() {
            let file = &files[f].0;
            report.warnings.push(format!(
                "{file}: its list is gone from iCloud (the file is {} renamed), so it is left here and no longer kept in step",
                state.lists[candidates[l]].file
            ));
            state.left.push(file.clone());
            save(state)?;
            continue;
        }
        let (id, file) = (&gone[l], &files[f].0);
        let list = list_of(id).expect("gone lists are in iCloud");
        let name = stem(file).to_owned();
        let ls = state.lists.get_mut(id).expect("gone lists have a state");
        ls.retitling = Some(Retitle {
            name: name.clone(),
            file: file.clone(),
        });
        save(state)?;
        crash("list-rename-pending");
        let sent = svc.update_list(list, &ListChange::Name(name.clone()));
        let ls = state.lists.get_mut(id).expect("gone lists have a state");
        ls.retitling = None;
        match sent {
            Ok(saved) => {
                report.warnings.extend(saved.warning);
                crash("list-renamed");
                renamed(dir, ls, file.clone());
                ls.missing_since = None;
                report.lists_renamed.push((list.name.clone(), name));
            }
            Err(e) => {
                report.warnings.push(format!(
                    "{file}: could not rename the list \"{}\" to \"{name}\": {e}",
                    list.name
                ));
                waits.insert(id.clone());
            }
        }
        save(state)?;
    }
    for &f in &ambiguous.files {
        let (name, text) = &files[f];
        if state.declined.get(name) != Some(text) {
            report.warnings.push(format!(
                "{name}: not read, as it is as like two lists whose files are gone; their files were written back (rename one list file at a time)"
            ));
            state.declined.insert(name.clone(), text.clone());
        }
    }

    // New files make lists.
    for (f, (name, text)) in files.iter().enumerate() {
        if matched.iter().any(|&(mf, _)| mf == f) || ambiguous.files.contains(&f) || was_of(state, name).is_some() {
            continue;
        }
        if listed(state, name) || state.declined.get(name) == Some(text) {
            continue;
        }
        let new = stem(name).to_owned();
        let why = match holder(state, &lists, name, None) {
            // Once the list of that name has its first run, the file is its own.
            Some((_, false)) => continue,
            Some((l, true)) => Some(format!("the list \"{}\" has this name", l.name)),
            None if new.trim().is_empty() => Some("it has no name".to_owned()),
            None => None,
        };
        if let Some(why) = why {
            report.warnings.push(format!(
                "{name}: no list was made of it, as {why}; rename the file to make one"
            ));
            state.declined.insert(name.clone(), text.clone());
            continue;
        }
        let uuid = crate::service::new_uuid();
        let id = format!("List/{uuid}");
        state.creating.push(NewList {
            id: id.clone(),
            name: new.clone(),
            file: name.clone(),
        });
        state.declined.remove(name);
        save(state)?;
        crash("list-create-pending");
        match svc.create_list(&uuid, &new) {
            Ok(saved) => {
                report.warnings.extend(saved.warning);
                crash("list-created");
                state.creating.retain(|c| c.id != id);
                state.lists.insert(id, made_from(name));
                report.lists_created.push(new);
            }
            // It may have been made all the same: the next run looks.
            Err(e) => report
                .warnings
                .push(format!("{name}: could not make the list \"{new}\": {e}")),
        }
        save(state)?;
    }

    // Removed files delete their lists, when all the guards hold; a list
    // that is not deleted has its file written back.
    let left: Vec<&String> = gone
        .iter()
        .enumerate()
        .filter(|(l, _)| !matched.iter().any(|&(_, ml)| ml == *l))
        .map(|(_, id)| id)
        .collect();
    let present = state
        .lists
        .iter()
        .any(|(id, ls)| !left.contains(&id) && list_of(id).is_some() && ls.adopted && dir.join(&ls.file).exists());
    let all_why = if !existed {
        Some("the whole folder was gone".to_owned())
    } else if left.len() > 1 {
        Some(format!(
            "{} list files are gone at once (remove one at a time)",
            left.len()
        ))
    } else if !present {
        Some("no other list's file is in the folder".to_owned())
    } else {
        None
    };
    for (l, id) in gone.iter().enumerate() {
        if !left.contains(&id) {
            continue;
        }
        let list = list_of(id).expect("gone lists are in iCloud");
        let why = all_why
            .clone()
            .or_else(|| {
                ambiguous
                    .lists
                    .contains(&l)
                    .then(|| "a new file is as like it as another list".to_owned())
            })
            .or_else(|| {
                list.shared
                    .then(|| "it is shared with other people (delete a shared list in the app)".to_owned())
            });
        let ls = state.lists.get_mut(id).expect("gone lists have a state");
        if let Some(why) = why {
            ls.missing_since = None;
            ls.restoring = true;
            report.warnings.push(format!(
                "{}: removed, but its list was not deleted: {why}; the file was written back",
                ls.file
            ));
            report.lists_kept.push(KeptList {
                list: list.name.clone(),
                file: ls.file.clone(),
                why,
            });
            continue;
        }
        // A file still being written may be this one renamed.
        let since = *ls.missing_since.get_or_insert(now_ms);
        if unsettled || now_ms - since < MISSING_MS {
            waits.insert(id.clone());
            continue;
        }
        ls.deleting = Some(0);
        save(state)?;
        crash("list-delete-pending");
        if let Err(e) = finish_delete(svc, state, id, Some(list), report) {
            report.warnings.push(format!(
                "{}: the list is not all deleted yet ({e}); the next run goes on",
                state.lists[id].file
            ));
            waits.insert(id.clone());
        }
    }
    save(state)?;
    Ok(waits)
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
    let existed = dir.is_dir();
    fs::create_dir_all(dir)?;
    // iCloud keeps these files: Omacloud leaves the folder alone
    icloud_app::leave::hold(dir)?;

    // A rename a killed run began: done when the file is at its new name
    // and not its old, else not done (never an old name's file come back).
    for ls in state.lists.values_mut() {
        // A write a killed run began: its temp file is this program's own,
        // also once the list is renamed or gone.
        if let Some(w) = &ls.writing {
            let tmp = tmp_path(&dir.join(&ls.file));
            // Still there with (the start of) what was being written: the
            // rename never happened, so the file is as last recorded, and
            // the user's edits are of that, whatever they look closer to.
            if fs::read(&tmp).is_ok_and(|t| w.text.as_bytes().starts_with(&t)) {
                ls.writing = None;
            }
            let _ = fs::remove_file(tmp);
        }
        if let Some(to) = ls.renaming.take()
            && dir.join(&to).exists()
            && !dir.join(&ls.file).exists()
        {
            renamed(dir, ls, to);
        }
    }

    let waits = lists_from_files(svc, dir, existed, state, now, &mut report)?;
    // A group is not a list: it has no file.
    let mut cache = svc.cache()?;
    cache.lists.retain(|l| !l.group);

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
        // As a run killed before it recorded so wrote it: nothing unsent.
        let written = |text: &String| *text == ls.text || ls.writing.as_ref().is_some_and(|w| w.text == *text);
        let beside = match names.as_slice() {
            [] => String::new(),
            names => format!(" (and beside it {})", names.join(", ")),
        };
        // Left as it is when it cannot be read or removed: one list's
        // trouble, not the run's, and never taken by a new list of its name.
        let cannot = |what: &str, e: io::Error| {
            format!(
                "{}: its list is gone from iCloud, and the file could not be {what} ({e}), so it is left here{beside} and no longer kept in step",
                ls.file
            )
        };
        let left = match read(&path) {
            Err(e) => Some(cannot("read", e)),
            Ok(Some(text)) if !written(&text) => Some(format!(
                "{}: its list is gone from iCloud; the file has changes that were never sent, so it is left here{beside} and no longer kept in step",
                ls.file
            )),
            Ok(file) => match file.map_or(Ok(()), |_| fs::remove_file(&path)) {
                Err(e) => Some(cannot("removed", e)),
                Ok(()) => None,
            },
        };
        match left {
            Some(warning) => {
                report.warnings.push(warning);
                state.left.push(ls.file);
                // Its copies stay beside it, and keep its name from a new
                // list also once the file itself is removed.
                state.left.extend(names);
            }
            None => {
                state.removed.push(ls.file.clone());
                // Nothing can take them in now: the list and its reminders
                // are gone, and a new list of its name would read them as
                // its own.
                for (copy, name) in copies.iter().zip(&names) {
                    report.warnings.push(match fs::remove_file(copy) {
                        Ok(()) => format!("{name}: its list is gone from iCloud, so this conflict copy was removed unread"),
                        Err(e) => {
                            state.left.push(name.clone());
                            format!(
                                "{name}: its list is gone from iCloud, and this conflict copy could not be removed ({e}), so it is left here"
                            )
                        }
                    });
                }
            }
        }
        // Its old names too: one Syncthing brings back is left, as above.
        for was in ls.was {
            if !state.removed.contains(&was) {
                state.removed.push(was);
            }
        }
        crash("gone-removed");
    }
    // A removed file back (Syncthing restores one the phone had changed):
    // left with what it holds, never taken by a new list of its name. Once a
    // list has the name, a file of it is that list's.
    state
        .removed
        .retain(|n| !state.lists.values().any(|ls| ls.file.eq_ignore_ascii_case(n)));
    for name in std::mem::take(&mut state.removed) {
        if dir.join(&name).exists() {
            report.warnings.push(format!(
                "{name}: came back after its list was deleted in iCloud (it holds changes from another device), so it is left here and no longer kept in step"
            ));
            state.left.push(name);
        } else {
            state.removed.push(name);
        }
    }
    // A conflict copy Syncthing makes of such a file (under any name its
    // list had), or of a file left, after the list was deleted: left the
    // same way, so a new list of that name never reads it.
    let gone: Vec<&String> = state
        .removed
        .iter()
        .chain(state.left.iter().filter(|f| copy_of(f).is_none()))
        .collect();
    let mut late = Vec::new();
    for name in fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
    {
        if copy_of(&name).is_some_and(|of| gone.contains(&&of)) && !state.left.contains(&name) {
            late.push(name);
        }
    }
    late.sort();
    for name in late {
        report.warnings.push(format!(
            "{name}: a conflict copy of a list deleted in iCloud (it holds changes from another device), so it is left here and no longer kept in step"
        ));
        state.left.push(name);
    }

    for list in &cache.lists {
        // Its file is gone, and the list is not deleted yet: nothing is
        // read or written until it is, or the file is back.
        if waits.contains(&list.id) {
            continue;
        }
        let others: Vec<String> = state
            .lists
            .iter()
            .filter(|(id, _)| **id != list.id)
            .flat_map(|(_, ls)| {
                // A renamed list's old name, while a file of it is there to read.
                let back = ls.was.iter().filter(|w| dir.join(w).exists()).cloned();
                std::iter::once(ls.file.clone()).chain(back)
            })
            // A conflict copy left keeps its file's name: a list of that name
            // would read it as its own.
            .chain(state.left.iter().map(|f| copy_of(f).unwrap_or_else(|| f.clone())))
            .collect();
        let wanted = free_name(&file_stem(&list.name), &others);
        let mut ls = state.lists.get(&list.id).cloned().unwrap_or_default();
        // An old name another list now has is that list's file.
        ls.was.retain(|w| !others.iter().any(|o| o.eq_ignore_ascii_case(w)));
        if ls.file.is_empty() {
            ls.file = wanted;
        } else if ls.file != wanted {
            // The list was renamed: the file follows, when nothing is in the
            // way. With no file at its old name (removed), there is nothing
            // to move.
            let (from, to) = (dir.join(&ls.file), dir.join(&wanted));
            if !from.exists() {
                renamed(dir, &mut ls, wanted);
                state.lists.insert(list.id.clone(), ls.clone());
                svc.store.save_folder(state)?;
            } else if !to.exists() {
                // Recorded before and after: a run killed in between leaves
                // the next to tell whether the file moved.
                ls.renaming = Some(wanted.clone());
                state.lists.insert(list.id.clone(), ls.clone());
                svc.store.save_folder(state)?;
                crash("rename-pending");
                if fs::rename(&from, &to).is_ok() {
                    crash("renamed");
                    renamed(dir, &mut ls, wanted);
                }
                ls.renaming = None;
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
    // Written back: removed again, it is read as removed again.
    for ls in state.lists.values_mut() {
        if ls.restoring && dir.join(&ls.file).exists() {
            ls.restoring = false;
        }
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
            order: order.iter().map(|s| (*s).to_owned()).collect(),
            ..List::default()
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

    #[test]
    fn only_syncthing_s_exact_shape_is_a_conflict_copy() {
        let of = |n: &str| copy_of(n);
        assert_eq!(
            of("Work.sync-conflict-20261114-093000-ABCDEFG.md").as_deref(),
            Some("Work.md")
        );
        // A copy of a copy is of the file.
        assert_eq!(
            of("Work.sync-conflict-20261114-093000-ABCDEFG.sync-conflict-20261115-101500-HIJKLMN.md").as_deref(),
            Some("Work.md")
        );
        assert_eq!(
            of("Plan.sync-conflict-20261114-093000-ABCDEFG.sync-conflict-draft.md"),
            None
        );
        for name in [
            "Plan.sync-conflict-draft.md",
            "Work.sync-conflict-20261114-093000-ABCDEF.md",
            "Work.sync-conflict-2026111-0930001-ABCDEFG.md",
            "Work.sync-conflict-20261114-093000-ABCDEFG.txt",
            "Work.md",
            "é.md",
            "Wörk-sync-conflict-20261114-093000-ABCDEFG.md",
        ] {
            assert_eq!(of(name), None, "{name}");
        }
    }
}
