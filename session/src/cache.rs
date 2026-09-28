//! The shared validate cache, `<cache dir>/<dsid>.json`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Result, store};

/// Reuse a `/validate` result for this long: under the browser's own
/// 14 minute heartbeat, so the machine rotates the token at most about once
/// per interval from our side, however many apps are open.
pub const MAX_AGE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Cache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) validated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) webservices: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) apple_id: Option<String>,
    /// Set when Apple answered 421/401; `status` reports signed out until
    /// the session file is rewritten after this moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sign_in_required_at: Option<String>,
}

impl Cache {
    /// A missing or unreadable cache is an empty one: it only saves calls.
    pub(crate) fn read(path: &Path) -> Cache {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            store::create_private_dir(dir)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self).expect("cache always serializes");
        bytes.push(b'\n');
        store::write_atomic(path, &bytes)
    }

    pub(crate) fn validated_at(&self) -> Option<SystemTime> {
        self.validated_at.as_deref().and_then(parse_time)
    }

    pub(crate) fn sign_in_required_at(&self) -> Option<SystemTime> {
        self.sign_in_required_at.as_deref().and_then(parse_time)
    }

    /// The cached webservices, if validated less than [`MAX_AGE`] ago and
    /// not contradicted by a later 421/401.
    pub(crate) fn fresh_webservices(&self, now: SystemTime) -> Option<&HashMap<String, String>> {
        let validated = self.validated_at()?;
        let age = now.duration_since(validated).ok()?;
        if age >= MAX_AGE {
            return None;
        }
        if self.sign_in_required_at().is_some_and(|marked| marked >= validated) {
            return None;
        }
        self.webservices.as_ref()
    }
}

pub(crate) fn parse_time(s: &str) -> Option<SystemTime> {
    humantime::parse_rfc3339(s).ok()
}

/// Largest time humantime can format (9999-12-31T23:59:59Z).
const MAX_FORMATTABLE: Duration = Duration::from_secs(253_402_300_799);

fn clamp(t: SystemTime) -> SystemTime {
    t.clamp(UNIX_EPOCH, UNIX_EPOCH + MAX_FORMATTABLE)
}

/// RFC 3339 UTC, whole seconds: `2026-10-28T09:00:00Z`.
pub fn format_time(t: SystemTime) -> String {
    humantime::format_rfc3339_seconds(clamp(t)).to_string()
}

/// RFC 3339 UTC with milliseconds, for markers compared against file mtimes.
pub(crate) fn format_time_millis(t: SystemTime) -> String {
    humantime::format_rfc3339_millis(clamp(t)).to_string()
}
