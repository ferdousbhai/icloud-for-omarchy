//! The suite's stand-in for icloud-sessiond: the part of its D-Bus interface
//! icloud-notes uses (the account's properties, `SignIn`,
//! `ReportSignInRequired`), served from the test process on the test's
//! private bus. The repository's other fake is C++ linked into the Qt tests
//! (tests/fake_session.h), which a separate binary under test cannot reach.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use zbus::blocking::Connection;

use crate::world::{APPLE_ID, DSID};

const BUS_NAME: &str = "io.github.ferdousbhai.ICloudSession";
const OBJECT_PATH: &str = "/io/github/ferdousbhai/ICloudSession";

#[derive(Default)]
struct State {
    signed_in: AtomicBool,
    dsid: Mutex<String>,
    /// When the session ends, in seconds since the epoch; 0 for no known end.
    expires_at: AtomicU64,
    /// What `ReportSignInRequired` answers: is the sign-in still good?
    still_signed_in: AtomicBool,
    reports: AtomicUsize,
    sign_ins: AtomicUsize,
}

struct Service(Arc<State>);

#[zbus::interface(name = "io.github.ferdousbhai.ICloudSession")]
impl Service {
    #[zbus(property, name = "SignedIn")]
    fn signed_in(&self) -> bool {
        self.0.signed_in.load(Ordering::SeqCst)
    }

    #[zbus(property, name = "AppleId")]
    fn apple_id(&self) -> String {
        APPLE_ID.to_string()
    }

    #[zbus(property, name = "Dsid")]
    fn dsid(&self) -> String {
        self.0.dsid.lock().unwrap().clone()
    }

    #[zbus(property, name = "ExpiresAt")]
    fn expires_at(&self) -> u64 {
        self.0.expires_at.load(Ordering::SeqCst)
    }

    #[zbus(property, name = "SigningIn")]
    fn signing_in(&self) -> bool {
        false
    }

    #[zbus(name = "SignIn")]
    fn sign_in(&self) {
        self.0.sign_ins.fetch_add(1, Ordering::SeqCst);
    }

    #[zbus(name = "ReportSignInRequired")]
    fn report_sign_in_required(&self) -> bool {
        self.0.reports.fetch_add(1, Ordering::SeqCst);
        self.0.still_signed_in.load(Ordering::SeqCst)
    }
}

pub struct FakeSession {
    state: Arc<State>,
    /// `None` once stopped: nobody owns the name, as with no daemon installed.
    connection: Option<Connection>,
}

impl FakeSession {
    /// Signed in, on the bus at `address`.
    pub fn start(address: &str) -> FakeSession {
        let state = Arc::new(State::default());
        state.signed_in.store(true, Ordering::SeqCst);
        *state.dsid.lock().unwrap() = DSID.to_string();
        let connection = zbus::blocking::connection::Builder::address(address)
            .unwrap()
            .name(BUS_NAME)
            .unwrap()
            .serve_at(OBJECT_PATH, Service(state.clone()))
            .unwrap()
            .build()
            .expect("the fake icloud-session owns its name on the private bus");
        FakeSession {
            state,
            connection: Some(connection),
        }
    }

    /// Leaves the bus: the sign-in is unknown from here on.
    pub fn stop(&mut self) {
        self.connection = None;
    }

    pub fn set_signed_in(&self, signed_in: bool) {
        self.state.signed_in.store(signed_in, Ordering::SeqCst);
    }

    pub fn set_dsid(&self, dsid: &str) {
        *self.state.dsid.lock().unwrap() = dsid.to_string();
    }

    /// Sets when the session ends: `seconds` from now (before now, when negative).
    pub fn set_expires_in(&self, seconds: i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.state
            .expires_at
            .store(now.checked_add_signed(seconds).unwrap(), Ordering::SeqCst);
    }

    /// Makes `ReportSignInRequired` vouch for the sign-in (a retry follows).
    pub fn vouch(&self) {
        self.state.still_signed_in.store(true, Ordering::SeqCst);
    }

    /// How often the binary reported a refused sign-in.
    pub fn reports(&self) -> usize {
        self.state.reports.load(Ordering::SeqCst)
    }

    /// How often the binary asked for the sign-in window.
    pub fn sign_ins(&self) -> usize {
        self.state.sign_ins.load(Ordering::SeqCst)
    }
}
