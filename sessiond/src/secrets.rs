//! The daemon's secrets, in the Secret Service (GNOME Keyring) and nowhere
//! else; only the daemon reads or writes them. Both kinds of item are in
//! the default collection:
//!
//! - The session: the signed-in account's cookie jars (the main one and
//!   Find My's), as JSON. Attributes `application=icloud-session`,
//!   `kind=session`; one at a time, labelled `iCloud session
//!   (icloud-session): <apple id>`.
//! - The Apple ID password for automatic Find My re-authorization, only
//!   if the user stores it. Attributes `application=icloud-session`,
//!   `apple-id=<apple id>` (no `kind`: as items stored before sessions
//!   moved here have it), labelled `iCloud (icloud-session): <apple id>`.
//!
//! `ICLOUD_SESSION_TEST_SECRET_FILE` swaps the keyring for a JSON file, for
//! the tests only: they run on a private bus with no Secret Service, and
//! must never reach the real one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zeroize::Zeroizing;

const APPLICATION: &str = "icloud-session";
const KIND: &str = "kind";
const SESSION: &str = "session";

pub type Password = Zeroizing<String>;

pub trait SecretStore: Send + Sync {
    /// The password stored for `apple_id`.
    fn get(&self, apple_id: &str) -> Result<Option<Password>, String>;
    /// Whether one is stored for `apple_id` (no secret read, no unlock).
    fn contains(&self, apple_id: &str) -> Result<bool, String>;
    /// Stores (or replaces) the password for `apple_id`.
    fn set(&self, apple_id: &str, password: &str) -> Result<(), String>;
    /// Removes every stored password (every icloud-session item but the
    /// session); returns how many.
    fn forget_passwords(&self) -> Result<usize, String>;
    /// The session's jars, as stored.
    fn get_session(&self) -> Result<Option<Zeroizing<String>>, String>;
    /// Stores (or replaces) the session's jars.
    fn set_session(&self, apple_id: &str, secret: &str) -> Result<(), String>;
    /// Removes the session item.
    fn remove_session(&self) -> Result<(), String>;
}

pub fn label(apple_id: &str) -> String {
    format!("iCloud (icloud-session): {apple_id}")
}

pub fn session_label(apple_id: &str) -> String {
    format!("iCloud session (icloud-session): {apple_id}")
}

fn attributes(apple_id: &str) -> HashMap<&'static str, String> {
    HashMap::from([
        ("application", APPLICATION.to_string()),
        ("apple-id", apple_id.to_string()),
    ])
}

fn session_attributes() -> HashMap<&'static str, String> {
    HashMap::from([("application", APPLICATION.to_string()), (KIND, SESSION.to_string())])
}

/// The keyring, or the test file when `ICLOUD_SESSION_TEST_SECRET_FILE` is set.
pub fn from_env() -> Box<dyn SecretStore> {
    match std::env::var_os("ICLOUD_SESSION_TEST_SECRET_FILE").filter(|v| !v.is_empty()) {
        Some(path) => Box::new(TestFile(path.into())),
        None => keyring(),
    }
}

/// The Secret Service, whatever `ICLOUD_SESSION_TEST_SECRET_FILE` says
/// (the keyring tests run their own fake Secret Service).
pub fn keyring() -> Box<dyn SecretStore> {
    Box::new(Keyring::default())
}

// ------------------------------------------------------------ keyring

const BUS_NAME: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";
/// The alias of the collection items are kept in (GNOME Keyring's "login").
const DEFAULT_ALIAS: &str = "default";
/// The label of the collection made when there is no default one.
const DEFAULT_LABEL: &str = "Default";
const CONTENT_TYPE: &str = "text/plain";

/// The Secret Service's `(session, parameters, value, content_type)`.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

/// The Secret Service (`org.freedesktop.secrets`, GNOME Keyring) on the
/// session bus, spoken to directly: one bus connection and one "plain"
/// session (the secret crosses only the local session bus, as every other
/// call's arguments do), opened on first use and kept. An error drops them,
/// so the next call starts afresh (the keyring may have restarted).
#[derive(Default)]
struct Keyring {
    open: Mutex<Option<Open>>,
}

#[derive(Clone)]
struct Open {
    bus: Connection,
    session: OwnedObjectPath,
}

fn failure(msg: &str) -> zbus::Error {
    zbus::Error::Failure(msg.into())
}

impl Keyring {
    /// Runs `f` on the open connection and session, opening them first if
    /// need be. Not under the lock: a call waiting on a prompt holds up no
    /// other.
    fn run<T>(&self, f: impl FnOnce(&Open) -> zbus::Result<T>) -> Result<T, String> {
        let error = |e: zbus::Error| format!("Secret Service: {e}");
        let open = {
            let mut slot = self.open.lock().unwrap_or_else(|e| e.into_inner());
            match &*slot {
                Some(open) => open.clone(),
                None => slot.insert(Open::new().map_err(error)?).clone(),
            }
        };
        f(&open).map_err(|e| {
            let mut slot = self.open.lock().unwrap_or_else(|e| e.into_inner());
            if slot.as_ref().is_some_and(|o| o.session == open.session) {
                *slot = None;
            }
            error(e)
        })
    }
}

