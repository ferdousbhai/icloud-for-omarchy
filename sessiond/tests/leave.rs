//! `icloud-session leave --dry-run`, run as the built binary (linked as
//! `icloud-session`) with stand-ins for the apps and Omacloud on its PATH and
//! HOME in a temp directory: each app's state, nothing changed.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn dry_run_lists_each_app_and_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, home) = (tmp.path().join("bin"), tmp.path().join("home"));
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_icloud-sessiond"), bin.join("icloud-session")).unwrap();
    for app in ["icloud-notes", "icloud-photos"] {
        script(&bin.join(app), "exit 1");
    }
    // Photos is in use (a catalog); Notes isn't (no cloned vault)
    let data = home.join(".local/share");
    std::fs::create_dir_all(data.join("icloud-photos")).unwrap();
    std::fs::write(data.join("icloud-photos/catalog.db"), b"").unwrap();
    std::fs::create_dir_all(home.join("Pictures")).unwrap();
    script(
        &bin.join("omacloud"),
        &format!(
            "echo '{{\"ok\":true,\"data\":{{\"set_up\":true,\"folder\":\"{h}\",\"folders\":{{\"synced\":[{{\"name\":\"Pictures\",\"path\":\"{h}/Pictures\"}}]}}}}}}'",
            h = home.display()
        ),
    );
    let out = Command::new(bin.join("icloud-session"))
        .args(["--quiet", "leave", "--dry-run"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let state = |app: &str| {
        v["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["app"] == app)
            .map(|a| a["state"].as_str().unwrap().to_owned())
            .unwrap()
    };
    assert_eq!(state("notes"), "not_used");
    assert_eq!(state("reminders"), "not_installed");
    assert_eq!(state("drive"), "not_installed");
    assert_eq!(state("photos"), "ready");
    let photos = v["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["app"] == "photos")
        .unwrap();
    assert_eq!(photos["folder"], home.join("Pictures").to_str().unwrap());
    assert_eq!(photos["name"], "Photos");
}
