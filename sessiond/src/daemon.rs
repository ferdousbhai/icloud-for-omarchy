//! The daemon: the only owner of the account, its D-Bus interface, the
//! sign-in child, the heartbeat and the idle exit.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use icloud_session::{INTERFACE, OBJECT_PATH, SessionReply, time};
use serde::Deserialize;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::apple::{self, ValidateError};
use crate::cookies::{self, Cookie};
use crate::files::{self, Account, FindMyJar, Paths};
use crate::secrets::{self, Password, SecretStore};

/// Heartbeat keeps running while a client called within this window
/// (the browser's own heartbeat is 14 minutes).
const ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// How long start-up waits for the keyring's answer before taking the bus
/// name (well inside D-Bus's activation timeout).
const KEYRING_LOOK_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct Config {
    pub paths: Paths,
    pub setup_url: String,
    signin_bin: PathBuf,
    /// Exit after this long with no clients and no sign-in (5 minutes).
    pub idle: Duration,
    /// Revalidate when the last `/validate` is older than this (10 minutes):
    /// in the background, without holding up the caller, while the jar is
    /// younger than `handout_max_age`.
    validate_max_age: Duration,
    /// `Session()` hands out a jar validated within this long at once (6
    /// hours); an older one is validated before it is handed out.
    handout_max_age: Duration,
    /// After a `/validate` that got no answer, hand out the session as it is
    /// for this long before trying Apple again (1 minute).
    validate_retry: Duration,
}

fn env_secs(name: &str, default: u64) -> Duration {
    let secs = std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|s| *s > 0.0);
    secs.map_or(Duration::from_secs(default), Duration::from_secs_f64)
}

impl Config {
    pub fn from_env() -> Config {
        Config {
            paths: Paths::from_env(),
            setup_url: apple::setup_url(),
            signin_bin: signin_bin(),
            idle: env_secs("ICLOUD_SESSIOND_IDLE_SECS", 5 * 60),
            validate_max_age: env_secs("ICLOUD_SESSIOND_VALIDATE_SECS", 10 * 60),
            handout_max_age: env_secs("ICLOUD_SESSIOND_HANDOUT_SECS", 6 * 3600),
            validate_retry: env_secs("ICLOUD_SESSIOND_RETRY_SECS", 60),
        }
    }
}

/// `$ICLOUD_SESSION_SIGNIN_BIN`, else `icloud-session-signin` beside this
/// executable, else on `PATH`.
fn signin_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("ICLOUD_SESSION_SIGNIN_BIN").filter(|v| !v.is_empty()) {
        return bin.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("icloud-session-signin")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| "icloud-session-signin".into())
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.ferdousbhai.ICloudSession.Error")]
enum ServiceError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Signed out: the apps show a sign-in banner.
    SignInRequired(String),
    /// No Find My session: the apps offer `AuthorizeFindMy()`.
    FindMyAuthRequired(String),
    /// `SetPassword()`: Apple refused the password.
    PasswordRejected(String),
    /// `SetPassword()`/`ForgetPassword()`: Apple unreachable, keyring failed.
    Failed(String),
}

fn sign_in_required() -> ServiceError {
    ServiceError::SignInRequired("sign in to iCloud required".into())
}

fn find_my_auth_required() -> ServiceError {
    ServiceError::FindMyAuthRequired("Find My needs the Apple ID password".into())
}

/// The D-Bus properties, as last announced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Props {
    signed_in: bool,
    apple_id: String,
    dsid: String,
    expires_at: u64,
    signing_in: bool,
    find_my_authorized: bool,
    find_my_password_stored: bool,
}

/// How the last `/validate` of an account ended.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    finished: Instant,
    /// Apple answered 2xx (a 421/401 forgets the account instead).
    ok: bool,
    generation: u64,
}

/// Why automatic Find My sign-in is holding off.
#[derive(Debug, Clone, Copy)]
enum LoginBlock {
    /// Apple refused this password (by fingerprint), or Find My refused
    /// the session it just made: wait until the stored password changes.
    Password(u64),
    /// Apple could not be reached: not before then.
    Until(Instant),
}

/// How an automatic Find My sign-in failed.
#[derive(Debug)]
enum LoginError {
    /// Apple said the password is wrong (the autofill window exits 3). Not
    /// worth retrying until the password changes.
    Rejected,
    /// No answer, another status, an answer without Find My's cookie.
    Failed(String),
}

/// How the last automatic Find My sign-in ended.
#[derive(Debug, Clone, Copy)]
struct LoginAttempt {
    finished: Instant,
    ok: bool,
}

/// `FindMySession()`'s reply: cookie header, client params.
type FindMyReply = (String, HashMap<String, String>);

/// What asked for an automatic Find My sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginWhy {
    /// A client got 450 with the Find My jar.
    Reported,
    /// `FindMySession()` found no jar.
    NoJar,
}

struct State {
    account: Option<Account>,
    /// Bumped by every `SignIn()` or `AuthorizeFindMy()` that opens the window and by `SignOut()`,
    /// so a window that was signed out from under drops its late result.
    signin_seq: u64,
    /// Bumped whenever `account` is replaced or forgotten, so a `/validate`
    /// of an earlier account never touches the current one.
    generation: u64,
    last_attempt: Option<Attempt>,
    signing_in: bool,
    /// Last method call from a client (heartbeat).
    last_call: Instant,
    /// Last call or sign-in end (idle exit).
    last_activity: Instant,
    /// Unique bus names that called us, pruned when they leave the bus.
    clients: HashSet<String>,
    /// A password for the account's Apple ID is in the keyring, as of the
    /// last look (start, sign-in, `SetPassword`, `ForgetPassword`, a login).
    password_stored: bool,
    find_my_block: Option<LoginBlock>,
    find_my_last_login: Option<LoginAttempt>,
    /// The last Find My sign-in that worked, and its password's fingerprint.
    find_my_last_ok: Option<(Instant, u64)>,
    /// Bumped by every [`Daemon::save_account`], so the writes done after
    /// the lock is dropped never put an older account over a newer one.
    save_seq: u64,
}

/// An `account.json` write, taken under the state lock and done after it
/// with [`Daemon::write`]: the account as it was then (`None`: removed).
#[must_use = "write it once the state lock is dropped"]
struct Save {
    seq: u64,
    account: Option<Account>,
}

