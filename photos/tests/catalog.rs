mod support;

use std::collections::HashSet;
use std::path::Path;

use icloud_photos::catalog::{Catalog, PathKind, SCHEMA_VERSION};
use icloud_photos::cloudkit::{Asset, Kind, MasterInfo, Resource};

fn asset(id: &str, created: i64) -> Asset {
    let res = |k: &str| {
        Some(Resource {
            url: format!("https://example.invalid/{k}/{id}"),
            size: 1,
            file_type: None,
        })
    };
    Asset {
        id: id.into(),
        change_tag: None,
        created,
        deleted: false,
        master: MasterInfo {
            master_id: format!("M-{id}"),
            filename: format!("{id}.JPG"),
            size: 1,
            width: 1,
            height: 1,
            kind: Kind::Photo,
            original: res("orig"),
            thumb: res("thumb"),
            medium: res("med"),
            live: None,
        },
    }
}

#[test]
fn path_lookups_use_the_partial_indexes() {
    let cat = Catalog::open_in_memory().unwrap();
    for (sql, index) in [
        (
            "SELECT EXISTS(SELECT 1 FROM assets WHERE local_path = ?1 AND id != ?2)
                 OR EXISTS(SELECT 1 FROM assets WHERE live_path = ?1 AND id != ?2)",
            "assets_by_local_path",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM assets WHERE local_path = ?1 AND id != ?2)
                 OR EXISTS(SELECT 1 FROM assets WHERE live_path = ?1 AND id != ?2)",
            "assets_by_live_path",
        ),
        (
            "UPDATE assets SET medium_path = NULL WHERE medium_path = ?1",
            "assets_by_medium_path",
        ),
    ] {
        let plan = cat.explain(sql).unwrap();
        assert!(plan.contains(index), "{index} not in plan:\n{plan}");
        assert!(!plan.contains("SCAN assets"), "full scan:\n{plan}");
    }
}

#[test]
fn an_old_catalog_is_migrated_once_and_keeps_its_rows() {
    let tmp = support::temp_dir("migrate");
    let path = tmp.path().join("catalog.db");
    {
        // A catalog as the previous release left it: tables, no indexes on paths.
        let cat = Catalog::open(&path).unwrap();
        cat.upsert_asset(&asset("A", 10)).unwrap();
        cat.set_path("A", PathKind::Original, Some(Path::new("/lib/A.JPG")))
            .unwrap();
        drop(cat);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP INDEX assets_by_local_path; DROP INDEX assets_by_live_path;
             DROP INDEX assets_by_medium_path; PRAGMA user_version = 0;",
        )
        .unwrap();
    }
    let cat = Catalog::open(&path).unwrap();
    assert!(cat.path_taken(Path::new("/lib/A.JPG"), "B").unwrap());
    assert!(!cat.path_taken(Path::new("/lib/A.JPG"), "A").unwrap());
    drop(cat);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version as usize, SCHEMA_VERSION);
    let indexes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'assets_by_%_path'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 3);
    // Opening again runs nothing new.
    drop(conn);
    Catalog::open(&path).unwrap();
}

#[test]
fn live_paths_count_as_taken_and_forget_medium_matches_exactly() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.upsert_asset(&asset("A", 10)).unwrap();
    cat.upsert_asset(&asset("B", 20)).unwrap();
    cat.set_path("A", PathKind::Live, Some(Path::new("/lib/A.MOV")))
        .unwrap();
    cat.set_path("B", PathKind::Medium, Some(Path::new("/cache/medium/B.jpg")))
        .unwrap();
    assert!(cat.path_taken(Path::new("/lib/A.MOV"), "B").unwrap());
    assert!(!cat.path_taken(Path::new("/lib/B.MOV"), "B").unwrap());
    cat.forget_medium(Path::new("/cache/medium/A.jpg")).unwrap();
    assert!(cat.asset("B").unwrap().unwrap().medium_path.is_some());
    cat.forget_medium(Path::new("/cache/medium/B.jpg")).unwrap();
    assert!(cat.asset("B").unwrap().unwrap().medium_path.is_none());
}

#[test]
fn move_paths_moves_what_is_under_the_folder_only() {
    let cat = Catalog::open_in_memory().unwrap();
    for (id, path) in [
        ("A", "/P/icloud-photos/2025/09/A.JPG"),
        ("B", "/P/icloud-photos-old/2025/09/B.JPG"),
        ("C", "/P/icloud-photos/2024/C.JPG"),
    ] {
        cat.upsert_asset(&asset(id, 10)).unwrap();
        cat.set_path(id, PathKind::Original, Some(Path::new(path))).unwrap();
    }
    cat.set_path("A", PathKind::Live, Some(Path::new("/P/icloud-photos/2025/09/A.MOV")))
        .unwrap();
    let moved = cat
        .move_paths(Path::new("/P/icloud-photos/2025"), Path::new("/P/2025"))
        .unwrap();
    assert_eq!(moved, 2);
    let a = cat.asset("A").unwrap().unwrap();
    assert_eq!(a.local_path.as_deref(), Some(Path::new("/P/2025/09/A.JPG")));
    assert_eq!(a.live_path.as_deref(), Some(Path::new("/P/2025/09/A.MOV")));
    let path = |id: &str| cat.asset(id).unwrap().unwrap().local_path;
    assert_eq!(
        path("B").as_deref(),
        Some(Path::new("/P/icloud-photos-old/2025/09/B.JPG"))
    );
    assert_eq!(path("C").as_deref(), Some(Path::new("/P/icloud-photos/2024/C.JPG")));
}

#[test]
fn mark_missing_deleted_marks_exactly_the_unseen() {
    let mut cat = Catalog::open_in_memory().unwrap();
    // More than one batch of ids.
    let n = 1_234;
    for i in 0..n {
        cat.upsert_asset(&asset(&format!("A{i:05}"), i)).unwrap();
    }
    let seen: HashSet<String> = (0..n).filter(|i| i % 3 != 0).map(|i| format!("A{i:05}")).collect();
    let marked = cat.transaction(|cat| cat.mark_missing_deleted(&seen)).unwrap();
    assert_eq!(marked, (0..n).filter(|i| i % 3 == 0).count());
    assert_eq!(cat.count().unwrap() as usize, seen.len());
    assert!(cat.asset("A00000").unwrap().unwrap().deleted);
    assert!(!cat.asset("A00001").unwrap().unwrap().deleted);
    // Nothing new to mark the second time.
    assert_eq!(cat.mark_missing_deleted(&seen).unwrap(), 0);
}

#[test]
fn grid_rows_match_the_full_rows() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.upsert_asset(&asset("A", 10)).unwrap();
    cat.upsert_asset(&asset("B", 20)).unwrap();
    cat.set_path("A", PathKind::Thumb, Some(Path::new("/cache/thumbs/A.jpg")))
        .unwrap();
    let full = cat.assets(None).unwrap();
    let slim = cat.grid_assets(None).unwrap();
    assert_eq!(slim.len(), full.len());
    for (s, f) in slim.iter().zip(&full) {
        assert_eq!(
            (&s.id, s.created, &s.thumb_path, s.kind, s.is_live, &s.filename),
            (&f.id, f.created, &f.thumb_path, f.kind, f.is_live, &f.filename)
        );
    }
    assert_eq!(slim[0].id, "B", "newest first");
}
