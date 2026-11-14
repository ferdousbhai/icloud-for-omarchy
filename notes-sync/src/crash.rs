//! Named crash points, for the tests that kill a run part way
//! (`tests/crash_points.rs`): every durable write clone, pull and push make,
//! and every CloudKit request that changes iCloud, calls [`point`] right
//! after it. In a debug build, `ICLOUD_NOTES_SYNC_CRASH_AT=<point>[:<n>]`
//! aborts the process at the `n`th time (1 by default) that point is reached,
//! as a run killed there would stop: nothing after it runs, no destructor,
//! no state write. `ICLOUD_NOTES_SYNC_CRASH_TRACE=<file>` appends each point
//! reached to `file`, one name a line, which is how the tests find every
//! place a run can be cut short. A release build has neither: [`point`] is
//! empty and reads no environment.

/// `ICLOUD_NOTES_SYNC_CRASH_AT`.
pub const CRASH_AT_ENV: &str = "ICLOUD_NOTES_SYNC_CRASH_AT";
/// `ICLOUD_NOTES_SYNC_CRASH_TRACE`.
pub const CRASH_TRACE_ENV: &str = "ICLOUD_NOTES_SYNC_CRASH_TRACE";
/// `ICLOUD_NOTES_SYNC_DOWNLOADS_IN_LINE`: attachment downloads run one at a
/// time, when queued, as over a transport that can't overlap them, so a
/// test cuts a run short at the same place every time.
pub const DOWNLOADS_IN_LINE_ENV: &str = "ICLOUD_NOTES_SYNC_DOWNLOADS_IN_LINE";

/// Whether [`DOWNLOADS_IN_LINE_ENV`] is set (a debug build only).
pub fn downloads_in_line() -> bool {
    cfg!(debug_assertions) && std::env::var_os(DOWNLOADS_IN_LINE_ENV).is_some_and(|v| !v.is_empty())
}

/// state.json written (it is not when unchanged).
pub const STATE: &str = "state";
/// A note file written.
pub const NOTE_FILE: &str = "note-file";
/// A base copy written.
pub const BASE_COPY: &str = "base-copy";
/// A history snapshot written.
pub const HISTORY_SNAPSHOT: &str = "history-snapshot";
/// A history epoch written.
pub const HISTORY_EPOCH: &str = "history-epoch";
/// An attachment file downloaded into place.
pub const ATTACHMENT_DOWNLOAD: &str = "attachment-download";
/// A file or directory renamed in the vault: a note or an attachment moved
/// to another folder or renamed for its title.
pub const RENAME: &str = "rename";
/// A file removed: a note file, an attachment, a base copy, a pruned
/// history file.
pub const FILE_REMOVE: &str = "file-remove";
/// An atomic write's temporary file written and synced, not yet renamed
/// into place.
pub const TEMP_FILE: &str = "temp-file";
/// state.json written with a create announced in `pendingCreates`.
pub const PENDING_CREATES_WRITE: &str = "pending-creates-write";
/// state.json written with `pendingCreates` cleared.
pub const PENDING_CREATES_CLEAR: &str = "pending-creates-clear";
/// A `records/modify` answered: iCloud has the change, nothing here
/// recorded it yet.
pub const ICLOUD_MODIFY: &str = "icloud-modify";
/// An asset upload answered.
pub const ICLOUD_UPLOAD: &str = "icloud-upload";
/// A step of the layout 3 → 4 migration done: the backup copied, renamed
/// into place, `.icloud-notes/` made, a state subdirectory moved, the
/// tombstone written.
pub const MIGRATE: &str = "migrate";

/// Every point.
pub const POINTS: &[&str] = &[
    STATE,
    NOTE_FILE,
    BASE_COPY,
    HISTORY_SNAPSHOT,
    HISTORY_EPOCH,
    ATTACHMENT_DOWNLOAD,
    RENAME,
    FILE_REMOVE,
    TEMP_FILE,
    PENDING_CREATES_WRITE,
    PENDING_CREATES_CLEAR,
    ICLOUD_MODIFY,
    ICLOUD_UPLOAD,
    MIGRATE,
];

/// The point `name` was reached (see the module docs).
#[inline]
pub fn point(name: &'static str) {
    #[cfg(debug_assertions)]
    reached(name);
    #[cfg(not(debug_assertions))]
    let _ = name;
}

#[cfg(debug_assertions)]
fn reached(name: &'static str) {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;

    static HITS: Mutex<Option<HashMap<&'static str, u64>>> = Mutex::new(None);
    if let Some(trace) = std::env::var_os(CRASH_TRACE_ENV).filter(|v| !v.is_empty())
        && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(trace)
    {
        // One write per line: with O_APPEND it lands whole, so the download
        // pool and the main thread never interleave their points.
        let _ = file.write_all(format!("{name}\n").as_bytes());
    }
    let Some(spec) = std::env::var(CRASH_AT_ENV).ok().filter(|v| !v.is_empty()) else {
        return;
    };
    let (wanted, nth) = match spec.split_once(':') {
        Some((wanted, n)) => (wanted.to_owned(), n.parse::<u64>().unwrap_or(1)),
        None => (spec, 1),
    };
    if wanted != name {
        return;
    }
    let mut hits = HITS.lock().unwrap_or_else(|e| e.into_inner());
    let count = hits.get_or_insert_with(HashMap::new).entry(name).or_default();
    *count += 1;
    if *count == nth {
        eprintln!("icloud-notes-sync: crash point {name}:{nth}");
        // No core dump for a crash asked for.
        let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
        std::process::abort();
    }
}
