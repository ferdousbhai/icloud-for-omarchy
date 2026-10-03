//! The command-line interface, run as the built binary against the fake
//! CloudKit server (`ICLOUD_SESSION_MOCK=1`), every command end to end.
//! HOME and the XDG variables point into a temp directory as well, so a
//! mistake cannot reach the real catalog, cache or ~/Pictures.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use support::fake_server::FakeServer;
use support::temp_dir;

struct Env {
    server: FakeServer,
    root: PathBuf,
    _tmp: tempfile::TempDir,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}\nstderr:\n{}", self.stdout, self.stderr))
    }
}

impl Env {
    fn new(name: &str, count: usize) -> Env {
        let tmp = temp_dir(&format!("cli-{name}"));
        Env {
            server: FakeServer::start(0, count),
            root: tmp.path().to_owned(),
            _tmp: tmp,
        }
    }

    fn data(&self) -> PathBuf {
        self.root.join("data-dir")
    }

    /// The binary with the fake server, a sandboxed HOME/XDG, and no --data-dir.
    fn bare(&self, args: &[&str]) -> Run {
        self.bare_with(args, &[])
    }

    /// `bare` with extra environment variables.
    fn bare_with(&self, args: &[&str], vars: &[(&str, &Path)]) -> Run {
        let home = self.root.join("home");
        let out: Output = Command::new(env!("CARGO_BIN_EXE_icloud-photos"))
            .args(args)
            .envs(vars.iter().map(|(k, v)| (*k, *v)))
            .env("ICLOUD_SESSION_MOCK", "1")
            .env("ICLOUD_SESSION_MOCK_URL", &self.server.url)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join("xdg-data"))
            .env("XDG_CACHE_HOME", home.join("xdg-cache"))
            .env("XDG_CONFIG_HOME", home.join("xdg-config"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .expect("run icloud-photos");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// `icloud-photos --data-dir <root>/data-dir <args>`.
    fn run(&self, args: &[&str]) -> Run {
        let data = self.data();
        let mut all = vec!["--data-dir", data.to_str().unwrap()];
        all.extend_from_slice(args);
        self.bare(&all)
    }

    /// Run with --json, expect exit 0, parse stdout.
    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let r = self.run(&all);
        assert_eq!(r.code, 0, "{args:?} failed:\n{}\n{}", r.stdout, r.stderr);
        r.json()
    }

    fn synced(name: &str, count: usize) -> Env {
        let env = Env::new(name, count);
        let r = env.json(&["sync"]);
        assert_eq!(r["mode"], "full");
        env
    }

    fn ids(&self, args: &[&str]) -> Vec<String> {
        let mut all = vec!["list"];
        all.extend_from_slice(args);
        self.json(&all)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect()
    }
}

fn is_jpeg(p: &Path) -> bool {
    std::fs::read(p).is_ok_and(|b| b.starts_with(b"\xFF\xD8"))
}

#[test]
fn usage_help_and_exit_codes() {
    let env = Env::new("usage", 3);
    let help = env.bare(&["--help"]);
    assert_eq!(help.code, 0);
    for cmd in [
        "status",
        "sync",
        "albums",
        "list",
        "info",
        "thumb",
        "download",
        "upload",
        "delete",
        "open",
        "prune-cache",
        "sign-in",
        "config",
    ] {
        assert!(help.stdout.contains(cmd), "--help lists {cmd}");
        let own = env.bare(&[cmd, "--help"]);
        assert_eq!(own.code, 0, "{cmd} --help");
        assert!(own.stdout.contains("Exit codes"), "{cmd} --help names the exit codes");
    }
    assert_eq!(env.bare(&["--version"]).code, 0);
    assert_eq!(env.bare(&["frobnicate"]).code, 64);
    assert_eq!(env.run(&["list", "--kind", "painting"]).code, 64);
    assert_eq!(env.run(&["list", "--since", "last tuesday"]).code, 64);
    assert_eq!(env.run(&["download"]).code, 64, "an id or --all is required");
    assert_eq!(env.run(&["upload"]).code, 64);

    // Errors: exit 1, and with --json a JSON object on stderr.
    let r = env.run(&["--json", "info", "NO-SUCH-ID"]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.is_empty());
    let err: Value = serde_json::from_str(r.stderr.trim()).unwrap();
    assert_eq!(
        (err["error"]["code"].as_str(), err["error"]["exit_code"].as_i64()),
        (Some("not_found"), Some(1))
    );
    assert!(err["error"]["message"].as_str().unwrap().contains("NO-SUCH-ID"));
    // A usage error with --json is JSON too.
    let r = env.run(&["--json", "list", "--kind", "painting"]);
    assert_eq!(r.code, 64);
    let err: Value = serde_json::from_str(r.stderr.trim()).unwrap();
    assert_eq!(err["error"]["code"], "usage");
    assert_eq!(err["error"]["exit_code"], 64);
    // Delete never asks without a terminal: --yes or a usage error.
    let r = env.run(&["--json", "delete", "NO-SUCH-ID"]);
    assert_eq!(r.code, 1, "the id is checked first: {}", r.stderr);
}

#[test]
fn status_and_sync() {
    let env = Env::new("status", 12);
    let s = env.json(&["status"]);
    assert_eq!(s["signed_in"], true);
    assert_eq!(s["mock"], true);
    assert_eq!(s["catalog_exists"], false);
    assert_eq!(s["last_sync"], Value::Null);
    let data = env.data();
    assert_eq!(
        s["catalog"].as_str().map(PathBuf::from),
        Some(data.join("data/catalog.db"))
    );
    assert_eq!(s["library_dir"].as_str().map(PathBuf::from), Some(data.join("library")));
    assert_eq!(s["cache_dir"].as_str().map(PathBuf::from), Some(data.join("cache")));
    assert!(
        !data.join("data/catalog.db").exists(),
        "status does not create the catalog"
    );

    let r = env.run(&["sync"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.starts_with("Full sync: 12 updated, 0 removed, 3 albums"),
        "{}",
        r.stdout
    );
    let again = env.json(&["sync"]);
    assert_eq!(
        (again["mode"].as_str(), again["assets"].as_i64()),
        (Some("incremental"), Some(0))
    );
    let full = env.json(&["sync", "--full"]);
    assert_eq!(
        (full["mode"].as_str(), full["assets"].as_i64(), full["albums"].as_i64()),
        (Some("full"), Some(12), Some(3))
    );

    let s = env.json(&["status"]);
    assert_eq!(
        (s["assets"].as_i64(), s["albums"].as_i64(), s["downloaded"].as_i64()),
        (Some(12), Some(3), Some(0))
    );
    assert_eq!(s["incremental_sync_ready"], true);
    assert!(s["last_sync"].as_str().is_some_and(|t| t.ends_with('Z')));
    let human = env.run(&["status"]);
    assert!(
        human.stdout.contains("Signed in:     yes [mock]") && human.stdout.contains("Items:         12"),
        "{}",
        human.stdout
    );
}

#[test]
fn albums_list_and_info() {
    let env = Env::synced("list", 30);
    let albums = env.json(&["albums"]);
    let names: Vec<(&str, i64)> = albums
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["name"].as_str().unwrap(), a["count"].as_i64().unwrap()))
        .collect();
    assert_eq!(names, vec![("Trips", 10), ("Family", 6), ("Empty album", 0)]);
    let human = env.run(&["albums"]);
    assert_eq!(human.stdout.lines().next(), Some("ALBUM-TRIPS\t10\tTrips"));

