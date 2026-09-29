//! Where the Apple ID password for automatic Find My re-authorization
//! lives: the Secret Service (GNOME Keyring), and nowhere else. Only the
//! daemon reads or writes it.
//!
//! Items carry the attributes `application=icloud-session` and
//! `apple-id=<apple id>`, labelled `iCloud (icloud-session): <apple id>`,
//! in the default collection.
//!
//! `ICLOUD_SESSION_TEST_SECRET_FILE` swaps the keyring for a JSON file, for
//! the tests only: they run on a private bus with no Secret Service, and
//! must never reach the real one.

use std::collections::HashMap;
use std::path::PathBuf;

use zeroize::Zeroizing;

pub const APPLICATION: &str = "icloud-session";

pub type Password = Zeroizing<String>;

pub trait SecretStore: Send + Sync {
    /// The password stored for `apple_id`.
    fn get(&self, apple_id: &str) -> Result<Option<Password>, String>;
    /// Whether one is stored for `apple_id` (no secret read, no unlock).
    fn contains(&self, apple_id: &str) -> Result<bool, String>;
    /// Stores (or replaces) the password for `apple_id`.
    fn set(&self, apple_id: &str, password: &str) -> Result<(), String>;
    /// Removes every icloud-session item; returns how many.
    fn forget_all(&self) -> Result<usize, String>;
}

pub fn label(apple_id: &str) -> String {
    format!("iCloud (icloud-session): {apple_id}")
}

fn attributes(apple_id: &str) -> HashMap<&'static str, String> {
    HashMap::from([
        ("application", APPLICATION.to_string()),
        ("apple-id", apple_id.to_string()),
    ])
}

/// The keyring, or the test file when `ICLOUD_SESSION_TEST_SECRET_FILE` is set.
pub fn from_env() -> Box<dyn SecretStore> {
    match std::env::var_os("ICLOUD_SESSION_TEST_SECRET_FILE").filter(|v| !v.is_empty()) {
        Some(path) => Box::new(TestFile(path.into())),
        None => Box::new(Keyring),
    }
}

// ------------------------------------------------------------ keyring

/// The Secret Service on the session bus, through oo7.
struct Keyring;

fn keyring_error(e: oo7::dbus::Error) -> String {
    format!("Secret Service: {e}")
}

impl Keyring {
    fn run<T>(
        &self,
        f: impl AsyncFnOnce(&oo7::dbus::Service<'static>) -> Result<T, oo7::dbus::Error>,
    ) -> Result<T, String> {
        futures_lite::future::block_on(async {
            let service = oo7::dbus::Service::new().await?;
            f(&service).await
        })
        .map_err(keyring_error)
    }
}

impl SecretStore for Keyring {
    fn get(&self, apple_id: &str) -> Result<Option<Password>, String> {
        self.run(async |service| {
            for item in service
                .default_collection()
                .await?
                .search_items(&attributes(apple_id))
                .await?
            {
                if item.is_locked().await? {
                    // Normally unlocked at login; otherwise the keyring asks.
                    item.unlock(None).await?;
                }
                let secret = item.secret().await?;
                if let Ok(text) = std::str::from_utf8(secret.as_bytes()) {
                    return Ok(Some(Zeroizing::new(text.to_string())));
                }
            }
            Ok(None)
        })
    }

    fn contains(&self, apple_id: &str) -> Result<bool, String> {
        self.run(async |service| {
            Ok(!service
                .default_collection()
                .await?
                .search_items(&attributes(apple_id))
                .await?
                .is_empty())
        })
    }

    fn set(&self, apple_id: &str, password: &str) -> Result<(), String> {
        self.run(async |service| {
            let collection = service.default_collection().await?;
            if collection.is_locked().await? {
                collection.unlock(None).await?;
            }
            collection
                .create_item(&label(apple_id), &attributes(apple_id), password, true, None)
                .await?;
            Ok(())
        })
    }

    fn forget_all(&self) -> Result<usize, String> {
        self.run(async |service| {
            let mut n = 0;
            let ours = HashMap::from([("application", APPLICATION)]);
            for collection in service.collections().await? {
                for item in collection.search_items(&ours).await? {
                    item.delete(None).await?;
                    n += 1;
                }
            }
            Ok(n)
        })
    }
}

// --------------------------------------------------------------- tests

/// A JSON file standing in for the keyring in the tests:
/// `[{"label", "attributes": {..}, "secret"}]`.
struct TestFile(PathBuf);

#[derive(serde::Serialize, serde::Deserialize)]
struct TestItem {
    label: String,
    attributes: HashMap<String, String>,
    secret: String,
}

impl TestFile {
    fn load(&self) -> Result<Vec<TestItem>, String> {
        match std::fs::read(&self.0) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", self.0.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{}: {e}", self.0.display())),
        }
    }

    fn save(&self, items: &[TestItem]) -> Result<(), String> {
        let value = serde_json::to_value(items).expect("items serialize");
        crate::files::write_json(&self.0, &value).map_err(|e| format!("{}: {e}", self.0.display()))
    }

    fn matches(item: &TestItem, apple_id: &str) -> bool {
        item.attributes.get("application").map(String::as_str) == Some(APPLICATION)
            && item.attributes.get("apple-id").map(String::as_str) == Some(apple_id)
    }
}

impl SecretStore for TestFile {
    fn get(&self, apple_id: &str) -> Result<Option<Password>, String> {
        Ok(self
            .load()?
            .into_iter()
            .find(|i| TestFile::matches(i, apple_id))
            .map(|i| Zeroizing::new(i.secret)))
    }

    fn contains(&self, apple_id: &str) -> Result<bool, String> {
        Ok(self.load()?.iter().any(|i| TestFile::matches(i, apple_id)))
    }

    fn set(&self, apple_id: &str, password: &str) -> Result<(), String> {
        let mut items = self.load()?;
        items.retain(|i| !TestFile::matches(i, apple_id));
        items.push(TestItem {
            label: label(apple_id),
            attributes: attributes(apple_id)
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            secret: password.to_string(),
        });
        self.save(&items)
    }

    fn forget_all(&self) -> Result<usize, String> {
        let mut items = self.load()?;
        let before = items.len();
        items.retain(|i| i.attributes.get("application").map(String::as_str) != Some(APPLICATION));
        let n = before - items.len();
        self.save(&items)?;
        Ok(n)
    }
}
