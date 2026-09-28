//! Where things live, the account file, the icloud-md mirror, and atomic
//! 0600 writes.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    /// `~/.config/icloud-md/accounts` (icloud-md uses `os.homedir()`, not XDG)
    pub icloud_md_accounts: PathBuf,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

pub fn home() -> PathBuf {
    env_dir("HOME").unwrap_or_else(|| PathBuf::from("/"))
}

pub fn state_dir() -> PathBuf {
    env_dir("XDG_STATE_HOME")
        .unwrap_or_else(|| home().join(".local/state"))
        .join("icloud-session")
}

pub fn data_dir() -> PathBuf {
    env_dir("XDG_DATA_HOME")
        .unwrap_or_else(|| home().join(".local/share"))
        .join("icloud-session")
}

pub fn cache_dir() -> PathBuf {
    env_dir("XDG_CACHE_HOME")
        .unwrap_or_else(|| home().join(".cache"))
        .join("icloud-session")
}

impl Paths {
    pub fn from_env() -> Paths {
        Paths {
            account: state_dir().join("account.json"),
            webkit_data: data_dir().join("webkit"),
            webkit_cache: cache_dir().join("webkit"),
            icloud_md_accounts: home().join(".config/icloud-md/accounts"),
        }
    }

    pub fn mirror_dir(&self, dsid: &str) -> PathBuf {
        self.icloud_md_accounts.join(dsid)
    }

    pub fn mirror_session(&self, dsid: &str) -> PathBuf {
        self.mirror_dir(dsid).join("session.local.json")
    }
}

/// The one signed-in account, `account.json`. Written by the daemon only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub apple_id: String,
    pub dsid: String,
    pub cookies: Vec<Cookie>,
    /// clientId, clientBuildNumber, clientMasteringNumber.
    #[serde(default)]
    pub client_params: BTreeMap<String, String>,
    #[serde(default)]
    pub webservices: BTreeMap<String, String>,
    /// Unix seconds of the last successful `/validate`.
    #[serde(default)]
    pub validated_at: u64,
    /// RFC 3339 time of the sign-in, the mirror's `capturedAt`.
    #[serde(default)]
    pub captured_at: String,
}

impl Account {
    pub fn param(&self, name: &str) -> &str {
        self.client_params.get(name).map(String::as_str).unwrap_or_default()
    }

    pub fn cookie_header(&self, now: u64) -> String {
        cookies::header(&self.cookies, now)
    }

