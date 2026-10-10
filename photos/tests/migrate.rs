//! The one-time move of the library from `<Pictures>/icloud-photos` (the
//! old default) straight into `<Pictures>`.

mod support;

use std::path::{Path, PathBuf};

use icloud_photos::catalog::{Catalog, PathKind};
use icloud_photos::cloudkit::CloudKit;
use icloud_photos::config::{Dirs, DownloadMode, Settings};
use icloud_photos::migrate::{self, MOVED_KEY};
use icloud_photos::sync::sync;
use support::{FixtureTransport, library, temp_dir};

/// A synced catalog under `root`, and `root/Pictures`.
fn setup(root: &Path) -> (Dirs, PathBuf) {
    let dirs = Dirs::under(root);
    let mut cat = Catalog::open(&dirs.catalog()).unwrap();
    sync(
        &CloudKit::connect(&FixtureTransport::new(library)).unwrap(),
        &mut cat,
        &|_| {},
    )
    .unwrap();
    let pictures = root.join("Pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    (dirs, pictures)
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A downloaded original, on disk and recorded in the catalog.
fn downloaded(dirs: &Dirs, id: &str, kind: PathKind, path: &Path, bytes: &[u8]) {
    write(path, bytes);
    Catalog::open(&dirs.catalog())
        .unwrap()
        .set_path(id, kind, Some(path))
        .unwrap();
}

fn paths(dirs: &Dirs, id: &str) -> (Option<PathBuf>, Option<PathBuf>) {
    let row = Catalog::open(&dirs.catalog()).unwrap().asset(id).unwrap().unwrap();
    (row.local_path, row.live_path)
}

fn save(dirs: &Dirs, library_dir: PathBuf) {
    Settings {
        library_dir,
        download: DownloadMode::All,
    }
    .save(dirs)
    .unwrap();
}

#[test]
fn the_old_default_library_moves_into_pictures_with_its_catalog_paths() {
    let tmp = temp_dir("migrate-default");
    let (dirs, pictures) = setup(tmp.path());
    let old = pictures.join("icloud-photos");
    downloaded(
        &dirs,
        "ASSET-001",
        PathKind::Original,
        &old.join("2025/09/IMG_0001.HEIC"),
        b"heic",
    );
    downloaded(
        &dirs,
        "ASSET-001",
        PathKind::Live,
        &old.join("2025/09/IMG_0001.MOV"),
        b"mov",
    );
    write(&pictures.join("screenshot-2026-10-10.png"), b"png");

    let settings = migrate::run(&dirs, &pictures);

    assert_eq!(settings.library_dir, pictures);
    assert!(!old.exists());
    assert_eq!(std::fs::read(pictures.join("2025/09/IMG_0001.HEIC")).unwrap(), b"heic");
    assert_eq!(
        paths(&dirs, "ASSET-001"),
        (
            Some(pictures.join("2025/09/IMG_0001.HEIC")),
            Some(pictures.join("2025/09/IMG_0001.MOV"))
        )
    );
    // The user's own files are not touched, and no settings file appears.
    assert_eq!(
        std::fs::read(pictures.join("screenshot-2026-10-10.png")).unwrap(),
        b"png"
    );
    assert!(!dirs.settings().exists());
    let cat = Catalog::open(&dirs.catalog()).unwrap();
    assert!(cat.meta(MOVED_KEY).unwrap().is_some());
    // A second start finds nothing to do.
    assert_eq!(migrate::run(&dirs, &pictures).library_dir, pictures);
    assert_eq!(
        paths(&dirs, "ASSET-001").0,
        Some(pictures.join("2025/09/IMG_0001.HEIC"))
    );
}

#[test]
fn a_saved_old_default_is_moved_and_rewritten() {
    let tmp = temp_dir("migrate-saved");
    let (dirs, pictures) = setup(tmp.path());
    let old = pictures.join("icloud-photos");
    downloaded(
        &dirs,
        "ASSET-001",
        PathKind::Original,
        &old.join("2025/09/IMG_0001.HEIC"),
        b"heic",
    );
    save(&dirs, old.clone());

    let settings = migrate::run(&dirs, &pictures);

    assert_eq!(settings.library_dir, pictures);
    assert_eq!(settings.download, DownloadMode::All);
    assert_eq!(Settings::saved(&dirs), Some(settings));
    assert!(!old.exists());
    assert_eq!(
        paths(&dirs, "ASSET-001").0,
        Some(pictures.join("2025/09/IMG_0001.HEIC"))
    );
}

#[test]
fn an_existing_year_folder_is_merged_and_a_collision_left_in_place() {
    let tmp = temp_dir("migrate-merge");
    let (dirs, pictures) = setup(tmp.path());
    let old = pictures.join("icloud-photos");
    // The user's own 2025 folder, with a file of the same name as a download.
    write(&pictures.join("2025/09/IMG_0001.HEIC"), b"mine");
    write(&pictures.join("2025/12/party.jpg"), b"party");
    downloaded(
        &dirs,
        "ASSET-001",
        PathKind::Original,
        &old.join("2025/09/IMG_0001.HEIC"),
        b"heic",
    );
    downloaded(
        &dirs,
        "ASSET-002",
        PathKind::Original,
        &old.join("2025/09/IMG_0002.JPG"),
        b"jpg",
    );
    write(&old.join("2024/01/IMG_0100.JPG"), b"old");

    assert_eq!(migrate::run(&dirs, &pictures).library_dir, pictures);

    // Moved: a whole year, and a file into the existing month folder.
    assert_eq!(std::fs::read(pictures.join("2024/01/IMG_0100.JPG")).unwrap(), b"old");
    assert_eq!(std::fs::read(pictures.join("2025/09/IMG_0002.JPG")).unwrap(), b"jpg");
    assert_eq!(paths(&dirs, "ASSET-002").0, Some(pictures.join("2025/09/IMG_0002.JPG")));
    // Nothing replaced: the collision stays where it was, and so does its
    // catalog path; the old folder is kept because it is not empty.
    assert_eq!(std::fs::read(pictures.join("2025/09/IMG_0001.HEIC")).unwrap(), b"mine");
    assert_eq!(std::fs::read(old.join("2025/09/IMG_0001.HEIC")).unwrap(), b"heic");
    assert_eq!(paths(&dirs, "ASSET-001").0, Some(old.join("2025/09/IMG_0001.HEIC")));
    assert_eq!(std::fs::read(pictures.join("2025/12/party.jpg")).unwrap(), b"party");
    assert!(!old.join("2024").exists());

    // The move ran once: what was left is not looked at again.
    write(&old.join("2023/late.jpg"), b"late");
    assert_eq!(migrate::run(&dirs, &pictures).library_dir, pictures);
    assert!(old.join("2023/late.jpg").exists());
    assert!(!pictures.join("2023").exists());
}

#[test]
fn a_library_folder_the_user_chose_is_left_alone() {
    let tmp = temp_dir("migrate-custom");
    let root = tmp.path();
    let dirs = Dirs::under(root);
    let pictures = root.join("Pictures");
    let old = pictures.join("icloud-photos");
    write(&old.join("2025/09/IMG_0001.HEIC"), b"heic");
    let chosen = root.join("Photos");
    save(&dirs, chosen.clone());

    let settings = migrate::run(&dirs, &pictures);

    assert_eq!(settings.library_dir, chosen);
    assert_eq!(Settings::saved(&dirs), Some(settings));
    assert!(old.join("2025/09/IMG_0001.HEIC").exists());
    assert!(!pictures.join("2025").exists());
    assert!(!dirs.catalog().exists());
}
