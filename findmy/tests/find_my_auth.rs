//! A Find My 450 through the real `icloud-session` client in mock mode (no
//! D-Bus; every request goes to a local server): it surfaces as
//! `FindMyAuthRequired` after one request, with no retry loop. One test,
//! because it sets process environment.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use icloud_findmy::findme::{Error, FindMe, SessionTransport};

#[test]
fn http_450_is_find_my_auth_required_without_a_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut len = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            seen2
                .lock()
                .unwrap()
                .push(line.split_whitespace().nth(1).unwrap().to_string());
            let _ = stream.write_all(b"HTTP/1.1 450 \r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    // SAFETY: the only test in this binary; no other thread reads the environment.
    unsafe {
        std::env::set_var("ICLOUD_SESSION_MOCK", "1");
        std::env::set_var("ICLOUD_SESSION_MOCK_URL", &base);
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
    }

    let mut fm = FindMe::new(SessionTransport::default());
    assert!(matches!(fm.refresh(true), Err(Error::FindMyAuthRequired)));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].starts_with("/fmipservice/client/web/initClient?"), "{seen:?}");
    assert_eq!(
        Error::FindMyAuthRequired.to_string(),
        "Find My needs your Apple password"
    );
    // Mock mode: authorizing is a no-op and reports Find My authorized.
    icloud_session::authorize_find_my().unwrap();
    assert!(icloud_session::status().unwrap().find_my_authorized);
}