    pub fn load(path: &Path) -> io::Result<Option<Account>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// [`Account::load`], but a file that cannot be read or parsed is moved
    /// aside to `account.json.bad` and treated as signed out, so one bad
    /// write never keeps the daemon from starting.
    pub fn load_or_set_aside(path: &Path) -> Option<Account> {
        match Account::load(path) {
            Ok(account) => account,
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

// ----------------------------------------------------------- icloud-md

/// What [`write_mirror`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum MirrorWrite {
    /// Written; the cookie header it holds.
    Written(String),
    /// icloud-md rewrote the session file while we were writing ours, so
    /// ours was not put in place: adopt its jar, then write again.
    Changed,
}

/// Writes `accounts/<dsid>/{session.local.json,meta.json}` in icloud-md's
/// shape. Fields icloud-md added to an existing session file are kept.
pub fn write_mirror(paths: &Paths, account: &Account, now: u64) -> io::Result<MirrorWrite> {
    write_mirror_checked(paths, account, now, || {})
}

/// [`write_mirror`], with a hook that runs just before the rename (tests).
fn write_mirror_checked(
    paths: &Paths,
    account: &Account,
    now: u64,
    before_rename: impl FnOnce(),
) -> io::Result<MirrorWrite> {
    let dir = paths.mirror_dir(&account.dsid);
    create_private_dir(&dir)?;
    let session_path = paths.mirror_session(&account.dsid);
    // Read-modify-rename: icloud-md writes this file in place, and a
    // rotation it writes between our read and our rename would be lost.
    // The file must still hold what we read when ours goes in.
    let original = read_file(&session_path)?;
    let mut fields = original
        .as_deref()
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .and_then(|v: Value| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    let cookie = account.cookie_header(now);
    let set = |fields: &mut Map<String, Value>, k: &str, v: &str| {
        fields.insert(k.to_string(), Value::String(v.to_string()));
    };
    set(&mut fields, "cookie", &cookie);
    set(&mut fields, CLIENT_ID, account.param(CLIENT_ID));
    set(&mut fields, CLIENT_BUILD_NUMBER, account.param(CLIENT_BUILD_NUMBER));
    set(
        &mut fields,
        CLIENT_MASTERING_NUMBER,
        account.param(CLIENT_MASTERING_NUMBER),
    );
    set(&mut fields, "capturedAt", &account.captured_at);
    let unchanged = write_json_if(&session_path, &Value::Object(fields), || {
        before_rename();
        read_file(&session_path).is_ok_and(|now| now == original)
    })?;
    if !unchanged {
        return Ok(MirrorWrite::Changed);
    }

    let meta_path = dir.join("meta.json");
    let meta = serde_json::json!({"appleId": account.apple_id, "dsid": account.dsid});
    if read_json_object(&meta_path).map(Value::Object).as_ref() != Some(&meta) {
        write_json(&meta_path, &meta)?;
    }
    Ok(MirrorWrite::Written(cookie))
}

/// The file's bytes, `None` when it does not exist.
fn read_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// What icloud-md last wrote to the mirrored session file, if it parses.
pub struct MirrorSession {
    pub cookie: String,
    pub params: BTreeMap<String, String>,
}

pub fn read_mirror(paths: &Paths, dsid: &str) -> Option<MirrorSession> {
    let fields = read_json_object(&paths.mirror_session(dsid))?;
    let cookie = fields.get("cookie")?.as_str().filter(|c| !c.is_empty())?.to_string();
    let params = [CLIENT_ID, CLIENT_BUILD_NUMBER, CLIENT_MASTERING_NUMBER]
        .into_iter()
        .filter_map(|k| {
            let v = fields.get(k)?.as_str().filter(|v| !v.is_empty())?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();
    Some(MirrorSession { cookie, params })
}

fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    match serde_json::from_slice(&fs::read(path).ok()?).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

// --------------------------------------------------------------- writes

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `JSON.stringify(value, null, 2) + "\n"`, written atomically, mode 0600,
/// creating the parent directory 0700.
pub fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    write_json_if(path, value, || true).map(|_| ())
}

/// [`write_json`], put in place only if `proceed()` says so just before the
/// rename. Returns whether it was.
fn write_json_if(path: &Path, value: &Value, proceed: impl FnOnce() -> bool) -> io::Result<bool> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).expect("JSON serializes");
    bytes.push(b'\n');
    write_atomic_if(path, &bytes, proceed)
}

/// Writes `bytes` to a temp file (mode 0600) beside `path`, then, if
/// `proceed()` agrees, renames it over `path`, so a reader sees either the
/// old file or the new one. Returns whether it did.
fn write_atomic_if(path: &Path, bytes: &[u8], proceed: impl FnOnce() -> bool) -> io::Result<bool> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if !proceed() {
            return Ok(false);
        }
        fs::rename(&tmp, path).map(|()| true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn account() -> Account {
        Account {
            apple_id: "a@example.com".into(),
            dsid: "123".into(),
            cookies: vec![Cookie::new("A", "1"), Cookie::new("B", "2")],
            client_params: [
                (CLIENT_ID, "id"),
                (CLIENT_BUILD_NUMBER, "b"),
                (CLIENT_MASTERING_NUMBER, "m"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into(),
            webservices: BTreeMap::new(),
            validated_at: 1,
            captured_at: "2026-09-28T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn account_round_trips_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/account.json");
        assert_eq!(Account::load(&path).unwrap(), None);
        account().save(&path).unwrap();
        assert_eq!(Account::load(&path).unwrap(), Some(account()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temp files left");
    }

    #[test]
    fn a_bad_account_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        fs::write(&path, "{not json").unwrap();
        assert_eq!(Account::load_or_set_aside(&path), None);
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("account.json.bad")).unwrap(),
            "{not json"
        );
        assert_eq!(
            Account::load_or_set_aside(&path),
            None,
            "a missing file is plain signed out"
        );
    }

    #[test]
    fn optional_account_fields_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        fs::write(&path, r#"{"dsid":"1","cookies":[{"name":"A","value":"1"}]}"#).unwrap();
        let a = Account::load(&path).unwrap().unwrap();
        assert_eq!(a.dsid, "1");
        assert_eq!(a.validated_at, 0);
        assert!(a.webservices.is_empty() && a.client_params.is_empty());
    }

    #[test]
    fn mirror_keeps_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            account: dir.path().join("a.json"),
            webkit_data: dir.path().join("w"),
            webkit_cache: dir.path().join("c"),
            icloud_md_accounts: dir.path().join("accounts"),
        };
        create_private_dir(&paths.mirror_dir("123")).unwrap();
        fs::write(paths.mirror_session("123"), r#"{"cookie":"old","extra":[1]}"#).unwrap();
        assert_eq!(
            write_mirror(&paths, &account(), 0).unwrap(),
            MirrorWrite::Written("A=1; B=2".into())
        );
        let text = fs::read_to_string(paths.mirror_session("123")).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["cookie"], "A=1; B=2");
        assert_eq!(v["extra"], serde_json::json!([1]));
        assert_eq!(v["clientMasteringNumber"], "m");
        let meta: Value =
            serde_json::from_slice(&fs::read(paths.mirror_dir("123").join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta, serde_json::json!({"appleId": "a@example.com", "dsid": "123"}));
        let m = read_mirror(&paths, "123").unwrap();
        assert_eq!(m.cookie, "A=1; B=2");
        assert_eq!(m.params[CLIENT_ID], "id");
    }

    #[test]
    fn a_rotation_written_during_our_write_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            account: dir.path().join("a.json"),
            webkit_data: dir.path().join("w"),
            webkit_cache: dir.path().join("c"),
            icloud_md_accounts: dir.path().join("accounts"),
        };
        create_private_dir(&paths.mirror_dir("123")).unwrap();
        let session = paths.mirror_session("123");
        fs::write(&session, r#"{"cookie":"A=0"}"#).unwrap();
        let rotation = r#"{"cookie":"A=icloud-md"}"#;
        let wrote = write_mirror_checked(&paths, &account(), 0, || fs::write(&session, rotation).unwrap()).unwrap();
        assert_eq!(wrote, MirrorWrite::Changed);
        assert_eq!(fs::read_to_string(&session).unwrap(), rotation);
        let names: Vec<_> = fs::read_dir(paths.mirror_dir("123")).unwrap().collect();
        assert_eq!(names.len(), 1, "no temp file left");
        // Nothing in between: written.
        assert!(matches!(
            write_mirror(&paths, &account(), 0).unwrap(),
            MirrorWrite::Written(_)
        ));
    }
}
