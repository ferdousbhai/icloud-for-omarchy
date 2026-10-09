//! Desktop notifications for due reminders, through Omarchy's own
//! `omarchy-notification-send` (its look, its glyph, click to open) when it
//! is on `PATH`, else straight to `org.freedesktop.Notifications`.
//!
//! The background timer runs [`due_now`] every minute against the cache.
//! A reminder notifies once per due time: when that time has come, within
//! [`CATCH_UP`] (a machine that slept through it still says so on waking),
//! if it is still open and was last changed before it fell due (one made
//! or moved into the past is not news). The first run only marks what is
//! already due, so installing does not replay a backlog.

use std::process::Command;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::model::Reminder;
use crate::store::{Cache, Notified};

/// How late a notification may still come.
pub const CATCH_UP_MS: i64 = 12 * 3600 * 1000;
const GLYPH: &str = "󰢌";
const APP_NAME: &str = "iCloud Reminders";

/// The reminders to notify about now, oldest due first, and the state
/// that records them (and forgets reminders that no longer wait).
pub fn due_now<'c>(cache: &'c Cache, state: &Notified, now: Timestamp, local: &TimeZone) -> (Vec<&'c Reminder>, Notified) {
    let now_ms = now.as_millisecond();
    let mut next = Notified {
        started: true,
        fired: Default::default(),
        sync_attempt_ms: state.sync_attempt_ms,
    };
    let mut fire = Vec::new();
    for r in cache.reminders.values() {
        let Some(due) = r.due.as_ref().filter(|_| !r.completed) else {
            continue;
        };
        let at = due.instant(local).as_millisecond();
        if at > now_ms {
            continue;
        }
        let already = state.fired.get(&r.id) == Some(&at);
        let fresh = state.started
            && !already
            && at > now_ms - CATCH_UP_MS
            && r.modified_ms.is_none_or(|m| m < at);
        if fresh {
            fire.push((at, r));
        }
        // Remembered while it is due and open, fired or not: a later edit
        // of the due time makes it news again.
        next.fired.insert(r.id.clone(), at);
    }
    fire.sort_by_key(|(at, r)| (*at, r.title.to_lowercase()));
    (fire.into_iter().map(|(_, r)| r).collect(), next)
}

/// The notification's two lines: the title, then the list and the time.
pub fn text(r: &Reminder, cache: &Cache, local: &TimeZone) -> (String, String) {
    let list = cache.list(&r.list_id).map(|l| l.name.as_str()).unwrap_or("Reminders");
    let when = match &r.due {
        Some(d) if d.all_day => "today".to_owned(),
        Some(d) => d.display(local).rsplit(' ').next().unwrap_or_default().to_owned(),
        None => String::new(),
    };
    (r.title.clone(), format!("{list} · {when}"))
}

