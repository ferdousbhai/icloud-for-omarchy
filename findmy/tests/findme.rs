//! Find My against recorded-style fixtures (shape from pyicloud's
//! FindMyiPhoneServiceManager and its test constants).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use icloud_findmy::findme::{self, Error, FindMe, Transport};
use icloud_findmy::history::History;
use icloud_findmy::models::DeviceClass;
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const ROOT: &str = "https://p42-fmipweb.icloud.com:443";

type Log = Arc<Mutex<Vec<(String, Value)>>>;

/// Answers each POST from a queue and records what was sent.
struct Fake {
    replies: VecDeque<findme::Result<Value>>,
    log: Log,
}

impl Transport for Fake {
    fn service_root(&mut self) -> findme::Result<String> {
        Ok(ROOT.into())
    }
    fn post_json(&mut self, url: &str, body: &Value) -> findme::Result<Value> {
        self.log
            .lock()
            .unwrap()
            .push((url.to_string(), body.clone()));
        self.replies.pop_front().expect("unexpected request")
    }
}

fn client(replies: Vec<findme::Result<Value>>) -> (FindMe<Fake>, Log) {
    let log = Log::default();
    let fake = Fake {
        replies: replies.into(),
        log: log.clone(),
    };
    (FindMe::new(fake), log)
}

#[test]
fn init_then_refresh_carries_server_context() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Ok(fixture("refreshClient")),
    ]);

    let devices = fm.refresh().unwrap();
    assert_eq!(devices.len(), 4);
    let devices2 = fm.refresh().unwrap();
    assert_eq!(devices2.len(), 4);

    let log = log.lock().unwrap();
    assert_eq!(
        log[0].0,
        format!("{ROOT}/fmipservice/client/web/initClient")
    );
    assert_eq!(
        log[1].0,
        format!("{ROOT}/fmipservice/client/web/refreshClient")
    );

    let init = &log[0].1;
    assert_eq!(init["clientContext"]["appName"], "iCloud Find (Web)");
    assert_eq!(init["clientContext"]["apiVersion"], "3.0");
    assert_eq!(init["clientContext"]["fmly"], false);
    assert!(init.get("serverContext").is_none());
    assert!(init["clientContext"].get("shouldLocate").is_none());

    let refresh = &log[1].1;
    assert_eq!(refresh["isUpdatingAllLocations"], true);
    assert_eq!(refresh["clientContext"]["shouldLocate"], true);
    assert_eq!(refresh["clientContext"]["selectedDevice"], "all");
    let ctx = &refresh["serverContext"];
    assert_eq!(ctx["prsId"], 12345678901_i64);
    assert!(
        ctx["theftLoss"].is_null(),
        "theftLoss must be nulled before sending back"
    );
}

#[test]
fn parses_devices() {
    let snap = findme::parse_response(&fixture("initClient")).unwrap();
    let [phone, mac, watch, pods] = &snap.devices[..] else {
        panic!("want 4 devices")
    };

    assert_eq!(phone.name, "Test's iPhone");
    assert_eq!(phone.model_name, "iPhone 15 Pro");
    assert_eq!(phone.class, DeviceClass::IPhone);
    assert_eq!(phone.battery, Some(0.82));
    assert!(phone.online && !phone.charging);
    let fix = phone.location.unwrap();
    assert!((fix.lat - 60.169857).abs() < 1e-9 && (fix.lon - 24.938379).abs() < 1e-9);
    assert_eq!(fix.ts_ms, 1_790_000_000_000);
    assert!(phone.can_play_sound && phone.can_lost_mode);

    assert_eq!(mac.class, DeviceClass::Mac);
    assert!(mac.charging);
    assert_eq!(mac.location.unwrap().accuracy, 65.0);

    assert_eq!(watch.class, DeviceClass::Watch);
    assert!(!watch.online);
    assert!(watch.location.unwrap().is_old);

    assert_eq!(pods.class, DeviceClass::AirPods);
    assert_eq!(
        pods.battery, None,
        "batteryStatus Unknown hides the 0.0 level"
    );
    assert_eq!(pods.location, None);
    assert!(!pods.can_lost_mode);

    assert!(phone.summary(1_790_000_300_000).contains("82%"));
    assert!(phone.summary(1_790_000_300_000).ends_with("5 min ago"));
}