impl Open {
    fn new() -> zbus::Result<Open> {
        let bus = Connection::session()?;
        let (_, session): (OwnedValue, OwnedObjectPath) = bus
            .call_method(
                Some(BUS_NAME),
                SERVICE_PATH,
                Some(SERVICE),
                "OpenSession",
                &("plain", Value::from("")),
            )?
            .body()
            .deserialize()?;
        Ok(Open { bus, session })
    }

    fn call<R>(
        &self,
        path: &str,
        interface: &str,
        method: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) -> zbus::Result<R>
    where
        R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        self.bus
            .call_method(Some(BUS_NAME), path, Some(interface), method, body)?
            .body()
            .deserialize()
    }

    fn locked(&self, path: &str, interface: &str) -> zbus::Result<bool> {
        let value: OwnedValue = self.call(path, "org.freedesktop.DBus.Properties", "Get", &(interface, "Locked"))?;
        Ok(bool::try_from(value)?)
    }

    /// Runs the prompt at `path` ("/": none) and waits for its `Completed`:
    /// its result, or an error if the user dismissed it.
    fn prompt(&self, path: &OwnedObjectPath) -> zbus::Result<Option<OwnedValue>> {
        if path.as_str() == "/" {
            return Ok(None);
        }
        let prompt = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(&self.bus)
            .destination(BUS_NAME)?
            .path(path.as_str())?
            .interface(PROMPT)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        // Listening before asking: the answer can come at once.
        let mut completed = prompt.receive_signal("Completed")?;
        prompt.call_method("Prompt", &("",))?;
        let message = completed
            .next()
            .ok_or_else(|| failure("the keyring prompt went away"))?;
        let (dismissed, result): (bool, OwnedValue) = message.body().deserialize()?;
        if dismissed {
            return Err(failure("the keyring prompt was dismissed"));
        }
        Ok(Some(result))
    }

    /// Unlocks `path` (a collection or an item); the keyring may ask.
    fn unlock(&self, path: &OwnedObjectPath) -> zbus::Result<()> {
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
            self.call(SERVICE_PATH, SERVICE, "Unlock", &(vec![path],))?;
        self.prompt(&prompt)?;
        Ok(())
    }

    /// The collection with the `default` alias, made (labelled `Default`)
    /// if there is none.
    fn default_collection(&self) -> zbus::Result<OwnedObjectPath> {
        let path: OwnedObjectPath = self.call(SERVICE_PATH, SERVICE, "ReadAlias", &(DEFAULT_ALIAS,))?;
        if path.as_str() != "/" {
            return Ok(path);
        }
        let properties = HashMap::from([("org.freedesktop.Secret.Collection.Label", Value::from(DEFAULT_LABEL))]);
        let (path, prompt): (OwnedObjectPath, OwnedObjectPath) =
            self.call(SERVICE_PATH, SERVICE, "CreateCollection", &(properties, DEFAULT_ALIAS))?;
        match self.prompt(&prompt)? {
            Some(made) => Ok(OwnedObjectPath::try_from(made)?),
            None => Ok(path),
        }
    }

    fn search(&self, collection: &str, attributes: &HashMap<&str, String>) -> zbus::Result<Vec<OwnedObjectPath>> {
        self.call(collection, COLLECTION, "SearchItems", &(attributes,))
    }
}

impl Open {
    /// The secret of the first item in the default collection matching
    /// `attributes`, unlocking it first if need be.
    fn read(&self, attributes: &HashMap<&str, String>) -> zbus::Result<Option<Zeroizing<String>>> {
        for item in self.search(&self.default_collection()?, attributes)? {
            if self.locked(&item, ITEM)? {
                // Normally unlocked at login; otherwise the keyring asks.
                self.unlock(&item)?;
            }
            let (_, _, value, _): Secret = self.call(&item, ITEM, "GetSecret", &(&self.session,))?;
            let value = Zeroizing::new(value);
            if let Ok(text) = std::str::from_utf8(&value) {
                return Ok(Some(Zeroizing::new(text.to_string())));
            }
        }
        Ok(None)
    }

