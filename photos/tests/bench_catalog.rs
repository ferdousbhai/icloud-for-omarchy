//! Catalog timings on a synthetic 50k-asset library. Ignored by default:
//! `cargo test --release -p icloud-photos --test bench_catalog -- --ignored --nocapture`

mod support;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use icloud_photos::catalog::{Catalog, PathKind};
use icloud_photos::cloudkit::{Asset, Kind, MasterInfo, Resource};

const N: usize = 50_000;

fn res(kind: &str, i: usize) -> Option<Resource> {
    Some(Resource {
        url: format!(
            "https://cvws.icloud-content.com/B/{kind}/{i:08}/AbCdEfGhIjKlMnOpQrStUvWxYz0123456789?o=AtokenThatIsQuiteLongIndeed&v=1&x=3&a=CAog&e=1790000000&k=_&fl=&r=abc&ckc=com.apple.photos.cloud&ckz=PrimarySync&p=31&s=sig"
        ),
        size: 1_000_000,
        file_type: Some("public.heic".into()),
    })
}

fn asset(i: usize) -> Asset {
    Asset {
        id: format!("ASSET-{i:08}"),
        change_tag: Some(format!("tag{i}")),
        created: 1_600_000_000 + i as i64 * 600,
        deleted: false,
        master: MasterInfo {
            master_id: format!("MASTER-{i:08}"),
            filename: format!("IMG_{:04}.HEIC", i % 10_000),
            size: 1_000_000,
            width: 4032,
            height: 3024,
            kind: Kind::Photo,
            original: res("orig", i),
            thumb: res("thumb", i),
            medium: res("med", i),
            live: if i.is_multiple_of(3) { res("live", i) } else { None },
        },
    }
}

fn library(root: &Path, i: usize) -> PathBuf {
    root.join(format!("lib/{}/IMG_{:04}.HEIC", i % 120, i % 10_000))
}

fn time<T>(what: &str, n: usize, mut f: impl FnMut(usize) -> T) {
    let start = Instant::now();
    for i in 0..n {
        std::hint::black_box(f(i));
    }
    let total = start.elapsed();
    println!(
        "{what:<40} {n:>6} x  {:>10.1} us each  ({:.1} ms total)",
        total.as_secs_f64() * 1e6 / n as f64,
        total.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn catalog_timings_on_50k_assets() {
    let tmp = support::temp_dir("bench");
    let root = tmp.path();
    let mut cat = Catalog::open(&root.join("catalog.db")).unwrap();
    let start = Instant::now();
    cat.transaction(|cat| {
        for i in 0..N {
            cat.upsert_asset(&asset(i))?;
        }
        Ok(())
    })
    .unwrap();
    println!(
        "upsert {N} assets (one transaction)      {:>10.1} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    let start = Instant::now();
    cat.transaction(|cat| {
        for i in 0..N {
            let id = format!("ASSET-{i:08}");
            if i.is_multiple_of(2) {
                cat.set_path(&id, PathKind::Original, Some(&library(root, i)))?;
            }
            cat.set_path(&id, PathKind::Thumb, Some(&root.join(format!("thumbs/{id}.jpg"))))?;
            if i.is_multiple_of(5) {
                cat.set_path(&id, PathKind::Medium, Some(&root.join(format!("medium/{id}.jpg"))))?;
            }
        }
        Ok(())
    })
    .unwrap();
    println!(
        "set_path ~{} paths (one transaction)   {:>10.1} ms",
        N * 17 / 10,
        start.elapsed().as_secs_f64() * 1e3
    );

    time("path_taken (hit)", 2_000, |i| {
        cat.path_taken(&library(root, i * 2), "nobody").unwrap()
    });
    time("path_taken (miss)", 2_000, |i| {
        cat.path_taken(&root.join(format!("lib/none/{i}.HEIC")), "nobody")
            .unwrap()
    });
    time("forget_medium", 500, |i| {
        cat.forget_medium(&root.join(format!("medium/nothing-{i}.jpg")))
            .unwrap()
    });
    time("assets(None) (all columns)", 5, |_| cat.assets(None).unwrap().len());
    time("grid_assets(None) (grid columns)", 5, |_| {
        cat.grid_assets(None).unwrap().len()
    });

    let seen: HashSet<String> = (0..N)
        .filter(|i| i % 10 != 0)
        .map(|i| format!("ASSET-{i:08}"))
        .collect();
    let start = Instant::now();
    let n = cat.transaction(|cat| cat.mark_missing_deleted(&seen)).unwrap();
    println!(
        "mark_missing_deleted ({n} of {N})        {:>10.1} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(n, N / 10);
}