    let all = env.json(&["list"]);
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 30);
    let created: Vec<i64> = all.iter().map(|r| r["created_unix"].as_i64().unwrap()).collect();
    assert!(created.windows(2).all(|w| w[0] >= w[1]), "newest first");
    for key in ["id", "filename", "created", "kind", "size", "local_path"] {
        assert!(all[0].get(key).is_some(), "list has {key}");
    }

    assert_eq!(env.ids(&["--album", "ALBUM-FAMILY"]).len(), 6);
    assert_eq!(env.ids(&["--limit", "4"]).len(), 4);
    // The fake library: every 10th item from #7 is a video, from #2 a Live Photo.
    assert_eq!(env.ids(&["--kind", "video"]).len(), 3);
    assert_eq!(env.ids(&["--kind", "live"]).len(), 3);
    assert_eq!(env.ids(&["--kind", "photo"]).len(), 24);
    let cutoff = all[9]["created"].as_str().unwrap();
    assert_eq!(env.ids(&["--since", cutoff]).len(), 10);
    let day = &cutoff[..10];
    assert!(env.ids(&["--since", day]).len() >= 10);
    assert_eq!(env.run(&["list", "--album", "NOPE"]).code, 1);

    let human = env.run(&["list", "--limit", "1"]);
    let cols: Vec<&str> = human.stdout.trim_end().split('\t').collect();
    assert_eq!(cols.len(), 6, "{}", human.stdout);
    assert_eq!(cols[0], all[0]["id"]);
    assert_eq!(cols[5], "-", "not downloaded");

    let id = all[0]["id"].as_str().unwrap();
    let info = env.json(&["info", id]);
    assert_eq!(info["id"], id);
    assert_eq!(info["deleted"], false);
    let in_albums: Vec<&str> = info["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(in_albums, vec!["Trips", "Family"], "item #0 is in both albums");
    assert!(env.run(&["info", id]).stdout.contains("Albums:    Trips, Family"));
}