impl State {
    fn props(&self) -> Props {
        let account = self.account.as_ref();
        Props {
            signed_in: account.is_some(),
            apple_id: account.map(|a| a.apple_id.clone()).unwrap_or_default(),
            dsid: account.map(|a| a.dsid.clone()).unwrap_or_default(),
            expires_at: account.map_or(0, |a| cookies::token_expiry(&a.cookies)),
            signing_in: self.signing_in,
            find_my_authorized: account.is_some_and(|a| a.find_my_ready(time::now_secs())),
            find_my_password_stored: account.is_some() && self.password_stored,
        }
    }
}

pub struct Daemon {
    cfg: Config,
    agent: ureq::Agent,
    state: Mutex<State>,
    /// One `/validate` at a time; a waiter re-checks freshness after it.
    validate_lock: Mutex<()>,
    /// Orders property announcements.
    published: Mutex<Props>,
    conn: OnceLock<Connection>,
    /// A background `/validate` is running (start-up, heartbeat, or a
    /// `Session()` that handed out a jar due for one), on its own thread so
    /// a slow Apple never holds up a caller or the idle exit.
    refreshing: AtomicBool,
    /// Sign-in windows running (a sign-in and a hidden Find My one can
    /// overlap); the last to close clears WebKit's HTTP cache.
    windows: AtomicUsize,
    /// The open sign-in window, with the `signin_seq` that opened it.
    signin_child: Mutex<Option<(u64, Child)>>,
    /// The keyring holding the Apple ID password (opt-in).
    secrets: Box<dyn SecretStore>,
    /// One Find My one-factor sign-in at a time; a waiter shares its answer.
    find_my_login_lock: Mutex<()>,
    /// Keys the in-memory password fingerprints (never stored or logged).
    fingerprint_key: RandomState,
    /// The `save_seq` of the last `account.json` write; held while writing.
    written: Mutex<u64>,
}

/// How fresh the session must be before `ensure_fresh` skips `/validate`.
/// Either way, a `/validate` that finished while the caller waited for the
/// one in flight answers for it.
#[derive(Debug, Clone, Copy)]
enum Fresh {
    /// A client got 421/401: ask Apple, whatever the last answer was.
    Confirm,
    /// Validated at or after this unix time. After a failed attempt, the
    /// session is handed out as it is until `validate_retry` has passed.
    Since(u64),
}