    /// Stores `secret` in the default collection, replacing the item with
    /// the same attributes.
    fn store(&self, label: String, attributes: HashMap<&str, String>, secret: &str) -> zbus::Result<()> {
        let collection = self.default_collection()?;
        if self.locked(&collection, COLLECTION)? {
            self.unlock(&collection)?;
        }
        let properties = HashMap::from([
            ("org.freedesktop.Secret.Item.Label", Value::from(label)),
            ("org.freedesktop.Secret.Item.Attributes", Value::from(attributes)),
        ]);
        let secret = (&self.session, Vec::<u8>::new(), secret.as_bytes(), CONTENT_TYPE);
        let (_, prompt): (OwnedObjectPath, OwnedObjectPath) =
            self.call(&collection, COLLECTION, "CreateItem", &(properties, secret, true))?;
        self.prompt(&prompt)?;
        Ok(())
    }

    /// Deletes, in every collection, the items matching `attributes` but
    /// not `keep`; returns how many.
    fn delete(&self, attributes: &HashMap<&str, String>, keep: Option<&HashMap<&str, String>>) -> zbus::Result<usize> {
        let mut n = 0;
        let collections: OwnedValue = self.call(
            SERVICE_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(SERVICE, "Collections"),
        )?;
        for collection in Vec::<OwnedObjectPath>::try_from(collections)? {
            let kept = match keep {
                Some(keep) => self.search(&collection, keep)?,
                None => Vec::new(),
            };
            for item in self.search(&collection, attributes)? {
                if kept.contains(&item) {
                    continue;
                }
                let prompt: OwnedObjectPath = self.call(&item, ITEM, "Delete", &())?;
                self.prompt(&prompt)?;
                n += 1;
            }
        }
        Ok(n)
    }
}

impl SecretStore for Keyring {
    fn get(&self, apple_id: &str) -> Result<Option<Password>, String> {
        self.run(|k| k.read(&attributes(apple_id)))
    }

    fn contains(&self, apple_id: &str) -> Result<bool, String> {
        self.run(|k| Ok(!k.search(&k.default_collection()?, &attributes(apple_id))?.is_empty()))
    }

    fn set(&self, apple_id: &str, password: &str) -> Result<(), String> {
        self.run(|k| k.store(label(apple_id), attributes(apple_id), password))
    }

    fn forget_passwords(&self) -> Result<usize, String> {
        let ours = HashMap::from([("application", APPLICATION.to_string())]);
        self.run(|k| k.delete(&ours, Some(&session_attributes())))
    }

    fn get_session(&self) -> Result<Option<Zeroizing<String>>, String> {
        self.run(|k| k.read(&session_attributes()))
    }

    fn set_session(&self, apple_id: &str, secret: &str) -> Result<(), String> {
        self.run(|k| k.store(session_label(apple_id), session_attributes(), secret))
    }

    fn remove_session(&self) -> Result<(), String> {
        self.run(|k| k.delete(&session_attributes(), None).map(drop))
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

    fn has(item: &TestItem, attributes: &HashMap<&str, String>) -> bool {
        attributes.iter().all(|(k, v)| item.attributes.get(*k) == Some(v))
    }

    fn put(&self, label: String, attributes: HashMap<&str, String>, secret: &str) -> Result<(), String> {
        let mut items = self.load()?;
        let attributes: HashMap<String, String> = attributes.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        items.retain(|i| i.attributes != attributes);
        items.push(TestItem {
            label,
            attributes,
            secret: secret.to_string(),
        });
        self.save(&items)
    }

    fn find(&self, attributes: &HashMap<&str, String>) -> Result<Option<Zeroizing<String>>, String> {
        Ok(self
            .load()?
            .into_iter()
            .find(|i| TestFile::has(i, attributes))
            .map(|i| Zeroizing::new(i.secret)))
    }

    fn remove(&self, attributes: &HashMap<&str, String>, keep: Option<&HashMap<&str, String>>) -> Result<usize, String> {
        let mut items = self.load()?;
        let before = items.len();
        items.retain(|i| !TestFile::has(i, attributes) || keep.is_some_and(|k| TestFile::has(i, k)));
        let n = before - items.len();
        self.save(&items)?;
        Ok(n)
    }
}

impl SecretStore for TestFile {
    fn get(&self, apple_id: &str) -> Result<Option<Password>, String> {
        self.find(&attributes(apple_id))
    }

    fn contains(&self, apple_id: &str) -> Result<bool, String> {
        Ok(self.find(&attributes(apple_id))?.is_some())
    }

    fn set(&self, apple_id: &str, password: &str) -> Result<(), String> {
        self.put(label(apple_id), attributes(apple_id), password)
    }

    fn forget_passwords(&self) -> Result<usize, String> {
        let ours = HashMap::from([("application", APPLICATION.to_string())]);
        self.remove(&ours, Some(&session_attributes()))
    }

    fn get_session(&self) -> Result<Option<Zeroizing<String>>, String> {
        self.find(&session_attributes())
    }

    fn set_session(&self, apple_id: &str, secret: &str) -> Result<(), String> {
        self.put(session_label(apple_id), session_attributes(), secret)
    }

    fn remove_session(&self) -> Result<(), String> {
        self.remove(&session_attributes(), None).map(drop)
    }
}