/// Shows one notification; clicking it opens the app.
pub fn send(headline: &str, body: &str) -> Result<(), String> {
    if let Some(omarchy) = omarchy_notifier() {
        let status = Command::new(omarchy)
            .args(["--app-name", APP_NAME, "-g", GLYPH, "-u", "normal", headline, body])
            .args(["--exec", "icloud-reminders-app"])
            .status()
            .map_err(|e| format!("omarchy-notification-send: {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("omarchy-notification-send failed ({status})"))
        };
    }
    freedesktop(headline, body)
}

/// `omarchy-notification-send` on `PATH`, else in Omarchy's own `bin`
/// (`$OMARCHY_PATH`, `~/.local/share/omarchy`): the systemd user manager
/// that runs the timer may not have Omarchy's `PATH`.
fn omarchy_notifier() -> Option<std::path::PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(std::path::PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(var("OMARCHY_PATH").map(|p| p.join("bin")))
        .chain(var("HOME").map(|h| h.join(".local/share/omarchy/bin")))
        .map(|dir| dir.join("omarchy-notification-send"))
        .find(|p| p.is_file())
}

/// `Notify` on the session bus, for desktops without Omarchy.
fn freedesktop(headline: &str, body: &str) -> Result<(), String> {
    use std::collections::HashMap;
    let conn = zbus::blocking::Connection::session().map_err(|e| format!("session bus: {e}"))?;
    let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::from([("urgency", zbus::zvariant::Value::U8(1))]);
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(APP_NAME, 0u32, "", headline, body, Vec::<&str>::new(), hints, -1i32),
    )
    .map_err(|e| format!("notification: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::due::Due;
    use crate::model::List;

    fn reminder(id: &str, due_utc_wall: Option<&str>, modified: Option<&str>) -> Reminder {
        let ms = |s: &str| s.parse::<Timestamp>().unwrap().as_millisecond();
        Reminder {
            id: id.into(),
            list_id: "List/A".into(),
            title: id.trim_start_matches("Reminder/").into(),
            notes: String::new(),
            completed: false,
            completed_ms: None,
            due: due_utc_wall.map(|w| Due {
                wall_ms: ms(w),
                all_day: false,
                time_zone: None,
            }),
            priority: 0,
            flagged: false,
            parent_id: None,
            alarms: 0,
            created_ms: None,
            modified_ms: modified.map(ms),
            change_tag: None,
            tokens: None,
        }
    }

    fn cache(rs: Vec<Reminder>) -> Cache {
        let mut c = Cache::default();
        c.lists.push(List {
            id: "List/A".into(),
            name: "Groceries".into(),
            color: None,
            order: vec![],
        });
        for r in rs {
            c.reminders.insert(r.id.clone(), r);
        }
        c
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn titles(rs: &[&Reminder]) -> Vec<String> {
        rs.iter().map(|r| r.title.clone()).collect()
    }

    #[test]
    fn fires_once_when_due_and_not_before() {
        let utc = TimeZone::UTC;
        let c = cache(vec![
            reminder("Reminder/milk", Some("2026-10-09T10:00:00Z"), Some("2026-10-09T08:00:00Z")),
            reminder("Reminder/later", Some("2026-10-09T18:00:00Z"), None),
            reminder("Reminder/undated", None, None),
        ]);
        let started = Notified {
            started: true,
            ..Notified::default()
        };
        let (fire, state) = due_now(&c, &started, at("2026-10-09T09:59:00Z"), &utc);
        assert!(fire.is_empty());
        let (fire, state) = due_now(&c, &state, at("2026-10-09T10:00:30Z"), &utc);
        assert_eq!(titles(&fire), ["milk"]);
        let (fire, _) = due_now(&c, &state, at("2026-10-09T10:01:30Z"), &utc);
        assert!(fire.is_empty(), "once");
    }

    #[test]
    fn the_first_run_marks_the_backlog_without_firing() {
        let utc = TimeZone::UTC;
        let c = cache(vec![reminder("Reminder/old", Some("2026-10-09T09:00:00Z"), None)]);
        let (fire, state) = due_now(&c, &Notified::default(), at("2026-10-09T10:00:00Z"), &utc);
        assert!(fire.is_empty());
        assert!(state.started);
        let (fire, _) = due_now(&c, &state, at("2026-10-09T10:01:00Z"), &utc);
        assert!(fire.is_empty());
    }

    #[test]
    fn skips_completed_stale_and_back_dated_reminders() {
        let utc = TimeZone::UTC;
        let mut done = reminder("Reminder/done", Some("2026-10-09T09:00:00Z"), None);
        done.completed = true;
        let c = cache(vec![
            done,
            // Due 13 h ago: past the catch-up window.
            reminder("Reminder/stale", Some("2026-10-08T21:00:00Z"), None),
            // Made after its due time had passed.
            reminder("Reminder/backdated", Some("2026-10-09T09:00:00Z"), Some("2026-10-09T09:30:00Z")),
            // Synced late, but set ahead of time: still news.
            reminder("Reminder/late", Some("2026-10-09T09:58:00Z"), Some("2026-10-09T09:57:00Z")),
        ]);
        let started = Notified {
            started: true,
            ..Notified::default()
        };
        let (fire, state) = due_now(&c, &started, at("2026-10-09T10:00:00Z"), &utc);
        assert_eq!(titles(&fire), ["late"]);
        assert!(!state.fired.contains_key("Reminder/done"));
    }

    #[test]
    fn moving_the_due_time_makes_it_news_again() {
        let utc = TimeZone::UTC;
        let mut r = reminder("Reminder/call", Some("2026-10-09T10:00:00Z"), Some("2026-10-09T08:00:00Z"));
        let started = Notified {
            started: true,
            ..Notified::default()
        };
        let first = cache(vec![r.clone()]);
        let (fire, state) = due_now(&first, &started, at("2026-10-09T10:00:00Z"), &utc);
        assert_eq!(fire.len(), 1);
        // Snoozed to 10:30, from 10:05.
        r.due.as_mut().unwrap().wall_ms = at("2026-10-09T10:30:00Z").as_millisecond();
        r.modified_ms = Some(at("2026-10-09T10:05:00Z").as_millisecond());
        let c = cache(vec![r]);
        let (fire, state) = due_now(&c, &state, at("2026-10-09T10:20:00Z"), &utc);
        assert!(fire.is_empty());
        let (fire, _) = due_now(&c, &state, at("2026-10-09T10:30:00Z"), &utc);
        assert_eq!(fire.len(), 1);
    }

    #[test]
    fn the_body_names_the_list_and_the_time() {
        let helsinki = TimeZone::get("Europe/Helsinki").unwrap();
        let r = reminder("Reminder/milk", Some("2026-10-09T10:00:00Z"), None);
        let c = cache(vec![r.clone()]);
        assert_eq!(text(&r, &c, &helsinki), ("milk".into(), "Groceries · 10:00".into()));
        let mut all_day = r;
        all_day.due.as_mut().unwrap().all_day = true;
        assert_eq!(text(&all_day, &c, &helsinki).1, "Groceries · today");
    }
}
