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

use icloud_photos::catalog::Catalog;
use icloud_photos::migrate::MOVED_KEY;
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

    fn catalog(&self) -> PathBuf {
        self.root.join("data-dir/data/catalog.db")
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

/// The user's own files in the library's month folder: one with the name of
/// the newest photo (another size), one of no photo's name.
fn users_files(month: &Path) -> BTreeMap<PathBuf, Digest> {
    write(&month.join("IMG_1003.JPG"), b"mine");
    write(&month.join("notes.txt"), b"notes");
    files(month)
}

/// Every visible item's original (and a Live Photo's video) is recorded,
/// exists and is of iCloud's size; and the library holds exactly those files
/// beside the user's own.
fn catalog_and_files_agree(env: &Env, users: &BTreeMap<PathBuf, Digest>, month: &Path) {
    let mut recorded = BTreeMap::new();
    for r in env.rows() {
        let photo = PathBuf::from(r["local_path"].as_str().unwrap_or_else(|| panic!("not recorded: {r}")));
        assert_eq!(
            std::fs::metadata(&photo).unwrap().len(),
            r["size"].as_u64().unwrap(),
            "{r}"
        );
        recorded.insert(photo, ());
        if r["kind"] == "live" {
            let video = r["live_path"]
                .as_str()
                .unwrap_or_else(|| panic!("video not recorded: {r}"));
            recorded.insert(PathBuf::from(video), ());
        }
    }
    let on_disk = files(&env.library());
    let ours: Vec<PathBuf> = on_disk
        .keys()
        .map(|p| env.library().join(p))
        .filter(|p| !users.contains_key(p.strip_prefix(month).unwrap_or(p)))
        .collect();
    assert_eq!(
        ours,
        recorded.into_keys().collect::<Vec<_>>(),
        "one file per record, no copies"
    );
    kept(users, &files(month), "the user's files");
}

#[test]
fn a_download_killed_at_any_point_finishes_with_one_copy_of_each() {
    // Three items in one month; the oldest is a Live Photo.
    for point in [
        "download.original.fetched",
        "download.original.fetched:3",
        "download.original.placed",
        "download.original.placed:2",
        "download.live.fetched",
        "download.live.placed",
    ] {
        let env = Env::new("download", 3);
        env.run(&["sync"]).ok();
        let month = env.library().join("2026/09");
        let users = users_files(&month);
        env.crash(point, &["download", "--all"]);
        kept(&users, &files(&month), point);
        let again = env.run(&["download", "--all"]);
        assert_eq!(again.code, Some(0), "{point}: {}\n{}", again.stdout, again.stderr);
        catalog_and_files_agree(&env, &users, &month);
        // Nothing is left to do, and doing it changes nothing.
        let before = files(&env.library());
        assert_eq!(env.run(&["download", "--all"]).ok(), serde_json::json!([]), "{point}");
        for id in env.ids() {
            env.run(&["download", &id]).ok();
        }
        assert_eq!(files(&env.library()), before, "{point}");
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

/// A library downloaded into `~/Pictures`, then put back as an earlier
/// version had it: in `~/Pictures/icloud-photos`, recorded there, not yet
/// moved. The user also has a `2025` folder in the old one and their own
/// files in `~/Pictures/2026/09`, so the move renames `2025` whole and
/// merges the month file by file.
fn old_library(env: &Env) -> (PathBuf, PathBuf) {
    let bare = |args: &[&str]| env.exec(false, args, None).ok();
    bare(&["sync"]);
    bare(&["download", "--all"]);
    let pictures = env.home().join("Pictures");
    let old = pictures.join("icloud-photos");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::rename(pictures.join("2026"), old.join("2026")).unwrap();
    let cat = Catalog::open(&env.home().join("xdg-data/icloud-photos/catalog.db")).unwrap();
    cat.move_paths(&pictures.join("2026"), &old.join("2026")).unwrap();
    cat.set_meta(MOVED_KEY, None).unwrap();
    write(&old.join("2025/07/user.jpg"), b"user");
    write(&pictures.join("2026/09/mine.txt"), b"mine");
    (pictures, old)
}

#[test]
fn a_library_move_killed_at_any_point_keeps_every_file_and_its_record() {
    // Renames: 2025, then the month's four files (a Live Photo has two).
    let points = (1..=5).map(|n| format!("migrate.renamed:{n}"));
    for point in points.chain(["migrate.moved".into(), "migrate.marked".into()]) {
        let env = Env::new("move", 3);
        let (pictures, old) = old_library(&env);
        let before: BTreeMap<PathBuf, Digest> = files(&pictures)
            .into_iter()
            .map(|(p, b)| (p.strip_prefix("icloud-photos").map(Path::to_path_buf).unwrap_or(p), b))
            .collect();
        env.exec(false, &["status"], Some(&point)).crashed(&point);
        env.exec(false, &["status"], None).ok();
        assert_eq!(files(&pictures), before, "{point}: every file, moved");
        assert!(!old.exists(), "{point}");
        let rows = env.exec(false, &["list"], None).ok();
        for r in rows.as_array().unwrap() {
            for key in ["local_path", "live_path"] {
                if r["kind"] == "live" || key == "local_path" {
                    let p = r[key].as_str().unwrap_or_else(|| panic!("{point}: {key} lost: {r}"));
                    assert!(Path::new(p).starts_with(pictures.join("2026/09")), "{point}: {p}");
                }
            }
        }
        let again = env.exec(false, &["download", "--all"], None).ok();
        assert_eq!(again, serde_json::json!([]), "{point}: nothing to fetch again");
        assert_eq!(files(&pictures), before, "{point}");
    }
}

/// The catalog schema of the releases before `live_size`.
const SCHEMA_BEFORE_LIVE_SIZE: &str = "
CREATE TABLE assets(
  id TEXT PRIMARY KEY,
  master_id TEXT NOT NULL,
  filename TEXT NOT NULL,
  created INTEGER NOT NULL,
  size INTEGER NOT NULL DEFAULT 0,
  w INTEGER NOT NULL DEFAULT 0,
  h INTEGER NOT NULL DEFAULT 0,
  kind TEXT NOT NULL DEFAULT 'photo',
  is_live INTEGER NOT NULL DEFAULT 0,
  local_path TEXT NULL,
  live_path TEXT NULL,
  thumb_path TEXT NULL,
  medium_path TEXT NULL,
  deleted INTEGER NOT NULL DEFAULT 0,
  change_tag TEXT NULL,
  orig_url TEXT NULL, orig_type TEXT NULL,
  thumb_url TEXT NULL, medium_url TEXT NULL, live_url TEXT NULL, live_type TEXT NULL
);
CREATE INDEX assets_by_date ON assets(deleted, created DESC);
CREATE INDEX assets_by_master ON assets(master_id);
CREATE TABLE albums(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE album_assets(
  album_id TEXT NOT NULL,
  asset_id TEXT NOT NULL,
  relation_id TEXT NULL,
  PRIMARY KEY(album_id, asset_id)
);
CREATE INDEX album_assets_by_asset ON album_assets(asset_id);
CREATE INDEX album_assets_by_relation ON album_assets(relation_id);
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
";

#[test]
fn a_catalog_from_before_live_size_is_upgraded_and_takes_up_the_files_on_disk() {
    let env = Env::new("upgrade", 3);
    env.run(&["sync"]).ok();
    let month = env.library().join("2026/09");
    let users = users_files(&month);
    env.run(&["download", "--all"]).ok();
    let library = files(&env.library());
    // The same rows in a catalog of the old schema, the downloads' paths
    // lost (a download cut off before its record, or killed mid-way).
    let old = env.root.join("old.db");
    {
        let conn = rusqlite::Connection::open(&old).unwrap();
        conn.execute_batch(SCHEMA_BEFORE_LIVE_SIZE).unwrap();
        conn.execute("ATTACH ?1 AS cur", [env.catalog().to_str().unwrap()])
            .unwrap();
        conn.execute_batch(
            "INSERT INTO assets SELECT id, master_id, filename, created, size, w, h, kind, is_live, NULL, NULL,
               thumb_path, medium_path, deleted, change_tag, orig_url, orig_type, thumb_url, medium_url, live_url,
               live_type FROM cur.assets;
             INSERT INTO albums SELECT * FROM cur.albums;
             INSERT INTO album_assets SELECT * FROM cur.album_assets;
             INSERT INTO meta SELECT * FROM cur.meta;
             DETACH cur;",
        )
        .unwrap();
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", env.catalog().display()));
    }
    std::fs::rename(&old, env.catalog()).unwrap();

    // It opens, gains the column, and the video's size is not known.
    let live = env.rows().into_iter().find(|r| r["kind"] == "live").unwrap();
    let id = live["id"].as_str().unwrap();
    let row = Catalog::open(&env.catalog()).unwrap().asset(id).unwrap().unwrap();
    assert_eq!((row.live_size, row.live_path.is_none()), (0, true));
    assert!(row.size > 0);

    // The originals on disk are taken up, the video too: no " (2)" copies.
    env.run(&["download", "--all"]).ok();
    assert_eq!(files(&env.library()), library);
    catalog_and_files_agree(&env, &users, &month);
}
