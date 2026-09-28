//! The daemon: the only owner of the account, its D-Bus interface, the
//! sign-in child, the icloud-md mirror, the heartbeat and the idle exit.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use icloud_session::{INTERFACE, OBJECT_PATH, SessionReply};
use serde::Deserialize;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::apple::{self, ValidateError};
use crate::cookies::{self, Cookie};
use crate::files::{self, Account, Paths};

/// Heartbeat keeps running while a client called within this window
/// (the browser's own heartbeat is 14 minutes).
const ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
pub struct Config {
    pub paths: Paths,
    pub setup_url: String,
    pub signin_bin: PathBuf,
    /// Exit after this long with no clients and no sign-in (5 minutes).
    pub idle: Duration,
    /// Revalidate when the last `/validate` is older than this (10 minutes).
    pub validate_max_age: Duration,
    /// After a `/validate` that got no answer, hand out the session as it is
    /// for this long before trying Apple again (1 minute).
    pub validate_retry: Duration,
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

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.ferdousbhai.ICloudSession.Error")]
pub enum ServiceError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Signed out: the apps show a sign-in banner.
    SignInRequired(String),
}

fn sign_in_required() -> ServiceError {
    ServiceError::SignInRequired("sign in to iCloud required".into())
}

/// The D-Bus properties, as last announced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Props {
    signed_in: bool,
    apple_id: String,
    dsid: String,
    expires_at: u64,
    signing_in: bool,
}

/// How the last `/validate` of an account ended.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    finished: Instant,
    /// Apple answered 2xx (a 421/401 forgets the account instead).
    ok: bool,
    generation: u64,
}

struct State {
    account: Option<Account>,
    /// Bumped whenever `account` is replaced or forgotten, so a `/validate`
    /// of an earlier account never touches the current one.
    generation: u64,
    last_attempt: Option<Attempt>,
    signing_in: bool,
    /// The cookie header now in icloud-md's session file, as far as we know
    /// (written by us or adopted from it). Tells our own writes apart.
    mirror_cookie: Option<String>,
    /// Last method call from a client (heartbeat).
    last_call: Instant,
    /// Last call or sign-in end (idle exit).
    last_activity: Instant,
    /// Unique bus names that called us, pruned when they leave the bus.
    clients: HashSet<String>,
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
        }
    }
}

struct MirrorWatch {
    watches: inotify::Watches,
    current: Option<(inotify::WatchDescriptor, String)>,
}

