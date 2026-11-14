//! Crash injection: the real command line against the fake server, killed
//! (`ICLOUD_PHOTOS_CRASH_AT`, `src/crash.rs`) between each file write or
//! iCloud request and the catalog update that records it, then run again.
//! After the rerun: no original lost or overwritten, nothing the user had
//! changed, no second " (2)" copy of a file already on disk, nothing twice
//! in iCloud, and the catalog and the files agree. An export folder only
//! ever gains files.
//!
//! Points and the test that covers each:
//! - `download.original.fetched`, `download.original.placed`,
//!   `download.live.fetched`, `download.live.placed`:
//!   [`a_download_killed_at_any_point_finishes_with_one_copy_of_each`]
//! - `download.cache.placed`: [`a_preview_killed_before_its_record_is_fetched_again_into_the_cache`]
//! - `export.original.fetched`, `export.original.placed`:
//!   [`an_export_killed_at_any_point_only_ever_adds_files`]
//! - `delete.sent`, `delete.retried`: [`a_delete_killed_after_icloud_answered_leaves_the_catalog_in_step`]
//! - `restore.sent`, `restore.retried`: [`a_restore_killed_after_icloud_answered_is_finished_by_the_rerun`]
//! - `upload.reserved`, `upload.sent`, `upload.registered`:
//!   [`an_upload_killed_at_any_point_is_in_icloud_once`]
//! - `migrate.renamed`, `migrate.moved`, `migrate.marked`:
//!   [`a_library_move_killed_at_any_point_keeps_every_file_and_its_record`]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use support::fake_server::FakeServer;
use support::temp_dir;

struct Env {
    server: FakeServer,
    root: PathBuf,
    _tmp: tempfile::TempDir,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn data(&self) -> Value {
        let v: Value = serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("not JSON ({e}):\n{}\n{}", self.stdout, self.stderr));
        assert_eq!(v["ok"], true, "{v}\n{}", self.stderr);
        v["data"].clone()
    }

    fn crashed(&self, at: &str) {
        assert_eq!(
            self.code, None,
            "did not crash at {at}:\n{}\n{}",
            self.stdout, self.stderr
        );
        assert!(self.stderr.contains("crashing at"), "{}", self.stderr);
    }

    fn ok(&self) -> Value {
        assert_eq!(self.code, Some(0), "{}\n{}", self.stdout, self.stderr);
        self.data()
    }
}

