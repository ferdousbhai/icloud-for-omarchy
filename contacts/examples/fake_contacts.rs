//! A fake iCloud Contacts server, for running the app and the command line
//! without an Apple account.
//!
//!     cargo run -p icloud-contacts --example fake_contacts   # 127.0.0.1:8765
//!     ICLOUD_SESSION_MOCK=1 cargo run -p icloud-contacts --bin icloud-contacts-app   # the app
//!
//! It holds an address book in memory, seeded with a few contacts, and
//! answers the calls the app makes as iCloud does (see `src/api.rs`):
//! `GET /co/startup`, `GET /co/contacts`, and `POST /co/contacts/card/`
//! (create, `method=PUT`, `method=DELETE`). Like iCloud:
//!
//! - every token it hands out is accepted until [`State::expire_tokens`];
//!   then any call carrying an old one is HTTP 420 "Invalid sync token";
//! - a write's answer is the changeset since the request's `syncToken`,
//!   other cards first, so a client that reads it by position goes wrong;
//! - a `PUT` or `DELETE` with a stale `etag` is refused in `errors` (412).
//!
//! Every request is recorded: `GET /fake/requests` lists them.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

#[derive(Default)]
struct Book {
    /// By contactId, with the change seq each was last written at.
    cards: BTreeMap<String, (u64, Map<String, Value>)>,
    seq: u64,
    /// The oldest seq whose token is still accepted.
    valid_from: u64,
}

impl Book {
    /// Stores (or replaces) a card as the next change, with a new etag.
    fn store(&mut self, mut card: Map<String, Value>) {
        self.seq += 1;
        let seq = self.seq;
        card.insert("etag".into(), json!(format!("etag-{seq}")));
        card.entry("isCompany").or_insert(json!(false));
        card.insert("dateModified".into(), json!("2026-10-12T00:00:00Z"));
        let id = card["contactId"].as_str().unwrap().to_owned();
        self.cards.insert(id, (seq, card));
    }
}

/// One server's state.
#[derive(Default)]
pub struct State {
    book: Mutex<Book>,
    requests: Mutex<Vec<Value>>,
    /// Answer 421 to everything: a signed-out account.
    pub signed_out: Mutex<bool>,
}

fn query(url: &str) -> BTreeMap<String, String> {
    let q = url.split_once('?').map_or("", |(_, q)| q);
    url::form_urlencoded::parse(q.as_bytes()).into_owned().collect()
}

impl State {
    /// A few contacts, one of them the "me" card.
    pub fn seeded() -> State {
        let s = State::default();
        s.put(json!({
            "contactId": "ME-CARD", "firstName": "Mock", "lastName": "User",
            "emailAddresses": [{ "label": "HOME", "field": "mock@example.com" }],
        }));
        s.put(json!({
            "contactId": "ANN-LEE", "firstName": "Ann", "lastName": "Lee",
            "companyName": "Acme", "jobTitle": "Engineer",
            "phones": [{ "label": "MOBILE", "field": "+1 (555) 010-0100" }, { "label": "WORK", "field": "+1 555 010 0199" }],
            "emailAddresses": [{ "label": "WORK", "field": "ann@acme.example" }],
            "streetAddresses": [{ "label": "HOME", "field": { "street": "1 Main St", "city": "Springfield", "postalCode": "12345", "country": "United States", "countryCode": "us" } }],
            "birthday": "1990-05-01",
            "photo": { "url": "https://example.invalid/ann.jpg", "crop": { "x": 0, "y": 0, "width": 100, "height": 100 }, "signature": "sig" },
            "profiles": [{ "field": "https://x.example/ann", "user": "ann", "displayname": "X", "userId": "1" }],
        }));
        s.put(json!({
            "contactId": "BOB-BERG", "firstName": "Bob", "lastName": "Berg",
            "phones": [{ "label": "IPHONE", "field": "+47 555 01 234" }],
            "notes": "Met at the conference",
        }));
        s.put(json!({
            "contactId": "ACME-CO", "companyName": "Acme Hardware", "isCompany": true,
            "phones": [{ "label": "MAIN", "field": "+1 555 010 0000" }],
            "urls": [{ "label": "HOMEPAGE", "field": "https://acme.example" }],
        }));
        s
    }

