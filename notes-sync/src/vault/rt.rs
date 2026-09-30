//! Clock and randomness, with the differential harness's hooks
//! (`tests/differential/README.md`, "Determinism"):
//!
//! - `ICLOUD_NOTES_SYNC_NOW=<ms>` freezes [`now_ms`] (icloud-md's
//!   `Date.now()` / `new Date()` under the driver's `--now`);
//! - `ICLOUD_NOTES_SYNC_DETERMINISTIC=1` makes [`random_uuid`] return
//!   `00000000-0000-4000-8000-<n as 12 hex digits>` on its n-th call
//!   (1-based) and [`random_bytes`] return bytes `(m + j) & 0xff` on its
//!   m-th call - one process-wide counter each, like the driver's.
//!
//! Every random draw icloud-md makes goes through `randomUUID` or
//! `randomBytes`, and the Rust side must draw from these two functions in
//! the same order for request bodies and ids to compare equal. The codec
//! (formatReconcile's todo uuids, mergeableDataPool/tableEdit's replica
//! uuids) draws from here too.

use std::sync::atomic::{AtomicU64, Ordering};

pub const NOW_ENV: &str = "ICLOUD_NOTES_SYNC_NOW";
pub const DETERMINISTIC_ENV: &str = "ICLOUD_NOTES_SYNC_DETERMINISTIC";

static UUID_COUNTER: AtomicU64 = AtomicU64::new(0);
static BYTES_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `Date.now()`.
pub fn now_ms() -> i64 {
    if let Some(ms) = std::env::var(NOW_ENV).ok().and_then(|v| v.trim().parse::<i64>().ok()) {
        return ms;
    }
    icloud_session::time::now_ms()
}

fn deterministic() -> bool {
    std::env::var(DETERMINISTIC_ENV).is_ok_and(|v| v == "1")
}

/// `crypto.randomUUID()`: lowercase, hyphenated, version 4.
pub fn random_uuid() -> String {
    if deterministic() {
        let n = UUID_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
        return format!("00000000-0000-4000-8000-{n:012x}");
    }
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// `crypto.randomBytes(k)`.
pub fn random_bytes(k: usize) -> Vec<u8> {
    if deterministic() {
        let m = BYTES_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
        return (0..k).map(|j| ((m + j as u64) & 0xff) as u8).collect();
    }
    let mut out = Vec::with_capacity(k);
    while out.len() < k {
        out.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    out.truncate(k);
    out
}

/// `random_bytes(16)` as a fixed array.
pub fn random_16() -> [u8; 16] {
    let bytes = random_bytes(16);
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    out
}
