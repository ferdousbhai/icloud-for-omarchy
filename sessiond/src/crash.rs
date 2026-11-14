//! Crash injection, for the tests that kill the daemon between two of its
//! durable writes or hold it there while another daemon starts. Debug
//! builds only: in a release build [`point`] is empty and no variable is
//! read.
//!
//! The points, each passed just after the step it names:
//!
//! - `set:` / `remove:` the keyring's `session`, `trust` or `password`;
//! - `write:` / `remove:` / `rename:` (moved aside) a state file, by its
//!   name: `account.json`, `session-change-pending`,
//!   `session-removal-pending`, `sign-out-reason`;
//! - `lock:change` / `unlock:change`: `change.lock`'s `flock` taken, about
//!   to be released;
//! - `captured:sign-in`, `captured:find-my`, `captured:autofill`: a
//!   window's output read, nothing of it stored yet;
//! - `release:name`: the idle exit gave up the bus name.
//!
//! - `ICLOUD_SESSIOND_CRASH_AT=<point>[:<n>]` kills the daemon (SIGKILL, as
//!   `kill -9` would: no destructor, no flush) the `n`th time (default 1)
//!   it passes `<point>`; `trace` logs every point passed instead.
//! - `ICLOUD_SESSIOND_PAUSE_AT=<point>[:<n>]@<file>` creates `<file>` there
//!   and waits until it is removed; several, separated by `,`, each count
//!   their own passes.

#[cfg(debug_assertions)]
pub fn point(op: &str, target: &str) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `<op>:<target>[:<n>]`, `n` from 1.
    fn parse(spec: &str) -> Option<(String, u64)> {
        let mut parts = spec.splitn(3, ':');
        let point = format!("{}:{}", parts.next()?, parts.next()?);
        let n = parts.next().map_or(Some(1), |n| n.parse().ok())?;
        Some((point, n))
    }
    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|s| !s.is_empty())
    }
    /// Whether this pass of `point` is the `n`th of `at`.
    fn hit(at: &Option<(String, u64)>, hits: &AtomicU64, point: &str) -> bool {
        at.as_ref()
            .is_some_and(|(at, n)| at == point && hits.fetch_add(1, Ordering::SeqCst) + 1 == *n)
    }

    static TRACE: OnceLock<bool> = OnceLock::new();
    static CRASH: OnceLock<Option<(String, u64)>> = OnceLock::new();
    type Pause = (Option<(String, u64)>, AtomicU64, String);
    static PAUSE: OnceLock<Vec<Pause>> = OnceLock::new();
    static CRASH_HITS: AtomicU64 = AtomicU64::new(0);

    let point = format!("{op}:{target}");
    if *TRACE.get_or_init(|| var("ICLOUD_SESSIOND_CRASH_AT").as_deref() == Some("trace")) {
        eprintln!("icloud-sessiond: crash point {point}");
    }
    let pauses = PAUSE.get_or_init(|| {
        let specs = var("ICLOUD_SESSIOND_PAUSE_AT").unwrap_or_default();
        (specs.split(',').filter_map(|spec| spec.split_once('@')))
            .map(|(at, file)| (parse(at), AtomicU64::new(0), file.to_string()))
            .collect()
    });
    for (at, hits, file) in pauses {
        if hit(at, hits, &point) {
            eprintln!("icloud-sessiond: paused at {point}");
            let _ = std::fs::write(file, "");
            while std::path::Path::new(file).exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    if hit(
        CRASH.get_or_init(|| parse(&var("ICLOUD_SESSIOND_CRASH_AT")?)),
        &CRASH_HITS,
        &point,
    ) {
        eprintln!("icloud-sessiond: crash injected at {point}");
        let pid = std::process::id().to_string();
        let _ = std::process::Command::new("kill").args(["-KILL", &pid]).status();
        // `kill` missing: still never past this point.
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn point(_op: &str, _target: &str) {}