enum Refresh {
    SignedOut,
    Failed,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Daemon {
    pub fn new(cfg: Config) -> Arc<Daemon> {
        let account = Account::load_or_set_aside(&cfg.paths.account);
        let state = State {
            account,
            generation: 0,
            last_attempt: None,
            signin_seq: 0,
            signing_in: false,
            last_call: Instant::now(),
            last_activity: Instant::now(),
            clients: HashSet::new(),
            password_stored: false,
            find_my_block: None,
            find_my_last_login: None,
            find_my_last_ok: None,
            save_seq: 0,
        };
        let props = state.props();
        Arc::new(Daemon {
            cfg,
            agent: apple::agent(),
            state: Mutex::new(state),
            validate_lock: Mutex::new(()),
            published: Mutex::new(props),
            conn: OnceLock::new(),
            refreshing: AtomicBool::new(false),
            windows: AtomicUsize::new(0),
            signin_child: Mutex::new(None),
            secrets: secrets::from_env(),
            find_my_login_lock: Mutex::new(()),
            fingerprint_key: RandomState::new(),
            written: Mutex::new(0),
        })
    }

    /// Serves on the session bus until idle. Returns the process exit code.
    pub fn run(self: &Arc<Daemon>) -> zbus::Result<()> {
        let conn = zbus::blocking::connection::Builder::session()?
            .serve_at(OBJECT_PATH, Service(self.clone()))?
            .build()?;
        let _ = self.conn.set(conn.clone());

        // Listen for callers before taking the name: the call that
        // activated us is delivered as soon as we own it.
        let messages = zbus::blocking::MessageIterator::from(&conn);
        let me = self.clone();
        thread::spawn(move || me.track_clients(messages));
        // Look in the keyring before taking the name: the call that
        // activated us (often `status`, reading every property) is answered
        // as soon as we own it, and must not see FindMyPasswordStored
        // false for want of a look. A keyring slower than that is still
        // announced when it answers.
        let (looked, keyring_looked) = std::sync::mpsc::channel();
        let me = self.clone();
        thread::spawn(move || {
            me.look_for_password();
            let _ = looked.send(());
            me.publish();
        });
        let _ = keyring_looked.recv_timeout(KEYRING_LOOK_WAIT);
        // Never queue behind another instance; fail instead.
        let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
        if conn.request_name_with_flags(icloud_session::BUS_NAME, flags)? != zbus::fdo::RequestNameReply::PrimaryOwner {
            return Err(zbus::Error::NameTaken);
        }
        // A restarted daemon may differ from the one watchers last heard
        // from: announce every property once.
        self.announce(true);

        // Validate on start if the stored jar is due (`validated_at` is
        // kept in account.json, so a restart alone does not make it due).
        self.refresh_in_background();

        self.tick_until_idle();
        Ok(())
    }

    fn conn(&self) -> Option<&Connection> {
        self.conn.get()
    }

    // ----------------------------------------------------------- publish

    /// Announces every property that changed since the last announcement
    /// with one `PropertiesChanged`.
    fn publish(&self) {
        self.announce(false);
    }

    /// `PropertiesChanged` with the properties that changed, or with all of
    /// them.
    fn announce(&self, all: bool) {
        let mut published = lock(&self.published);
        let now = lock(&self.state).props();
        if *published == now && !all {
            return;
        }
        let mut changed: HashMap<&str, Value<'_>> = HashMap::new();
        if all || published.signed_in != now.signed_in {
            changed.insert("SignedIn", now.signed_in.into());
        }
        if all || published.apple_id != now.apple_id {
            changed.insert("AppleId", now.apple_id.as_str().into());
        }
        if all || published.dsid != now.dsid {
            changed.insert("Dsid", now.dsid.as_str().into());
        }
        if all || published.expires_at != now.expires_at {
            changed.insert("ExpiresAt", now.expires_at.into());
        }
        if all || published.signing_in != now.signing_in {
            changed.insert("SigningIn", now.signing_in.into());
        }
        if all || published.find_my_authorized != now.find_my_authorized {
            changed.insert("FindMyAuthorized", now.find_my_authorized.into());
        }
        if all || published.find_my_password_stored != now.find_my_password_stored {
            changed.insert("FindMyPasswordStored", now.find_my_password_stored.into());
        }
        if let Some(conn) = self.conn() {
            let body = (INTERFACE, changed, Vec::<&str>::new());
            if let Err(e) = conn.emit_signal(
                None::<zbus::names::BusName<'_>>,
                OBJECT_PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &body,
            ) {
                eprintln!("icloud-sessiond: PropertiesChanged: {e}");
            }
        }
        *published = now;
    }

    // ------------------------------------------------------------- store

    /// Forgets the account (confirmed 421/401 or `SignOut`); the returned
    /// [`Save`] removes `account.json`.
    fn forget(&self, st: &mut State) -> Save {
        st.account = None;
        st.generation += 1;
        self.save_account(st)
    }

    /// Takes `account.json`'s next contents (the account now, or none):
    /// cheap, under the state lock. [`Daemon::write`] it once the lock is
    /// dropped, so no caller waits on the disk for the state.
    fn save_account(&self, st: &mut State) -> Save {
        st.save_seq += 1;
        Save {
            seq: st.save_seq,
            account: st.account.clone(),
        }
    }

    /// Writes (or removes) `account.json` as `save` says, unless a later
    /// [`Save`] got there first. Errors are logged: the session in memory
    /// is still right, and the next change retries.
    fn write(&self, save: Save) {
        let mut written = lock(&self.written);
        if save.seq <= *written {
            return;
        }
        *written = save.seq;
        let path = &self.cfg.paths.account;
        let result = match &save.account {
            Some(a) => a.save(path).map_err(|e| format!("saving {}: {e}", path.display())),
            None => files::remove(path).map_err(|e| format!("removing {}: {e}", path.display())),
        };
        if let Err(e) = result {
            eprintln!("icloud-sessiond: {e}");
        }
    }

    // ---------------------------------------------------------- validate

    /// Calls `/validate` unless the session is already fresh enough, then
    /// merges rotated cookies and new webservices. A 421/401 signs out.
    fn ensure_fresh(&self, fresh: Fresh) -> Result<(), Refresh> {
        let arrived = Instant::now();
        let _one = lock(&self.validate_lock);
        let (cookie, params, dsid, generation) = {
            let st = lock(&self.state);
            let generation = st.generation;
            let Some(a) = &st.account else {
                return Err(Refresh::SignedOut);
            };
            let last = st.last_attempt.filter(|at| at.generation == generation);
            // Waited behind an attempt that finished meanwhile: its answer
            // is ours too, rather than another serial round trip to Apple.
            if let Some(at) = last.filter(|at| at.finished >= arrived) {
                return if at.ok { Ok(()) } else { Err(Refresh::Failed) };
            }
            if let Fresh::Since(t) = fresh {
                if a.validated_at >= t {
                    return Ok(());
                }
                if last.is_some_and(|at| !at.ok && at.finished.elapsed() < self.cfg.validate_retry) {
                    return Err(Refresh::Failed);
                }
            }
            (
                a.cookie_header(time::now_secs()),
                a.client_params.clone(),
                a.dsid.clone(),
                generation,
            )
        };
        let result = apple::validate(&self.agent, &self.cfg.setup_url, &cookie, &params, Some(&dsid));
        let mut st = lock(&self.state);
        // Signed out or signed in again meanwhile: this answer is about a
        // jar we no longer hold.
        let same = st.generation == generation && st.account.is_some();
        if same {
            st.last_attempt = Some(Attempt {
                finished: Instant::now(),
                ok: result.is_ok(),
                generation,
            });
        }
        let mut save = None;
        let outcome = match result {
            Ok(v) => {
                if let Some(a) = st.account.as_mut().filter(|_| same) {
                    cookies::merge_set_cookies(&mut a.cookies, &v.set_cookies, time::now_secs());
                    a.webservices = v.webservices;
                    a.apple_id = v.apple_id;
                    a.validated_at = time::now_secs();
                    save = Some(self.save_account(&mut st));
                }
                Ok(())
            }
            Err(ValidateError::SignedOut) => {
                if same {
                    eprintln!("icloud-sessiond: Apple ended the session; signed out");
                    save = Some(self.forget(&mut st));
                }
                Err(Refresh::SignedOut)
            }
            Err(ValidateError::Offline(msg)) => {
                eprintln!("icloud-sessiond: Apple unreachable, offline? {msg}");
                Err(Refresh::Failed)
            }
            Err(ValidateError::Failed(msg)) => {
                eprintln!("icloud-sessiond: {msg}");
                Err(Refresh::Failed)
            }
        };
        drop(st);
        if let Some(save) = save {
            self.write(save);
        }
        self.publish();
        outcome
    }

    /// [`Daemon::ensure_fresh`] with [`Daemon::fresh_enough`] on its own
    /// thread, unless one is running already; nobody waits for it. What it
    /// finds is announced as any `/validate`'s is: a 421/401 signs out and
    /// `PropertiesChanged` says so.
    fn refresh_in_background(self: &Arc<Daemon>) {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        thread::spawn(move || {
            let _ = me.ensure_fresh(me.fresh_enough());
            me.refreshing.store(false, Ordering::SeqCst);
        });
    }

    // ----------------------------------------------------------- methods

    /// Validated within `validate_max_age`.
    fn fresh_enough(&self) -> Fresh {
        Fresh::Since(time::now_secs().saturating_sub(whole_secs(self.cfg.validate_max_age)))
    }

    /// Hands out the jar. One validated within `handout_max_age` goes out
    /// at once, and if it is due (older than `validate_max_age`) is
    /// revalidated behind the caller: Apple almost surely still takes it,
    /// and if not, the app's request gets 421/401 and
    /// `ReportSignInRequired()` confirms. An older one is validated first.
    fn session(self: &Arc<Daemon>) -> Result<SessionReply, ServiceError> {
        let validated_at = lock(&self.state).account.as_ref().map(|a| a.validated_at);
        let age = time::now_secs().saturating_sub(validated_at.ok_or_else(sign_in_required)?);
        if age >= whole_secs(self.cfg.handout_max_age) {
            match self.ensure_fresh(self.fresh_enough()) {
                Err(Refresh::SignedOut) => return Err(sign_in_required()),
                // Apple unreachable: hand out what we have; the app's own
                // request will fail the same way and say so.
                Err(Refresh::Failed) | Ok(()) => {}
            }
        } else if age >= whole_secs(self.cfg.validate_max_age) {
            self.refresh_in_background();
        }
        let st = lock(&self.state);
        let a = st.account.as_ref().ok_or_else(sign_in_required)?;
        let map = |m: &BTreeMap<String, String>| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok((
            a.cookie_header(time::now_secs()),
            map(&a.client_params),
            map(&a.webservices),
        ))
    }

    fn merge_cookies(&self, set_cookies: &[String]) {
        let mut st = lock(&self.state);
        let changed = st
            .account
            .as_mut()
            .is_some_and(|a| cookies::merge_set_cookies(&mut a.cookies, set_cookies, time::now_secs()));
        let save = changed.then(|| self.save_account(&mut st));
        drop(st);
        if let Some(save) = save {
            self.write(save);
        }
        self.publish();
    }

    /// Confirms with Apple before signing every app out. A 2xx keeps the
    /// session; an unreachable Apple changes
    /// nothing. A `/validate` that finishes after the report arrived counts
    /// as the confirmation, so a burst of reports costs one round trip.
    /// Returns whether the account is still signed in.
    fn report_sign_in_required(&self) -> bool {
        match self.ensure_fresh(Fresh::Confirm) {
            Ok(()) | Err(Refresh::Failed) => lock(&self.state).account.is_some(),
            Err(Refresh::SignedOut) => false,
        }
    }

    /// `FindMySession()`: the Find My jar's cookie header and client params.
    /// With no jar, and a password stored, signs in to Find My first.
    fn find_my_session(&self) -> Result<FindMyReply, ServiceError> {
        let held = |st: &State| -> Result<Option<FindMyReply>, ServiceError> {
            let a = st.account.as_ref().ok_or_else(sign_in_required)?;
            Ok(a.find_my.as_ref().map(|f| {
                let params = f.client_params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                (cookies::header(&f.cookies, time::now_secs()), params)
            }))
        };
        if let Some(jar) = held(&lock(&self.state))? {
            return Ok(jar);
        }
        self.auto_find_my_login(LoginWhy::NoJar);
        held(&lock(&self.state))?.ok_or_else(find_my_auth_required)
    }

    /// `MergeFindMyCookies()`: `Set-Cookie`s a client got from Find My.
    fn merge_find_my_cookies(&self, set_cookies: &[String]) {
        let mut st = lock(&self.state);
        let changed = st
            .account
            .as_mut()
            .and_then(|a| a.find_my.as_mut())
            .is_some_and(|f| cookies::merge_set_cookies(&mut f.cookies, set_cookies, time::now_secs()));
        let save = changed.then(|| self.save_account(&mut st));
        drop(st);
        if let Some(save) = save {
            self.write(save);
        }
        self.publish();
    }

    /// A client got HTTP 450 from Find My: its session is spent. Forgets
    /// the Find My jar, then signs in to Find My again if a password is
    /// stored. Returns whether a new jar is held (the client retries once).
    fn report_find_my_auth_required(&self) -> bool {
        self.auto_find_my_login(LoginWhy::Reported)
    }

    // ------------------------------------------------ Find My password

    /// An in-memory fingerprint of a password, to recognise one Apple
    /// refused without keeping it.
    fn fingerprint(&self, apple_id: &str, password: &str) -> u64 {
        self.fingerprint_key.hash_one((apple_id, password))
    }

    /// Looks in the keyring for the account's password and announces
    /// `FindMyPasswordStored`.
    fn refresh_password_stored(&self) {
        self.look_for_password();
        self.publish();
    }

    /// [`Daemon::refresh_password_stored`] without the announcement.
    fn look_for_password(&self) {
        let Some((apple_id, generation)) = ({
            let st = lock(&self.state);
            st.account.as_ref().map(|a| (a.apple_id.clone(), st.generation))
        }) else {
            return;
        };
        let stored = match self.secrets.contains(&apple_id) {
            Ok(stored) => stored,
            Err(e) => {
                eprintln!("icloud-sessiond: looking for the stored password: {e}");
                false
            }
        };
        let mut st = lock(&self.state);
        if st.generation == generation {
            st.password_stored = stored;
        }
    }

    /// Signs in to Find My in the hidden autofill window with a fresh jar
    /// and, if it worked for the account it was made for, keeps that jar
    /// (and the window's client params) as the Find My jar. The main jar
    /// is neither sent nor touched. Records the attempt and what
    /// it means for the next one. Call under `find_my_login_lock`.
    fn find_my_login(&self, generation: u64, apple_id: &str, dsid: &str, password: &str) -> Result<(), LoginError> {
        let fingerprint = self.fingerprint(apple_id, password);
        // Apple's own sign-in page, filled in as a password manager would
        // (a plain-password accountLogin answers 421 now). The page's Find
        // My call answering is what completes it.
        let result = self
            .autofill_find_my(apple_id, password)
            .and_then(|(jar, params, found)| {
                same_account(found, dsid).map_err(LoginError::Failed)?;
                Ok((jar, params))
            });
        let mut st = lock(&self.state);
        let finished = Instant::now();
        st.find_my_last_login = Some(LoginAttempt {
            finished,
            ok: result.is_ok(),
        });
        match &result {
            Ok(_) => {
                st.find_my_block = None;
                st.find_my_last_ok = Some((finished, fingerprint));
            }
            Err(LoginError::Rejected) => st.find_my_block = Some(LoginBlock::Password(fingerprint)),
            Err(LoginError::Failed(_)) => {
                st.find_my_block = Some(LoginBlock::Until(finished + self.cfg.validate_retry))
            }
        }
        let (jar, params) = result?;
        let same = st.generation == generation;
        let Some(account) = st.account.as_mut().filter(|_| same) else {
            return Err(LoginError::Failed("signed out meanwhile".into()));
        };
        account.find_my = Some(FindMyJar {
            cookies: jar,
            client_params: params,
            captured_at: time::rfc3339_millis(time::now_ms()),
        });
        let save = self.save_account(&mut st);
        drop(st);
        self.write(save);
        Ok(())
    }

    /// Signs in to Find My with the stored password, unless there is none,
    /// Apple refused it, or Apple was unreachable a moment ago. `Reported`
    /// forgets the jar a client got 450 with first; `NoJar` does nothing
    /// if a jar is held by now. Concurrent callers share one attempt.
    /// Returns whether a Find My jar is held afterwards.
    fn auto_find_my_login(&self, why: LoginWhy) -> bool {
        let arrived = Instant::now();
        let _one = lock(&self.find_my_login_lock);
        let mut save = None;
        let (apple_id, dsid, generation) = {
            let mut st = lock(&self.state);
            // Waited behind a sign-in that finished meanwhile: its answer
            // is ours too (a 450 about the jar it replaced forgets nothing).
            if let Some(at) = st.find_my_last_login.filter(|at| at.finished >= arrived) {
                return at.ok && st.account.as_ref().is_some_and(|a| a.find_my.is_some());
            }
            let recent_ok = st
                .find_my_last_ok
                .filter(|(at, _)| at.elapsed() < self.cfg.validate_retry);
            let Some(a) = st.account.as_mut() else {
                return false;
            };
            match why {
                LoginWhy::NoJar if a.find_my.is_some() => return true,
                LoginWhy::NoJar => {}
                LoginWhy::Reported => {
                    if a.find_my.take().is_some() {
                        eprintln!("icloud-sessiond: Find My asked for the password again");
                        save = Some(self.save_account(&mut st));
                    }
                    // Find My refused the session the stored password just
                    // made: signing in again would only do the same.
                    if let Some((_, fingerprint)) = recent_ok {
                        eprintln!(
                            "icloud-sessiond: Find My refused the session the stored password made; \
                             not signing in again until the password changes"
                        );
                        st.find_my_block = Some(LoginBlock::Password(fingerprint));
                    }
                }
            }
            let a = st.account.as_ref().expect("checked above");
            let found = (a.apple_id.clone(), a.dsid.clone(), st.generation);
            let waiting = matches!(st.find_my_block, Some(LoginBlock::Until(t)) if Instant::now() < t);
            if !st.password_stored || waiting {
                drop(st);
                if let Some(save) = save {
                    self.write(save);
                }
                self.publish();
                return false;
            }
            found
        };
        if let Some(save) = save {
            self.write(save);
        }
        self.publish();
        let password: Password = match self.secrets.get(&apple_id) {
            Ok(Some(p)) => p,
            Ok(None) => {
                let mut st = lock(&self.state);
                if st.generation == generation {
                    st.password_stored = false;
                }
                drop(st);
                self.publish();
                return false;
            }
            Err(e) => {
                eprintln!("icloud-sessiond: reading the stored password: {e}");
                return false;
            }
        };
        let fingerprint = self.fingerprint(&apple_id, &password);
        if matches!(lock(&self.state).find_my_block, Some(LoginBlock::Password(f)) if f == fingerprint) {
            return false;
        }
        let result = self.find_my_login(generation, &apple_id, &dsid, &password);
        drop(password);
        match result {
            Ok(()) => eprintln!("icloud-sessiond: signed in to Find My with the stored password"),
            Err(LoginError::Rejected) => eprintln!(
                "icloud-sessiond: Apple refused the stored password for Find My; \
                 not trying it again until it changes (icloud-session set-password)"
            ),
            Err(LoginError::Failed(m)) => eprintln!("icloud-sessiond: Find My sign-in: {m}"),
        }
        self.publish();
        lock(&self.state).account.as_ref().is_some_and(|a| a.find_my.is_some())
    }

    /// `SetPassword()`: signs in to Find My with `password` once, and only
    /// if Apple accepts it, keeps the jar and stores the password in the
    /// keyring for the signed-in Apple ID.
    fn set_password(&self, password: Password) -> Result<(), ServiceError> {
        if password.is_empty() {
            return Err(ServiceError::Failed("the password is empty".into()));
        }
        let _one = lock(&self.find_my_login_lock);
        let (apple_id, dsid, generation) = {
            let st = lock(&self.state);
            let a = st.account.as_ref().ok_or_else(sign_in_required)?;
            (a.apple_id.clone(), a.dsid.clone(), st.generation)
        };
        let result = self.find_my_login(generation, &apple_id, &dsid, &password);
        self.publish();
        // Only Apple refusing the password keeps it out of the keyring; any
        // other failure (a changed or misread endpoint) says nothing about
        // the password, which usually comes from the user's own vault.
        let login_failure = match result {
            Ok(()) => None,
            Err(LoginError::Rejected) => {
                return Err(ServiceError::PasswordRejected(format!(
                    "Apple refused the password for {apple_id}; nothing stored"
                )));
            }
            Err(LoginError::Failed(m)) => Some(m),
        };
        self.secrets
            .set(&apple_id, &password)
            .map_err(|e| ServiceError::Failed(format!("storing the password in the keyring failed: {e}")))?;
        eprintln!("icloud-sessiond: stored the Find My password for {apple_id} in the keyring");
        let mut st = lock(&self.state);
        if st.generation == generation {
            st.password_stored = true;
        }
        drop(st);
        self.publish();
        match login_failure {
            None => Ok(()),
            Some(m) => Err(ServiceError::Failed(format!(
                "stored the password, but the Find My sign-in with it failed: {m}"
            ))),
        }
    }

    /// `ForgetPassword()`: removes every icloud-session keyring item.
    fn forget_password(&self) -> Result<(), ServiceError> {
        let _one = lock(&self.find_my_login_lock);
        let n = self.secrets.forget_all().map_err(ServiceError::Failed)?;
        eprintln!("icloud-sessiond: removed {n} stored password(s) from the keyring");
        let mut st = lock(&self.state);
        st.password_stored = false;
        st.find_my_block = None;
        drop(st);
        self.publish();
        Ok(())
    }

    /// Opens the sign-in window (`find`: on Find My, to authorize it)
    /// unless one is open. The outcome arrives as property changes.
    fn sign_in(self: &Arc<Daemon>, find: bool) {
        let seq = {
            let mut st = lock(&self.state);
            if st.signing_in {
                return;
            }
            st.signing_in = true;
            st.signin_seq += 1;
            st.signin_seq
        };
        self.publish();
        let me = self.clone();
        thread::spawn(move || {
            let result = me.run_sign_in(seq, find);
            if let Err(e) = &result {
                let what = if find { "Find My authorization" } else { "sign-in" };
                eprintln!("icloud-sessiond: {what}: {e}");
            }
            let mut st = lock(&me.state);
            // A SignOut (and maybe a new SignIn) took over this window.
            if st.signin_seq == seq {
                st.signing_in = false;
            }
            st.last_activity = Instant::now();
            drop(st);
            me.publish();
            if result.is_ok() && !find {
                me.refresh_password_stored();
            }
        });
    }

    /// Counts a sign-in window in until the guard drops.
    fn window_opened(&self) -> Window<'_> {
        self.windows.fetch_add(1, Ordering::SeqCst);
        Window(self)
    }

