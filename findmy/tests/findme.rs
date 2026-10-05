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
        self.log.lock().unwrap().push((url.to_string(), body.clone()));
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
    let (mut fm, log) = client(vec![Ok(fixture("initClient")), Ok(fixture("refreshClient"))]);

    let devices = fm.refresh(false).unwrap();
    assert_eq!(devices.len(), 4);
    let devices2 = fm.refresh(true).unwrap();
    assert_eq!(devices2.len(), 4);

    let log = log.lock().unwrap();
    assert_eq!(log[0].0, format!("{ROOT}/fmipservice/client/web/initClient"));
    assert_eq!(log[1].0, format!("{ROOT}/fmipservice/client/web/refreshClient"));

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

/// The app's first load and its load after a sign-in pass `locate` to a
/// fresh client: `initClient` cannot ask devices to report, so a locating
/// `refreshClient` follows it, and its devices are the answer.
#[test]
fn locate_on_a_fresh_session_inits_then_locates() {
    let (mut fm, log) = client(vec![Ok(fixture("initClient")), Ok(fixture("refreshClient"))]);

    let devices = fm.refresh(true).unwrap();
    // The refreshClient fixture's iPhone has walked from the initClient one.
    let init_phone = findme::parse_response(fixture("initClient")).unwrap().devices.remove(0);
    assert_ne!(devices[0].location, init_phone.location);

    let log = log.lock().unwrap();
    assert_eq!(endpoints_of(&log), ["initClient", "refreshClient"]);
    assert!(log[0].1["clientContext"].get("shouldLocate").is_none());
    let asked = &log[1].1;
    assert_eq!(asked["clientContext"]["shouldLocate"], true);
    assert_eq!(asked["isUpdatingAllLocations"], true);
    assert_eq!(asked["serverContext"]["prsId"], 12345678901_i64);
}

/// After a reset (a new sign-in) the next locate inits and locates again.
#[test]
fn locate_after_reset_inits_then_locates() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Ok(fixture("initClient")),
        Ok(fixture("refreshClient")),
    ]);
    fm.refresh(false).unwrap();
    fm.reset();
    fm.refresh(true).unwrap();
    let log = log.lock().unwrap();
    assert_eq!(endpoints_of(&log), ["initClient", "initClient", "refreshClient"]);
    assert_eq!(log[2].1["clientContext"]["shouldLocate"], true);
}

/// An `initClient` that returns no server context has nothing to locate
/// with: its devices are the answer, with no second request.
#[test]
fn locate_without_a_server_context_stops_after_init() {
    let mut init = fixture("initClient");
    init.as_object_mut().unwrap().remove("serverContext");
    let (mut fm, log) = client(vec![Ok(init)]);
    assert_eq!(fm.refresh(true).unwrap().len(), 4);
    assert_eq!(endpoints(&log), ["initClient"]);
}

fn endpoints_of(log: &[(String, Value)]) -> Vec<String> {
    log.iter()
        .map(|(u, _)| u.rsplit('/').next().unwrap().to_string())
        .collect()
}

#[test]
fn parses_devices() {
    let snap = findme::parse_response(fixture("initClient")).unwrap();
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
    assert_eq!(pods.battery, None, "batteryStatus Unknown hides the 0.0 level");
    assert_eq!(pods.location, None);
    assert!(!pods.can_lost_mode);

    assert!(phone.summary(1_790_000_300_000).contains("82%"));
    assert!(phone.summary(1_790_000_300_000).ends_with("5 min ago"));
}

#[test]
fn empty_content_is_no_devices() {
    let snap = findme::parse_response(serde_json::json!({"serverContext": {"a": 1}})).unwrap();
    assert!(snap.devices.is_empty());
    assert!(findme::parse_response(serde_json::json!([1])).is_err());
    assert!(findme::parse_response(serde_json::json!({"content": 3})).is_err());
}

#[test]
fn play_sound_and_lost_mode_payloads() {
    let snap = findme::parse_response(fixture("initClient")).unwrap();
    let phone = &snap.devices[0];
    let (mut fm, log) = client(vec![Ok(fixture("playSound")), Ok(fixture("lostDevice"))]);

    fm.play_sound(phone).unwrap();
    fm.lost_mode(phone, "+358401234567", "Lost, please call").unwrap();

    let log = log.lock().unwrap();
    assert_eq!(log[0].0, format!("{ROOT}/fmipservice/client/web/playSound"));
    assert_eq!(log[0].1["device"], phone.id.as_str());
    assert_eq!(log[0].1["subject"], "Find My iPhone Alert");
    assert_eq!(log[0].1["clientContext"]["fmly"], true);

    assert_eq!(log[1].0, format!("{ROOT}/fmipservice/client/web/lostDevice"));
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
    let snap = findme::parse_response(fixture("initClient")).unwrap();
    let pods = &snap.devices[3];
    let (mut fm, log) = client(vec![]);
    assert!(matches!(fm.lost_mode(pods, "1", "x"), Err(Error::Unsupported(_))));
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
    fm.refresh(false).unwrap();
    assert!(matches!(fm.refresh(false), Err(Error::SignInRequired)));
    fm.refresh(false).unwrap();
    let urls: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .map(|(u, _)| u.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(urls, ["initClient", "refreshClient", "initClient"]);
}

fn endpoints(log: &Log) -> Vec<String> {
    endpoints_of(&log.lock().unwrap())
}

#[test]
fn http_500_reinitialises_once() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Err(Error::Http(500)),
        Ok(fixture("refreshClient")),
    ]);
    fm.refresh(false).unwrap();
    assert_eq!(fm.refresh(false).unwrap().len(), 4);
    assert_eq!(endpoints(&log), ["initClient", "refreshClient", "initClient"]);
}