pub struct Daemon {
    cfg: Config,
    agent: ureq::Agent,
    started_at: u64,
    state: Mutex<State>,
    /// One `/validate` at a time; a waiter re-checks freshness after it.
    validate_lock: Mutex<()>,
    /// Orders property announcements.
    published: Mutex<Props>,
    conn: OnceLock<Connection>,
    mirror_watch: Mutex<Option<MirrorWatch>>,
    /// A heartbeat `/validate` is running (on its own thread, so a slow
    /// Apple never holds up the idle exit).
    heartbeat_busy: AtomicBool,
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
            signing_in: false,
            mirror_cookie: None,
            last_call: Instant::now(),
            last_activity: Instant::now(),
            clients: HashSet::new(),
        };
        let props = state.props();
        Arc::new(Daemon {
            cfg,
            agent: apple::agent(),
            started_at: now_unix(),
            state: Mutex::new(state),
            validate_lock: Mutex::new(()),
            published: Mutex::new(props),
            conn: OnceLock::new(),
            mirror_watch: Mutex::new(None),
            heartbeat_busy: AtomicBool::new(false),
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
        // Never queue behind another instance; fail instead.
        let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
        if conn.request_name_with_flags(icloud_session::BUS_NAME, flags)? != zbus::fdo::RequestNameReply::PrimaryOwner {
            return Err(zbus::Error::NameTaken);
        }

        // Pick up what icloud-md rotated while we were not running, and make
        // sure the mirror exists. Holding the validate lock makes a
        // `Session()` that is already waiting use the adopted jar.
        {
            let _one = lock(&self.validate_lock);
            let dsid = lock(&self.state).account.as_ref().map(|a| a.dsid.clone());
            if let Some(dsid) = dsid {
                self.adopt_mirror(&dsid);
                let mut st = lock(&self.state);
                self.store(&mut st);
            }
            self.start_mirror_watch();
        }

        // Validate once on start (unless a caller already made us).
        let me = self.clone();
        thread::spawn(move || {
            let _ = me.ensure_fresh(Fresh::Since(me.started_at));
        });

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
        let mut published = lock(&self.published);
        let now = lock(&self.state).props();
        if *published == now {
            return;
        }
        let mut changed: HashMap<&str, Value<'_>> = HashMap::new();
        if published.signed_in != now.signed_in {
            changed.insert("SignedIn", now.signed_in.into());
        }
        if published.apple_id != now.apple_id {
            changed.insert("AppleId", now.apple_id.as_str().into());
        }
        if published.dsid != now.dsid {
            changed.insert("Dsid", now.dsid.as_str().into());
        }
        if published.expires_at != now.expires_at {
            changed.insert("ExpiresAt", now.expires_at.into());
        }
        if published.signing_in != now.signing_in {
            changed.insert("SigningIn", now.signing_in.into());
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

    /// Persists the account and, if its cookie header changed, the
    /// icloud-md mirror. Errors are logged: the session in memory is still
    /// right, and the next change retries.
    fn store(&self, st: &mut State) {
        let Some(account) = &st.account else { return };
        if let Err(e) = account.save(&self.cfg.paths.account) {
            eprintln!("icloud-sessiond: saving {}: {e}", self.cfg.paths.account.display());
        }
        let now = now_unix();
        if st.mirror_cookie.as_deref() == Some(account.cookie_header(now).as_str()) {
            return;
        }
        match files::write_mirror(&self.cfg.paths, account, now) {
            Ok(cookie) => st.mirror_cookie = Some(cookie),
            Err(e) => eprintln!("icloud-sessiond: writing the icloud-md mirror: {e}"),
        }
    }

    /// Forgets the account (confirmed 421/401 or `SignOut`).
    fn forget(&self, st: &mut State) {
        st.account = None;
        st.generation += 1;
        st.mirror_cookie = None;
        if let Err(e) = files::remove(&self.cfg.paths.account) {
            eprintln!("icloud-sessiond: removing {}: {e}", self.cfg.paths.account.display());
        }
    }

    // ---------------------------------------------------------- validate

    /// Calls `/validate` unless the session is already fresh enough, then
    /// merges rotated cookies and new webservices. A 421/401 signs out.
    fn ensure_fresh(&self, fresh: Fresh) -> Result<(), Refresh> {
        let arrived = Instant::now();
        let _one = lock(&self.validate_lock);
        let (cookie, params, dsid, generation) = {
            let mut st = lock(&self.state);
            let generation = st.generation;
            let Some(a) = &st.account else {
                return Err(Refresh::SignedOut);
            };
            let last = st.last_attempt.filter(|at| at.generation == generation);
            // Waited behind an attempt that finished meanwhile: its answer
            // is ours too, rather than another serial round trip to Apple.
            if let Some(at) = last.filter(|at| at.finished >= arrived) {
                if !at.ok {
                    return Err(Refresh::Failed);
                }
                if matches!(fresh, Fresh::Confirm) {
                    st.mirror_cookie = None;
                    self.store(&mut st);
                }
                return Ok(());
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
                a.cookie_header(now_unix()),
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
        let outcome = match result {
            Ok(v) => {
                if let Some(a) = st.account.as_mut().filter(|_| same) {
                    cookies::merge_set_cookies(&mut a.cookies, &v.set_cookies, now_unix());
                    a.webservices = v.webservices;
                    a.apple_id = v.apple_id;
                    a.validated_at = now_unix();
                    if matches!(fresh, Fresh::Confirm) {
                        // A client (icloud-md) may hold a stale jar: rewrite
                        // the mirror so it can retry with the fresh one.
                        st.mirror_cookie = None;
                    }
                    self.store(&mut st);
                }
                Ok(())
            }
            Err(ValidateError::SignedOut) => {
                if same {
                    eprintln!("icloud-sessiond: Apple ended the session; signed out");
                    self.forget(&mut st);
                }
                Err(Refresh::SignedOut)
            }
            Err(ValidateError::Failed(msg)) => {
                eprintln!("icloud-sessiond: {msg}");
                Err(Refresh::Failed)
            }
        };
        drop(st);
        self.rewatch();
        self.publish();
        outcome
    }

    // ----------------------------------------------------------- methods

    /// Validated within `validate_max_age`.
    fn fresh_enough(&self) -> Fresh {
        let max_age = self.cfg.validate_max_age.as_secs_f64().ceil() as u64;
        Fresh::Since(now_unix().saturating_sub(max_age))
    }

    fn session(&self) -> Result<SessionReply, ServiceError> {
        match self.ensure_fresh(self.fresh_enough()) {
            Err(Refresh::SignedOut) => return Err(sign_in_required()),
            // Apple unreachable: hand out what we have; the app's own
            // request will fail the same way and say so.
            Err(Refresh::Failed) | Ok(()) => {}
        }
        let st = lock(&self.state);
        let a = st.account.as_ref().ok_or_else(sign_in_required)?;
        let map = |m: &BTreeMap<String, String>| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok((a.cookie_header(now_unix()), map(&a.client_params), map(&a.webservices)))
    }

    fn merge_cookies(&self, set_cookies: &[String]) {
        let mut st = lock(&self.state);
        if let Some(a) = st.account.as_mut()
            && cookies::merge_set_cookies(&mut a.cookies, set_cookies, now_unix())
        {
            self.store(&mut st);
        }
        drop(st);
        self.publish();
    }

    /// Confirms with Apple before signing every app out. A 2xx keeps the
    /// session (and rewrites the mirror); an unreachable Apple changes
    /// nothing. A `/validate` that finishes after the report arrived counts
    /// as the confirmation, so a burst of reports costs one round trip.
    /// Returns whether the account is still signed in.
    fn report_sign_in_required(&self) -> bool {
        match self.ensure_fresh(Fresh::Confirm) {
            Ok(()) | Err(Refresh::Failed) => lock(&self.state).account.is_some(),
            Err(Refresh::SignedOut) => false,
        }
    }

    fn sign_in(self: &Arc<Daemon>) {
        {
            let mut st = lock(&self.state);
            if st.signing_in {
                return;
            }
            st.signing_in = true;
        }
        self.publish();
        let me = self.clone();
        thread::spawn(move || {
            if let Err(e) = me.run_sign_in() {
                eprintln!("icloud-sessiond: sign-in: {e}");
            }
            let mut st = lock(&me.state);
            st.signing_in = false;
            st.last_activity = Instant::now();
            drop(st);
            me.rewatch();
            me.publish();
        });
    }

    /// Runs the sign-in window, validates what it captured, stores it.
    fn run_sign_in(&self) -> Result<(), String> {
        let bin = &self.cfg.signin_bin;
        let output = Command::new(bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("running {}: {e}", bin.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} exited with {} (window closed?)",
                bin.display(),
                output.status
            ));
        }
        let capture: Capture =
            serde_json::from_slice(&output.stdout).map_err(|e| format!("reading the sign-in window's output: {e}"))?;
        let (mut jar, params) = capture.into_parts();
        if jar.is_empty() {
            return Err("the sign-in window captured no icloud.com cookies".into());
        }
        let now = now_unix();
        let v = apple::validate(
            &self.agent,
            &self.cfg.setup_url,
            &cookies::header(&jar, now),
            &params,
            None,
        )
        .map_err(|e| match e {
            ValidateError::SignedOut => "Apple did not accept the captured session".to_string(),
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
            captured_at: humantime::format_rfc3339_millis(SystemTime::now()).to_string(),
        };
        let mut st = lock(&self.state);
        st.account = Some(account);
        st.generation += 1;
        st.mirror_cookie = None;
        self.store(&mut st);
        Ok(())
    }

    fn sign_out(&self) {
        let mut st = lock(&self.state);
        if let Some(dsid) = st.account.as_ref().map(|a| a.dsid.clone()) {
            if let Err(e) = files::remove(&self.cfg.paths.mirror_session(&dsid)) {
                eprintln!("icloud-sessiond: removing the icloud-md mirror: {e}");
            }
            self.forget(&mut st);
        }
        drop(st);
        for dir in [&self.cfg.paths.webkit_data, &self.cfg.paths.webkit_cache] {
            if let Err(e) = files::remove_dir(dir) {
                eprintln!("icloud-sessiond: removing {}: {e}", dir.display());
            }
        }
        self.rewatch();
        self.publish();
    }

    // ------------------------------------------------------------ mirror

    fn start_mirror_watch(self: &Arc<Daemon>) {
        let mut inotify = match inotify::Inotify::init() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("icloud-sessiond: inotify: {e}; icloud-md rotations will not be adopted");
                return;
            }
        };
        *lock(&self.mirror_watch) = Some(MirrorWatch {
            watches: inotify.watches(),
            current: None,
        });
        self.rewatch();
        let me = self.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                let events = match inotify.read_events_blocking(&mut buffer) {
                    Ok(events) => events,
                    Err(e) => {
                        eprintln!("icloud-sessiond: inotify: {e}");
                        return;
                    }
                };
                let mut hit = None;
                for event in events {
                    if event.name != Some(OsStr::new("session.local.json")) {
                        continue;
                    }
                    let watch = lock(&me.mirror_watch);
                    if let Some((wd, dsid)) = watch.as_ref().and_then(|w| w.current.as_ref())
                        && *wd == event.wd
                    {
                        hit = Some(dsid.clone());
                    }
                }
                if let Some(dsid) = hit {
                    me.adopt_mirror(&dsid);
                }
            }
        });
    }

    /// Points the inotify watch at the current account's mirror directory.
    fn rewatch(&self) {
        let dsid = lock(&self.state).account.as_ref().map(|a| a.dsid.clone());
        let mut guard = lock(&self.mirror_watch);
        let Some(w) = guard.as_mut() else { return };
        if w.current.as_ref().map(|(_, d)| d) == dsid.as_ref() {
            return;
        }
        if let Some((wd, _)) = w.current.take() {
            let _ = w.watches.remove(wd);
        }
        let Some(dsid) = dsid else { return };
        let dir = self.cfg.paths.mirror_dir(&dsid);
        let mask = inotify::WatchMask::CLOSE_WRITE | inotify::WatchMask::MOVED_TO;
        match files::create_private_dir(&dir).and_then(|()| w.watches.add(&dir, mask)) {
            Ok(wd) => w.current = Some((wd, dsid)),
            Err(e) => eprintln!("icloud-sessiond: watching {}: {e}", dir.display()),
        }
    }

    /// Adopts a cookie jar icloud-md wrote to the mirror (its own
    /// `/validate` rotation). Our own writes are recognised and skipped.
    fn adopt_mirror(&self, dsid: &str) {
        let Some(m) = files::read_mirror(&self.cfg.paths, dsid) else {
            return;
        };
        let mut st = lock(&self.state);
        if st.mirror_cookie.as_deref() == Some(m.cookie.as_str()) {
            return;
        }
        let Some(a) = st.account.as_mut().filter(|a| a.dsid == dsid) else {
            return;
        };
        let mut changed = cookies::adopt_header(&mut a.cookies, &m.cookie);
        for (k, v) in m.params {
            if a.client_params.get(&k) != Some(&v) {
                a.client_params.insert(k, v);
                changed = true;
            }
        }
        st.mirror_cookie = Some(m.cookie);
        if changed {
            eprintln!("icloud-sessiond: adopted the session icloud-md rotated");
            if let Some(a) = &st.account
                && let Err(e) = a.save(&self.cfg.paths.account)
            {
                eprintln!("icloud-sessiond: saving {}: {e}", self.cfg.paths.account.display());
            }
        }
        drop(st);
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
            if heartbeat && !self.heartbeat_busy.swap(true, Ordering::SeqCst) {
                let me = self.clone();
                thread::spawn(move || {
                    let _ = me.ensure_fresh(me.fresh_enough());
                    me.heartbeat_busy.store(false, Ordering::SeqCst);
                });
            }
        }
    }
}