    /// Runs the sign-in window hidden in `--find --autofill` mode: it signs
    /// in on Apple's own page with the stored password (showing itself only
    /// if Apple asks for more, like a 2FA code) and prints the jar once Find
    /// My answers. The password reaches it only on its stdin.
    fn autofill_find_my(&self, apple_id: &str, password: &str) -> Result<FindMyCapture, LoginError> {
        use std::io::Write;
        let bin = &self.cfg.signin_bin;
        let failed = |m: String| LoginError::Failed(m);
        let mut child = Command::new(bin)
            .args(["--find", "--autofill"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| failed(spawn_error(bin, &e)))?;
        let _window = self.window_opened();
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            let mut input = zeroize::Zeroizing::new(format!("{apple_id}\n{password}\n"));
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| failed(format!("giving the sign-in window the password: {e}")))?;
            zeroize::Zeroize::zeroize(&mut *input);
        }
        let mut out = Vec::new();
        let read = child.stdout.take().expect("stdout is piped").read_to_end(&mut out);
        let status = child
            .wait()
            .map_err(|e| failed(format!("waiting for {}: {e}", bin.display())))?;
        read.map_err(|e| failed(format!("reading the sign-in window's output: {e}")))?;
        if status.code() == Some(3) {
            return Err(LoginError::Rejected);
        }
        if !status.success() {
            return Err(failed(format!(
                "the Find My sign-in did not finish ({} exited with {status})",
                bin.display()
            )));
        }
        find_my_capture(&out).map_err(failed)
    }

    /// Runs the sign-in window, validates what it captured, stores it.
    /// With `find` the window authorizes Find My (`--find`), and what it
    /// captures is kept as the Find My jar instead (see [`FindMyJar`]).
    fn run_sign_in(&self, seq: u64, find: bool) -> Result<(), String> {
        let bin = &self.cfg.signin_bin;
        let cancelled = || "cancelled by SignOut".to_string();
        let (mut stdout, window) = {
            // Spawned under the slot's lock, so a SignOut either sees the
            // child (and kills it) or has already cancelled this sign-in.
            let mut slot = lock(&self.signin_child);
            if lock(&self.state).signin_seq != seq {
                return Err(cancelled());
            }
            let mut child = Command::new(bin)
                .args(find.then_some("--find"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| spawn_error(bin, &e))?;
            let stdout = child.stdout.take().expect("stdout is piped");
            *slot = Some((seq, child));
            (stdout, self.window_opened())
        };
        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out);
        let child = {
            let mut slot = lock(&self.signin_child);
            match slot.take() {
                Some((s, child)) if s == seq => Some(child),
                other => {
                    *slot = other;
                    None
                }
            }
        };
        // Gone from the slot: SignOut killed and reaped it.
        let mut child = child.ok_or_else(cancelled)?;
        let status = child
            .wait()
            .map_err(|e| format!("waiting for {}: {e}", bin.display()))?;
        drop(window);
        read.map_err(|e| format!("reading the sign-in window's output: {e}"))?;
        if !status.success() {
            return Err(format!("{} exited with {status} (window closed?)", bin.display()));
        }
        if find {
            return self.store_find_my(seq, find_my_capture(&out)?);
        }
        let (mut jar, params) = parse_capture(&out)?.into_parts();
        if jar.is_empty() {
            return Err("the sign-in window captured no icloud.com cookies".into());
        }
        let now = time::now_secs();
        let v = apple::validate(
            &self.agent,
            &self.cfg.setup_url,
            &cookies::header(&jar, now),
            &params,
            None,
        )
        .map_err(|e| match e {
            ValidateError::SignedOut => "Apple did not accept the captured session".to_string(),
            ValidateError::Offline(m) => format!("Apple unreachable, offline? {m}"),
            ValidateError::Failed(m) => m,
        })?;
        cookies::merge_set_cookies(&mut jar, &v.set_cookies, now);
        let account = Account {
            apple_id: v.apple_id,
            dsid: v.dsid,
            cookies: jar,
            client_params: params,
            webservices: v.webservices,
            validated_at: now,
            captured_at: time::rfc3339_millis(time::now_ms()),
            find_my: None,
        };
        let mut st = lock(&self.state);
        if st.signin_seq != seq {
            return Err(cancelled());
        }
        let mut account = account;
        // Signing in again as the same Apple ID keeps its Find My session,
        // which Apple judges on its own.
        if let Some(old) = st.account.as_mut().filter(|old| old.dsid == account.dsid) {
            account.find_my = old.find_my.take();
        }
        st.account = Some(account);
        st.generation += 1;
        let save = self.save_account(&mut st);
        drop(st);
        self.write(save);
        Ok(())
    }

    /// Keeps what the Find My window captured as the account's Find My jar:
    /// not validated (Apple's `/validate` refuses a one-factor session),
    /// and the main jar and its generation are left alone. The account
    /// it names (see [`find_my_capture`]) must be the signed-in one.
    fn store_find_my(&self, seq: u64, (jar, params, dsid): FindMyCapture) -> Result<(), String> {
        let mut st = lock(&self.state);
        if st.signin_seq != seq {
            return Err("cancelled by SignOut".into());
        }
        let Some(account) = st.account.as_mut() else {
            return Err("signed out meanwhile; sign in first".into());
        };
        same_account(dsid, &account.dsid)?;
        account.find_my = Some(FindMyJar {
            cookies: jar,
            client_params: params,
            captured_at: time::rfc3339_millis(time::now_ms()),
        });
        let save = self.save_account(&mut st);
        drop(st);
        self.write(save);
        Ok(())
    }

    /// Forgets the account, closes an open sign-in window (its result is
    /// dropped), then deletes the window's WebKit profile.
    fn sign_out(&self) {
        let save = {
            let mut slot = lock(&self.signin_child);
            if let Some((_, mut child)) = slot.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let mut st = lock(&self.state);
            st.signin_seq += 1;
            st.signing_in = false;
            st.last_activity = Instant::now();
            st.account.is_some().then(|| self.forget(&mut st))
        };
        if let Some(save) = save {
            self.write(save);
        }
        for dir in [&self.cfg.paths.webkit_data, &self.cfg.paths.webkit_cache] {
            if let Err(e) = files::remove_dir(dir) {
                eprintln!("icloud-sessiond: removing {}: {e}", dir.display());
            }
        }
        self.publish();
    }

    // ------------------------------------------------ clients, heartbeat

    /// Records every caller of our object: the heartbeat runs while one
    /// called recently, and the daemon stays up while one is connected.
    fn track_clients(&self, messages: zbus::blocking::MessageIterator) {
        let own = self.conn().and_then(|c| c.unique_name()).map(|n| n.to_string());
        for msg in messages {
            let Ok(msg) = msg else { break };
            if msg.message_type() != zbus::message::Type::MethodCall {
                continue;
            }
            let header = msg.header();
            if header.path().map(|p| p.as_str()) != Some(OBJECT_PATH) {
                continue;
            }
            let Some(sender) = header.sender().map(|s| s.to_string()) else {
                continue;
            };
            if Some(&sender) == own.as_ref() {
                continue;
            }
            let mut st = lock(&self.state);
            st.clients.insert(sender);
            st.last_call = Instant::now();
            st.last_activity = Instant::now();
        }
        // The bus went away (session ended): nothing left to serve.
        eprintln!("icloud-sessiond: lost the bus connection, exiting");
        std::process::exit(0);
    }

    fn tick_until_idle(self: &Arc<Daemon>) {
        let tick = (self.cfg.idle.min(self.cfg.validate_max_age) / 4)
            .clamp(Duration::from_millis(50), Duration::from_secs(30));
        let dbus = self.conn().and_then(|c| zbus::blocking::fdo::DBusProxy::new(c).ok());
        loop {
            thread::sleep(tick);

            let clients: Vec<String> = lock(&self.state).clients.iter().cloned().collect();
            let gone: Vec<String> = clients
                .into_iter()
                .filter(|name| {
                    let Some(dbus) = &dbus else { return false };
                    let Ok(name) = zbus::names::BusName::try_from(name.as_str()) else {
                        return true;
                    };
                    !dbus.name_has_owner(name).unwrap_or(true)
                })
                .collect();

            let heartbeat = {
                let mut st = lock(&self.state);
                for name in &gone {
                    st.clients.remove(name);
                }
                if st.clients.is_empty() && !st.signing_in && st.last_activity.elapsed() >= self.cfg.idle {
                    return;
                }
                st.account.is_some() && st.last_call.elapsed() < ACTIVE_WINDOW
            };
            if heartbeat {
                self.refresh_in_background();
            }
        }
    }
}

