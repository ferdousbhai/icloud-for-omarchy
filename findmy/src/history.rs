//! Location history in SQLite at `~/.local/share/icloud-findmy/history.db`.
//!
//! One row per refresh per device, and only when the device moved: more than
//! [`MOVE_THRESHOLD_M`] from the last stored point, and more than the larger
//! of the two accuracy radii (so a stationary phone whose fix wobbles between
//! Wi-Fi and GPS does not draw a scribble). Rows older than
//! [`RETENTION_SECS`] are deleted on open and after every batch of inserts.
//!
//! Location history is private: the directory is kept `0700` and the
//! database (with its `-wal` and `-shm` files) `0600`, and both are fixed on
//! every open.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::models::{self, Device, Fix};

/// Minimum movement in metres before a new point is stored.
pub const MOVE_THRESHOLD_M: f64 = 25.0;

/// How long positions are kept: 30 days.
pub const RETENTION_SECS: i64 = 30 * 24 * 3600;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Unix seconds.
    pub ts: i64,
    pub lat: f64,
    pub lon: f64,
    pub accuracy: f64,
    pub battery: Option<f64>,
}

pub struct History {
    conn: Connection,
    pruned_on_open: usize,
}

/// `~/.local/share/icloud-findmy/history.db` (honours an absolute
/// `XDG_DATA_HOME`); `None` without either.
pub fn default_path() -> Option<PathBuf> {
    let absolute = |var| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let data =
        absolute("XDG_DATA_HOME").or_else(|| Some(absolute("HOME")?.join(".local/share")))?;
    Some(data.join("icloud-findmy").join("history.db"))
}

impl History {
    pub fn open_default() -> rusqlite::Result<History> {
        let path = default_path().ok_or_else(|| {
            rusqlite::Error::InvalidPath(PathBuf::from("~/.local/share/icloud-findmy/history.db"))
        })?;
        Self::open(&path)
    }

    /// Opens (creating if needed) the database at `path`, tightening the
    /// permissions of its directory and files and pruning old rows.
    pub fn open(path: &Path) -> rusqlite::Result<History> {
        let io = |e: std::io::Error| {
            rusqlite::Error::InvalidPath(PathBuf::from(format!("{}: {e}", path.display())))
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            DirBuilder::new()
                .recursive(true)
                .mode(DIR_MODE)
                .create(dir)
                .map_err(io)?;
            std::fs::set_permissions(dir, Permissions::from_mode(DIR_MODE)).map_err(io)?;
        }
        // Create the file 0600 before SQLite does; SQLite gives the -wal and
        // -shm files the database file's mode.
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(FILE_MODE)
            .open(path)
            .map_err(io)?;
        restrict_files(path).map_err(io)?;
        let history = Self::init(Connection::open(path)?)?;
        restrict_files(path).map_err(io)?;
        Ok(history)
    }

    pub fn open_in_memory() -> rusqlite::Result<History> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<History> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS history (
                 device_id TEXT NOT NULL,
                 ts        INTEGER NOT NULL,
                 lat       REAL NOT NULL,
                 lon       REAL NOT NULL,
                 accuracy  REAL NOT NULL,
                 battery   REAL
             );
             CREATE INDEX IF NOT EXISTS history_device_ts ON history(device_id, ts);",
        )?;
        let mut history = History {
            conn,
            pruned_on_open: 0,
        };
        history.pruned_on_open = history.prune(models::now_ms() / 1000)?;
        Ok(history)
    }

    /// Deletes rows older than [`RETENTION_SECS`] before `now` (Unix
    /// seconds). Returns rows deleted.
    pub fn prune(&self, now: i64) -> rusqlite::Result<usize> {
        self.conn.execute(
            "DELETE FROM history WHERE ts < ?1",
            params![now - RETENTION_SECS],
        )
    }

    /// Rows the prune on open deleted.
    pub fn pruned_on_open(&self) -> usize {
        self.pruned_on_open
    }

    /// Rows stored, for every device.
    pub fn count(&self) -> rusqlite::Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
    }

    /// The newest stored point for a device.
    pub fn last(&self, device_id: &str) -> rusqlite::Result<Option<Point>> {
        self.conn
            .query_row(
                "SELECT ts, lat, lon, accuracy, battery FROM history
                 WHERE device_id = ?1 ORDER BY ts DESC, rowid DESC LIMIT 1",
                params![device_id],
                row_to_point,
            )
            .optional()
    }

    /// Stores `fix` if the device moved since the last stored point.
    /// Returns whether a row was written.
    pub fn record(
        &self,
        device_id: &str,
        fix: &Fix,
        battery: Option<f64>,
    ) -> rusqlite::Result<bool> {
        let ts = fix.ts_ms / 1000;
        if let Some(last) = self.last(device_id)? {
            if ts <= last.ts {
                return Ok(false);
            }
            let moved = distance_m(last.lat, last.lon, fix.lat, fix.lon);
            if moved <= MOVE_THRESHOLD_M.max(last.accuracy.max(fix.accuracy)) {
                return Ok(false);
            }
        }
        self.conn.execute(
            "INSERT INTO history (device_id, ts, lat, lon, accuracy, battery)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![device_id, ts, fix.lat, fix.lon, fix.accuracy, battery],
        )?;
        Ok(true)
    }

    /// Records every device with a current (not old) fix, then prunes rows
    /// past the retention as of `now` (Unix seconds). Returns rows written.
    pub fn record_devices(&self, devices: &[Device], now: i64) -> rusqlite::Result<usize> {
        let mut written = 0;
        for d in devices {
            if let Some(fix) = d.location.as_ref().filter(|f| !f.is_old && f.ts_ms > 0)
                && self.record(&d.id, fix, d.battery)?
            {
                written += 1;
            }
        }
        self.prune(now)?;
        Ok(written)
    }

    /// A device's trail since `since` (Unix seconds), oldest first.
    pub fn trail(&self, device_id: &str, since: i64) -> rusqlite::Result<Vec<Point>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT ts, lat, lon, accuracy, battery FROM history
             WHERE device_id = ?1 AND ts >= ?2 ORDER BY ts, rowid",
        )?;
        stmt.query_map(params![device_id, since], row_to_point)?
            .collect()
    }
}

/// Sets the database and its `-wal` / `-shm` files (where present) to 0600.
fn restrict_files(path: &Path) -> std::io::Result<()> {
    std::fs::set_permissions(path, Permissions::from_mode(FILE_MODE))?;
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        match std::fs::set_permissions(&side, Permissions::from_mode(FILE_MODE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

fn row_to_point(r: &rusqlite::Row) -> rusqlite::Result<Point> {
    Ok(Point {
        ts: r.get(0)?,
        lat: r.get(1)?,
        lon: r.get(2)?,
        accuracy: r.get(3)?,
        battery: r.get(4)?,
    })
}

/// Great-circle distance in metres (haversine).
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_008.8;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}
