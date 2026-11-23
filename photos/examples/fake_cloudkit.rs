//! Run icloud-photos without an Apple account:
//!
//!   cargo run --example fake_cloudkit -- [--port 8765] [--count 120] [--signed-out] [--hold-uploads <seconds>]
//!     [--hold-deletes <milliseconds>]
//!   ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:8765 cargo run
//!
//! `--signed-out` answers 421 until the sign-in banner's button is pressed
//! (in mock mode there is no icloud-sessiond or sign-in window: the button
//! just tells this server, and the app syncs again). `--hold-uploads` takes
//! that long over each uploaded file, so the upload dialog can be seen at work.
//! `--hold-deletes` (1500 unless given) takes that long to answer a delete,
//! as iCloud can: long enough for the window to close while one is on its
//! way, which `dev/qa/close.jsonl` does to see the app finish it first. Each
//! delete and restore is said on stdout (`deleted <id> (<filename>)`).

#[path = "../tests/support/fake_server.rs"]
mod fake_server;

fn main() {
    let mut port = 8765u16;
    let mut count = 120usize;
    let mut signed_out = false;
    let mut hold_uploads = 0u64;
    let mut hold_deletes = 1500u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|v| v.parse().ok()).expect("--port <number>"),
            "--count" => count = args.next().and_then(|v| v.parse().ok()).expect("--count <number>"),
            "--signed-out" => signed_out = true,
            "--hold-uploads" => {
                hold_uploads = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--hold-uploads <seconds>")
            }
            "--hold-deletes" => {
                hold_deletes = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--hold-deletes <milliseconds>")
            }
            other => {
                eprintln!("unknown argument {other}; see the comment at the top of examples/fake_cloudkit.rs");
                std::process::exit(1);
            }
        }
    }
    let server = fake_server::FakeServer::start(port, count);
    if signed_out {
        server.sign_out();
    }
    server.hold_uploads(hold_uploads);
    server.hold_deletes(hold_deletes);
    println!("Fake CloudKit with {count} assets at {}", server.url);
    println!("ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL={} cargo run", server.url);
    loop {
        std::thread::park();
    }
}
