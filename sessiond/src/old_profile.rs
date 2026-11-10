//! The WebKit profile sign-in windows up to v0.9.4 kept: its
//! `cookies.sqlite` (libsoup's `moz_cookies` table) held the whole web
//! session in plaintext, and the only copy of Apple's trust token (the
//! idmsa.apple.com `DES…` cookie; the session's jar keeps icloud.com's
//! cookies only). The daemon reads the device trust out of it once, into
//! the keyring, and deletes the rest.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::cookies::{self, Cookie};

/// The device-trust cookies in the profile's `cookies.sqlite` that have not
/// expired at `now` (see [`cookies::trust_cookies`]). Only persistent
/// cookies are ever written there, each with its expiry.
pub fn trust(db: &Path, now: u64) -> Result<Vec<Cookie>, String> {
    let conn = open(db)?;
    let mut rows = conn
        .prepare("SELECT name, value, host, path, expiry FROM moz_cookies")
        .map_err(|e| format!("{}: {e}", db.display()))?;
    let all = rows
        .query_map([], |r| {
            let expiry: i64 = r.get(4)?;
            Ok(Cookie {
                name: r.get(0)?,
                value: r.get(1)?,
                domain: r.get(2)?,
                path: r.get(3)?,
                expires: u64::try_from(expiry).ok(),
            })
        })
        .and_then(|cookies| cookies.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("{}: {e}", db.display()))?;
    Ok(cookies::trust_cookies(all, now))
}

/// Deletes every cookie in the profile's `cookies.sqlite` but the device
/// trust that has not expired at `now`, and what SQLite kept of them in
/// free pages.
pub fn keep_only_trust(db: &Path, now: u64) -> Result<(), String> {
    let conn = open(db)?;
    let names: Vec<(String, String, i64, i64)> = conn
        .prepare("SELECT name, host, id, expiry FROM moz_cookies")
        .and_then(|mut rows| {
            rows.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect()
        })
        .map_err(|e| format!("{}: {e}", db.display()))?;
    for (name, host, id, expiry) in names {
        if !cookies::is_trust_cookie(&name, &host) || u64::try_from(expiry).map_or(true, |e| e <= now) {
            conn.execute("DELETE FROM moz_cookies WHERE id = ?1", [id])
                .map_err(|e| format!("{}: {e}", db.display()))?;
        }
    }
    conn.execute_batch("VACUUM")
        .map_err(|e| format!("{}: {e}", db.display()))
}

fn open(db: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| format!("{}: {e}", db.display()))
}
