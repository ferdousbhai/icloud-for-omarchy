//! A fake Find My server for running the app without an Apple account.
//!
//!     cargo run --example fake_findme      # listens on 127.0.0.1:8765
//!     ICLOUD_SESSION_MOCK=1 cargo run       # icloud-session's default mock URL
//!
//! Serves tests/fixtures/*.json on the Find My endpoints, stamps positions
//! with the current time, and walks the iPhone a little on every
//! refreshClient so the history trail has something to draw.
//!
//! Every `playSound` and `lostDevice` request is recorded (endpoint and
//! JSON body): `GET /fake/actions` lists them, so a test or an agent can
//! check what the app sent without a real device ringing.
//!
//! A `lostDevice` whose phone number is [`SLOW_PHONE`] is answered only
//! after [`SLOW`], and the fake prints `answered lostDevice` once it has
//! sent that answer: the window's QA script closes the window while such a
//! request is on its way, and its `.after` check reads the line to see that
//! the app stayed until its request was answered.
//!
//! `POST /fake/sign-out` answers every Find My request 421 (signed out), and
//! `POST /fake/find-my-password` answers them 450 (Find My wants the Apple
//! password), until the app's Sign In (`/mock/reauthenticate`).

use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use icloud_session::time::now_ms;
use serde_json::{Value, json};

/// The phone number that makes a `lostDevice` slow.
const SLOW_PHONE: &str = "+1 555 0199";
/// How long a slow `lostDevice` waits before it is answered: longer than an
/// app that did not wait would take to end, well inside a QA command's five
/// seconds.
const SLOW: Duration = Duration::from_secs(2);

/// One server's state: how far the iPhone has walked, and the actions sent.
#[derive(Default)]
pub struct State {
    refreshes: AtomicU64,
    actions: Mutex<Vec<Value>>,
    /// What every Find My request answers instead: 421 or 450; 0 for none.
    refusing: AtomicU16,
}

impl State {
    /// The recorded `playSound` / `lostDevice` requests, oldest first, each
    /// `{"endpoint": ..., "body": ...}`.
    pub fn actions(&self) -> Vec<Value> {
        self.actions.lock().unwrap().clone()
    }
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture JSON")
}

/// Re-stamps every fresh fix with "now" (old fixes keep their age) and moves
/// the first device `step` × ~80 m north-east.
fn live(mut v: Value, step: u64) -> Value {
    let now = now_ms();
    let base = v["serverContext"]["serverTimestamp"].as_i64().unwrap_or(now);
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

fn route(state: &State, path: &str, sent: &Value) -> (u16, Value) {
    let path = path.split('?').next().unwrap_or(path);
    let record = |endpoint: &str| {
        state
            .actions
            .lock()
            .unwrap()
            .push(json!({"endpoint": endpoint, "body": sent}));
    };
    let refusing = state.refusing.load(Ordering::SeqCst);
    if refusing != 0 && path.starts_with("/fmipservice/") {
        return (refusing, Value::Null);
    }
    match path {
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
            if sent["ownerNbr"] == SLOW_PHONE {
                std::thread::sleep(SLOW);
            }
            (200, fixture("lostDevice"))
        }
        "/fake/actions" => (200, json!(state.actions())),
        "/fake/sign-out" => {
            state.refusing.store(421, Ordering::SeqCst);
            (200, json!({"ok": true}))
        }
        "/fake/find-my-password" => {
            state.refusing.store(450, Ordering::SeqCst);
            (200, json!({"ok": true}))
        }
        // icloud_session::sign_in() in mock mode.
        "/mock/reauthenticate" => {
            state.refusing.store(0, Ordering::SeqCst);
            (200, json!({"ok": true}))
        }
        _ => (404, json!({"error": "not found"})),
    }
}

fn handle(mut request: tiny_http::Request, state: &State) -> std::io::Result<()> {
    let mut body = Vec::new();
    request.as_reader().read_to_end(&mut body)?;
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (status, reply) = route(state, request.url(), &sent);
    let device = sent.get("device").and_then(Value::as_str).unwrap_or("");
    eprintln!("{} {} -> {status} {device}", request.method(), request.url());

    let header = tiny_http::Header::from_bytes("Content-Type", "application/json").expect("header");
    let path = request.url().split('?').next().unwrap_or_default();
    let endpoint = path.rsplit('/').next().unwrap_or_default().to_owned();
    request.respond(
        tiny_http::Response::from_data(serde_json::to_vec(&reply)?)
            .with_status_code(status)
            .with_header(header),
    )?;
    if matches!(endpoint.as_str(), "playSound" | "lostDevice") {
        eprintln!("answered {endpoint}");
    }
    Ok(())
}

/// Serves requests on `listener` forever, one thread per request, recording
/// actions into `state`.
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
    eprintln!("Fake Find My on {base}");
    eprintln!("Run the app with: ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL={base} cargo run");
    serve_with(listener, Arc::default())
}