    /// Stores (or replaces) a card as a change, with a new etag: what
    /// another device's edit looks like.
    pub fn put(&self, card: Value) {
        self.book.lock().unwrap().store(card.as_object().unwrap().clone());
    }

    /// A card as the server holds it now.
    pub fn card(&self, id: &str) -> Option<Value> {
        self.book
            .lock()
            .unwrap()
            .cards
            .get(id)
            .map(|(_, c)| Value::Object(c.clone()))
    }

    pub fn count(&self) -> usize {
        self.book.lock().unwrap().cards.len()
    }

    /// Every token handed out so far stops being accepted.
    pub fn expire_tokens(&self) {
        let mut b = self.book.lock().unwrap();
        b.seq += 1;
        b.valid_from = b.seq;
    }

    /// Every request, oldest first: `{"method", "path", "query", "body"}`.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn tokens(b: &Book) -> (String, String) {
        // Base64-ish, so a client that does not percent-encode them breaks.
        (format!("pref+/{}==", b.seq), format!("sync+/{}==", b.seq))
    }

    /// The seq a token names, if it is still accepted.
    fn token_seq(b: &Book, q: &BTreeMap<String, String>) -> Option<u64> {
        let seq: u64 = q
            .get("syncToken")?
            .strip_prefix("sync+/")?
            .strip_suffix("==")?
            .parse()
            .ok()?;
        q.get("prefToken").filter(|p| p.starts_with("pref+/"))?;
        (seq >= b.valid_from && seq <= b.seq).then_some(seq)
    }

    fn header(b: &Book) -> Map<String, Value> {
        let (pref, sync) = State::tokens(b);
        let mut m = Map::new();
        m.insert("prefToken".into(), json!(pref));
        m.insert("syncToken".into(), json!(sync));
        m.insert("meCardId".into(), json!("ME-CARD"));
        m.insert(
            "collections".into(),
            json!([{ "collectionId": "card", "etag": "c", "groupsOrder": [] }]),
        );
        m
    }

    fn startup(&self) -> Value {
        let b = self.book.lock().unwrap();
        let mut m = State::header(&b);
        m.insert(
            "contacts".into(),
            json!(b.cards.values().map(|(_, c)| c.clone()).collect::<Vec<_>>()),
        );
        m.insert("groups".into(), json!([]));
        Value::Object(m)
    }

    fn contacts(&self, q: &BTreeMap<String, String>) -> (u16, Value) {
        let b = self.book.lock().unwrap();
        if State::token_seq(&b, q).is_none() {
            return invalid_token();
        }
        (
            200,
            json!({ "contacts": b.cards.values().map(|(_, c)| c.clone()).collect::<Vec<_>>() }),
        )
    }

    fn card_call(&self, q: &BTreeMap<String, String>, sent: &Value) -> (u16, Value) {
        let mut b = self.book.lock().unwrap();
        let Some(since) = State::token_seq(&b, q) else {
            return invalid_token();
        };
        let Some(card) = sent["contacts"][0].as_object().cloned() else {
            return (400, json!({ "errorReason": "no contacts" }));
        };
        let id = card.get("contactId").and_then(Value::as_str).unwrap_or("").to_owned();
        let method = q.get("method").map(String::as_str);
        let refuse = |code: i64, why: &str| {
            (
                200,
                json!({ "errors": [{ "contactId": id, "etag": card.get("etag"), "errorCode": code, "errorMessage": why }] }),
            )
        };
        let stale = |b: &Book| b.cards.get(&id).is_none_or(|(_, c)| c.get("etag") != card.get("etag"));
        match method {
            None => {
                if b.cards.contains_key(&id) || id.is_empty() {
                    return refuse(409, "Persist Error");
                }
            }
            Some("PUT") | Some("DELETE") if stale(&b) => return refuse(412, "Persist Error"),
            Some("PUT") | Some("DELETE") => {}
            Some(_) => return (400, json!({ "errorReason": "unknown method" })),
        }
        if method == Some("DELETE") {
            b.seq += 1;
            b.cards.remove(&id);
        } else {
            b.store(card);
        }
        // The changeset since the request's token: older changes (other
        // cards) first, the one just written last.
        let mut changed: Vec<(u64, Map<String, Value>)> =
            b.cards.values().filter(|(s, _)| *s > since).cloned().collect();
        changed.sort_by_key(|(s, _)| *s);
        let mut m = State::header(&b);
        m.insert(
            "contacts".into(),
            json!(changed.into_iter().map(|(_, c)| c).collect::<Vec<_>>()),
        );
        if method == Some("DELETE") {
            m.insert("deletes".into(), json!({ "contactIds": [id] }));
        }
        (200, Value::Object(m))
    }

    fn route(&self, method: &str, url: &str, sent: &Value) -> (u16, Value) {
        let path = url.split('?').next().unwrap_or(url);
        if path == "/mock/reauthenticate" {
            *self.signed_out.lock().unwrap() = false;
            return (200, json!({ "ok": true }));
        }
        if path == "/fake/requests" {
            return (200, json!(self.requests()));
        }
        if *self.signed_out.lock().unwrap() {
            return (421, Value::Null);
        }
        let q = query(url);
        self.requests
            .lock()
            .unwrap()
            .push(json!({ "method": method, "path": path, "query": q, "body": sent }));
        match (method, path) {
            ("GET", "/co/startup") => (200, self.startup()),
            ("GET", "/co/contacts") => self.contacts(&q),
            ("POST", "/co/contacts/card/") => self.card_call(&q, sent),
            _ => (404, json!({ "error": "not found" })),
        }
    }
}

