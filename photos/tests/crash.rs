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
//! - `export.live.fetched`, `export.live.placed`:
//!   [`a_live_photo_export_killed_at_any_point_only_ever_adds_its_two_files`]
//! - `sweep.removed`: [`a_sweep_of_temporary_files_killed_part_way_removes_only_old_ones`]
//! - taking up files a lost catalog had (`download.original.placed`,
//!   `download.live.placed`, `download.live.fetched`):
//!   [`originals_taken_up_after_the_catalog_was_lost_are_taken_up_once_when_killed`],
//!   [`a_catalog_from_before_live_size_is_upgraded_and_takes_up_the_files_on_disk`]
//!
//! [`random_sequences_keep_every_original_once_and_the_catalog_in_step`]
//! runs seeded random sequences of downloads, deletes, restores, uploads,
//! catalog loss and changes on another device, a third of the commands
//! killed at a random point and run again, checking after each step that no
//! file is lost, changed or copied, iCloud has each upload once and the
//! library the model expects, and the catalog agrees with both.
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

/// The report of a `delete` or `restore` (`key`) that did every item, the
/// ones of `done` found so already.
fn already(ids: &[&str], key: &str, done: &[bool]) -> Value {
    let items = ids.iter().zip(done);
    items
        .map(|(id, &done)| serde_json::json!({ "id": id, key: true, "already": done }))
        .collect()
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
        // Run again: both were deleted by the run that was killed.
        let r = env.run(&["delete", "--yes", a, b]).ok();
        assert_eq!(r, already(&[a, b], "deleted", &[true, true]), "{point}");
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
    for (point, edited, done) in [
        ("restore.sent", false, [true, false]),
        ("restore.sent:2", false, [true, true]),
        ("restore.retried", true, [true, false]),
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
        // Run again: what the killed run restored is done already.
        let r = env.run(&["restore", a, b]).ok();
        assert_eq!(r, already(&[a, b], "restored", &done), "{point}");
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
    // Also killed while taking up the video, which is told by its bytes.
    for point in [None, Some("download.live.fetched"), Some("download.live.placed")] {
        let env = Env::new("upgrade", 3);
        env.run(&["sync"]).ok();
        let month = env.library().join("2026/09");
        let users = users_files(&month);
        env.run(&["download", "--all"]).ok();
        let library = files(&env.library());
        old_schema_catalog(&env);

        // It opens, gains the column, and the video's size is not known.
        let live = env.rows().into_iter().find(|r| r["kind"] == "live").unwrap();
        let id = live["id"].as_str().unwrap();
        let row = Catalog::open(&env.catalog()).unwrap().asset(id).unwrap().unwrap();
        assert_eq!((row.live_size, row.live_path.is_none()), (0, true));
        assert!(row.size > 0);

        // The originals on disk are taken up, the video too: no " (2)" copies.
        if let Some(point) = point {
            env.crash(point, &["download", "--all"]);
        }
        env.run(&["download", "--all"]).ok();
        assert_eq!(files(&env.library()), library, "{point:?}");
        catalog_and_files_agree(&env, &users, &month);
    }
}

/// The catalog's rows put in a catalog of the schema before `live_size`,
/// the downloads' paths lost (a download cut off before its record, or
/// killed mid-way).
fn old_schema_catalog(env: &Env) {
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
}

/// The catalog deleted: what a sync rebuilds knows no downloads.
fn lose_catalog(env: &Env) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", env.catalog().display()));
    }
    env.run(&["sync"]).ok();
}

#[test]
fn originals_taken_up_after_the_catalog_was_lost_are_taken_up_once_when_killed() {
    for point in [
        "download.original.placed",
        "download.original.placed:3",
        "download.live.placed",
    ] {
        let env = Env::new("adopt", 3);
        env.run(&["sync"]).ok();
        let month = env.library().join("2026/09");
        let users = users_files(&month);
        env.run(&["download", "--all"]).ok();
        let library = files(&env.library());
        lose_catalog(&env);
        env.crash(point, &["download", "--all"]);
        env.run(&["download", "--all"]).ok();
        assert_eq!(files(&env.library()), library, "{point}: nothing downloaded again");
        catalog_and_files_agree(&env, &users, &month);
    }
}