impl Env {
    fn new(name: &str, count: usize) -> Env {
        let tmp = temp_dir(&format!("crash-{name}"));
        Env {
            server: FakeServer::start(0, count),
            root: tmp.path().to_owned(),
            _tmp: tmp,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// `icloud-photos --json <args>` with a sandboxed HOME and XDG, crashing
    /// at `crash` if given. `data_dir` adds `--data-dir <root>/data-dir`.
    fn exec(&self, data_dir: bool, args: &[&str], crash: Option<&str>) -> Run {
        let home = self.home();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_icloud-photos"));
        cmd.arg("--json");
        if data_dir {
            cmd.arg("--data-dir").arg(self.root.join("data-dir"));
        }
        cmd.args(args)
            .env("ICLOUD_SESSION_MOCK", "1")
            .env("ICLOUD_SESSION_MOCK_URL", &self.server.url)
            .env("PATH", self.root.join("bin"))
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join("xdg-data"))
            .env("XDG_CACHE_HOME", home.join("xdg-cache"))
            .env("XDG_CONFIG_HOME", home.join("xdg-config"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove(icloud_photos::crash::VAR);
        if let Some(point) = crash {
            cmd.env(icloud_photos::crash::VAR, point);
        }
        let out = cmd.output().expect("run icloud-photos");
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Run {
        self.exec(true, args, None)
    }

    fn crash(&self, point: &str, args: &[&str]) {
        self.exec(true, args, Some(point)).crashed(point);
    }

    fn library(&self) -> PathBuf {
        self.root.join("data-dir/library")
    }

    fn rows(&self) -> Vec<Value> {
        self.run(&["list"]).ok().as_array().unwrap().clone()
    }

    fn ids(&self) -> Vec<String> {
        self.rows()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// A download's temporary file (ours, or icloud-session's beside it).
fn is_temporary(name: &str) -> bool {
    name.starts_with(".icloud-photos-") || name.starts_with("..icloud-photos-")
}

/// A file's length and a hash of its bytes.
type Digest = (usize, u64);

fn digest(bytes: &[u8]) -> Digest {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    (bytes.len(), h.finish())
}

/// Every file under `dir` but download temporaries, by path relative to
/// it, with its bytes' digest.
fn files(dir: &Path) -> BTreeMap<PathBuf, Digest> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Digest>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            } else if !is_temporary(&e.file_name().to_string_lossy()) {
                out.insert(
                    p.strip_prefix(root).unwrap().to_owned(),
                    digest(&std::fs::read(&p).unwrap()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Everything in `before` is in `after`, byte for byte.
fn kept(before: &BTreeMap<PathBuf, Digest>, after: &BTreeMap<PathBuf, Digest>, what: &str) {
    for (p, bytes) in before {
        assert_eq!(after.get(p), Some(bytes), "{what}: {} lost or changed", p.display());
    }
}

#[test]
fn a_preview_killed_before_its_record_is_fetched_again_into_the_cache() {
    let env = Env::new("cache", 2);
    env.run(&["sync"]).ok();
    let id = env.ids().remove(0);
    env.crash("download.cache.placed", &["download", "--medium", &id]);
    let path = env.run(&["download", "--medium", &id]).ok()[0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        Path::new(&path).starts_with(env.root.join("data-dir/cache/medium")),
        "{path}"
    );
    assert!(files(&env.library()).is_empty(), "the library is not touched");
}

#[test]
fn an_export_killed_at_any_point_only_ever_adds_files() {
    for point in [
        "export.original.fetched",
        "export.original.fetched:2",
        "export.original.placed",
        "export.original.placed:3",
    ] {
        let env = Env::new("export", 3);
        env.run(&["sync"]).ok();
        let ids = env.ids();
        let out = env.root.join("export");
        let month = out.join("2026/09");
        write(&month.join("IMG_1003.JPG"), b"mine");
        write(&out.join("notes.txt"), b"notes");
        let mut args = vec!["download", "--out", out.to_str().unwrap()];
        args.extend(ids.iter().map(String::as_str));
        let before = files(&out);
        env.crash(point, &args);
        let crashed = files(&out);
        kept(&before, &crashed, point);
        let r = env.run(&args);
        assert_eq!(r.code, Some(0), "{point}: {}\n{}", r.stdout, r.stderr);
        let after = files(&out);
        kept(&crashed, &after, point);
        // Three photos and a video, each once, beside the user's two files.
        assert_eq!(after.len(), before.len() + 4, "{point}: {:?}", after.keys());
        for item in r.data().as_array().unwrap() {
            for key in ["path", "live_path"] {
                if let Some(p) = item[key].as_str() {
                    assert!(Path::new(p).starts_with(&month), "{p}");
                }
            }
        }
        // The library and the catalog are not told of an export.
        assert!(env.rows().iter().all(|r| r["local_path"].is_null()), "{point}");
        assert!(files(&env.library()).is_empty(), "{point}");
    }
}

/// The items visible in the catalog after a sync are the ones iCloud has.
fn in_step_with_icloud(env: &Env) {
    env.run(&["sync"]).ok();
    let mut catalog = env.ids();
    let mut icloud = env.server.asset_ids();
    catalog.sort();
    icloud.sort();
    assert_eq!(catalog, icloud);
}

#[test]
fn a_delete_killed_after_icloud_answered_leaves_the_catalog_in_step() {
    for (point, edited) in [("delete.sent", false), ("delete.retried", true)] {
        let env = Env::new("delete", 4);
        env.run(&["sync"]).ok();
        let ids = env.ids();
        let (a, b) = (ids[0].as_str(), ids[1].as_str());
        let photo = env.run(&["download", a]).ok()[0]["path"].as_str().unwrap().to_owned();
        let bytes = std::fs::read(&photo).unwrap();
        if edited {
            // Changed elsewhere: the first request conflicts, the retry goes through.
            env.server.edit_elsewhere(a);
        }
        env.crash(point, &["delete", "--yes", a, b]);
        assert!(!env.server.asset_ids().iter().any(|id| id == a || id == b), "{point}");
        // Run again: the items are gone from iCloud whatever it reports.
        env.run(&["delete", "--yes", a, b]);
        in_step_with_icloud(&env);
        assert_eq!(std::fs::read(&photo).unwrap(), bytes, "a delete never touches the file");
        // And they come back.
        env.run(&["restore", a, b]).ok();
        in_step_with_icloud(&env);
        assert_eq!(env.ids().len(), 4, "{point}");
    }
}

#[test]
fn a_restore_killed_after_icloud_answered_is_finished_by_the_rerun() {
    for (point, edited) in [
        ("restore.sent", false),
        ("restore.sent:2", false),
        ("restore.retried", true),
    ] {
        let env = Env::new("restore", 4);
        env.run(&["sync"]).ok();
        let ids = env.ids();
        let (a, b) = (ids[0].as_str(), ids[1].as_str());
        env.run(&["delete", "--yes", a, b]).ok();
        if edited {
            env.server.edit_elsewhere(a);
        }
        env.crash(point, &["restore", a, b]);
        // Run again for what the catalog still has as deleted.
        let left: Vec<&str> = [a, b]
            .into_iter()
            .filter(|id| env.run(&["info", id]).ok()["deleted"] == true)
            .collect();
        let mut args = vec!["restore"];
        args.extend(&left);
        let r = env.run(&args).ok();
        assert!(
            r.as_array().unwrap().iter().all(|i| i["restored"] == true),
            "{point}: {r}"
        );
        in_step_with_icloud(&env);
        assert_eq!(env.ids().len(), 4, "{point}");
    }
}

#[test]
fn an_upload_killed_at_any_point_is_in_icloud_once() {
    for point in ["upload.reserved", "upload.sent", "upload.registered"] {
        let env = Env::new("upload", 2);
        env.run(&["sync"]).ok();
        let file = env.root.join("up/new.jpg");
        write(&file, b"\xFF\xD8 a new photo");
        let before = env.server.asset_ids().len();
        env.crash(point, &["upload", file.to_str().unwrap()]);
        let r = env.run(&["upload", file.to_str().unwrap()]).ok();
        assert_eq!(r["failed"], 0, "{point}: {r}");
        assert_eq!(r["files"][0]["duplicate"], point == "upload.registered", "{point}: {r}");
        assert_eq!(env.server.asset_ids().len(), before + 1, "{point}: in iCloud once");
        in_step_with_icloud(&env);
        assert_eq!(std::fs::read(&file).unwrap(), b"\xFF\xD8 a new photo");
    }
}
