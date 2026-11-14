//! Named points at every durable step of a sync (each write to the state,
//! each request that changes iCloud, each change to the folder), where a
//! test can stop the run as a kill would: [`arm`] it to panic at the `n`th
//! point reached, run a sync inside `catch_unwind`, and the folder, iCloud
//! and the state are left exactly as that step left them (an open SQLite
//! transaction rolls back, as it would). Only in debug builds: in a release
//! build [`point`] is empty and nothing is ever armed.

/// Every point, for tests that check each is reached.
pub const POINTS: &[&str] = &[
    // The state.
    "state.store_remote",
    "state.set_meta",
    "state.keep_ignored",
    "state.ignore",
    "state.set_not_synced",
    "state.set_pending_move",
    "state.clear_pending_move",
    "state.put",
    "state.set_placed",
    "state.clear_placed",
    "state.set_sending",
    "state.clear_sending",
    "state.forget",
    "state.forget_under",
    "state.move_paths",
    // iCloud, once it has done what it was asked.
    "remote.create_folder",
    "remote.upload_new",
    "remote.replace",
    "remote.move_to",
    "remote.rename",
    "remote.trash",
    // The folder.
    "local.create_root",
    "local.mkdir",
    "local.rename",
    "local.set_aside",
    "local.download_temp",
    "local.download_rename",
    "local.trash_claimed",
    "local.trash_moved",
    "local.trash",
];

/// The panic payload of a stop at a point.
#[derive(Debug)]
pub struct Crash(pub &'static str);

#[cfg(debug_assertions)]
#[derive(Default)]
struct Hook {
    at: Option<usize>,
    named: Option<&'static str>,
    seen: Vec<&'static str>,
}

#[cfg(debug_assertions)]
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// A durable step just happened.
#[inline]
pub fn point(name: &'static str) {
    #[cfg(debug_assertions)]
    {
        debug_assert!(POINTS.contains(&name), "{name} is not in crash::POINTS");
        let stop = HOOK.with_borrow_mut(|hook| {
            let hook = hook.as_mut()?;
            let n = hook.seen.len();
            hook.seen.push(name);
            (hook.at == Some(n) || hook.named == Some(name)).then_some(name)
        });
        if let Some(name) = stop {
            std::panic::panic_any(Crash(name));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = name;
}

/// From now on this thread notes every point it reaches, and with `at`
/// stops at the point of that index (from 0).
pub fn arm(at: Option<usize>) {
    #[cfg(debug_assertions)]
    HOOK.with_borrow_mut(|hook| *hook = Some(Hook { at, ..Hook::default() }));
    #[cfg(not(debug_assertions))]
    let _ = at;
}

/// As [`arm`], stopping at the first point of this name.
pub fn arm_named(name: &'static str) {
    #[cfg(debug_assertions)]
    HOOK.with_borrow_mut(|hook| {
        *hook = Some(Hook {
            named: Some(name),
            ..Hook::default()
        })
    });
    #[cfg(not(debug_assertions))]
    let _ = name;
}

/// Stops noting points; the ones reached since [`arm`], in order.
pub fn disarm() -> Vec<&'static str> {
    #[cfg(debug_assertions)]
    return HOOK.with_borrow_mut(|hook| hook.take().map(|h| h.seen).unwrap_or_default());
    #[cfg(not(debug_assertions))]
    Vec::new()
}
