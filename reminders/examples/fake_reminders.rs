//! A fake iCloud Reminders (CloudKit) server, for running the app and the
//! command line without an Apple account.
//!
//!     cargo run -p icloud-reminders --example fake_reminders   # 127.0.0.1:8765
//!     ICLOUD_SESSION_MOCK=1 cargo run -p icloud-reminders --bin icloud-reminders-app   # the app
//!
//! It holds the `Reminders` zone in memory, seeded with two lists and a few
//! reminders due around now, and answers the four calls the app makes as
//! CloudKit web services do: `changes/zone` (by sync token, paged, with
//! `moreComing`), `records/lookup`, and `records/modify` (partial updates,
//! change tags, `CONFLICT`, atomic batches). Every request is recorded:
//! `GET /fake/requests` lists them.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

const DB: &str = "/database/1/com.apple.reminders/production/private";

#[derive(Clone)]
struct Stored {
    seq: u64,
    record: Value,
}

#[derive(Default)]
struct Zone {
    /// By recordName; `seq` orders changes (the sync token is a seq).
    records: BTreeMap<String, Stored>,
    seq: u64,
    tags: u64,
}

/// One server's state.
#[derive(Default)]
pub struct State {
    zone: Mutex<Zone>,
    requests: Mutex<Vec<Value>>,
    /// Answer 421 to everything: a signed-out account.
    pub signed_out: Mutex<bool>,
    /// Records per changes/zone page (0: as many as asked for).
    pub page: Mutex<usize>,
}

fn doc(text: &str) -> Value {
    json!({ "type": "ENCRYPTED_BYTES", "value": icloud_reminders::topotext::encode(text) })
}

fn int(v: i64) -> Value {
    json!({ "type": "INT64", "value": v })
}

fn list_ref(id: &str) -> Value {
    json!({ "type": "REFERENCE", "value": { "recordName": id, "action": "VALIDATE" } })
}

impl State {
    /// Two lists, "Reminders" and "Groceries", and reminders due a little
    /// before and after `now_ms` (Unix ms), as the iPhone would hold them:
    /// floating wall-clock due dates in `zone`'s local time.
    pub fn seeded(now_ms: i64, local_offset_ms: i64) -> State {
        let s = State::default();
        let wall = |offset_ms: i64| now_ms + local_offset_ms + offset_ms;
        let minute = 60_000;
        s.put(list("List/LIST-REMINDERS", "Reminders", &["REM-CALL", "REM-PASSPORT"], "#007AFF"));
        s.put(list("List/LIST-GROCERIES", "Groceries", &["REM-MILK", "REM-EGGS"], "#FF9500"));
        s.put(reminder("REM-MILK", "List/LIST-GROCERIES", "Milk", "Oat, 2 cartons", Some(wall(-minute)), now_ms - 60 * minute));
        s.put(reminder("REM-EGGS", "List/LIST-GROCERIES", "Eggs", "", None, now_ms - 60 * minute));
        s.put(reminder("REM-CALL", "List/LIST-REMINDERS", "Call the dentist", "", Some(wall(90 * minute)), now_ms - 60 * minute));
        s.put(reminder("REM-PASSPORT", "List/LIST-REMINDERS", "Renew passport", "Photos first", Some(wall(3 * 24 * 60 * minute)), now_ms - 60 * minute));
        let mut done = reminder("REM-PAINT", "List/LIST-REMINDERS", "Buy paint", "", None, now_ms - 120 * minute);
        done["fields"]["Completed"] = int(1);
        done["fields"]["CompletionDate"] = json!({ "type": "TIMESTAMP", "value": now_ms - 100 * minute });
        s.put(done);
        s
    }

    /// Stores (or replaces) a record as a change.
    pub fn put(&self, mut record: Value) {
        let mut z = self.zone.lock().unwrap();
        z.seq += 1;
        z.tags += 1;
        record["recordChangeTag"] = json!(format!("tag-{}", z.tags));
        let name = record["recordName"].as_str().unwrap().to_owned();
        let seq = z.seq;
        z.records.insert(name, Stored { seq, record });
    }

    /// A record as the server holds it now.
    pub fn record(&self, name: &str) -> Option<Value> {
        self.zone.lock().unwrap().records.get(name).map(|s| s.record.clone())
    }

