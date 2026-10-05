use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use icloud_findmy::history::{
    History, LazyHistory, MOVE_THRESHOLD_M, PRUNE_EVERY_SECS, REOPEN_AFTER, RETENTION_SECS, distance_m,
};
use icloud_findmy::models::{Device, DeviceClass, Fix};

fn fix(lat: f64, lon: f64, accuracy: f64, ts: i64) -> Fix {
    Fix {
        lat,
        lon,
        accuracy,
        ts_ms: ts * 1000,
        is_old: false,
    }
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
    assert!(
        !h.record("a", &fix(north(MOVE_THRESHOLD_M - 0.01), 25.0, 5.0, 170), None)
            .unwrap()
    );
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
    // A precise fix 60 m away still sits inside the old fix's 100 m radius.
    assert!(!h.record("a", &fix(north(60.0), 25.0, 5.0, 300), None).unwrap());
    // 150 m is past both radii: moved.
    assert!(h.record("a", &fix(north(150.0), 25.0, 5.0, 400), None).unwrap());
}

#[test]
fn gps_wifi_wobble_on_a_desk_is_not_movement() {
    let h = History::open_in_memory().unwrap();
    h.record("a", &fix(60.0, 25.0, 5.0, 100), None).unwrap();
    // Alternating a 5 m GPS fix and a 65 m Wi-Fi fix about 50 m apart.
    for (i, (m, acc)) in [(50.0, 65.0), (0.0, 5.0), (50.0, 65.0), (0.0, 5.0)]
        .into_iter()
        .enumerate()
    {
        let t = 200 + i as i64 * 60;
        assert!(!h.record("a", &fix(north(m), 25.0, acc, t), None).unwrap());
    }
    assert_eq!(h.trail("a", 0).unwrap().len(), 1);
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
        h.record("a", &fix(north(*m), 25.0, 5.0, 1000 + i as i64 * 60), None)
            .unwrap();
    }
    h.record("b", &fix(10.0, 10.0, 5.0, 1000), None).unwrap();
    assert_eq!(h.trail("a", 0).unwrap().len(), 4);
    assert_eq!(h.trail("a", 1100).unwrap().len(), 2);
    assert_eq!(h.trail("b", 0).unwrap().len(), 1);
    assert!(h.trail("c", 0).unwrap().is_empty());
    assert_eq!(h.last("a").unwrap().unwrap().ts, 1180);
}

fn now() -> i64 {
    icloud_session::time::now_ms() / 1000
}

#[test]
fn persists_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("history.db");
    let t = now() - 60;
    {
        let h = History::open(&path).unwrap();
        h.record("a", &fix(60.0, 25.0, 5.0, t), None).unwrap();
    }
    let h = History::open(&path).unwrap();
    assert_eq!(h.trail("a", 0).unwrap().len(), 1);
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn side(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    p.into()
}

#[test]
fn new_database_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("icloud-findmy");
    let path = data.join("history.db");
    let h = History::open(&path).unwrap();
    h.record("a", &fix(60.0, 25.0, 5.0, now()), None).unwrap();
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&path), 0o600);
    // WAL mode: the side files exist while the connection is open.
    for suffix in ["-wal", "-shm"] {
        assert_eq!(mode(&side(&path, suffix)), 0o600, "{suffix}");
    }
}

#[test]
fn existing_permissions_are_fixed_on_open() {
    use std::fs::Permissions;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("icloud-findmy");
    let path = data.join("history.db");
    {
        let h = History::open(&path).unwrap();
        h.record("a", &fix(60.0, 25.0, 5.0, now()), None).unwrap();
        // Loosen everything the way an older version left it.
        std::fs::set_permissions(&data, Permissions::from_mode(0o755)).unwrap();
        for p in [path.clone(), side(&path, "-wal"), side(&path, "-shm")] {
            std::fs::set_permissions(&p, Permissions::from_mode(0o644)).unwrap();
        }
        let _h2 = History::open(&path).unwrap();
        assert_eq!(mode(&data), 0o700);
        for p in [path.clone(), side(&path, "-wal"), side(&path, "-shm")] {
            assert_eq!(mode(&p), 0o600, "{}", p.display());
        }
    }
}

