//! Due dates. A reminder's `DueDate` is a wall-clock time stored in a
//! CloudKit timestamp as though it were UTC: 09:00 is `…T09:00:00Z`
//! whatever the zone. `TimeZone` (an IANA name), when present, anchors that
//! wall clock; without it the time floats in the zone of whichever device
//! shows it. `AllDay` makes it a date. (Psavvas/iCloud-Reminders-for-Windows
//! `docs/protocol-findings.md`, from live-account probes.)
//!
//! UNVERIFIED AGAINST APPLE here: that an all-day `DueDate` is the date's
//! midnight in that UTC encoding, and that a timed reminder written with
//! the system's zone in `TimeZone` shows at the same instant on an iPhone
//! in another zone.

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

/// When an all-day reminder notifies, in local time (Reminders' own
/// default for all-day alerts is 09:00).
pub const ALL_DAY_HOUR: i8 = 9;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Due {
    /// `DueDate` as stored: the wall clock read as UTC, in ms.
    pub wall_ms: i64,
    pub all_day: bool,
    /// `TimeZone`: the zone the wall clock is in; `None` floats (local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

impl Due {
    /// The stored wall clock.
    pub fn wall(&self) -> DateTime {
        Timestamp::from_millisecond(self.wall_ms)
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_zoned(TimeZone::UTC)
            .datetime()
    }

    pub fn date(&self) -> Date {
        self.wall().date()
    }

    /// The zone the wall clock is read in: `TimeZone`, else `local`. An
    /// unknown zone name floats too.
    fn zone(&self, local: &TimeZone) -> TimeZone {
        self.time_zone
            .as_deref()
            .and_then(|name| TimeZone::get(name).ok())
            .unwrap_or_else(|| local.clone())
    }

    /// The moment it is due (an all-day one at [`ALL_DAY_HOUR`] local),
    /// which is when it notifies.
    pub fn instant(&self, local: &TimeZone) -> Timestamp {
        let at = if self.all_day {
            self.date().at(ALL_DAY_HOUR, 0, 0, 0).to_zoned(local.clone())
        } else {
            self.wall().to_zoned(self.zone(local))
        };
        at.map(|z| z.timestamp()).unwrap_or(Timestamp::UNIX_EPOCH)
    }

    /// A date, or a date and time in `local` ("2026-10-10 09:00"): how the
    /// command line and the window show it.
    pub fn display(&self, local: &TimeZone) -> String {
        if self.all_day {
            return self.date().to_string();
        }
        let z = self.instant(local).to_zoned(local.clone());
        format!("{} {}", z.date(), z.time().strftime("%H:%M"))
    }

    /// A timed reminder at `wall` in `zone` (anchored to it when it has an
    /// IANA name).
    pub fn at(wall: DateTime, zone: &TimeZone) -> Due {
        Due {
            wall_ms: wall_as_utc(wall),
            all_day: false,
            time_zone: zone.iana_name().map(str::to_owned),
        }
    }

    pub fn on(date: Date) -> Due {
        Due {
            wall_ms: wall_as_utc(date.at(0, 0, 0, 0)),
            all_day: true,
            time_zone: None,
        }
    }
}

fn wall_as_utc(wall: DateTime) -> i64 {
    wall.to_zoned(TimeZone::UTC)
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or(0)
}

/// Parses `--due`: `2026-10-10`, `2026-10-10 17:30` (or `T17:30`),
/// `today`, `tomorrow`, either followed by a time, a bare `17:30` (today),
/// or `+30m`, `+2h`, `+3d` from `now`. A date alone is all-day.
pub fn parse_when(s: &str, now: &Zoned) -> Result<Due, String> {
    let bad = || {
        format!(
            "wants a due date like 2026-10-10, \"2026-10-10 17:30\", today, \"tomorrow 9:00\", 17:30 or +2h, not \"{s}\""
        )
    };
    let s = s.trim();
    let zone = now.time_zone();
    if let Some(rel) = s.strip_prefix('+') {
        let (num, unit) = rel.split_at(rel.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?);
        let n: i64 = num.parse().map_err(|_| bad())?;
        let span = match unit {
            "m" | "min" => n.minutes(),
            "h" => n.hours(),
            "d" => n.days(),
            _ => return Err(bad()),
        };
        let at = now.checked_add(span).map_err(|_| bad())?;
        let wall = at.datetime().round(jiff::Unit::Minute).map_err(|_| bad())?;
        return Ok(Due::at(wall, zone));
    }
    // "2026-10-10T17:30" is two words.
    let iso = s
        .split_once('T')
        .filter(|(date, _)| date.parse::<Date>().is_ok())
        .map(|(date, time)| format!("{date} {time}"));
    let mut words = iso.as_deref().unwrap_or(s).split_whitespace();
    let first = words.next().ok_or_else(bad)?;
    let (date, time) = match first.to_ascii_lowercase().as_str() {
        "today" => (now.date(), words.next()),
        "tomorrow" => (now.date().tomorrow().map_err(|_| bad())?, words.next()),
        w if w.contains(':') => (now.date(), Some(first)),
        _ => (first.parse::<Date>().map_err(|_| bad())?, words.next()),
    };
    if words.next().is_some() {
        return Err(bad());
    }
    match time {
        None => Ok(Due::on(date)),
        Some(t) => Ok(Due::at(date.to_datetime(parse_time(t).ok_or_else(bad)?), zone)),
    }
}

/// `9:00`, `17:30`.
fn parse_time(s: &str) -> Option<Time> {
    let (h, m) = s.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    Time::new(h.parse().ok()?, m.parse().ok()?, 0, 0).ok()
}

/// The machine's zone (`TZ`, else `/etc/localtime`), UTC if neither.
pub fn local_zone() -> TimeZone {
    TimeZone::try_system().unwrap_or(TimeZone::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helsinki() -> TimeZone {
        TimeZone::get("Europe/Helsinki").unwrap()
    }

    fn now() -> Zoned {
        // Friday 2026-10-09 14:20 in Helsinki (UTC+3).
        "2026-10-09T14:20:00+03:00[Europe/Helsinki]".parse().unwrap()
    }

    #[test]
    fn the_wall_clock_is_stored_as_utc() {
        let due = parse_when("2026-10-10 09:00", &now()).unwrap();
        assert_eq!(due.wall_ms, 1_791_622_800_000); // 2026-10-10T09:00:00Z
        assert!(!due.all_day);
        assert_eq!(due.time_zone.as_deref(), Some("Europe/Helsinki"));
        // ...and is due at 09:00 Helsinki, 06:00 UTC.
        assert_eq!(due.instant(&helsinki()).to_string(), "2026-10-10T06:00:00Z");
        assert_eq!(due.display(&helsinki()), "2026-10-10 09:00");
    }

    #[test]
    fn an_anchored_time_is_shown_in_local_time() {
        let due = Due {
            wall_ms: 1_791_622_800_000,
            all_day: false,
            time_zone: Some("America/New_York".into()),
        };
        // 09:00 in New York (UTC-4) is 16:00 in Helsinki.
        assert_eq!(due.display(&helsinki()), "2026-10-10 16:00");
        // Floating: 09:00 wherever it is shown.
        let floating = Due { time_zone: None, ..due };
        assert_eq!(floating.display(&helsinki()), "2026-10-10 09:00");
        assert_eq!(floating.display(&TimeZone::UTC), "2026-10-10 09:00");
    }

    #[test]
    fn all_day_is_a_date_that_notifies_in_the_morning() {
        let due = parse_when("2026-10-12", &now()).unwrap();
        assert!(due.all_day);
        assert_eq!(due.time_zone, None);
        assert_eq!(due.wall_ms, 1_791_763_200_000); // 2026-10-12T00:00:00Z
        assert_eq!(due.display(&helsinki()), "2026-10-12");
        assert_eq!(due.instant(&helsinki()).to_string(), "2026-10-12T06:00:00Z");
    }

    #[test]
    fn words_times_and_offsets() {
        let n = now();
        let show = |s: &str| parse_when(s, &n).unwrap().display(&helsinki());
        assert_eq!(show("today"), "2026-10-09");
        assert_eq!(show("tomorrow"), "2026-10-10");
        assert_eq!(show("Tomorrow 9:00"), "2026-10-10 09:00");
        assert_eq!(show("17:30"), "2026-10-09 17:30");
        assert_eq!(show("2026-10-10T17:30"), "2026-10-10 17:30");
        assert_eq!(show("+30m"), "2026-10-09 14:50");
        assert_eq!(show("+2h"), "2026-10-09 16:20");
        assert_eq!(show("+3d"), "2026-10-12 14:20");
        for bad in ["", "soon", "2026-13-01", "25:00", "9", "today 9", "+2y", "+h", "today 9:00 x"] {
            assert!(parse_when(bad, &n).is_err(), "{bad}");
        }
    }

    #[test]
    fn across_a_dst_change_the_wall_clock_holds() {
        // Helsinki leaves summer time on 2026-10-25: 09:00 stays 09:00.
        let due = parse_when("2026-10-26 09:00", &now()).unwrap();
        assert_eq!(due.instant(&helsinki()).to_string(), "2026-10-26T07:00:00Z");
    }
}
