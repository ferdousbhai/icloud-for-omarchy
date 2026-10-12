//! The keyring client against a fake Secret Service (`org.freedesktop.secrets`)
//! on a private dbus-daemon: the plain session, a default collection made
//! behind a prompt, locked collections and items unlocked behind prompts, a
//! dismissed prompt, replacing and deleting items. Nothing touches the real
//! session bus or keyring. One test, because it sets process environment.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use icloud_sessiond::secrets;
use zbus::object_server::{ObjectServer, SignalEmitter};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const ROOT: &str = "/org/freedesktop/secrets";

#[derive(Debug, Clone)]
struct FakeItem {
    path: String,
    collection: String,
    label: String,
    attributes: HashMap<String, String>,
    secret: Vec<u8>,
    content_type: String,
    locked: bool,
}

#[derive(Default)]
struct Store {
    /// The `default` alias.
    default: Option<String>,
    /// path → (label, locked)
    collections: HashMap<String, (String, bool)>,
    items: Vec<FakeItem>,
    sessions: Vec<String>,
    prompts: usize,
    /// The next prompt is dismissed.
    dismiss: bool,
    next: usize,
}

type Shared = Arc<Mutex<Store>>;

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p.to_string()).unwrap()
}

/// What a prompt does once the user answers.
enum Action {
    MakeCollection(String),
    Unlock(Vec<String>),
}