/// A running sign-in window ([`Daemon::window_opened`]). When the last one
/// closes, WebKit's HTTP cache (`WebKitCache`, tens of MB of iCloud's
/// scripts) goes: a sign-in is rare, and the next one fetches afresh. The
/// rest of the profile (cookies, device trust, HSTS) stays.
struct Window<'a>(&'a Daemon);

impl Drop for Window<'_> {
    fn drop(&mut self) {
        if self.0.windows.fetch_sub(1, Ordering::SeqCst) == 1 {
            let dir = self.0.cfg.paths.webkit_cache.join("WebKitCache");
            if let Err(e) = files::remove_dir(&dir) {
                eprintln!("icloud-sessiond: removing {}: {e}", dir.display());
            }
        }
    }
}

/// Why the sign-in window could not be started, saying how to install it
/// when it is not there.
fn spawn_error(bin: &std::path::Path, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!(
            "the sign-in window ({}) is not installed; reinstall icloud-session (sudo pacman -S icloud-session)",
            bin.display()
        )
    } else {
        format!("running {}: {e}", bin.display())
    }
}

/// `d` in whole seconds, rounded up (unix-second timestamps compare with it).
fn whole_secs(d: Duration) -> u64 {
    d.as_secs_f64().ceil() as u64
}

/// What `icloud-session-signin` prints.
#[derive(Debug, Deserialize)]
struct Capture {
    cookies: Vec<Cookie>,
    /// The account the window saw (`--find` only).
    #[serde(default)]
    dsid: Option<String>,
    #[serde(rename = "clientId")]
    client_id: Option<String>,
    #[serde(rename = "clientBuildNumber")]
    client_build_number: Option<String>,
    #[serde(rename = "clientMasteringNumber")]
    client_mastering_number: Option<String>,
}