#[test]
fn a_live_photo_export_killed_at_any_point_only_ever_adds_its_two_files() {
    // From iCloud, and copied from the library where it is downloaded.
    for (point, downloaded) in [
        ("export.live.fetched", false),
        ("export.live.placed", false),
        ("export.live.fetched", true),
        ("export.live.placed", true),
    ] {
        let env = Env::new("export-live", 3);
        env.run(&["sync"]).ok();
        let live = env.rows().into_iter().find(|r| r["kind"] == "live").unwrap();
        let id = live["id"].as_str().unwrap();
        if downloaded {
            env.run(&["download", id]).ok();
        }
        let out = env.root.join("export");
        write(&out.join("notes.txt"), b"notes");
        let before = files(&out);
        let args = ["download", "--out", out.to_str().unwrap(), id];
        env.crash(point, &args);
        let crashed = files(&out);
        kept(&before, &crashed, point);
        let item = env.run(&args).ok()[0].clone();
        let after = files(&out);
        kept(&crashed, &after, point);
        assert_eq!(after.len(), before.len() + 2, "{point}: {:?}", after.keys());
        let (photo, video) = (item["path"].as_str().unwrap(), item["live_path"].as_str().unwrap());
        assert!(photo.ends_with(".HEIC") && video.ends_with(".MOV"), "{item}");
        assert_eq!(std::fs::read(video).unwrap(), b"not really a movie", "{point}");
        // And once more: nothing new.
        env.run(&args).ok();
        assert_eq!(files(&out), after, "{point}");
    }
}

/// A download's temporary file, last written two days ago when `stale`.
fn temporary(dir: &Path, n: u8, stale: bool) -> PathBuf {
    let p = dir.join(format!(".icloud-photos-{}.part", format!("{n:x}").repeat(32)));
    write(&p, b"part");
    if stale {
        let two_days = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(two_days)
            .unwrap();
    }
    p
}

#[test]
fn a_sweep_of_temporary_files_killed_part_way_removes_only_old_ones() {
    let env = Env::new("sweep", 3);
    env.run(&["sync"]).ok();
    let month = env.library().join("2026/09");
    let users = users_files(&month);
    let old: Vec<PathBuf> = (1..=3).map(|n| temporary(&month, n, true)).collect();
    // Another process's download, running.
    let running = temporary(&month, 9, false);
    env.crash("sweep.removed:2", &["download", "--all"]);
    assert_eq!(old.iter().filter(|p| p.exists()).count(), 1, "two removed, then killed");
    env.run(&["download", "--all"]).ok();
    // The sweep runs beside the downloads: wait for it to be done.
    for _ in 0..100 {
        if old.iter().all(|p| !p.exists()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(old.iter().all(|p| !p.exists()));
    assert_eq!(std::fs::read(&running).unwrap(), b"part");
    catalog_and_files_agree(&env, &users, &month);
}

// ---- model-based ----------------------------------------------------------

/// A small seeded PRNG (xorshift64*), so a failing case can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Up to `most` distinct items of `from`, at least one if any.
    fn some(&mut self, from: &std::collections::BTreeSet<String>, most: usize) -> Vec<String> {
        let mut all: Vec<String> = from.iter().cloned().collect();
        let mut out = Vec::new();
        for _ in 0..(1 + self.below(most)).min(all.len()) {
            out.push(all.remove(self.below(all.len())));
        }
        out
    }
}

/// What the model expects, and what it has seen.
#[derive(Default)]
struct Model {
    /// In the library in iCloud.
    visible: std::collections::BTreeSet<String>,
    /// Deleted with `delete` and known to the catalog: `restore` can bring them back.
    restorable: std::collections::BTreeSet<String>,
    /// Uploaded files still in iCloud's library, by asset id.
    uploaded: BTreeMap<String, PathBuf>,
    fresh_files: usize,
    /// Every file the library has held: none may go or change.
    seen: BTreeMap<PathBuf, Digest>,
    /// The library's files when the catalog was last lost: these may be
    /// unrecorded (until downloaded again, or for good if not in iCloud).
    unrecorded_ok: std::collections::BTreeSet<PathBuf>,
}

/// The command and the crash points it reaches.
fn points_of(command: &str) -> &'static [&'static str] {
    match command {
        "download" => &[
            "download.original.fetched",
            "download.original.placed",
            "download.live.fetched",
            "download.live.placed",
        ],
        "delete" => &["delete.sent", "delete.retried"],
        "restore" => &["restore.sent", "restore.retried"],
        "upload" => &["upload.reserved", "upload.sent", "upload.registered"],
        _ => &[],
    }
}

/// Run `args`, killed first at a random point of the command a third of
/// the time and then run again, as someone would. The run that finished.
fn run_maybe_killed(env: &Env, rng: &mut Rng, args: &[&str], log: &mut Vec<String>) -> Run {
    let points = points_of(args[0]);
    if !points.is_empty() && rng.below(3) == 0 {
        let point = format!("{}:{}", points[rng.below(points.len())], 1 + rng.below(2));
        log.push(format!("  killed at {point}?"));
        let r = env.exec(true, args, Some(&point));
        if r.code.is_some() {
            log.push("  (not reached)".into());
            return r;
        }
    }
    env.run(args)
}

