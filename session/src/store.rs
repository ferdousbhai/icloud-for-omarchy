//! Reading and writing icloud-md's `session.local.json`, the lock that
//! orders our own writers, and atomic file replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::{Error, Result, cookies};

/// icloud-md writes with a plain `writeFile` (truncate, then write), so a
/// reader can catch the file empty or half-written. Retry that many times.
const READ_ATTEMPTS: u32 = 6;
const READ_RETRY_DELAY: Duration = Duration::from_millis(50);

const REQUIRED_FIELDS: [&str; 5] = [
    "cookie",
    "clientId",
    "clientBuildNumber",
    "clientMasteringNumber",
    "capturedAt",
];

/// The parsed session file. Keeps every field, known or not, in file order.
#[derive(Debug, Clone)]
pub(crate) struct SessionFile {
    pub(crate) fields: Map<String, Value>,
}

impl SessionFile {
    pub(crate) fn field(&self, name: &str) -> &str {
        self.fields.get(name).and_then(Value::as_str).unwrap_or_default()
    }

    pub(crate) fn cookie(&self) -> &str {
        self.field("cookie")
    }

    fn parse(bytes: &[u8]) -> std::result::Result<SessionFile, String> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let Value::Object(fields) = value else {
            return Err("not a JSON object".into());
        };
        for name in REQUIRED_FIELDS {
            match fields.get(name) {
                Some(Value::String(s)) if !s.is_empty() => {}
                _ => return Err(format!("missing a non-empty \"{name}\" field")),
            }
        }
        Ok(SessionFile { fields })
    }

    fn to_bytes(&self) -> Vec<u8> {
        // Same shape icloud-md writes: JSON.stringify(session, null, 2) + "\n".
        let mut out = serde_json::to_vec_pretty(&self.fields).expect("a JSON map always serializes");
        out.push(b'\n');
        out
    }
}

/// Reads the session file fresh from disk. Missing → `SignInRequired`;
/// unparsable after a few short retries → `Corrupt`.
pub(crate) fn read_session(path: &Path) -> Result<SessionFile> {
    let mut last_error = String::new();
    for attempt in 0..READ_ATTEMPTS {
        if attempt > 0 {
            thread::sleep(READ_RETRY_DELAY);
        }
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::SignInRequired),
            Err(e) => return Err(e.into()),
        };
        match SessionFile::parse(&bytes) {
            Ok(session) => return Ok(session),
            Err(e) => last_error = e,
        }
    }
    Err(Error::Corrupt(format!("{}: {last_error}", path.display())))
}

/// An exclusive `flock` on `<session file>.lock`, released on drop.
/// It orders our own processes only; icloud-md does not take it.
#[derive(Debug)]
pub(crate) struct Lock {
    _file: File,
}

pub(crate) fn lock(session_path: &Path) -> Result<Lock> {
    let path = lock_path(session_path);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.lock()?;
    Ok(Lock { _file: file })
}

pub(crate) fn lock_path(session_path: &Path) -> PathBuf {
    let mut name = session_path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    session_path.with_file_name(name)
}

/// Re-reads the session file and applies only the cookies these
/// `Set-Cookie` headers rotated, then writes it atomically. The caller must
/// hold [`lock`]. A no-op when nothing actually changed.
pub(crate) fn merge_rotation_locked(session_path: &Path, set_cookies: &[String]) -> Result<()> {
    if set_cookies.is_empty() {
        return Ok(());
    }
    let mut session = read_session(session_path)?;
    let Some(jar) = cookies::merge_set_cookies(session.cookie(), set_cookies) else {
        return Ok(());
    };
    session.fields.insert("cookie".into(), Value::String(jar));
    write_atomic(session_path, &session.to_bytes())
}

/// Takes the lock, then [`merge_rotation_locked`].
pub(crate) fn merge_rotation(session_path: &Path, set_cookies: &[String]) -> Result<()> {
    if set_cookies.is_empty() {
        return Ok(());
    }
    let _lock = lock(session_path)?;
    merge_rotation_locked(session_path, set_cookies)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique sibling path for a temp file, so `rename` stays on one filesystem.
pub(crate) fn temp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

/// Writes `bytes` to a temp file (mode 0600) beside `path`, then renames it
/// over `path`, so a reader sees either the old file or the new one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = temp_sibling(path);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const FULL: &str = r#"{
  "cookie": "A=1; X-APPLE-WEBAUTH-TOKEN=old; B=2",
  "zeta": {"kept": true},
  "clientId": "auth-123",
  "clientBuildNumber": "2624Build27",
  "clientMasteringNumber": "2624Build27",
  "capturedAt": "2026-09-01T00:00:00.000Z",
  "alpha": 1
}
"#;

    #[test]
    fn merge_preserves_unknown_fields_order_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.local.json");
        fs::write(&path, FULL).unwrap();
        merge_rotation(&path, &["X-APPLE-WEBAUTH-TOKEN=new; Secure".into(), "C=3".into()]).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let keys: Vec<String> = serde_json::from_str::<Map<String, Value>>(&text)
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "cookie",
                "zeta",
                "clientId",
                "clientBuildNumber",
                "clientMasteringNumber",
                "capturedAt",
                "alpha"
            ]
        );
        let session = read_session(&path).unwrap();
        assert_eq!(session.cookie(), "A=1; X-APPLE-WEBAUTH-TOKEN=new; B=2; C=3");
        assert_eq!(session.fields["zeta"], serde_json::json!({"kept": true}));
        assert!(text.ends_with("}\n"));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // No temp files left behind; only the session file and its lock.
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["session.local.json", "session.local.json.lock"]);
    }

    #[test]
    fn unchanged_rotation_does_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.local.json");
        fs::write(&path, FULL).unwrap();
        merge_rotation(&path, &["B=2".into()]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), FULL);
    }

    #[test]
    fn truncated_file_is_retried_until_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.local.json");
        fs::write(&path, &FULL[..40]).unwrap();
        let writer = {
            let path = path.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(120));
                fs::write(&path, FULL).unwrap();
            })
        };
        let session = read_session(&path).unwrap();
        writer.join().unwrap();
        assert_eq!(session.field("clientId"), "auth-123");
    }

    #[test]
    fn empty_file_mid_write_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.local.json");
        fs::write(&path, "").unwrap();
        let writer = {
            let path = path.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(60));
                fs::write(&path, FULL).unwrap();
            })
        };
        assert!(read_session(&path).is_ok());
        writer.join().unwrap();
    }

    #[test]
    fn persistently_truncated_file_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.local.json");
        fs::write(&path, &FULL[..40]).unwrap();
        assert!(matches!(read_session(&path), Err(Error::Corrupt(_))));
        fs::write(&path, r#"{"cookie": "A=1"}"#).unwrap();
        match read_session(&path) {
            Err(Error::Corrupt(msg)) => assert!(msg.contains("clientId"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_file_is_sign_in_required() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_session(&dir.path().join("nope.json")),
            Err(Error::SignInRequired)
        ));
    }
}
