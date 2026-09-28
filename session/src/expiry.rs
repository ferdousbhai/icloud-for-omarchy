//! How long the iCloud sign-in lasts, read from the Chromium profile
//! icloud-md signs in with (port of icloud-notes' `signin.h`).
//!
//! A "Keep me signed in" sign-in stores `X-APPLE-WEBAUTH-TOKEN` as a
//! persistent cookie (30 days); any other sign-in keeps it for the browser
//! session only, and then there is no expiry to report.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

const QUERY: &str = "SELECT MAX(expires_utc) FROM cookies WHERE host_key = '.icloud.com' \
                     AND name = 'X-APPLE-WEBAUTH-TOKEN' AND is_persistent = 1";

/// Seconds from 1601-01-01 (Chromium's epoch) to 1970-01-01.
const CHROMIUM_TO_UNIX_SECS: u64 = 11_644_473_600;

/// How deep below a directory to look for `Cookies` files
/// (`browser-profile/Default/Network/Cookies` is four levels).
const MAX_DEPTH: usize = 4;

/// Chromium stores times as microseconds since 1601-01-01 UTC.
pub fn from_chromium_time(micros: i64) -> SystemTime {
    let chromium_epoch = UNIX_EPOCH - Duration::from_secs(CHROMIUM_TO_UNIX_SECS);
    if micros >= 0 {
        chromium_epoch + Duration::from_micros(micros as u64)
    } else {
        chromium_epoch - Duration::from_micros(micros.unsigned_abs())
    }
}

/// When the persistent sign-in cookie in one Chromium `Cookies` database
/// expires. `None` when it has none or the database cannot be read.
///
/// The database is opened read-only. Chromium holds it with an exclusive
/// lock while running, so on failure the file is copied aside and read there.
pub fn token_expiry(cookies_db: &Path) -> Option<SystemTime> {
    let micros = match query(cookies_db) {
        Ok(micros) => micros,
        Err(_) => query_copy(cookies_db)?,
    };
    micros.map(from_chromium_time)
}

/// The latest such expiry across every `Cookies` database under `dir`
/// (an account directory, or the whole `accounts` directory).
pub fn latest_token_expiry(dir: &Path) -> Option<SystemTime> {
    let mut found = Vec::new();
    find_cookie_dbs(dir, 0, &mut found);
    found.iter().filter_map(|db| token_expiry(db)).max()
}

fn query(path: &Path) -> rusqlite::Result<Option<i64>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(Duration::from_millis(200))?;
    conn.query_row(QUERY, [], |row| row.get::<_, Option<i64>>(0))
}

fn query_copy(path: &Path) -> Option<Option<i64>> {
    let dir = std::env::temp_dir().join(format!(
        "icloud-session-cookies-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    crate::store::create_private_dir(&dir).ok()?;
    let result = (|| {
        let copy = dir.join("Cookies");
        fs::copy(path, &copy).ok()?;
        for suffix in ["-journal", "-wal"] {
            let mut side = path.as_os_str().to_os_string();
            side.push(suffix);
            let _ = fs::copy(&side, dir.join(format!("Cookies{suffix}")));
        }
        query(&copy).ok()
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

fn find_cookie_dbs(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_file() && entry.file_name() == "Cookies" {
            found.push(path);
        } else if kind.is_dir() && depth < MAX_DEPTH {
            find_cookie_dbs(&path, depth + 1, found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_epoch_conversion() {
        assert_eq!(from_chromium_time(CHROMIUM_TO_UNIX_SECS as i64 * 1_000_000), UNIX_EPOCH);
        // 2026-10-28T09:00:00Z
        let unix = 1_793_178_000u64;
        let micros = (unix + CHROMIUM_TO_UNIX_SECS) as i64 * 1_000_000;
        assert_eq!(from_chromium_time(micros), UNIX_EPOCH + Duration::from_secs(unix));
    }
}
