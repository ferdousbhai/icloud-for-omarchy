//! What icloud-session, the daemon that owns the Apple account for every
//! iCloud app, says about the sign-in. Notes never signs in by itself.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use icloud_session::{BUS_NAME, INTERFACE, OBJECT_PATH, Status};
use zbus::blocking::Connection;

/// Long enough for D-Bus activation to start the daemon.
const READ_LIMIT: Duration = Duration::from_secs(10);
/// ReportSignInRequired checks the session with Apple before it answers.
const REPORT_LIMIT: Duration = Duration::from_secs(60);

/// `ask`'s answer, or `None` when it has none within `limit` (the thread
/// asking is left to end with the process).
fn within<T: Send + 'static>(limit: Duration, ask: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (answer, waiting) = mpsc::channel();
    thread::spawn(move || {
        let _ = answer.send(ask());
    });
    waiting.recv_timeout(limit).ok()
}

/// The daemon's account, or `None` while the sign-in is unknown: no session
/// bus, no daemon, or no answer in time.
pub fn read() -> Option<Status> {
    within(READ_LIMIT, || icloud_session::status().ok()).flatten()
}

/// Tells icloud-session that iCloud refused the sign-in. It checks the
/// session with Apple (signing everyone out if it is gone) and answers
/// whether it still works: it refreshed the engine's copy then, so a refusal
/// came from a stale copy and one retry should go through. An error is why
/// it could not be asked, for the log.
pub fn report_sign_in_required() -> Result<bool, String> {
    let report = || -> Result<bool, String> {
        let bus = Connection::session().map_err(|_| "icloud-session: no D-Bus session bus".to_string())?;
        let failed = |e: zbus::Error| {
            let why = match e {
                zbus::Error::MethodError(_, Some(message), _) => message,
                other => other.to_string(),
            };
            format!("icloud-session ReportSignInRequired failed: {why}")
        };
        let reply = bus
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(INTERFACE),
                "ReportSignInRequired",
                &(),
            )
            .map_err(failed)?;
        reply.body().deserialize::<bool>().map_err(failed)
    };
    within(REPORT_LIMIT, report)
        .unwrap_or_else(|| Err("icloud-session ReportSignInRequired failed: no answer in time".into()))
}