#[test]
fn prune_drops_rows_past_retention() {
    let h = History::open_in_memory().unwrap();
    let now = 10 * RETENTION_SECS;
    let old = now - RETENTION_SECS - 1;
    h.record("a", &fix(60.0, 25.0, 5.0, old), None).unwrap();
    h.record("a", &fix(north(100.0), 25.0, 5.0, now - RETENTION_SECS), None)
        .unwrap();
    h.record("a", &fix(north(200.0), 25.0, 5.0, now), None).unwrap();
    assert_eq!(h.prune(now).unwrap(), 1);
    let trail = h.trail("a", 0).unwrap();
    assert_eq!(trail.len(), 2);
    assert_eq!(trail[0].ts, now - RETENTION_SECS);
}

#[test]
fn open_and_record_devices_prune() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let now = now();
    let stale = now - RETENTION_SECS - 3600;
    {
        let h = History::open(&path).unwrap();
        h.record("old", &fix(10.0, 10.0, 5.0, stale), None).unwrap();
        h.record("new", &fix(20.0, 20.0, 5.0, now - 60), None).unwrap();
    }
    // Opening drops the stale row, and says so.
    let h = History::open(&path).unwrap();
    assert_eq!(h.pruned_on_open(), 1);
    assert_eq!(h.count().unwrap(), 1);
    assert!(h.trail("old", 0).unwrap().is_empty());
    assert_eq!(h.trail("new", 0).unwrap().len(), 1);

    // A batch of inserts prunes too, but at most once a day: not on the
    // next refresh after the open...
    h.record("old", &fix(10.0, 10.0, 5.0, stale), None).unwrap();
    let phone = Device {
        location: Some(fix(30.0, 30.0, 5.0, now)),
        ..device("phone")
    };
    assert_eq!(h.record_devices(std::slice::from_ref(&phone), now).unwrap(), 1);
    assert_eq!(h.trail("old", 0).unwrap().len(), 1);
    assert_eq!(h.trail("phone", 0).unwrap().len(), 1);

    // ...but on the first one a day after it.
    let later = now + PRUNE_EVERY_SECS;
    assert_eq!(h.record_devices(&[phone], later).unwrap(), 0);
    assert!(h.trail("old", 0).unwrap().is_empty());
    assert_eq!(h.trail("phone", 0).unwrap().len(), 1);
}

#[test]
fn record_devices_writes_every_moved_device() {
    let h = History::open_in_memory().unwrap();
    let now = now();
    let devices: Vec<Device> = (0..5)
        .map(|i| Device {
            location: Some(fix(10.0 + f64::from(i), 20.0, 5.0, now - 60)),
            ..device(&format!("d{i}"))
        })
        .collect();
    assert_eq!(h.record_devices(&devices, now).unwrap(), 5);
    assert_eq!(h.count().unwrap(), 5);
    // The same fixes again: nothing moved, nothing written.
    assert_eq!(h.record_devices(&devices, now).unwrap(), 0);
    assert_eq!(h.count().unwrap(), 5);
}

/// A history that cannot be opened is tried again only after a while, not
/// on every refresh; once it opens it is kept.
#[test]
fn lazy_history_backs_off_after_a_failed_open() {
    let mut lazy = LazyHistory::default();
    let attempts = std::cell::Cell::new(0);
    let fail = || {
        attempts.set(attempts.get() + 1);
        Err(rusqlite::Error::InvalidPath("nowhere".into()))
    };
    let t0 = Instant::now();
    assert!(lazy.get_or_open(t0, fail).is_none());
    assert!(lazy.get_or_open(t0 + Duration::from_secs(60), fail).is_none());
    assert!(lazy.get_or_open(t0 + REOPEN_AFTER - Duration::from_secs(1), fail).is_none());
    assert_eq!(attempts.get(), 1);
    assert!(lazy.get_or_open(t0 + REOPEN_AFTER, fail).is_none());
    assert_eq!(attempts.get(), 2);

    let t1 = t0 + REOPEN_AFTER * 2;
    assert!(lazy.get_or_open(t1, History::open_in_memory).is_some());
    assert!(lazy.get().is_some());
    assert!(lazy.get_or_open(t1, fail).is_some());
    assert_eq!(attempts.get(), 2, "an open history is kept");
}

fn device(id: &str) -> Device {
    Device {
        id: id.into(),
        name: id.into(),
        model_name: String::new(),
        class: DeviceClass::Other,
        battery: None,
        charging: false,
        online: true,
        location: None,
        can_play_sound: false,
        can_lost_mode: false,
        lost_mode_enabled: false,
    }
}