struct Prompt {
    store: Shared,
    action: Mutex<Option<Action>>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl Prompt {
    async fn prompt(
        &self,
        _window_id: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let action = self.action.lock().unwrap().take();
        let (dismissed, result) = {
            let mut st = self.store.lock().unwrap();
            st.prompts += 1;
            if std::mem::take(&mut st.dismiss) {
                (true, Value::from(""))
            } else {
                match action {
                    Some(Action::MakeCollection(p)) => {
                        st.collections.get_mut(&p).unwrap().1 = false;
                        st.default = Some(p.clone());
                        (false, Value::from(path(&p)))
                    }
                    Some(Action::Unlock(paths)) => {
                        for p in &paths {
                            if let Some(c) = st.collections.get_mut(p) {
                                c.1 = false;
                            }
                            for i in st.items.iter_mut().filter(|i| &i.path == p) {
                                i.locked = false;
                            }
                        }
                        (false, Value::from(paths.iter().map(|p| path(p)).collect::<Vec<_>>()))
                    }
                    None => (false, Value::from("")),
                }
            }
        };
        Prompt::completed(&emitter, dismissed, result).await?;
        Ok(())
    }

    #[zbus(signal)]
    async fn completed(emitter: &SignalEmitter<'_>, dismissed: bool, result: Value<'_>) -> zbus::Result<()>;
}

async fn new_prompt(store: &Shared, server: &ObjectServer, action: Action) -> OwnedObjectPath {
    let p = {
        let mut st = store.lock().unwrap();
        st.next += 1;
        format!("{ROOT}/prompt/p{}", st.next)
    };
    server
        .at(
            p.as_str(),
            Prompt {
                store: store.clone(),
                action: Mutex::new(Some(action)),
            },
        )
        .await
        .unwrap();
    path(&p)
}

struct Service(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl Service {
    fn open_session(&self, algorithm: String, _input: OwnedValue) -> zbus::fdo::Result<(OwnedValue, OwnedObjectPath)> {
        if algorithm != "plain" {
            return Err(zbus::fdo::Error::NotSupported(algorithm));
        }
        let mut st = self.0.lock().unwrap();
        let p = format!("{ROOT}/session/s{}", st.sessions.len() + 1);
        st.sessions.push(p.clone());
        Ok((OwnedValue::try_from(Value::from("")).unwrap(), path(&p)))
    }

    fn read_alias(&self, name: String) -> OwnedObjectPath {
        let st = self.0.lock().unwrap();
        match (name.as_str(), &st.default) {
            ("default", Some(p)) => path(p),
            _ => path("/"),
        }
    }

    async fn create_collection(
        &self,
        properties: HashMap<String, OwnedValue>,
        alias: String,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        assert_eq!(alias, "default");
        let label = String::try_from(properties["org.freedesktop.Secret.Collection.Label"].clone()).unwrap();
        let p = format!("{ROOT}/collection/made");
        // Locked until the prompt is answered, then made unlocked.
        self.0.lock().unwrap().collections.insert(p.clone(), (label, true));
        server
            .at(p.as_str(), Collection(self.0.clone(), p.clone()))
            .await
            .unwrap();
        let prompt = new_prompt(&self.0, server, Action::MakeCollection(p)).await;
        (path("/"), prompt)
    }

    async fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (Vec<OwnedObjectPath>, OwnedObjectPath) {
        let paths: Vec<String> = objects.iter().map(|o| o.as_str().to_string()).collect();
        let prompt = new_prompt(&self.0, server, Action::Unlock(paths)).await;
        (vec![], prompt)
    }

    #[zbus(property)]
    fn collections(&self) -> Vec<OwnedObjectPath> {
        let st = self.0.lock().unwrap();
        let mut all: Vec<&String> = st.collections.keys().collect();
        all.sort();
        all.into_iter().map(|p| path(p)).collect()
    }
}

struct Collection(Shared, String);

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl Collection {
    fn search_items(&self, attributes: HashMap<String, String>) -> Vec<OwnedObjectPath> {
        let st = self.0.lock().unwrap();
        // As KeePassXC: a locked collection's items are not found.
        if st.collections.get(&self.1).is_some_and(|c| c.1) {
            return Vec::new();
        }
        st.items
            .iter()
            .filter(|i| i.collection == self.1 && attributes.iter().all(|(k, v)| i.attributes.get(k) == Some(v)))
            .map(|i| path(&i.path))
            .collect()
    }

    async fn create_item(
        &self,
        properties: HashMap<String, OwnedValue>,
        secret: (OwnedObjectPath, Vec<u8>, Vec<u8>, String),
        replace: bool,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let label = String::try_from(properties["org.freedesktop.Secret.Item.Label"].clone()).unwrap();
        let attributes =
            HashMap::<String, String>::try_from(properties["org.freedesktop.Secret.Item.Attributes"].clone()).unwrap();
        let (session, parameters, value, content_type) = secret;
        let p = {
            let mut st = self.0.lock().unwrap();
            if st.collections[&self.1].1 {
                return Err(zbus::fdo::Error::Failed("collection is locked".into()));
            }
            assert!(st.sessions.iter().any(|s| s == session.as_str()), "an open session");
            assert!(parameters.is_empty(), "plain: no parameters");
            if replace
                && let Some(i) = st
                    .items
                    .iter_mut()
                    .find(|i| i.collection == self.1 && i.attributes == attributes)
            {
                i.label = label;
                i.secret = value;
                i.content_type = content_type;
                return Ok((path(&i.path), path("/")));
            }
            st.next += 1;
            let p = format!("{}/i{}", self.1, st.next);
            st.items.push(FakeItem {
                path: p.clone(),
                collection: self.1.clone(),
                label,
                attributes,
                secret: value,
                content_type,
                locked: false,
            });
            p
        };
        server.at(p.as_str(), Item(self.0.clone(), p.clone())).await.unwrap();
        Ok((path(&p), path("/")))
    }

    #[zbus(property)]
    fn locked(&self) -> bool {
        self.0.lock().unwrap().collections[&self.1].1
    }
}

struct Item(Shared, String);

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl Item {
    fn get_secret(&self, session: OwnedObjectPath) -> zbus::fdo::Result<(OwnedObjectPath, Vec<u8>, Vec<u8>, String)> {
        let st = self.0.lock().unwrap();
        assert!(st.sessions.iter().any(|s| s == session.as_str()), "an open session");
        let item = st.items.iter().find(|i| i.path == self.1).unwrap();
        if item.locked {
            return Err(zbus::fdo::Error::AccessDenied("item is locked".into()));
        }
        Ok((session, vec![], item.secret.clone(), item.content_type.clone()))
    }

    fn delete(&self) -> OwnedObjectPath {
        self.0.lock().unwrap().items.retain(|i| i.path != self.1);
        path("/")
    }

    #[zbus(property)]
    fn locked(&self) -> bool {
        let st = self.0.lock().unwrap();
        st.items.iter().find(|i| i.path == self.1).is_some_and(|i| i.locked)
    }
}

struct Bus(Child);

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_bus(dir: &std::path::Path) -> (Bus, String) {
    std::fs::write(
        dir.join("bus.conf"),
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
    )
    .unwrap();
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", dir.join("bus.conf").display()))
        .args(["--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("dbus-daemon runs");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (Bus(child), line.trim().to_string())
}

#[test]
fn keyring_speaks_the_secret_service() {
    let dir = tempfile::tempdir().unwrap();
    let (_bus, address) = private_bus(dir.path());
    let store: Shared = Arc::default();
    // Another application's collection, with an item of ours (an older
    // install's) and one of someone else's.
    let other = format!("{ROOT}/collection/other");
    {
        let mut st = store.lock().unwrap();
        st.collections.insert(other.clone(), ("Other".into(), false));
        for (n, app) in [(1, "icloud-session"), (2, "something-else")] {
            st.items.push(FakeItem {
                path: format!("{other}/o{n}"),
                collection: other.clone(),
                label: "old".into(),
                attributes: HashMap::from([("application".into(), app.into())]),
                secret: b"x".to_vec(),
                content_type: "text/plain".into(),
                locked: true,
            });
        }
    }
    let mut builder = zbus::blocking::connection::Builder::address(address.as_str())
        .unwrap()
        .name("org.freedesktop.secrets")
        .unwrap()
        .serve_at(ROOT, Service(store.clone()))
        .unwrap()
        .serve_at(other.as_str(), Collection(store.clone(), other.clone()))
        .unwrap();
    for n in [1, 2] {
        let p = format!("{other}/o{n}");
        builder = builder.serve_at(p.clone(), Item(store.clone(), p)).unwrap();
    }
    let _service = builder.build().unwrap();
    // SAFETY: the only test in this binary; set before any thread reads it.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

    let keyring = secrets::keyring();
    let st = || store.lock().unwrap();

    // No default collection: one is made (behind a prompt), labelled Default.
    assert!(!keyring.contains("someone@example.com").unwrap());
    let made = st().default.clone().expect("a default collection");
    assert_eq!(st().collections[&made], ("Default".to_string(), false));
    assert_eq!(st().prompts, 1);

    // A locked collection is unlocked before an item is made in it.
    st().collections.get_mut(&made).unwrap().1 = true;
    keyring.set("someone@example.com", "hunter2").unwrap();
    assert_eq!(st().prompts, 2);
    let ours: Vec<FakeItem> = st().items.iter().filter(|i| i.collection == made).cloned().collect();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].label, "iCloud (icloud-session): someone@example.com");
    assert_eq!(
        ours[0].attributes,
        HashMap::from([
            ("application".to_string(), "icloud-session".to_string()),
            ("apple-id".to_string(), "someone@example.com".to_string()),
        ])
    );
    // Stored base64: GNOME Keyring's unencrypted file mangles backslashes.
    assert_eq!(ours[0].secret, b"base64:aHVudGVyMg==");
    assert_eq!(ours[0].content_type, "text/plain");

    assert!(keyring.contains("someone@example.com").unwrap());
    assert!(!keyring.contains("other@example.com").unwrap());
    assert_eq!(
        keyring
            .get("someone@example.com")
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );
    assert_eq!(keyring.get("other@example.com").unwrap(), None);

    // A locked item is unlocked before its secret is read.
    st().items.iter_mut().find(|i| i.collection == made).unwrap().locked = true;
    assert_eq!(
        keyring
            .get("someone@example.com")
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );
    assert_eq!(st().prompts, 3);

    // Storing again replaces the item.
    keyring.set("someone@example.com", "correct horse").unwrap();
    assert_eq!(st().items.iter().filter(|i| i.collection == made).count(), 1);
    assert_eq!(
        keyring
            .get("someone@example.com")
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("correct horse")
    );

    // A password stored by icloud-session 0.6, unencoded, reads as it is.
    st().items.iter_mut().find(|i| i.collection == made).unwrap().secret = b"legacy".to_vec();
    assert_eq!(
        keyring
            .get("someone@example.com")
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("legacy")
    );
    keyring.set("someone@example.com", "correct horse").unwrap();

    // One connection, one session, however many calls.
    assert_eq!(st().sessions.len(), 1);

    // A dismissed prompt is an error, and nothing is stored.
    st().collections.get_mut(&made).unwrap().1 = true;
    st().dismiss = true;
    let err = keyring.set("other@example.com", "nope").unwrap_err();
    assert!(err.contains("dismissed"), "{err}");
    assert!(!st().items.iter().any(|i| i.secret == b"nope"));
    st().collections.get_mut(&made).unwrap().1 = false;

    // The session's jars: their own item, replaced in place.
    assert_eq!(keyring.get_session().unwrap(), None);
    keyring.set_session("someone@example.com", r#"{"v":1}"#).unwrap();
    keyring.set_session("someone@example.com", r#"{"v":2}"#).unwrap();
    let sessions: Vec<FakeItem> = st()
        .items
        .iter()
        .filter(|i| i.attributes.get("kind").map(String::as_str) == Some("session"))
        .cloned()
        .collect();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].label,
        "iCloud session (icloud-session): someone@example.com"
    );
    assert_eq!(
        keyring.get_session().unwrap().as_deref().map(String::as_str),
        Some(r#"{"v":2}"#)
    );
    assert_eq!(
        keyring
            .get("someone@example.com")
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("correct horse"),
        "the password is not the session"
    );

    // What the keyring's file would mangle is stored without a backslash
    // or a newline, and read back exactly.
    let awkward = "{\"a\":\"q\\\"x\\\\y\"}\nend";
    keyring.set_session("someone@example.com", awkward).unwrap();
    let stored = st()
        .items
        .iter()
        .find(|i| i.attributes.get("kind").map(String::as_str) == Some("session"))
        .unwrap()
        .secret
        .clone();
    assert!(!stored.contains(&b'\\') && !stored.contains(&b'\n'), "{stored:?}");
    assert_eq!(
        keyring.get_session().unwrap().as_deref().map(String::as_str),
        Some(awkward)
    );

    // Forgetting passwords removes every other icloud-session item in every
    // collection, and nobody else's, but not the session.
    assert_eq!(keyring.forget_passwords().unwrap(), 2);
    let mut left: Vec<String> = st().items.iter().map(|i| i.attributes["application"].clone()).collect();
    left.sort();
    assert_eq!(left, ["icloud-session", "something-else"]);
    assert!(!keyring.contains("someone@example.com").unwrap());
    assert!(keyring.get_session().unwrap().is_some());
    // The error above dropped the session; the next call opened another.
    assert_eq!(st().sessions.len(), 2);

    // A locked collection is unlocked before it is searched: never "no
    // session" for want of seeing it, and an error if it stays locked.
    st().collections.get_mut(&made).unwrap().1 = true;
    let prompts = st().prompts;
    assert!(keyring.get_session().unwrap().is_some());
    assert_eq!(st().prompts, prompts + 1);
    st().collections.get_mut(&made).unwrap().1 = true;
    st().dismiss = true;
    assert!(keyring.get_session().unwrap_err().contains("dismissed"));
    st().collections.get_mut(&made).unwrap().1 = true;
    st().dismiss = true;
    assert!(keyring.remove_session().unwrap_err().contains("dismissed"));
    assert!(keyring.get_session().unwrap().is_some());

    keyring.remove_session().unwrap();
    assert_eq!(keyring.get_session().unwrap(), None);
    let left: Vec<String> = st().items.iter().map(|i| i.attributes["application"].clone()).collect();
    assert_eq!(left, ["something-else"]);
}