/// What `icloud-session-signin` prints.
#[derive(Debug, Deserialize)]
struct Capture {
    cookies: Vec<CapturedCookie>,
    #[serde(rename = "clientId")]
    client_id: Option<String>,
    #[serde(rename = "clientBuildNumber")]
    client_build_number: Option<String>,
    #[serde(rename = "clientMasteringNumber")]
    client_mastering_number: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CapturedCookie {
    name: String,
    value: String,
    domain: Option<String>,
    path: Option<String>,
    expires: Option<u64>,
}

impl Capture {
    /// The icloud.com cookies, and client params with icloud-md's fallbacks.
    fn into_parts(self) -> (Vec<Cookie>, BTreeMap<String, String>) {
        let mut jar: Vec<Cookie> = Vec::new();
        for c in self.cookies {
            let domain = c.domain.unwrap_or_else(|| ".icloud.com".into());
            if !cookies::is_icloud_domain(&domain) || c.name.is_empty() {
                continue;
            }
            let cookie = Cookie {
                name: c.name,
                value: c.value,
                domain,
                path: c.path.unwrap_or_else(|| "/".into()),
                expires: c.expires.filter(|&e| e > 0),
            };
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

    /// `(cookie_header, client_params, webservices)`; revalidates first when
    /// the last `/validate` is older than 10 minutes.
    #[zbus(name = "Session", out_args("cookie_header", "client_params", "webservices"))]
    // The literal tuple (not `SessionReply`) lets the macro see three out args.
    #[allow(clippy::type_complexity)]
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
    /// with `/validate`: true = still signed in (the mirror was rewritten,
    /// retry once), false = signed out.
    #[zbus(name = "ReportSignInRequired", out_args("still_signed_in"))]
    async fn report_sign_in_required(&self) -> bool {
        let d = self.0.clone();
        blocking::unblock(move || d.report_sign_in_required()).await
    }

    /// Opens the sign-in window unless it is open; returns at once.
    #[zbus(name = "SignIn")]
    async fn sign_in(&self) {
        let d = self.0.clone();
        blocking::unblock(move || d.sign_in()).await
    }

    /// Forgets the account and the sign-in window's WebKit profile.
    #[zbus(name = "SignOut")]
    async fn sign_out(&self) {
        let d = self.0.clone();
        blocking::unblock(move || d.sign_out()).await
    }
}