/// A `--find` capture: its jar (holding Find My's cookie), its client
/// params, and the account it names, if it names one.
type FindMyCapture = (Vec<Cookie>, BTreeMap<String, String>, Option<String>);

fn parse_capture(out: &[u8]) -> Result<Capture, String> {
    serde_json::from_slice(out).map_err(|e| format!("reading the sign-in window's output: {e}"))
}

/// Reads what a `--find` window printed. The account is the dsid the
/// window reported, else the one the jar's X-APPLE-WEBAUTH-USER names; an
/// empty one counts as none.
fn find_my_capture(out: &[u8]) -> Result<FindMyCapture, String> {
    let capture = parse_capture(out)?;
    let window_dsid = capture.dsid.clone().filter(|d| !d.is_empty());
    let (jar, params) = capture.into_parts();
    if !cookies::find_my_cookie(&jar, time::now_secs()) {
        return Err(format!("the Find My window captured no {} cookie", cookies::FIND_MY));
    }
    let dsid = window_dsid.or_else(|| cookies::user_dsid(&jar).filter(|d| !d.is_empty()));
    Ok((jar, params, dsid))
}

/// Refuses a Find My jar made for another account than `account_dsid`.
fn same_account(found: Option<String>, account_dsid: &str) -> Result<(), String> {
    match found {
        Some(other) if other != account_dsid => Err(format!(
            "Find My was signed in as another Apple ID (dsid {other}, signed in as {account_dsid}); not kept"
        )),
        _ => Ok(()),
    }
}

