//! The dev fake server (examples/fake_findme.rs) speaks the same protocol the
//! client expects: drive a FindMe over icloud-session's mock against it.

#[path = "../examples/fake_findme.rs"]
mod fake;

use std::net::TcpListener;

use icloud_findmy::findme::{FindMe, SessionTransport};
use icloud_session::Session;

#[test]
fn app_client_runs_against_fake_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || fake::serve(listener));

    let mut fm = FindMe::new(SessionTransport::new(Session::mock(&base)));
    let first = fm.refresh(true).unwrap();
    let second = fm.refresh(true).unwrap();
    assert_eq!(first.len(), 4);
    let (a, b) = (first[0].location.unwrap(), second[0].location.unwrap());
    assert!(b.lat > a.lat, "the fake walks the phone on each refresh");
    let age = icloud_findmy::models::now_ms() - b.ts_ms;
    assert!((0..120_000).contains(&age), "fixes are stamped with the current time");
    fm.play_sound(&second[0]).unwrap();
    fm.lost_mode(&second[0], "+1555", "lost").unwrap();
}
