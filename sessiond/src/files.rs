//! Where things live, the account file, and atomic 0600 writes.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::cookies::{self, Cookie};
use crate::crash;

/// The client parameter names, as the web client sends them.
pub const CLIENT_ID: &str = "clientId";
pub const CLIENT_BUILD_NUMBER: &str = "clientBuildNumber";
pub const CLIENT_MASTERING_NUMBER: &str = "clientMasteringNumber";

/// icloud-md's fallbacks (`auth/clientConstants.js`) when the sign-in page's
/// own setup request did not carry them.
pub const DEFAULT_CLIENT_BUILD_NUMBER: &str = "2624Build27";
pub const DEFAULT_CLIENT_MASTERING_NUMBER: &str = "2624Build27";

#[derive(Debug, Clone)]
pub struct Paths {
    /// `$XDG_STATE_HOME/icloud-session/account.json`
    pub account: PathBuf,
    /// `$XDG_STATE_HOME/icloud-session/session-removal-pending`: a sign-out
    /// could not remove the session from the keyring; removed once it does.
    pub removal_pending: PathBuf,
    /// `$XDG_STATE_HOME/icloud-session/session-change-pending`: a sign-in
    /// or sign-out is changing whose session the keyring and
    /// `account.json` hold; removed once both are written. Left by a
    /// daemon killed in between.
    pub change_pending: PathBuf,
    /// `$XDG_STATE_HOME/icloud-session/change.lock`: `flock`ed by the
    /// daemon making that change, so another one starting meanwhile can
    /// tell a change under way from one cut off.
    pub change_lock: PathBuf,
    /// `$XDG_STATE_HOME/icloud-session/sign-out-reason`: why the account was
    /// last signed out (`SignOutReason`); removed by a sign-in.
    pub sign_out_reason: PathBuf,
    /// `$XDG_DATA_HOME/icloud-session/webkit`: the WebKit profile older
    /// sign-in windows kept, deleted at start (the window keeps none now)
    pub webkit_data: PathBuf,
    /// `$XDG_CACHE_HOME/icloud-session/webkit`
    pub webkit_cache: PathBuf,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

impl Paths {
    pub fn from_env() -> Paths {
        let home = env_dir("HOME").unwrap_or_else(|| PathBuf::from("/"));
        let xdg = |name: &str, fallback: &str| {
            env_dir(name)
                .unwrap_or_else(|| home.join(fallback))
                .join("icloud-session")
        };
        let state = xdg("XDG_STATE_HOME", ".local/state");
        Paths {
            account: state.join("account.json"),
            removal_pending: state.join("session-removal-pending"),
            change_pending: state.join("session-change-pending"),
            change_lock: state.join("change.lock"),
            sign_out_reason: state.join("sign-out-reason"),
            webkit_data: xdg("XDG_DATA_HOME", ".local/share").join("webkit"),
            webkit_cache: xdg("XDG_CACHE_HOME", ".cache").join("webkit"),
        }
    }
}

/// The one signed-in account, held in memory by the daemon. It is kept in
/// two places: `account.json` ([`Stored`]) has what is not a secret, the
/// keyring ([`SessionSecret`]) has the cookie jars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub apple_id: String,
    pub dsid: String,
    /// `dsInfo`'s name, from the sign-in and each `/validate`; may be empty.
    pub full_name: String,
    pub cookies: Vec<Cookie>,
    /// clientId, clientBuildNumber, clientMasteringNumber.
    pub client_params: BTreeMap<String, String>,
    pub webservices: BTreeMap<String, String>,
    /// Unix seconds of the last successful `/validate`.
    pub validated_at: u64,
    /// RFC 3339 time of the sign-in.
    pub captured_at: String,
    /// Find My's own session from `AuthorizeFindMy()`, until a client
    /// reports a Find My 450. Apple's password prompt on icloud.com/find is
    /// a one-factor sign-in: good for Find My, refused by `/validate`, so
    /// it is kept apart from the main jar and never validated.
    pub find_my: Option<FindMyJar>,
}

/// The cookies (session-only ones included) and client params the Find My
/// window captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindMyJar {
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub client_params: BTreeMap<String, String>,
    #[serde(default)]
    pub captured_at: String,
}

