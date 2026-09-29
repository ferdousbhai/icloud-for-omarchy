//! Run icloud-photos without an Apple account:
//!
//!   cargo run --example fake_cloudkit -- [--port 8765] [--count 120] [--signed-out]
//!   ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:8765 cargo run
//!
//! `--signed-out` answers 421 until the sign-in banner's button is pressed
//! (in mock mode there is no icloud-sessiond or sign-in window: the button
//! just tells this server, and the app syncs again).

#[path = "../tests/support/fake_server.rs"]
mod fake_server;

fn main() {
    let mut port = 8765u16;
    let mut count = 120usize;
    let mut signed_out = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|v| v.parse().ok()).expect("--port <number>"),
            "--count" => count = args.next().and_then(|v| v.parse().ok()).expect("--count <number>"),
            "--signed-out" => signed_out = true,
            other => {
                eprintln!("unknown argument {other}; see the comment at the top of examples/fake_cloudkit.rs");
                std::process::exit(2);
            }
        }
    }
    let server = fake_server::FakeServer::start(port, count);
    if signed_out {
        server.sign_out();
    }
    println!("Fake CloudKit with {count} assets at {}", server.url);
    println!("ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL={} cargo run", server.url);
    loop {
        std::thread::park();
    }
}