impl Capture {
    /// The icloud.com cookies, and client params with icloud-md's fallbacks.
    fn into_parts(self) -> (Vec<Cookie>, BTreeMap<String, String>) {
        let mut jar: Vec<Cookie> = Vec::new();
        for mut cookie in self.cookies {
            if !cookies::is_icloud_domain(&cookie.domain) || cookie.name.is_empty() {
                continue;
            }
            cookie.expires = cookie.expires.filter(|&e| e > 0);
            match jar.iter_mut().find(|x| x.name == cookie.name) {
                Some(existing) => *existing = cookie,
                None => jar.push(cookie),
            }
        }
        let pick = |v: Option<String>, default: &str| v.filter(|s| !s.is_empty()).unwrap_or_else(|| default.into());
        let params = BTreeMap::from([
            (
                files::CLIENT_ID.to_string(),
                pick(self.client_id, &uuid::Uuid::new_v4().to_string()),
            ),
            (
                files::CLIENT_BUILD_NUMBER.to_string(),
                pick(self.client_build_number, files::DEFAULT_CLIENT_BUILD_NUMBER),
            ),
            (
                files::CLIENT_MASTERING_NUMBER.to_string(),
                pick(self.client_mastering_number, files::DEFAULT_CLIENT_MASTERING_NUMBER),
            ),
        ]);
        (jar, params)
    }
}