    /// Every request, oldest first: `{"op": "changes/zone", "body": ...}`.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn changes(&self, sent: &Value) -> Value {
        let zone_req = &sent["zones"][0];
        let types: Vec<&str> = zone_req["desiredRecordTypes"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let since = match zone_req.get("syncToken").and_then(Value::as_str) {
            None => 0,
            Some(t) => match t.strip_prefix("tok-").and_then(|n| n.parse().ok()) {
                Some(n) => n,
                // Apple answers an expired token with CHANGE_TOKEN_EXPIRED and
                // one it can't parse with BAD_REQUEST.
                None if t.starts_with("garbled") => {
                    return json!({ "zones": [{
                        "zoneID": zone_req["zoneID"],
                        "serverErrorCode": "BAD_REQUEST",
                        "reason": "Unknown sync continuation type",
                    }] });
                }
                None => {
                    return json!({ "zones": [{
                        "zoneID": zone_req["zoneID"],
                        "serverErrorCode": "CHANGE_TOKEN_EXPIRED",
                        "reason": "the sync token is not valid any more",
                    }] });
                }
            },
        };
        let limit = sent["resultsLimit"].as_u64().unwrap_or(200) as usize;
        let page = match *self.page.lock().unwrap() {
            0 => limit,
            n => n.min(limit),
        };
        let z = self.zone.lock().unwrap();
        let mut changed: Vec<&Stored> = z
            .records
            .values()
            .filter(|s| s.seq > since)
            .filter(|s| {
                let t = s.record["recordType"].as_str().unwrap_or("");
                // Tombstones carry no type; they go to every stream.
                types.is_empty() || t.is_empty() || types.contains(&t)
            })
            .collect();
        changed.sort_by_key(|s| s.seq);
        let more = changed.len() > page;
        changed.truncate(page);
        // Up to the last record sent; everything, when nothing is left.
        let token = if more { changed.last().map_or(since, |s| s.seq) } else { z.seq };
        json!({ "zones": [{
            "zoneID": zone_req["zoneID"],
            "syncToken": format!("tok-{token}"),
            "moreComing": more,
            "records": changed.iter().map(|s| s.record.clone()).collect::<Vec<_>>(),
        }] })
    }

