//! A fake Find My server for running the app without an Apple account.
//!
//!     cargo run --example fake_findme      # listens on 127.0.0.1:8765
//!     ICLOUD_SESSION_MOCK=1 cargo run       # icloud-session's default mock URL
//!
//! Serves tests/fixtures/*.json on the Find My endpoints, stamps positions
//! with the current time, and walks the iPhone a little on every
//! refreshClient so the history trail has something to draw. Also answers
//! `/setup/ws/1/validate` with a webservices map pointing `findme` here, in
//! case the session crate's mock mode validates against the mock URL.
//!
//! Every `playSound` and `lostDevice` request is recorded (endpoint and
//! JSON body): `GET /fake/actions` lists them, so a test or an agent can
//! check what the app sent without a real device ringing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// One server's state: how far the iPhone has walked, and the actions sent.
#[derive(Default)]
pub struct State {
    refreshes: AtomicU64,
    actions: Mutex<Vec<Value>>,
}

impl State {
    /// The recorded `playSound` / `lostDevice` requests, oldest first, each
    /// `{"endpoint": ..., "body": ...}`.
    #[allow(dead_code)] // used by tests/cli.rs, not by the example itself.
    pub fn actions(&self) -> Vec<Value> {
        self.actions.lock().unwrap().clone()
    }
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture JSON")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Re-stamps every fresh fix with "now" (old fixes keep their age) and moves
/// the first device `step` × ~80 m north-east.
fn live(mut v: Value, step: u64) -> Value {
    let now = now_ms();
    let base = v["serverContext"]["serverTimestamp"]
        .as_i64()
        .unwrap_or(now);
    if let Some(devices) = v["content"].as_array_mut() {
        for (i, d) in devices.iter_mut().enumerate() {
            let loc = &mut d["location"];
            if loc.is_null() {
                continue;
            }
            let ts = loc["timeStamp"].as_i64().unwrap_or(base);
            loc["timeStamp"] = json!(now - (base - ts).max(0));
            if i == 0 {
                let lat = loc["latitude"].as_f64().unwrap_or(0.0);
                let lon = loc["longitude"].as_f64().unwrap_or(0.0);
                loc["latitude"] = json!(lat + step as f64 * 0.0005);
                loc["longitude"] = json!(lon + step as f64 * 0.0009);
            }
        }
    }
    v["serverContext"]["serverTimestamp"] = json!(now);
    v
}

fn route(state: &State, base: &str, path: &str, sent: &Value) -> (u16, Value) {
    let path = path.split('?').next().unwrap_or(path);
    let record = |endpoint: &str| {
        state
            .actions
            .lock()
            .unwrap()
            .push(json!({"endpoint": endpoint, "body": sent}));
    };
    match path {
        "/setup/ws/1/validate" => (
            200,
            json!({
                "dsInfo": {"dsid": "12345678901", "appleId": "test@example.com", "fullName": "Test User"},
                "webservices": {"findme": {"url": base, "status": "active"}},
            }),
        ),
        // A restarted app picks up where the walk left off.
        "/fmipservice/client/web/initClient" => match state.refreshes.load(Ordering::SeqCst) {
            0 => (200, live(fixture("initClient"), 0)),
            n => (200, live(fixture("refreshClient"), n)),
        },
        "/fmipservice/client/web/refreshClient" => {
            let n = state.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
            (200, live(fixture("refreshClient"), n))
        }
        "/fmipservice/client/web/playSound" => {
            record("playSound");
            (200, fixture("playSound"))
        }
        "/fmipservice/client/web/lostDevice" => {
            record("lostDevice");
            (200, fixture("lostDevice"))
        }
        "/fake/actions" => (200, json!(state.actions())),
        // icloud_session::sign_in() in mock mode; this account never signs out.
        "/mock/reauthenticate" => (200, json!({"ok": true})),
        _ => (404, json!({"error": "not found"})),
    }
}

fn handle(mut stream: TcpStream, state: &State, base: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;

    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (status, reply) = route(state, base, path, &sent);
    let device = sent.get("device").and_then(Value::as_str).unwrap_or("");
    eprintln!("{method} {path} -> {status} {device}");

    let payload = serde_json::to_vec(&reply)?;
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Not Found" },
        payload.len()
    )?;
    stream.write_all(&payload)
}

/// Serves requests on `listener` forever, one thread per connection.
pub fn serve(listener: TcpListener) -> std::io::Result<()> {
    serve_with(listener, Arc::default())
}

/// [`serve`] with a state the caller keeps, to read the recorded actions.
pub fn serve_with(listener: TcpListener, state: Arc<State>) -> std::io::Result<()> {
    let base = format!("http://{}", listener.local_addr()?);
    for stream in listener.incoming() {
        let (base, state) = (base.clone(), state.clone());
        match stream {
            Ok(s) => {
                std::thread::spawn(move || {
                    if let Err(e) = handle(s, &state, &base) {
                        eprintln!("request failed: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
    Ok(())
}

#[allow(dead_code)] // tests/fake_server.rs includes this file for `serve`.
fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8765".into());
    let listener = TcpListener::bind(&addr)?;
    let base = format!("http://{}", listener.local_addr()?);
    eprintln!("Fake Find My on {base}");
    eprintln!("Run the app with: ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL={base} cargo run");
    serve(listener)
}
