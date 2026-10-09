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
    /// `$XDG_DATA_HOME/icloud-session/webkit` (the sign-in window's profile)
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
        Paths {
            account: xdg("XDG_STATE_HOME", ".local/state").join("account.json"),
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
    pub fn param(&self, name: &str) -> &str {
        self.client_params.get(name).map(String::as_str).unwrap_or_default()
    }

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
    pub fn load(path: &Path) -> io::Result<Option<Stored>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// [`Stored::load`], but a file that cannot be read or parsed is moved
    /// aside to `account.json.bad` and treated as signed out, so one bad
    /// write never keeps the daemon from starting.
    pub fn load_or_set_aside(path: &Path) -> Option<Stored> {
        match Stored::load(path) {
            Ok(stored) => stored,
            Err(e) => {
                let bad = path.with_extension("json.bad");
                eprintln!(
                    "icloud-sessiond: cannot read {}: {e}; moved it to {} and starting signed out",
                    path.display(),
                    bad.display()
                );
                if let Err(e) = fs::rename(path, &bad) {
                    eprintln!("icloud-sessiond: moving {} aside: {e}", path.display());
                }
                None
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

/// Removes a file, treating "already gone" as success.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
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
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).expect("JSON serializes");
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Writes `bytes` to a temp file (mode 0600) beside `path`, then renames it
/// over `path`, so a reader sees either the old file or the new one.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Creates `dir` and any missing parents with mode 0700.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
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
        let stored = Stored::load(&path).unwrap().unwrap();
        assert_eq!(Account::join(stored.clone(), secret.clone()), Some(account));
        let other = SessionSecret {
            dsid: "999".into(),
            ..secret
        };
        assert_eq!(Account::join(stored, other), None, "another account's jars");
    }

    #[test]
    fn a_bad_account_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        fs::write(&path, "{not json").unwrap();
        assert_eq!(Stored::load_or_set_aside(&path), None);
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("account.json.bad")).unwrap(),
            "{not json"
        );
    }
}
