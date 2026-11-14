//! Crash injection for the tests (`tests/crash.rs`): named points between
//! a write and the catalog update that records it, and between a request to
//! iCloud and the catalog update after it. In a debug build,
//! `ICLOUD_PHOTOS_CRASH_AT=<point>[:<n>]` aborts the process the `n`th time
//! (default the first) it reaches `<point>`. A release build has no hook.

/// The environment variable naming the point.
pub const VAR: &str = "ICLOUD_PHOTOS_CRASH_AT";

/// Abort here if [`VAR`] names this point (debug builds only).
#[inline]
pub fn point(name: &str) {
    #[cfg(debug_assertions)]
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static HITS: AtomicUsize = AtomicUsize::new(0);
        let Some(want) = std::env::var_os(VAR) else { return };
        let want = want.to_string_lossy();
        let (at, nth) = match want.rsplit_once(':').map(|(p, n)| (p, n.parse::<usize>())) {
            Some((p, Ok(n))) => (p, n.max(1)),
            _ => (&*want, 1),
        };
        if at == name && HITS.fetch_add(1, Ordering::SeqCst) + 1 == nth {
            eprintln!("icloud-photos: crashing at {name} ({VAR})");
            // No core dump for a crash the tests asked for.
            // SAFETY: prctl with these arguments only changes a flag of this process.
            unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
            std::process::abort();
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = name;
}