impl FindMyJar {
    /// A jar captured just now.
    pub fn captured_now(cookies: Vec<Cookie>, client_params: BTreeMap<String, String>) -> Self {
        Self {
            cookies,
            client_params,
            captured_at: icloud_session::time::rfc3339_millis(icloud_session::time::now_ms()),
        }
    }
}

/// `account.json`: everything but the cookie jars, which are secrets and
/// live in the keyring ([`SessionSecret`]). This part stays a file so the
/// daemon knows who is signed in, and when it last validated, without
/// unlocking anything, and so `validated_at`, which changes every few
/// minutes, is no keyring write.
///
/// `cookies` and `find_my` are only ever read, from a file written before
/// the jars moved to the keyring: the daemon moves them there and rewrites
/// the file without them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub apple_id: String,
    pub dsid: String,
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub client_params: BTreeMap<String, String>,
    #[serde(default)]
    pub webservices: BTreeMap<String, String>,
    #[serde(default)]
    pub validated_at: u64,
    #[serde(default)]
    pub captured_at: String,
    #[serde(default, skip_serializing)]
    pub cookies: Option<Vec<Cookie>>,
    #[serde(default, skip_serializing)]
    pub find_my: Option<FindMyJar>,
}

/// The keyring item's secret, as JSON: the account's jars. `dsid` ties it
/// to the `account.json` it goes with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSecret {
    pub dsid: String,
    pub cookies: Vec<Cookie>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find_my: Option<FindMyJar>,
}

impl Account {
    pub fn cookie_header(&self, now: u64) -> String {
        cookies::header(&self.cookies, now)
    }

    /// Holds a Find My jar with its authorization cookie.
    pub fn find_my_ready(&self, now: u64) -> bool {
        self.find_my
            .as_ref()
            .is_some_and(|f| cookies::find_my_cookie(&f.cookies, now))
    }

    /// What goes in `account.json`.
    pub fn stored(&self) -> Stored {
        Stored {
            apple_id: self.apple_id.clone(),
            dsid: self.dsid.clone(),
            full_name: self.full_name.clone(),
            client_params: self.client_params.clone(),
            webservices: self.webservices.clone(),
            validated_at: self.validated_at,
            captured_at: self.captured_at.clone(),
            cookies: None,
            find_my: None,
        }
    }

    /// What goes in the keyring, serialized.
    pub fn secret(&self) -> Zeroizing<String> {
        let secret = SessionSecret {
            dsid: self.dsid.clone(),
            cookies: self.cookies.clone(),
            find_my: self.find_my.clone(),
        };
        Zeroizing::new(serde_json::to_string(&secret).expect("the session secret serializes"))
    }

    /// The account from `account.json` and its keyring secret; `None` when
    /// the secret is another account's.
    pub fn join(stored: Stored, secret: SessionSecret) -> Option<Account> {
        if secret.dsid != stored.dsid {
            return None;
        }
        Some(Account {
            apple_id: stored.apple_id,
            dsid: stored.dsid,
            full_name: stored.full_name,
            cookies: secret.cookies,
            client_params: stored.client_params,
            webservices: stored.webservices,
            validated_at: stored.validated_at,
            captured_at: stored.captured_at,
            find_my: secret.find_my,
        })
    }
}

impl Stored {
    /// `account.json` (`None`: there is none). A file that cannot be read
    /// or parsed is moved aside to `account.json.bad` and treated as signed
    /// out, so one bad write never keeps the daemon from starting. `Err`:
    /// it was (logged).
    pub fn load_or_set_aside(path: &Path) -> io::Result<Option<Stored>> {
        let loaded = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        };
        match loaded {
            Ok(stored) => Ok(stored),
            Err(e) => {
                eprintln!(
                    "icloud-sessiond: cannot read {}: {e}; moved it to {} and starting signed out",
                    path.display(),
                    path.with_extension("json.bad").display()
                );
                if let Err(e) = set_aside(path) {
                    eprintln!("icloud-sessiond: moving {} aside: {e}", path.display());
                }
                Err(e)
            }
        }
    }

    /// The jars of a file written before they moved to the keyring.
    pub fn legacy_secret(&self) -> Option<SessionSecret> {
        self.cookies.as_ref().map(|cookies| SessionSecret {
            dsid: self.dsid.clone(),
            cookies: cookies.clone(),
            find_my: self.find_my.clone(),
        })
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_json(path, &serde_json::to_value(self).expect("account serializes"))
    }
}

