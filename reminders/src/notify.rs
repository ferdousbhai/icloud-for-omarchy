//! Desktop notifications for due reminders, through Omarchy's own
//! `omarchy-notification-send` (its look, its glyph, click to open).
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
const APP_NAME: &str = "Reminders";

/// The reminders to notify about now, oldest due first, and the state
/// that records them (and forgets reminders that no longer wait).
pub fn due_now<'c>(cache: &'c Cache, state: &Notified, now: Timestamp, local: &TimeZone) -> (Vec<&'c Reminder>, Notified) {
    let now_ms = now.as_millisecond();
    // A run is the first until the cache has synced for this account: one
    // that saw an empty cache (signed out, offline) or another account's
    // marks nothing, so the backlog would otherwise fire after sign-in.
    let started = state.started && state.account == cache.account;
    let mut next = Notified {
        started: cache.synced_ms.is_some(),
        account: cache.account.clone(),
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
        let fresh = started
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

/// The notification's two lines: the title, then the list (when the cache
/// knows it) and the time.
pub fn text(r: &Reminder, cache: &Cache, local: &TimeZone) -> (String, String) {
    let when = match &r.due {
        Some(d) if d.all_day => "today".to_owned(),
        Some(d) => d.instant(local).to_zoned(local.clone()).time().strftime("%H:%M").to_string(),
        None => String::new(),
    };
    let body = match cache.list(&r.list_id) {
        Some(l) => format!("{} · {when}", l.name),
        None => when,
    };
    (r.title.clone(), body)
}

/// Shows one notification with Omarchy's `omarchy-notification-send`,
/// found on `PATH` (the omarchy package installs it in /usr/bin, and the
/// systemd user manager that runs the timer has Omarchy's `PATH`);
/// clicking it opens the app.
pub fn send(headline: &str, body: &str) -> Result<(), String> {
    let (headline, body) = (positional(headline), positional(body));
    let status = Command::new("omarchy-notification-send")
        .args(["--app-name", APP_NAME, "-g", GLYPH, "-u", "normal", &headline, &body])
        .args(["--exec", "icloud-reminders-app"])
        .status()
        .map_err(|e| format!("cannot run omarchy-notification-send (is Omarchy installed?): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("omarchy-notification-send failed ({status})"))
    }
}

/// Text the notifier reads as a value, never an option: a title such as
/// `-u` gets an invisible word joiner (U+2060) in front.
fn positional(text: &str) -> String {
    if text.starts_with('-') { format!("\u{2060}{text}") } else { text.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_like_an_option_stays_a_title() {
        assert_eq!(positional("-u"), "\u{2060}-u");
        assert_eq!(positional("Milk -2"), "Milk -2");
    }

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
        let mut c = Cache {
            synced_ms: Some(0),
            ..Cache::default()
        };
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
}