/// The HTTP 500 retry starts over with `initClient`, and a locate still
/// locates: a locating `refreshClient` follows.
#[test]
fn http_500_retry_still_locates() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Err(Error::Http(500)),
        Ok(fixture("initClient")),
        Ok(fixture("refreshClient")),
    ]);
    fm.refresh(false).unwrap();
    fm.refresh(true).unwrap();
    assert_eq!(
        endpoints(&log),
        ["initClient", "refreshClient", "initClient", "refreshClient"]
    );
    assert_eq!(log.lock().unwrap()[3].1["clientContext"]["shouldLocate"], true);
}

/// A 450 is Find My asking for the password again: surfaced at once, with
/// no retry from initClient (which would only answer 450 again), and the
/// next refresh after authorizing starts over with initClient.
#[test]
fn find_my_auth_required_surfaces_without_retrying() {
    let (mut fm, log) = client(vec![
        Err(Error::FindMyAuthRequired),
        Ok(fixture("initClient")),
        Err(Error::FindMyAuthRequired),
        Ok(fixture("initClient")),
    ]);
    assert!(matches!(fm.refresh(false), Err(Error::FindMyAuthRequired)));
    assert_eq!(endpoints(&log), ["initClient"]);
    fm.refresh(false).unwrap();
    assert!(matches!(fm.refresh(false), Err(Error::FindMyAuthRequired)));
    assert_eq!(endpoints(&log), ["initClient", "initClient", "refreshClient"]);
    fm.refresh(false).unwrap();
    assert_eq!(
        endpoints(&log),
        ["initClient", "initClient", "refreshClient", "initClient"]
    );
}

#[test]
fn session_errors_map_to_find_my_errors() {
    assert!(matches!(
        Error::from(icloud_session::Error::FindMyAuthRequired),
        Error::FindMyAuthRequired
    ));
    assert!(matches!(
        Error::from(icloud_session::Error::SignInRequired),
        Error::SignInRequired
    ));
    assert!(matches!(
        Error::from(icloud_session::Error::Http {
            status: 450,
            body: String::new()
        }),
        Error::Http(450)
    ));
}

#[test]
fn refreshes_feed_history_only_when_moved() {
    let (mut fm, _log) = client(vec![Ok(fixture("initClient")), Ok(fixture("refreshClient"))]);
    let history = History::open_in_memory().unwrap();

    let first = fm.refresh(false).unwrap();
    // "Now" is the fixture's time, so retention keeps its fixes.
    let now = first[0].location.unwrap().ts_ms / 1000;
    // phone + mac; the watch's fix is old and the AirPods have none.
    assert_eq!(history.record_devices(&first, now).unwrap(), 2);

    let second = fm.refresh(true).unwrap();
    // The phone walked ~400 m; the Mac wobbled 10 m inside its 65 m accuracy.
    assert_eq!(history.record_devices(&second, now).unwrap(), 1);

    let phone = &second[0];
    let trail = history.trail(&phone.id, 0).unwrap();
    assert_eq!(trail.len(), 2);
    assert!(trail[0].ts < trail[1].ts);
    assert_eq!(trail[1].battery, Some(0.81));
    assert_eq!(history.trail(&second[1].id, 0).unwrap().len(), 1);
}

#[test]
fn periodic_refresh_does_not_ask_devices_to_locate() {
    let (mut fm, log) = client(vec![
        Ok(fixture("initClient")),
        Ok(fixture("refreshClient")),
        Ok(fixture("refreshClient")),
        Ok(fixture("refreshClient")),
    ]);
    // First load (initClient, then a locating refreshClient) and a timer
    // tick, then the user presses refresh.
    fm.refresh(true).unwrap();
    fm.refresh(false).unwrap();
    fm.refresh(true).unwrap();

    let log = log.lock().unwrap();
    assert_eq!(log[1].1["clientContext"]["shouldLocate"], true);
    let tick = &log[2].1;
    assert!(log[2].0.ends_with("/refreshClient"));
    assert!(tick["clientContext"].get("shouldLocate").is_none());
    assert!(tick["clientContext"].get("selectedDevice").is_none());
    assert!(tick.get("isUpdatingAllLocations").is_none());
    assert_eq!(tick["serverContext"]["prsId"], 12345678901_i64);

    let asked = &log[3].1;
    assert_eq!(asked["clientContext"]["shouldLocate"], true);
    assert_eq!(asked["isUpdatingAllLocations"], true);
}

/// Counts `Transport::reset` calls.
struct Resettable {
    resets: Arc<Mutex<usize>>,
}

impl Transport for Resettable {
    fn service_root(&mut self) -> findme::Result<String> {
        Ok(ROOT.into())
    }
    fn post_json(&mut self, _url: &str, _body: &Value) -> findme::Result<Value> {
        Ok(fixture("initClient"))
    }
    fn reset(&mut self) {
        *self.resets.lock().unwrap() += 1;
    }
}

#[test]
fn reset_forgets_the_transport_session() {
    let resets = Arc::new(Mutex::new(0));
    let mut fm = FindMe::new(Resettable { resets: resets.clone() });
    fm.refresh(true).unwrap();
    assert_eq!(*resets.lock().unwrap(), 0);
    fm.reset();
    assert_eq!(*resets.lock().unwrap(), 1);
}