// ------------------------------------------------------------------ D-Bus

struct Service(Arc<Daemon>);

impl Service {
    fn props(&self) -> Props {
        lock(&self.0.state).props()
    }
}

#[zbus::interface(name = "io.github.ferdousbhai.ICloudSession")]
impl Service {
    #[zbus(property, name = "SignedIn")]
    fn signed_in(&self) -> bool {
        self.props().signed_in
    }

    #[zbus(property, name = "AppleId")]
    fn apple_id(&self) -> String {
        self.props().apple_id
    }

    #[zbus(property, name = "Dsid")]
    fn dsid(&self) -> String {
        self.props().dsid
    }

    #[zbus(property, name = "ExpiresAt")]
    fn expires_at(&self) -> u64 {
        self.props().expires_at
    }

    #[zbus(property, name = "SigningIn")]
    fn signing_in(&self) -> bool {
        self.props().signing_in
    }

    /// `(cookie_header, client_params, webservices)`; validates first only
    /// when the last `/validate` is older than 6 hours, and in the
    /// background when it is older than 10 minutes.
    #[zbus(name = "Session", out_args("cookie_header", "client_params", "webservices"))]
    // The literal tuple (not `SessionReply`) lets the macro see three out args.
    async fn session(&self) -> Result<(String, HashMap<String, String>, HashMap<String, String>), ServiceError> {
        let d = self.0.clone();
        blocking::unblock(move || d.session()).await
    }

    /// Raw `Set-Cookie` header values a client received from Apple.
    #[zbus(name = "MergeCookies")]
    async fn merge_cookies(&self, set_cookies: Vec<String>) {
        let d = self.0.clone();
        blocking::unblock(move || d.merge_cookies(&set_cookies)).await
    }

    /// A client got 421/401 with the current cookie. The daemon confirms
    /// with `/validate`: true = still signed in (fetch `Session()` and retry
    /// once), false = signed out.
    #[zbus(name = "ReportSignInRequired", out_args("still_signed_in"))]
    async fn report_sign_in_required(&self) -> bool {
        let d = self.0.clone();
        blocking::unblock(move || d.report_sign_in_required()).await
    }

    #[zbus(property, name = "FindMyAuthorized")]
    fn find_my_authorized(&self) -> bool {
        self.props().find_my_authorized
    }

    /// Opens the sign-in window unless it is open; returns at once.
    #[zbus(name = "SignIn")]
    async fn sign_in(&self) {
        let d = self.0.clone();
        blocking::unblock(move || d.sign_in(false)).await
    }

    /// Opens the sign-in window on Find My, where Apple asks for the
    /// password before Find My answers, unless a window is open; returns at
    /// once. The captured jar becomes the Find My session.
    #[zbus(name = "AuthorizeFindMy")]
    async fn authorize_find_my(&self) {
        let d = self.0.clone();
        blocking::unblock(move || d.sign_in(true)).await
    }

    /// `(cookie_header, client_params)` of the Find My session, for the
    /// `findme` host only; error `FindMyAuthRequired` when there is none.
    #[zbus(name = "FindMySession", out_args("cookie_header", "client_params"))]
    async fn find_my_session(&self) -> Result<(String, HashMap<String, String>), ServiceError> {
        let d = self.0.clone();
        blocking::unblock(move || d.find_my_session()).await
    }

    /// Raw `Set-Cookie` header values a client received from Find My.
    #[zbus(name = "MergeFindMyCookies")]
    async fn merge_find_my_cookies(&self, set_cookies: Vec<String>) {
        let d = self.0.clone();
        blocking::unblock(move || d.merge_find_my_cookies(&set_cookies)).await
    }

    /// A client got HTTP 450 from Find My: forgets the Find My session and,
    /// with a password stored, signs in to Find My again. True = a new
    /// Find My session is held (retry once).
    #[zbus(name = "ReportFindMyAuthRequired", out_args("reauthorized"))]
    async fn report_find_my_auth_required(&self) -> bool {
        let d = self.0.clone();
        blocking::unblock(move || d.report_find_my_auth_required()).await
    }

    #[zbus(property, name = "FindMyPasswordStored")]
    fn find_my_password_stored(&self) -> bool {
        self.props().find_my_password_stored
    }

    /// Verifies the Apple ID password with a Find My sign-in, then stores
    /// it in the keyring for automatic Find My re-authorization.
    #[zbus(name = "SetPassword")]
    async fn set_password(&self, password: String) -> Result<(), ServiceError> {
        let d = self.0.clone();
        let password = Password::new(password);
        blocking::unblock(move || d.set_password(password)).await
    }

    /// Removes the stored password(s) from the keyring.
    #[zbus(name = "ForgetPassword")]
    async fn forget_password(&self) -> Result<(), ServiceError> {
        let d = self.0.clone();
        blocking::unblock(move || d.forget_password()).await
    }

    /// Forgets the account and the sign-in window's WebKit profile.
    #[zbus(name = "SignOut")]
    async fn sign_out(&self) {
        let d = self.0.clone();
        blocking::unblock(move || d.sign_out()).await
    }
}