fn invalid_token() -> (u16, Value) {
    (420, json!({ "errorReason": "Invalid sync token", "errorCode": 420 }))
}

fn handle(mut request: tiny_http::Request, state: &State) -> std::io::Result<()> {
    let mut body = Vec::new();
    request.as_reader().read_to_end(&mut body)?;
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = request.method().to_string();
    let (status, reply) = state.route(&method, request.url(), &sent);
    if std::env::var_os("FAKE_CONTACTS_QUIET").is_none() {
        eprintln!("{method} {} -> {status}", request.url().split('?').next().unwrap_or(""));
    }
    let header = tiny_http::Header::from_bytes("Content-Type", "application/json").expect("header");
    let data = if reply.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&reply)?
    };
    request.respond(
        tiny_http::Response::from_data(data)
            .with_status_code(status)
            .with_header(header),
    )
}

/// Serves requests on `listener` forever, one thread per request.
pub fn serve_with(listener: TcpListener, state: Arc<State>) -> std::io::Result<()> {
    let server = tiny_http::Server::from_listener(listener, None).map_err(std::io::Error::other)?;
    for request in server.incoming_requests() {
        let state = state.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(request, &state) {
                eprintln!("request failed: {e}");
            }
        });
    }
    Ok(())
}

#[allow(dead_code)] // tests/cli.rs includes this file.
fn main() -> std::io::Result<()> {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8765".into());
    let listener = TcpListener::bind(&addr)?;
    let base = format!("http://{}", listener.local_addr()?);
    eprintln!("Fake iCloud Contacts on {base}");
    eprintln!(
        "Run the app with: ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL={base} cargo run -p icloud-contacts --bin icloud-contacts-app"
    );
    serve_with(listener, Arc::new(State::seeded()))
}