fn check(env: &Env, m: &mut Model, log: &[String]) {
    let ctx = || log.join("\n");
    // Nothing ever on disk is lost or changed; no second copy of an original.
    let now = files(&env.library());
    for (p, d) in &m.seen {
        assert_eq!(now.get(p), Some(d), "{} lost or changed\n{}", p.display(), ctx());
    }
    for p in now.keys() {
        assert!(
            !p.to_string_lossy().contains(" ("),
            "a copy: {}\n{}",
            p.display(),
            ctx()
        );
    }
    m.seen.extend(now.clone());
    // iCloud: everything uploaded once, and the library the model expects.
    assert_eq!(env.server.total_assets(), 8 + m.fresh_files, "{}", ctx());
    let mut icloud: Vec<String> = env.server.asset_ids();
    icloud.sort();
    assert_eq!(icloud, m.visible.iter().cloned().collect::<Vec<_>>(), "{}", ctx());
    // The catalog, synced, agrees with iCloud and with the files.
    env.run(&["sync"]).ok();
    let mut catalog = env.ids();
    catalog.sort();
    assert_eq!(catalog, icloud, "{}", ctx());
    let cat = Catalog::open(&env.catalog()).unwrap();
    let mut recorded = std::collections::BTreeSet::new();
    // Every row, those of items no longer in the library too.
    let all: Vec<String> = {
        let db = rusqlite::Connection::open(env.catalog()).unwrap();
        let mut st = db.prepare("SELECT id FROM assets").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    for id in &all {
        let row = cat.asset(id).unwrap().unwrap();
        if let Some(p) = &row.local_path {
            let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            assert_eq!(len, row.size as u64, "{id}: {}\n{}", p.display(), ctx());
        }
        for p in [&row.local_path, &row.live_path].into_iter().flatten() {
            assert!(p.exists(), "{id}: {} recorded, not there\n{}", p.display(), ctx());
            assert!(recorded.insert(p.clone()), "{} recorded twice\n{}", p.display(), ctx());
        }
    }
    for p in now.keys() {
        let p = env.library().join(p);
        assert!(
            recorded.contains(&p) || m.unrecorded_ok.contains(&p),
            "{} is no item's\n{}",
            p.display(),
            ctx()
        );
    }
}

#[test]
fn random_sequences_keep_every_original_once_and_the_catalog_in_step() {
    for seed in 1..=6u64 {
        let env = Env::new(&format!("model-{seed}"), 8);
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut m = Model::default();
        env.run(&["sync"]).ok();
        m.visible = env.server.asset_ids().into_iter().collect();
        let mut log = vec![format!("seed {seed}")];
        for _ in 0..14 {
            let op = rng.below(10);
            match op {
                0 | 1 if !m.visible.is_empty() => {
                    let ids = rng.some(&m.visible, 3);
                    log.push(format!("download {ids:?}"));
                    let mut args = vec!["download"];
                    args.extend(ids.iter().map(String::as_str));
                    run_maybe_killed(&env, &mut rng, &args, &mut log).ok();
                }
                2 => {
                    log.push("download --all".into());
                    run_maybe_killed(&env, &mut rng, &["download", "--all"], &mut log).ok();
                }
                3 if !m.visible.is_empty() => {
                    let ids = rng.some(&m.visible, 2);
                    log.push(format!("delete {ids:?}"));
                    let mut args = vec!["delete", "--yes"];
                    args.extend(ids.iter().map(String::as_str));
                    let r = run_maybe_killed(&env, &mut rng, &args, &mut log).ok();
                    for (id, item) in ids.iter().zip(r.as_array().unwrap()) {
                        assert_eq!(item["deleted"], true, "{item}\n{}", log.join("\n"));
                        m.visible.remove(id);
                        m.uploaded.remove(id);
                        m.restorable.insert(id.clone());
                    }
                }
                4 if !m.restorable.is_empty() => {
                    let ids = rng.some(&m.restorable, 2);
                    log.push(format!("restore {ids:?}"));
                    let mut args = vec!["restore"];
                    args.extend(ids.iter().map(String::as_str));
                    let r = run_maybe_killed(&env, &mut rng, &args, &mut log).ok();
                    for (id, item) in ids.iter().zip(r.as_array().unwrap()) {
                        assert_eq!(item["restored"], true, "{item}\n{}", log.join("\n"));
                        m.restorable.remove(id);
                        m.visible.insert(id.clone());
                    }
                }
                5 => {
                    // A new file, or one uploaded before and still there.
                    let again = (rng.below(3) == 0)
                        .then(|| m.uploaded.values().next().cloned())
                        .flatten();
                    let file = again.clone().unwrap_or_else(|| {
                        let n = m.fresh_files;
                        let file = env.root.join(format!("up/up-{seed}-{n}.jpg"));
                        write(
                            &file,
                            format!("\u{FF}\u{D8} photo {seed} {n} {}", rng.next()).as_bytes(),
                        );
                        file
                    });
                    log.push(format!("upload {} (again: {})", file.display(), again.is_some()));
                    let r = run_maybe_killed(&env, &mut rng, &["upload", file.to_str().unwrap()], &mut log).ok();
                    let id = r["files"][0]["asset_id"].as_str().unwrap().to_owned();
                    assert_eq!(r["not_in_catalog"], serde_json::json!([]), "{r}");
                    if again.is_none() {
                        m.fresh_files += 1;
                    }
                    m.visible.insert(id.clone());
                    m.uploaded.insert(id, file);
                }
                6 => {
                    log.push("catalog lost".into());
                    lose_catalog(&env);
                    m.restorable.clear();
                    m.unrecorded_ok = files(&env.library())
                        .into_keys()
                        .map(|p| env.library().join(p))
                        .collect();
                }
                7 if !m.visible.is_empty() => {
                    let id = rng.some(&m.visible, 1).remove(0);
                    log.push(format!("deleted on another device: {id}"));
                    env.server.delete_elsewhere(&id);
                    m.visible.remove(&id);
                    m.uploaded.remove(&id);
                    // Seen there by the next sync, not by this test's.
                    continue;
                }
                8 if !m.visible.is_empty() => {
                    let id = rng.some(&m.visible, 1).remove(0);
                    log.push(format!("edited on another device: {id}"));
                    env.server.edit_elsewhere(&id);
                    continue;
                }
                _ => {
                    log.push("sync".into());
                    env.run(&["sync"]).ok();
                }
            }
            check(&env, &mut m, &log);
        }
        // In the end every item's original is there once, and recorded.
        env.run(&["download", "--all"]).ok();
        check(&env, &mut m, &log);
        assert_eq!(
            env.run(&["download", "--all"]).ok(),
            serde_json::json!([]),
            "{}",
            log.join("\n")
        );
    }
}

#[test]
fn download_all_fetches_again_what_was_recorded_but_is_gone() {
    // Deleted by hand: fetched again, under its own name.
    let env = Env::new("gone", 3);
    env.run(&["sync"]).ok();
    let month = env.library().join("2026/09");
    let users = users_files(&month);
    env.run(&["download", "--all"]).ok();
    let library = files(&env.library());
    let live = env.rows().into_iter().find(|r| r["kind"] == "live").unwrap();
    let (photo, video) = (
        live["local_path"].as_str().unwrap(),
        live["live_path"].as_str().unwrap(),
    );
    std::fs::remove_file(video).unwrap();
    let again = env.run(&["download", "--all"]).ok();
    assert_eq!(again.as_array().unwrap().len(), 1, "{again}");
    assert_eq!(again[0]["live_path"], video, "{again}");
    std::fs::remove_file(photo).unwrap();
    assert_eq!(env.run(&["download", "--all"]).ok()[0]["path"], photo);
    assert_eq!(files(&env.library()), library);
    catalog_and_files_agree(&env, &users, &month);
}

#[test]
fn a_move_killed_with_a_video_of_unknown_size_leaves_it_to_be_taken_up_by_its_bytes() {
    let env = Env::new("move-unknown", 3);
    let (pictures, _old) = old_library(&env);
    let catalog = env.home().join("xdg-data/icloud-photos/catalog.db");
    let db = rusqlite::Connection::open(&catalog).unwrap();
    db.execute("UPDATE assets SET live_size = 0", []).unwrap();
    drop(db);
    let before = files(&pictures);
    // Renames: 2025, IMG_1001.HEIC, then the Live Photo's IMG_1001.MOV.
    env.exec(false, &["status"], Some("migrate.renamed:3"))
        .crashed("migrate.renamed:3");
    env.exec(false, &["status"], None).ok();
    let again = env.exec(false, &["download", "--all"], None).ok();
    assert_eq!(again.as_array().unwrap().len(), 1, "{again}");
    let video = pictures.join("2026/09/IMG_1001.MOV");
    assert_eq!(again[0]["live_path"], video.to_str().unwrap(), "{again}");
    let moved: Vec<PathBuf> = files(&pictures).into_keys().collect();
    let mut was: Vec<PathBuf> = before
        .into_keys()
        .map(|p| p.strip_prefix("icloud-photos").map(Path::to_path_buf).unwrap_or(p))
        .collect();
    was.sort();
    assert_eq!(moved, was, "no second copy");
    assert_eq!(
        env.exec(false, &["download", "--all"], None).ok(),
        serde_json::json!([])
    );
}
