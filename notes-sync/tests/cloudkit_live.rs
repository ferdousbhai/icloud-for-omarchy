//! LiveTransport over icloud-session's mock session (no D-Bus, no Apple):
//! a local HTTP server stands in for ckdatabasews and the asset host.

use std::sync::mpsc;
use std::thread;

use icloud_notes_sync::cloudkit::{CkError, Database, LiveTransport, note_zone};
use serde_json::{Value, json};

struct Seen {
    method: String,
    url: String,
    body: String,
}

/// Serves `answers` (status, body) in order, then stops; reports each request.
fn serve(answers: Vec<(u16, Vec<u8>)>) -> (String, mpsc::Receiver<Seen>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for (status, body) in answers {
            let mut request = server.recv().unwrap();
            let mut text = String::new();
            request.as_reader().read_to_string(&mut text).unwrap();
            tx.send(Seen {
                method: request.method().to_string(),
                url: request.url().to_owned(),
                body: text,
            })
            .unwrap();
            request
                .respond(tiny_http::Response::from_data(body).with_status_code(status))
                .unwrap();
        }
    });
    (base, rx)
}

fn transport(base: &str) -> LiveTransport {
    LiveTransport::from_session(icloud_session::Session::mock(base)).unwrap()
}

#[test]
fn posts_go_to_ckdatabasews_with_icloud_md_query_and_body() {
    let answer = json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "moreComing": false, "syncToken": "t", "records": [] }] });
    let (base, rx) = serve(vec![(200, answer.to_string().into_bytes())]);
    let database = Database::new(transport(&base));
    assert_eq!(database.transport.base, base);

    let result = database.fetch_all_note_records(None, &mut |_| {}).unwrap();
    assert_eq!(result.sync_token.as_deref(), Some("t"));

    let seen = rx.recv().unwrap();
    assert_eq!(seen.method, "POST");
    assert!(
        seen.url.starts_with(
            "/database/1/com.apple.notes/production/private/changes/zone?ckjsBuildVersion=2310ProjectDev27&ckjsVersion=2.6.4&"
        ),
        "{}",
        seen.url
    );
    // icloud-session appends its own client parameters.
    assert!(
        seen.url.contains("dsid=mock") && seen.url.contains("clientId=mock"),
        "{}",
        seen.url
    );
    let body: Value = serde_json::from_str(&seen.body).unwrap();
    let keys: Vec<_> = body["zones"][0].as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["zoneID", "desiredKeys", "desiredRecordTypes", "reverse"]);
}

#[test]
fn http_errors_and_sign_in_are_mapped() {
    let (base, _rx) = serve(vec![(500, b"{}".to_vec()), (421, b"{}".to_vec())]);
    let database = Database::new(transport(&base));
    let err = database
        .lookup_records(&note_zone(None), &["n".to_owned()])
        .unwrap_err();
    assert_eq!(err.to_string(), "records/lookup request failed (private db): HTTP 500");
    // A 421 that icloud-session can't recover from (mock mode: no daemon).
    let err = database
        .lookup_records(&note_zone(None), &["n".to_owned()])
        .unwrap_err();
    assert!(matches!(err, CkError::SignInRequired), "{err:?}");
}

#[test]
fn asset_downloads_stream_to_disk() {
    let (base, rx) = serve(vec![(200, b"bytes!".to_vec()), (403, Vec::new())]);
    let database = Database::new(transport(&base));
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("a/b.bin");
    let n = database.fetch_asset(&format!("{base}/B/abc?e=1"), &dest).unwrap();
    assert_eq!(n, 6);
    assert_eq!(std::fs::read(&dest).unwrap(), b"bytes!");
    let seen = rx.recv().unwrap();
    assert_eq!((seen.method.as_str(), seen.url.as_str()), ("GET", "/B/abc?e=1"));

    let err = database
        .fetch_asset(&format!("{base}/B/old"), &tmp.path().join("c"))
        .unwrap_err();
    assert_eq!(err.to_string(), "Attachment download failed: HTTP 403");
}
