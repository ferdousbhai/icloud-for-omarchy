//! A server cannot make a caller hold an unbounded body in memory: JSON
//! answers stop at 64 MiB and `get_bytes` at 32 MiB, counted after ureq has
//! ungzipped them, while `download` still streams any size to disk.

use std::io::{self, Read};
use std::sync::Arc;
use std::thread;

use flate2::Compression;
use flate2::write::GzEncoder;
use icloud_session::{Error, Result, Session};

const MIB: u64 = 1 << 20;

/// Serves `/plain/<MiB>` as that many spaces (chunked, no length) and
/// `/gzip/<MiB>` as that many zeros gzipped to a few KiB.
fn server() -> String {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let url = request.url().to_string();
            let path = url.split('?').next().unwrap_or_default();
            let mut parts = path.trim_start_matches('/').split('/');
            let kind = parts.next().unwrap_or_default().to_string();
            let mib: u64 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            if kind == "gzip" {
                let mut gz = GzEncoder::new(Vec::new(), Compression::best());
                io::copy(&mut io::repeat(0).take(mib * MIB), &mut gz).unwrap();
                let body = gz.finish().unwrap();
                let header = "Content-Encoding: gzip".parse::<tiny_http::Header>().unwrap();
                let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
            } else {
                let body = io::repeat(b' ').take(mib * MIB);
                let _ = request.respond(tiny_http::Response::new(200.into(), vec![], body, None, None));
            }
        }
    });
    base
}

/// The body's length, or the error's text, so a failure never prints megabytes.
fn outcome(result: Result<Vec<u8>>) -> std::result::Result<u64, String> {
    result.map(|b| b.len() as u64).map_err(|e| match e {
        Error::BadResponse(m) => m,
        e => format!("other error: {e:?}"),
    })
}

const TOO_LARGE_64: &str = "response larger than 64 MiB";
const TOO_LARGE_32: &str = "response larger than 32 MiB";

#[test]
fn json_bodies_stop_at_64_mib() {
    let base = server();
    let s = Session::mock(&base);
    let get = |p: &str| outcome(s.get(&format!("{base}{p}")).map(|r| r.body));
    assert_eq!(get("/plain/1"), Ok(MIB));
    assert_eq!(get("/plain/64"), Ok(64 * MIB));
    assert_eq!(get("/plain/65"), Err(TOO_LARGE_64.into()));
    let post = s.post_json(&format!("{base}/gzip/200"), &serde_json::json!({}));
    assert_eq!(
        outcome(post.map(|r| r.body)),
        Err(TOO_LARGE_64.into()),
        "counted decoded"
    );
}

#[test]
fn get_bytes_stops_at_32_mib_and_download_streams_on() {
    let base = server();
    let s = Session::mock(&base);
    let get = |p: &str| outcome(s.get_bytes(&format!("{base}{p}")));
    assert_eq!(get("/plain/32"), Ok(32 * MIB));
    assert_eq!(get("/plain/33"), Err(TOO_LARGE_32.into()));
    assert_eq!(get("/gzip/100"), Err(TOO_LARGE_32.into()), "counted decoded");

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("big");
    assert_eq!(s.download(&format!("{base}/plain/40"), &dest).unwrap(), 40 * MIB);
}
