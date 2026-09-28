use icloud_findmy::history::{History, MOVE_THRESHOLD_M, distance_m};
use icloud_findmy::models::Fix;

fn fix(lat: f64, lon: f64, accuracy: f64, ts: i64) -> Fix {
    Fix { lat, lon, accuracy, ts_ms: ts * 1000, is_old: false }
}

/// A point `m` metres north of (60, 25).
fn north(m: f64) -> f64 {
    60.0 + m / 111_195.0
}

#[test]
fn haversine_is_sane() {
    assert!(distance_m(60.0, 25.0, 60.0, 25.0) < 1e-6);
    let d = distance_m(60.0, 25.0, north(1000.0), 25.0);
    assert!((d - 1000.0).abs() < 1.0, "{d}");
    // Helsinki to Tallinn is about 80 km.
    let d = distance_m(60.1699, 24.9384, 59.4370, 24.7536);
    assert!((79_000.0..83_000.0).contains(&d), "{d}");
}

#[test]
fn stores_first_fix_then_only_moves_past_threshold() {
    let h = History::open_in_memory().unwrap();
    assert!(h.record("a", &fix(60.0, 25.0, 5.0, 100), Some(0.5)).unwrap());
    // 10 m: inside the threshold.
    assert!(!h.record("a", &fix(north(10.0), 25.0, 5.0, 160), None).unwrap());
    // Exactly at the threshold still counts as not moved.
    assert!(!h.record("a", &fix(north(MOVE_THRESHOLD_M - 0.01), 25.0, 5.0, 170), None).unwrap());
    // 40 m: moved.
    assert!(h.record("a", &fix(north(40.0), 25.0, 5.0, 220), Some(0.4)).unwrap());
    let trail = h.trail("a", 0).unwrap();
    assert_eq!(trail.len(), 2);
    assert_eq!(trail[0].ts, 100);
    assert_eq!(trail[1].battery, Some(0.4));
}

#[test]
fn inaccurate_wobble_is_not_movement() {
    let h = History::open_in_memory().unwrap();
    h.record("a", &fix(60.0, 25.0, 100.0, 100), None).unwrap();
    // 60 m apart, but both fixes are only good to 100 m.
    assert!(!h.record("a", &fix(north(60.0), 25.0, 100.0, 200), None).unwrap());
    // A precise fix 60 m away: the smaller radius (5 m) applies, so it moved.
    assert!(h.record("a", &fix(north(60.0), 25.0, 5.0, 300), None).unwrap());
}

#[test]
fn ignores_stale_or_repeated_timestamps() {
    let h = History::open_in_memory().unwrap();
    h.record("a", &fix(60.0, 25.0, 5.0, 100), None).unwrap();
    assert!(!h.record("a", &fix(north(500.0), 25.0, 5.0, 100), None).unwrap());
    assert!(!h.record("a", &fix(north(500.0), 25.0, 5.0, 50), None).unwrap());
}

#[test]
fn trail_is_per_device_and_since() {
    let h = History::open_in_memory().unwrap();
    for (i, m) in [0.0, 100.0, 200.0, 300.0].iter().enumerate() {
        h.record("a", &fix(north(*m), 25.0, 5.0, 1000 + i as i64 * 60), None).unwrap();
    }
    h.record("b", &fix(10.0, 10.0, 5.0, 1000), None).unwrap();
    assert_eq!(h.trail("a", 0).unwrap().len(), 4);
    assert_eq!(h.trail("a", 1100).unwrap().len(), 2);
    assert_eq!(h.trail("b", 0).unwrap().len(), 1);
    assert!(h.trail("c", 0).unwrap().is_empty());
    assert_eq!(h.last("a").unwrap().unwrap().ts, 1180);
}

#[test]
fn persists_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("history.db");
    {
        let h = History::open(&path).unwrap();
        h.record("a", &fix(60.0, 25.0, 5.0, 100), None).unwrap();
    }
    let h = History::open(&path).unwrap();
    assert_eq!(h.trail("a", 0).unwrap().len(), 1);
}
