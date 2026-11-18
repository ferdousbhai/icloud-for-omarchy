//! Reminders once they left iCloud on this computer (`icloud-reminders
//! leave`): the Markdown files in `~/Documents/Reminders` are the
//! reminders, carried between computers by Omacloud, and nothing here talks
//! to iCloud. What is left to do is what the files cannot do themselves:
//! notify when a reminder falls due, which the timer does from the files.
//!
//! A line is read as [`folder::parse`] reads it. A due date written
//! relatively (`tomorrow 9:00`, `+30m`) is pinned to the date it means when
//! first read ([`pin`]), as the folder step did while iCloud kept the
//! reminders, so it stays the same time on the next run.
//!
//! A reminder notifies once, when its due time comes, if a run saw it
//! waiting before that (one written already past due is not news), within
//! [`notify::CATCH_UP_MS`]. A line is known by its file, title and due
//! instant: an edited title or due time is another reminder, which waits
//! afresh.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use jiff::Zoned;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::due::{self, Due};
use crate::folder;
use crate::notify;

/// One open reminder of a file with a due date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDue {
    /// The list: the file's name without `.md`.
    pub list: String,
    pub title: String,
    pub due: Due,
    /// The due instant, Unix ms.
    pub at: i64,
}

impl FileDue {
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.list, self.title, self.at)
    }

    /// The notification's second line: the list and the time.
    pub fn text(&self, local: &TimeZone) -> String {
        let when = if self.due.all_day {
            "today".to_owned()
        } else {
            self.due
                .instant(local)
                .to_zoned(local.clone())
                .time()
                .strftime("%H:%M")
                .to_string()
        };
        format!("{} · {when}", self.list)
    }
}

/// What the files-only timer remembers: the reminders it saw waiting.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub version: u32,
    /// [`FileDue::key`]s of reminders not yet due when last seen (or due
    /// and not yet notified, after a notification that failed).
    pub keys: BTreeSet<String>,
}

/// The list files of the folder: `*.md`, not hidden, not a conflict copy.
fn list_files(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && !n.starts_with('.') && !folder::is_conflict_copy(n))
        .filter(|n| dir.join(n).is_file())
        .collect();
    names.sort();
    Ok(names)
}

/// `text` with each relative due date (`tomorrow`, `+2h`) written as the
/// date it means now; `None` when there is none.
pub fn pin(text: &str, now: &Zoned) -> Option<String> {
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, end) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let pinned = folder::DUE_MARK;
        let fixed = body.rfind(pinned).and_then(|at| {
            if body[..at].ends_with('\\') {
                return None;
            }
            let written = body[at + pinned.len()..].trim();
            let d = due::parse_when(written, now).ok()?;
            let shown = d.display(now.time_zone());
            (shown != written).then(|| format!("{}{pinned} {shown}", &body[..at]))
        });
        match fixed {
            Some(f) => {
                changed = true;
                out.push_str(&f);
            }
            None => out.push_str(body),
        }
        out.push_str(end);
    }
    changed.then_some(out)
}

/// Pins the folder's relative due dates in place ([`pin`]); a file that
/// changed while it was read is left for the next run. Returns the files
/// written.
pub fn pin_folder(dir: &Path, now: &Zoned) -> io::Result<Vec<String>> {
    let mut written = Vec::new();
    for name in list_files(dir)? {
        let path = dir.join(&name);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(new) = pin(&text, now) else {
            continue;
        };
        if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            continue;
        }
        let tmp = dir.join(format!(".{name}.pin-tmp"));
        fs::write(&tmp, new)?;
        fs::rename(&tmp, &path)?;
        written.push(name);
    }
    Ok(written)
}

/// Every open reminder with a due date in the folder's files.
pub fn open_due(dir: &Path, now: &Zoned) -> io::Result<Vec<FileDue>> {
    let local = now.time_zone();
    let mut out = Vec::new();
    for name in list_files(dir)? {
        let Ok(text) = fs::read_to_string(dir.join(&name)) else {
            continue;
        };
        let list = name.strip_suffix(".md").unwrap_or(&name).to_owned();
        for item in folder::parse(&text, now) {
            if item.done {
                continue;
            }
            let Some(due) = item.due.as_deref().and_then(|d| due::parse_when(d, now).ok()) else {
                continue;
            };
            let at = due.instant(local).as_millisecond();
            out.push(FileDue {
                list: list.clone(),
                title: item.title,
                due,
                at,
            });
        }
    }
    Ok(out)
}

/// The reminders to notify about now, soonest due first, and what to
/// remember for the next run (before any notification failed: put a
/// failed one's key back).
pub fn due_now(items: Vec<FileDue>, prev: &Waiting, now_ms: i64) -> (Vec<FileDue>, Waiting) {
    let mut next = Waiting {
        version: 1,
        keys: BTreeSet::new(),
    };
    let mut fire = Vec::new();
    for item in items {
        let key = item.key();
        if item.at > now_ms {
            next.keys.insert(key);
        } else if prev.keys.contains(&key) && item.at > now_ms - notify::CATCH_UP_MS {
            fire.push(item);
        }
    }
    fire.sort_by_key(|f| (f.at, f.title.to_lowercase()));
    fire.dedup_by_key(|f| f.key());
    (fire, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Zoned {
        "2026-11-18T10:00:00[UTC]".parse().unwrap()
    }

    #[test]
    fn relative_dates_are_pinned_and_absolute_ones_kept() {
        let now = now();
        let text = "# Work\n\n- [ ] Call 📅 tomorrow 9:00\n- [ ] Send 📅 2026-11-20\n- [ ] Odd \\📅 tomorrow\n";
        let pinned = pin(text, &now).unwrap();
        assert!(pinned.contains("- [ ] Call 📅 2026-11-19 09:00\n"), "{pinned}");
        assert!(pinned.contains("- [ ] Send 📅 2026-11-20\n"), "{pinned}");
        assert!(pinned.contains("- [ ] Odd \\📅 tomorrow\n"), "{pinned}");
        assert_eq!(pin(&pinned, &now), None);
    }

    fn item(title: &str, at: i64) -> FileDue {
        FileDue {
            list: "Work".into(),
            title: title.into(),
            due: Due::read(at, false, None).unwrap(),
            at,
        }
    }

    #[test]
    fn a_reminder_seen_waiting_notifies_once_when_due() {
        let (t0, min) = (1_800_000_000_000, 60_000);
        // seen waiting, and one already past: only the first will notify
        let (fire, w) = due_now(
            vec![item("Later", t0 + min), item("Past", t0 - min)],
            &Waiting::default(),
            t0,
        );
        assert!(fire.is_empty());
        assert_eq!(w.keys.len(), 1);
        let (fire, w) = due_now(vec![item("Later", t0 + min), item("Past", t0 - min)], &w, t0 + 2 * min);
        assert_eq!(fire.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["Later"]);
        let (fire, _) = due_now(vec![item("Later", t0 + min)], &w, t0 + 3 * min);
        assert!(fire.is_empty());
    }
}
