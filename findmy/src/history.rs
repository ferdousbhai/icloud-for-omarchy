//! Location history in SQLite at `~/.local/share/icloud-findmy/history.db`.
//!
//! One row per refresh per device, and only when the device moved: more than
//! [`MOVE_THRESHOLD_M`] from the last stored point, and more than the smaller
//! of the two accuracy radii (so a stationary phone whose fix wobbles between
//! Wi-Fi and GPS does not draw a scribble).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::models::{Device, Fix};

/// Minimum movement in metres before a new point is stored.
pub const MOVE_THRESHOLD_M: f64 = 25.0;

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
}

/// `~/.local/share/icloud-findmy/history.db` (honours `XDG_DATA_HOME`).
pub fn default_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("icloud-findmy").join("history.db"))
}

impl History {
    pub fn open_default() -> rusqlite::Result<History> {
        let path = default_path().ok_or_else(|| {
            rusqlite::Error::InvalidPath(PathBuf::from("~/.local/share/icloud-findmy/history.db"))
        })?;
        Self::open(&path)
    }

    pub fn open(path: &Path) -> rusqlite::Result<History> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
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
        Ok(History { conn })
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
    pub fn record(&self, device_id: &str, fix: &Fix, battery: Option<f64>) -> rusqlite::Result<bool> {
        let ts = fix.ts_ms / 1000;
        if let Some(last) = self.last(device_id)? {
            if ts <= last.ts {
                return Ok(false);
            }
            let moved = distance_m(last.lat, last.lon, fix.lat, fix.lon);
            if moved <= MOVE_THRESHOLD_M.max(last.accuracy.min(fix.accuracy)) {
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

    /// Records every device with a current (not old) fix. Returns rows written.
    pub fn record_devices(&self, devices: &[Device]) -> rusqlite::Result<usize> {
        let mut written = 0;
        for d in devices {
            if let Some(fix) = d.location.as_ref().filter(|f| !f.is_old && f.ts_ms > 0)
                && self.record(&d.id, fix, d.battery)?
            {
                written += 1;
            }
        }
        Ok(written)
    }

    /// A device's trail since `since` (Unix seconds), oldest first.
    pub fn trail(&self, device_id: &str, since: i64) -> rusqlite::Result<Vec<Point>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT ts, lat, lon, accuracy, battery FROM history
             WHERE device_id = ?1 AND ts >= ?2 ORDER BY ts, rowid",
        )?;
        stmt.query_map(params![device_id, since], row_to_point)?.collect()
    }
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
