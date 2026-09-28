//! The dev fake server (examples/fake_findme.rs) speaks the same protocol the
//! client expects: drive a FindMe over plain HTTP against it.

#[path = "../examples/fake_findme.rs"]
mod fake;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use icloud_findmy::findme::{self, FindMe, Transport};
use serde_json::Value;

struct PlainHttp {
    base: String,
}

impl Transport for PlainHttp {
    fn service_root(&mut self) -> findme::Result<String> {
        Ok(self.base.clone())
    }
    fn post_json(&mut self, url: &str, body: &Value) -> findme::Result<Value> {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let payload = serde_json::to_vec(body).unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        write!(
            s,
            "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        )
        .unwrap();
        s.write_all(&payload).unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap();
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        if status != 200 {
            return Err(findme::Error::Http(status));
        }
        Ok(serde_json::from_str(body).unwrap())
    }
}

#[test]
fn app_client_runs_against_fake_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || fake::serve(listener));

    let mut fm = FindMe::new(PlainHttp { base });
    let first = fm.refresh().unwrap();
    let second = fm.refresh().unwrap();
    assert_eq!(first.len(), 4);
    let (a, b) = (first[0].location.unwrap(), second[0].location.unwrap());
    assert!(b.lat > a.lat, "the fake walks the phone on each refresh");
    let age = icloud_findmy::models::now_ms() - b.ts_ms;
    assert!(
        (0..120_000).contains(&age),
        "fixes are stamped with the current time"
    );
    fm.play_sound(&second[0]).unwrap();
    fm.lost_mode(&second[0], "+1555", "lost").unwrap();
}