#[test]
fn thumb_and_download() {
    let env = Env::synced("download", 4);
    let live = env.ids(&["--kind", "live", "--limit", "1"]).remove(0);
    let photo = env.ids(&["--kind", "photo", "--limit", "1"]).remove(0);

    let t = env.json(&["thumb", &live]);
    let cached = PathBuf::from(t["path"].as_str().unwrap());
    assert!(cached.starts_with(env.data().join("cache/thumbs")) && is_jpeg(&cached));
    let out = env.root.join("exports/t.jpg");
    let r = env.run(&["thumb", &live, "--out", out.to_str().unwrap()]);
    assert_eq!((r.code, r.stdout.trim()), (0, out.to_str().unwrap()));
    assert!(is_jpeg(&out));

    // A Live Photo brings its video; both land in the library folder.
    let r = env.run(&["download", &live]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let paths: Vec<PathBuf> = r.stdout.lines().map(PathBuf::from).collect();
    assert_eq!(paths.len(), 2, "{}", r.stdout);
    assert!(
        paths
            .iter()
            .all(|p| p.starts_with(env.data().join("library")) && p.exists())
    );
    assert!(paths[0].extension().is_some_and(|e| e == "HEIC") && paths[1].extension().is_some_and(|e| e == "MOV"));
    let info = env.json(&["info", &live]);
    assert_eq!(info["local_path"].as_str().map(PathBuf::from).as_ref(), Some(&paths[0]));
    assert_eq!(info["live_path"].as_str().map(PathBuf::from).as_ref(), Some(&paths[1]));
    // Again: already there, same paths, no second copy.
    let again = env.json(&["download", &live]);
    assert_eq!(again[0]["path"].as_str().map(PathBuf::from).as_ref(), Some(&paths[0]));

    // --out DIR puts the original there instead.
    let dir = env.root.join("elsewhere");
    let r = env.json(&["download", &photo, "--out", dir.to_str().unwrap()]);
    let p = PathBuf::from(r[0]["path"].as_str().unwrap());
    assert!(p.starts_with(&dir) && is_jpeg(&p), "{p:?}");
    assert_eq!(r[0]["live_path"], Value::Null);

    // --medium: iCloud's viewer preview, into the cache or to --out.
    let m = env.json(&["download", &photo, "--medium"]);
    let mp = PathBuf::from(m[0]["path"].as_str().unwrap());
    assert!(mp.starts_with(env.data().join("cache/medium")) && is_jpeg(&mp));
    let mdir = env.root.join("medium-out");
    let m = env.json(&["download", &photo, "--medium", "--out", mdir.to_str().unwrap()]);
    assert!(PathBuf::from(m[0]["path"].as_str().unwrap()).starts_with(&mdir));

    // --all fetches every original still missing.
    let all = env.json(&["download", "--all"]);
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(env.json(&["status"])["downloaded"], 4);
    assert_eq!(env.json(&["download", "--all"]).as_array().unwrap().len(), 0);

    assert_eq!(env.run(&["download", "NO-SUCH-ID"]).code, 1);
    assert_eq!(env.run(&["thumb", "NO-SUCH-ID"]).code, 1);
    // Nothing escaped the sandbox.
    assert!(!env.root.join("home/Pictures").exists());
}

#[test]
fn open_downloads_then_hands_the_file_to_the_default_app() {
    let env = Env::synced("open", 4);
    let photo = env.ids(&["--kind", "photo", "--limit", "1"]).remove(0);
    // A stand-in for xdg-open that records what it was given.
    let opened = env.root.join("opened");
    let opener = env.root.join("opener.sh");
    std::fs::write(&opener, format!("#!/bin/sh\necho \"$1\" >> '{}'\n", opened.display())).unwrap();
    std::fs::set_permissions(&opener, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let data = env.data();
    let run = |args: &[&str]| {
        let mut all = vec!["--data-dir", data.to_str().unwrap(), "--json"];
        all.extend_from_slice(args);
        env.bare_with(&all, &[("ICLOUD_PHOTOS_OPENER", &opener)])
    };

    // Not downloaded yet: fetched into the library first, then opened.
    let r = run(&["open", &photo]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let v = r.json();
    let path = PathBuf::from(v["path"].as_str().unwrap());
    assert!(
        path.starts_with(env.data().join("library")) && is_jpeg(&path),
        "{path:?}"
    );
    assert_eq!(v["opened"], true);
    assert_eq!(std::fs::read_to_string(&opened).unwrap().trim(), path.to_str().unwrap());
    assert_eq!(env.json(&["info", &photo])["local_path"], v["path"]);

    // --medium opens the viewer's preview from the cache.
    let r = run(&["open", &photo, "--medium"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(PathBuf::from(r.json()["path"].as_str().unwrap()).starts_with(env.data().join("cache/medium")));

    // An opener that fails is an error; an unknown id is not_found.
    let r = env.bare_with(
        &["--data-dir", data.to_str().unwrap(), "--json", "open", &photo],
        &[("ICLOUD_PHOTOS_OPENER", Path::new("/bin/false"))],
    );
    assert_eq!(r.code, 1);
    let r = run(&["open", "NO-SUCH-ID"]);
    let err: Value = serde_json::from_str(r.stderr.trim()).unwrap();
    assert_eq!((r.code, err["error"]["code"].as_str()), (1, Some("not_found")));
}

#[test]
fn upload_then_sync_into_the_catalog() {
    let env = Env::synced("upload", 10);
    let a = env.root.join("NEW_A.JPG");
    let b = env.root.join("NEW_B.mov");
    std::fs::write(&a, b"\xFF\xD8 a pretend jpeg").unwrap();
    std::fs::write(&b, b"a pretend movie").unwrap();
    let txt = env.root.join("notes.txt");
    std::fs::write(&txt, b"not a photo").unwrap();

    let r = env.run(&[
        "upload",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--album",
        "ALBUM-EMPTY",
    ]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("[1/2] sending NEW_A.JPG") && r.stderr.contains("[2/2]"),
        "progress on stderr: {}",
        r.stderr
    );
    let ids: Vec<&str> = r.stdout.lines().take(2).collect();
    assert!(ids.iter().all(|id| id.contains("-UPLD-")), "{}", r.stdout);
    assert!(r.stdout.contains("Incremental sync: 2 updated"), "{}", r.stdout);

    // Synced in: the catalog has them, in the album too.
    assert_eq!(env.json(&["info", ids[0]])["filename"], "NEW_A.JPG");
    assert_eq!(env.json(&["info", ids[1]])["kind"], "video");
    assert_eq!(env.ids(&[]).len(), 12);
    let mut in_album = env.ids(&["--album", "ALBUM-EMPTY"]);
    in_album.sort();
    let mut want: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(in_album, want);

    // The same bytes again: a duplicate, same id, exit 0. An unsupported file
    // alongside is reported and fails the command.
    let r = env.run(&["--json", "upload", a.to_str().unwrap(), txt.to_str().unwrap()]);
    assert_eq!(r.code, 1);
    let v = r.json();
    assert_eq!(
        (v["uploaded"].as_i64(), v["duplicates"].as_i64(), v["failed"].as_i64()),
        (Some(0), Some(1), Some(1))
    );
    let files = v["files"].as_array().unwrap();
    assert!(files.iter().any(|f| f["asset_id"] == ids[0] && f["duplicate"] == true));
    assert!(
        files
            .iter()
            .any(|f| f["error"].is_string() && f["file"].as_str().is_some_and(|p| p.ends_with("notes.txt")))
    );

    // --no-sync leaves the catalog alone.
    let c = env.root.join("NEW_C.png");
    std::fs::write(&c, b"\x89PNG pretend").unwrap();
    let v = env.json(&["upload", c.to_str().unwrap(), "--no-sync"]);
    assert_eq!(v["sync"], Value::Null);
    assert_eq!(env.ids(&[]).len(), 12);
    assert_eq!(env.json(&["sync"])["assets"], 1);

    assert_eq!(env.run(&["upload", a.to_str().unwrap(), "--album", "NOPE"]).code, 1);
}

#[test]
fn delete_asks_refuses_or_moves_to_recently_deleted() {
    let env = Env::synced("delete", 10);
    let ids = env.ids(&[]);
    let (one, two, three) = (&ids[0], &ids[1], &ids[2]);

    // stdin is not a terminal here: without --yes, nothing happens.
    let r = env.run(&["delete", one]);
    assert_eq!(r.code, 64);
    assert!(r.stderr.contains("--yes"));
    assert!(env.server.asset_ids().contains(one));

    let r = env.run(&["delete", one, two, "--yes"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout.lines().count(), 2);
    assert!(!env.server.asset_ids().contains(one) && !env.server.asset_ids().contains(two));
    assert_eq!(env.ids(&[]).len(), 8, "gone from the catalog at once");
    assert_eq!(env.json(&["info", one])["deleted"], true);
    assert_eq!(env.run(&["delete", one, "--yes"]).code, 1, "already deleted");

    // Edited on another device since the last sync: the stale change tag
    // conflicts, so it syncs and deletes with the fresh one.
    env.server.edit_elsewhere(three);
    let v = env.json(&["delete", three, "--yes"]);
    assert_eq!(v[0]["deleted"], true, "{v}");
    assert!(!env.server.asset_ids().contains(three));

    // Deleted elsewhere first: reported, exit 1.
    let four = &ids[3];
    env.server.delete_elsewhere(four);
    let r = env.run(&["--json", "delete", four, "--yes"]);
    assert_eq!(r.code, 1);
    assert_eq!(r.json()[0]["deleted"], false);

    assert_eq!(env.run(&["delete", "NO-SUCH-ID", "--yes"]).code, 1);
    assert_eq!(env.json(&["sync"])["mode"], "incremental");
    assert_eq!(env.ids(&[]).len(), 6);
}

#[test]
fn prune_cache_drops_renditions_of_deleted_items() {
    let env = Env::synced("prune", 5);
    let ids = env.ids(&[]);
    let thumb = PathBuf::from(env.json(&["thumb", &ids[0]])["path"].as_str().unwrap());
    let medium = PathBuf::from(
        env.json(&["download", &ids[0], "--medium"])[0]["path"]
            .as_str()
            .unwrap(),
    );
    env.json(&["thumb", &ids[1]]);
    assert_eq!(env.json(&["prune-cache"])["removed"], 0);
    env.json(&["delete", &ids[0], "--yes"]);
    assert_eq!(env.json(&["prune-cache"])["removed"], 2);
    assert!(!thumb.exists() && !medium.exists());
    assert_eq!(env.run(&["prune-cache"]).stdout.trim(), "Removed 0 cached file(s)");
}

#[test]
fn signed_out_exits_2_until_sign_in() {
    let env = Env::synced("signin", 4);
    let id = env.ids(&[]).remove(0);
    env.server.sign_out();

    let s = env.run(&["--json", "status"]);
    assert_eq!(s.code, 2);
    assert_eq!(s.json()["signed_in"], false);
    for args in [
        vec!["sync"],
        vec!["sync", "--full"],
        vec!["delete", id.as_str(), "--yes"],
    ] {
        let r = env.run(&args);
        assert_eq!(r.code, 2, "{args:?}: {}", r.stderr);
        assert!(r.stderr.contains("sign in"), "{}", r.stderr);
    }
    let r = env.run(&["--json", "sync"]);
    let err: Value = serde_json::from_str(r.stderr.trim()).unwrap();
    assert_eq!(err["error"]["code"], "sign_in_required");
    assert_eq!(err["error"]["exit_code"], 2);
    let f = env.root.join("X.JPG");
    std::fs::write(&f, b"\xFF\xD8 x").unwrap();
    assert_eq!(env.run(&["upload", f.to_str().unwrap()]).code, 2);
    // Local-only commands still work, and so do content downloads (signed
    // URLs on Apple's content hosts carry no session).
    assert_eq!(env.run(&["list"]).code, 0);
    assert_eq!(env.run(&["thumb", &id]).code, 0);
    assert_eq!(env.run(&["prune-cache"]).code, 0);

    assert_eq!(env.run(&["sign-in"]).code, 0);
    assert_eq!(env.json(&["status"])["signed_in"], true);
    assert_eq!(env.run(&["sync"]).code, 0);
}

#[test]
fn config_reads_and_writes_the_preferences() {
    let env = Env::new("config", 3);
    let c = env.json(&["config"]);
    assert_eq!(c["download"], "on-demand");
    assert_eq!(c["saved"], false);
    let lib = env.root.join("my-library");
    let c = env.json(&["config", "--library-dir", lib.to_str().unwrap(), "--download", "all"]);
    assert_eq!(
        (c["download"].as_str(), c["saved"].as_bool()),
        (Some("all"), Some(true))
    );
    assert!(env.data().join("config/settings.json").exists());
    let s = env.json(&["status"]);
    assert_eq!(s["library_dir"].as_str().map(PathBuf::from), Some(lib.clone()));
    assert_eq!(s["download_mode"], "all");
    env.json(&["sync"]);
    let id = env.ids(&["--limit", "1"]).remove(0);
    let d = env.json(&["download", &id]);
    assert!(d[0]["path"].as_str().is_some_and(|p| Path::new(p).starts_with(&lib)));
}

#[test]
fn without_data_dir_it_uses_the_xdg_directories() {
    let env = Env::new("xdg", 3);
    let home = env.root.join("home");
    let r = env.bare(&["--json", "sync"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(home.join("xdg-data/icloud-photos/catalog.db").exists());
    let s: Value = serde_json::from_str(&env.bare(&["--json", "status"]).stdout).unwrap();
    assert_eq!(s["assets"], 3);
    assert_eq!(
        s["library_dir"].as_str().map(PathBuf::from),
        Some(home.join("Pictures/icloud-photos"))
    );
    assert_eq!(
        s["cache_dir"].as_str().map(PathBuf::from),
        Some(home.join("xdg-cache/icloud-photos"))
    );
    let id = serde_json::from_str::<Value>(&env.bare(&["--json", "list", "--limit", "1"]).stdout).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let t: Value = serde_json::from_str(&env.bare(&["--json", "thumb", &id]).stdout).unwrap();
    assert!(
        t["path"]
            .as_str()
            .is_some_and(|p| Path::new(p).starts_with(home.join("xdg-cache/icloud-photos/thumbs")))
    );
    assert!(!env.data().exists());
}

#[test]
fn the_library_sits_in_the_xdg_pictures_directory() {
    let env = Env::new("xdg-pictures", 0);
    let home = env.root.join("home");
    std::fs::create_dir_all(home.join("xdg-config")).unwrap();
    std::fs::write(
        home.join("xdg-config/user-dirs.dirs"),
        "XDG_PICTURES_DIR=\"$HOME/Photos\"\n",
    )
    .unwrap();
    let s: Value = serde_json::from_str(&env.bare(&["--json", "status"]).stdout).unwrap();
    assert_eq!(
        s["library_dir"].as_str().map(PathBuf::from),
        Some(home.join("Photos/icloud-photos"))
    );
}