    fn lookup(&self, sent: &Value) -> Value {
        let z = self.zone.lock().unwrap();
        let records: Vec<Value> = sent["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["recordName"].as_str())
            .map(|name| match z.records.get(name) {
                Some(s) => s.record.clone(),
                None => json!({ "recordName": name, "serverErrorCode": "NOT_FOUND", "reason": "no such record" }),
            })
            .collect();
        json!({ "records": records })
    }

    fn modify(&self, sent: &Value) -> Value {
        let mut z = self.zone.lock().unwrap();
        let ops = sent["operations"].as_array().cloned().unwrap_or_default();
        // Check every operation first: an atomic batch applies all or none.
        let mut errors = Vec::new();
        for op in &ops {
            let rec = &op["record"];
            let name = rec["recordName"].as_str().unwrap_or("");
            let current = z.records.get(name);
            let error = match (op["operationType"].as_str(), current) {
                (Some("create"), Some(_)) => Some(("CONFLICT", "record exists")),
                (Some("create"), None) => None,
                (Some("update"), None) => Some(("NOT_FOUND", "no such record")),
                (Some("update"), Some(s)) if rec.get("recordChangeTag") != s.record.get("recordChangeTag") => {
                    Some(("CONFLICT", "oplock error"))
                }
                (Some("update"), Some(_)) => None,
                _ => Some(("BAD_REQUEST", "unsupported operation")),
            };
            errors.push(error.map(|(code, reason)| json!({ "recordName": name, "serverErrorCode": code, "reason": reason })));
        }
        if errors.iter().any(Option::is_some) {
            let records: Vec<Value> = errors
                .into_iter()
                .zip(&ops)
                .map(|(e, op)| {
                    e.unwrap_or_else(|| json!({ "recordName": op["record"]["recordName"], "serverErrorCode": "ATOMIC_ERROR" }))
                })
                .collect();
            return json!({ "records": records });
        }
        let mut out = Vec::new();
        for op in ops {
            let rec = &op["record"];
            let name = rec["recordName"].as_str().unwrap_or("").to_owned();
            let mut stored = match z.records.get(&name) {
                Some(s) => s.record.clone(),
                None => json!({ "recordName": name, "recordType": rec["recordType"], "fields": {} }),
            };
            // An update writes only the fields it names.
            let fields = stored["fields"].as_object_mut().expect("fields");
            for (k, v) in rec["fields"].as_object().cloned().unwrap_or_default() {
                fields.insert(k, v);
            }
            z.seq += 1;
            z.tags += 1;
            stored["recordChangeTag"] = json!(format!("tag-{}", z.tags));
            // CloudKit's own record times, as every record carries them.
            let now = json!({ "timestamp": icloud_session::time::now_ms(), "userRecordName": "_fake" });
            if stored.get("created").is_none() {
                stored["created"] = now.clone();
            }
            stored["modified"] = now;
            let seq = z.seq;
            out.push(stored.clone());
            z.records.insert(name, Stored { seq, record: stored });
        }
        json!({ "records": out })
    }

    fn route(&self, path: &str, sent: &Value) -> (u16, Value) {
        let path = path.split('?').next().unwrap_or(path);
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
        let Some(op) = path.strip_prefix(DB).map(|p| p.trim_start_matches('/')) else {
            return (404, json!({ "error": "not found" }));
        };
        self.requests.lock().unwrap().push(json!({ "op": op, "body": sent }));
        match op {
            "changes/zone" => (200, self.changes(sent)),
            "records/lookup" => (200, self.lookup(sent)),
            "records/modify" => (200, self.modify(sent)),
            _ => (404, json!({ "error": "not found" })),
        }
    }
}

fn list(id: &str, name: &str, order: &[&str], hex: &str) -> Value {
    let order = serde_json::to_string(order).unwrap();
    let color = json!({ "daHexString": hex }).to_string();
    json!({
        "recordName": id,
        "recordType": "List",
        "fields": {
            "Name": { "type": "STRING", "value": name },
            "Color": { "type": "STRING", "value": color },
            "Deleted": int(0),
            "ReminderIDs": { "type": "STRING", "value": order },
        },
    })
}

/// A reminder record; `due_wall` is the wall clock read as UTC (ms).
pub fn reminder(uuid: &str, list_id: &str, title: &str, notes: &str, due_wall: Option<i64>, modified_ms: i64) -> Value {
    let mut fields = Map::new();
    fields.insert("TitleDocument".into(), doc(title));
    fields.insert("NotesDocument".into(), doc(notes));
    fields.insert("List".into(), list_ref(list_id));
    fields.insert("Completed".into(), int(0));
    fields.insert("Deleted".into(), int(0));
    fields.insert("Flagged".into(), int(0));
    fields.insert("AllDay".into(), int(0));
    fields.insert("Priority".into(), int(0));
    fields.insert("AlarmIDs".into(), json!({ "type": "STRING_LIST", "value": [] }));
    fields.insert(
        "ResolutionTokenMap".into(),
        json!({ "type": "STRING", "value": r#"{"map":{"completed":{"counter":3,"modificationTime":1.0,"replicaID":"PHONE"}}}"# }),
    );
    if let Some(due) = due_wall {
        fields.insert("DueDate".into(), json!({ "type": "TIMESTAMP", "value": due }));
    }
    let at = json!({ "timestamp": modified_ms, "userRecordName": "_fake" });
    json!({
        "recordName": format!("Reminder/{uuid}"),
        "recordType": "Reminder",
        "fields": fields,
        "created": at,
        "modified": at,
    })
}

fn handle(mut request: tiny_http::Request, state: &State) -> std::io::Result<()> {
    let mut body = Vec::new();
    request.as_reader().read_to_end(&mut body)?;
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (status, reply) = state.route(request.url(), &sent);
    if std::env::var_os("FAKE_REMINDERS_QUIET").is_none() {
        eprintln!("{} {} -> {status}", request.method(), request.url().split('?').next().unwrap_or(""));
    }
    let header = tiny_http::Header::from_bytes("Content-Type", "application/json").expect("header");
    let data = if reply.is_null() { Vec::new() } else { serde_json::to_vec(&reply)? };
    request.respond(tiny_http::Response::from_data(data).with_status_code(status).with_header(header))
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
    let local = icloud_reminders::due::local_zone().map_err(std::io::Error::other)?;
    let now = jiff::Timestamp::now();
    let offset_ms = i64::from(local.to_offset(now).seconds()) * 1000;
    eprintln!("Fake iCloud Reminders on {base}");
    eprintln!("Run the app with: ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL={base} cargo run -p icloud-reminders --bin icloud-reminders-app");
    serve_with(listener, Arc::new(State::seeded(now.as_millisecond(), offset_ms)))
}
