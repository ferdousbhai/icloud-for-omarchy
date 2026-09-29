//! The Rust cassette / request-log types read what the Node driver reads and
//! writes (tests/differential/README.md).

use std::path::Path;

use icloud_notes_sync::cloudkit::transport::{Cassette, RequestLog};

#[test]
fn cassettes_parse() {
    for name in ["tiny-clone.json", "tiny-lookup.json"] {
        let cassette = Cassette::load(&Path::new("tests/differential/cassettes").join(name)).unwrap();
        assert_eq!(cassette.version, 1);
        assert!(!cassette.interactions.is_empty());
    }
}

#[test]
fn node_request_logs_parse() {
    for dir in ["tiny-clone", "tiny-push-dry-run"] {
        let path = Path::new("tests/differential/expected").join(dir).join("requests.json");
        let log: RequestLog = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let ck: Vec<_> = log.requests.iter().filter(|r| r.service == "ckdatabasews").collect();
        assert!(!ck.is_empty());
        assert!(ck.iter().all(|r| r.matched.is_some()));
    }
}