#[test]
fn empty_content_is_no_devices() {
    let snap = findme::parse_response(&serde_json::json!({"serverContext": {"a": 1}})).unwrap();
    assert!(snap.devices.is_empty());
    assert!(findme::parse_response(&serde_json::json!([1])).is_err());
    assert!(findme::parse_response(&serde_json::json!({"content": 3})).is_err());
}

#[test]
fn play_sound_and_lost_mode_payloads() {
    let snap = findme::parse_response(&fixture("initClient")).unwrap();
    let phone = &snap.devices[0];
    let (mut fm, log) = client(vec![Ok(fixture("playSound")), Ok(fixture("lostDevice"))]);

    fm.play_sound(phone).unwrap();
    fm.lost_mode(phone, "+358401234567", "Lost, please call")
        .unwrap();

    let log = log.lock().unwrap();
    assert_eq!(log[0].0, format!("{ROOT}/fmipservice/client/web/playSound"));
    assert_eq!(log[0].1["device"], phone.id.as_str());
    assert_eq!(log[0].1["subject"], "Find My iPhone Alert");
    assert_eq!(log[0].1["clientContext"]["fmly"], true);

    assert_eq!(
        log[1].0,
        format!("{ROOT}/fmipservice/client/web/lostDevice")
    );
    let b = &log[1].1;
    assert_eq!(b["device"], phone.id.as_str());
    assert_eq!(b["ownerNbr"], "+358401234567");
    assert_eq!(b["text"], "Lost, please call");
    assert_eq!(b["lostModeEnabled"], true);
    assert_eq!(b["trackingEnabled"], true);
    assert_eq!(b["userText"], true);
    assert_eq!(b["passcode"], "");
}

#[test]
fn unsupported_actions_send_nothing() {
    let snap = findme::parse_response(&fixture("initClient")).unwrap();
    let pods = &snap.devices[3];
    let (mut fm, log) = client(vec![]);
    assert!(matches!(
        fm.lost_mode(pods, "1", "x"),
        Err(Error::Unsupported(_))
    ));
    let mut mute = pods.clone();
    mute.can_play_sound = false;
    assert!(matches!(fm.play_sound(&mute), Err(Error::Unsupported(_))));
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn sign_in_required_resets_to_init_client() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Err(Error::SignInRequired),
        Ok(fixture("initClient")),
    ]);
    fm.refresh().unwrap();
    assert!(matches!(fm.refresh(), Err(Error::SignInRequired)));
    fm.refresh().unwrap();
    let urls: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .map(|(u, _)| u.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(urls, ["initClient", "refreshClient", "initClient"]);
}

#[test]
fn http_450_reinitialises_once() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Err(Error::Http(450)),
        Ok(fixture("refreshClient")),
    ]);
    fm.refresh().unwrap();
    assert_eq!(fm.refresh().unwrap().len(), 4);
    let urls: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .map(|(u, _)| u.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(urls, ["initClient", "refreshClient", "initClient"]);
}

#[test]
fn refreshes_feed_history_only_when_moved() {
    let (mut fm, _log) = client(vec![
        Ok(fixture("initClient")),
        Ok(fixture("refreshClient")),
    ]);
    let history = History::open_in_memory().unwrap();

    let first = fm.refresh().unwrap();
    // phone + mac; the watch's fix is old and the AirPods have none.
    assert_eq!(history.record_devices(&first).unwrap(), 2);

    let second = fm.refresh().unwrap();
    // The phone walked ~400 m; the Mac wobbled 10 m inside its 65 m accuracy.
    assert_eq!(history.record_devices(&second).unwrap(), 1);

    let phone = &second[0];
    let trail = history.trail(&phone.id, 0).unwrap();
    assert_eq!(trail.len(), 2);
    assert!(trail[0].ts < trail[1].ts);
    assert_eq!(trail[1].battery, Some(0.81));
    assert_eq!(history.trail(&second[1].id, 0).unwrap().len(), 1);
}