/// `session-change-pending`: what the change of the account under way is,
/// for the start after a daemon killed in the middle of it to finish or
/// undo it. Empty (a sign-out, or one of a daemon before it said more):
/// `account.json` is removed and the keyring's session with it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Pending {
    /// A sign-in: the `account.json` it writes once its session is in the
    /// keyring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Stored>,
    /// A sign-out: the device trust goes too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forget_trust: bool,
}

impl Pending {
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_json(path, &serde_json::to_value(self).expect("the marker serializes"))
    }

    /// The marker at `path` (`None`: there is none). One that cannot be
    /// parsed is an empty one.
    pub fn load(path: &Path) -> io::Result<Option<Pending>> {
        match fs::read(path) {
            Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(Some(Pending::default())),
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!("icloud-sessiond: {}: {e}; read as an empty one", path.display());
                Pending::default()
            }))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Opens `path` for an `flock`, creating it empty (mode 0600, the parent
/// directory 0700) if need be. Never truncated or removed: every process
/// locks the same file.
pub fn open_lock(path: &Path) -> io::Result<fs::File> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

/// Removes a file, treating "already gone" as success.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        result => {
            if result.is_ok() {
                sync_dir(path)?;
            }
            crash::point("remove", &file_name(path));
            Ok(())
        }
    }
}

/// Moves `path` aside to `<path>.bad` (`account.json.bad`).
pub fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let bad = path.with_extension("json.bad");
    fs::rename(path, &bad)?;
    sync_dir(path)?;
    crash::point("rename", &file_name(path));
    Ok(bad)
}

fn file_name(path: &Path) -> std::borrow::Cow<'_, str> {
    path.file_name().unwrap_or_default().to_string_lossy()
}

pub fn remove_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// --------------------------------------------------------------- writes

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `JSON.stringify(value, null, 2) + "\n"`, written atomically, mode 0600,
/// creating the parent directory 0700.
pub fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("JSON serializes");
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Creates `path` empty (a marker), mode 0600, creating the parent
/// directory 0700.
pub fn touch(path: &Path) -> io::Result<()> {
    write_text(path, "")
}

/// `text`, written atomically, mode 0600, creating the parent directory
/// 0700.
pub fn write_text(path: &Path, text: &str) -> io::Result<()> {
    write_atomic(path, text.as_bytes())
}

/// Flushes `path`'s directory, where a rename or removal of it is kept.
fn sync_dir(path: &Path) -> io::Result<()> {
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => fs::File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

/// Writes `bytes` to a temp file (mode 0600) beside `path`, then renames it
/// over `path`, fsynced with its directory, so a reader sees either the old
/// file or the new one, after a power loss too. The
/// parent directory and any missing above it are created with mode 0700.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let name = file_name(path);
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        // The rename itself, so a power loss keeps the order of the writes.
        sync_dir(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    } else {
        crash::point("write", &name);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn account_json_holds_no_cookies_and_is_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/account.json");
        let account = Account {
            apple_id: "a@example.com".into(),
            dsid: "123".into(),
            full_name: "A B".into(),
            cookies: vec![Cookie::new("A", "secret-cookie")],
            client_params: BTreeMap::new(),
            webservices: BTreeMap::new(),
            validated_at: 1,
            captured_at: "2026-09-28T00:00:00.000Z".into(),
            find_my: Some(FindMyJar {
                cookies: vec![Cookie::new("F", "secret-fmip")],
                client_params: BTreeMap::new(),
                captured_at: String::new(),
            }),
        };
        account.stored().save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret-"), "{text}");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let secret: SessionSecret = serde_json::from_str(&account.secret()).unwrap();
        let stored = Stored::load_or_set_aside(&path).unwrap().unwrap();
        assert_eq!(Account::join(stored.clone(), secret.clone()), Some(account));
        let other = SessionSecret {
            dsid: "999".into(),
            ..secret
        };
        assert_eq!(Account::join(stored, other), None, "another account's jars");
    }
}
