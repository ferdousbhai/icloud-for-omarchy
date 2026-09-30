//! Bring the catalog up to date with iCloud.
//!
//! With a zone change token in `meta`, ask `changes/zone` for what changed
//! since. Without one, or when that endpoint errors (it is not verified
//! against Apple for PrimarySync from the web), list everything with
//! `records/query` and store a fresh token from `zones/list`.

use std::collections::{HashMap, HashSet};

use crate::catalog::{Catalog, LAST_SYNC_KEY, SYNC_TOKEN_KEY};
use crate::cloudkit::{Album, AssetPart, CloudKit, LIST_ALL, MasterInfo, Paired, Record, Relation, pair_assets};
use crate::config::Dirs;
use crate::thumbs;
use crate::transport::{Error, Result, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub mode: Mode,
    /// Assets inserted or updated.
    pub assets: usize,
    /// Assets that left the library (deleted, expunged, or gone from a full listing).
    pub removed: usize,
    pub albums: usize,
    /// Why the incremental path was abandoned, if it was.
    pub fell_back: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Albums,
    Assets(usize),
    AlbumMembers { done: usize, total: usize },
    Changes(usize),
}

/// Sync once. `SignInRequired` always propagates; any other error on the
/// incremental path falls back to a full listing.
pub fn sync(ck: &CloudKit, cat: &mut Catalog, progress: &dyn Fn(Progress)) -> Result<Report> {
    if let Some(token) = cat.meta(SYNC_TOKEN_KEY)? {
        match incremental(ck, cat, &token, progress) {
            Ok(report) => return finish(cat, report),
            Err(e) if e.is_sign_in() => return Err(e),
            Err(e) => {
                let mut report = full(ck, cat, progress)?;
                report.fell_back = Some(e.to_string());
                return finish(cat, report);
            }
        }
    }
    let report = full(ck, cat, progress)?;
    finish(cat, report)
}

/// A full listing even when a sync token is stored (`icloud-photos sync --full`).
pub fn sync_full(ck: &CloudKit, cat: &mut Catalog, progress: &dyn Fn(Progress)) -> Result<Report> {
    let report = full(ck, cat, progress)?;
    finish(cat, report)
}

/// What the app and the CLI run: open the catalog, sync it (`force_full`
/// skips the incremental path), then prune cached renditions of assets that
/// left the library. A pruning failure is logged, not returned.
pub fn run(t: &dyn Transport, dirs: &Dirs, force_full: bool, progress: &dyn Fn(Progress)) -> Result<Report> {
    let mut cat = Catalog::open(&dirs.catalog())?;
    let ck = CloudKit::connect(t)?;
    let report = if force_full {
        sync_full(&ck, &mut cat, progress)?
    } else {
        sync(&ck, &mut cat, progress)?
    };
    if let Err(e) = thumbs::prune_cache(&cat, dirs, thumbs::MEDIUM_CACHE_CAP) {
        eprintln!("icloud-photos: pruning the cache: {e}");
    }
    Ok(report)
}

fn finish(cat: &Catalog, report: Report) -> Result<Report> {
    let now = icloud_session::time::now_secs();
    cat.set_meta(LAST_SYNC_KEY, Some(&now.to_string()))?;
    Ok(report)
}

/// List the whole library. The token is read *before* listing so changes
/// made during the listing come round again on the next incremental sync.
pub fn full(ck: &CloudKit, cat: &mut Catalog, progress: &dyn Fn(Progress)) -> Result<Report> {
    let token = match ck.zone_sync_token() {
        Ok(t) => t,
        Err(e) if e.is_sign_in() => return Err(e),
        Err(_) => None,
    };
    if !ck.indexing_finished()? {
        return Err(Error::Other(
            "iCloud Photos is still indexing this library; try again in a few minutes".into(),
        ));
    }

    progress(Progress::Albums);
    let albums = ck.albums()?;

    let mut assets = Vec::new();
    let mut orphans: Vec<AssetPart> = Vec::new();
    let mut spare: HashMap<String, MasterInfo> = HashMap::new();
    ck.list_assets(LIST_ALL, &[], |records| {
        let page = pair_assets(records);
        assets.extend(page.assets);
        orphans.extend(page.orphans);
        spare.extend(page.lone_masters.into_iter().map(|m| (m.master_id.clone(), m)));
        progress(Progress::Assets(assets.len()));
        Ok(())
    })?;
    // A page boundary can split an asset from its master: join across pages,
    // and look up whatever is still missing.
    let missing: Vec<&str> = orphans
        .iter()
        .map(|o| o.master_id.as_str())
        .filter(|id| !spare.contains_key(*id))
        .collect();
    if !missing.is_empty() {
        spare.extend(lookup(ck, missing.into_iter())?);
    }
    for o in orphans {
        if let Some(m) = spare.get(&o.master_id) {
            let m = m.clone();
            assets.push(o.with_master(m));
        }
    }

    let mut members = Vec::with_capacity(albums.len());
    for (i, album) in albums.iter().enumerate() {
        progress(Progress::AlbumMembers {
            done: i,
            total: albums.len(),
        });
        members.push(ck.album_members(&album.id)?);
    }

    cat.transaction(|cat| {
        let mut seen = HashSet::new();
        for a in &assets {
            cat.upsert_asset(a)?;
            seen.insert(a.id.clone());
        }
        let removed = cat.mark_missing_deleted(&seen)?;
        cat.replace_albums(&albums)?;
        for (album, rels) in albums.iter().zip(&members) {
            cat.set_album_members(&album.id, rels)?;
        }
        cat.set_meta(SYNC_TOKEN_KEY, token.as_deref())?;
        Ok(Report {
            mode: Mode::Full,
            assets: assets.len(),
            removed,
            albums: albums.len(),
            fell_back: None,
        })
    })
}

fn lookup<'a>(ck: &CloudKit, ids: impl Iterator<Item = &'a str>) -> Result<HashMap<String, MasterInfo>> {
    let ids: Vec<&str> = ids.collect::<HashSet<_>>().into_iter().collect();
    let mut out = HashMap::new();
    for chunk in ids.chunks(100) {
        for m in ck.lookup_masters(chunk)? {
            out.insert(m.master_id.clone(), m);
        }
    }
    Ok(out)
}

/// Apply `changes/zone` pages until `moreComing` is false. Nothing is written
/// unless every page arrived, so a failure midway leaves the old token valid.
pub fn incremental(ck: &CloudKit, cat: &mut Catalog, token: &str, progress: &dyn Fn(Progress)) -> Result<Report> {
    let mut records: Vec<Record> = Vec::new();
    let mut token = token.to_owned();
    for _ in 0..10_000 {
        let page = ck.zone_changes(Some(&token))?;
        records.extend(page.records);
        progress(Progress::Changes(records.len()));
        let advanced = page.sync_token != token;
        token = page.sync_token;
        if !page.more_coming || !advanced {
            break;
        }
    }
    apply_changes(ck, cat, &records, &token)
}

/// Apply one batch of changed records and store the new token, atomically.
pub fn apply_changes(ck: &CloudKit, cat: &mut Catalog, records: &[Record], new_token: &str) -> Result<Report> {
    // Later changes to the same record win.
    let mut latest: HashMap<&str, &Record> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for r in records {
        if latest.insert(r.name.as_str(), r).is_none() {
            order.push(r.name.as_str());
        }
    }
    let records: Vec<Record> = order.iter().map(|n| latest[n].clone()).collect();

    let Paired {
        assets: paired,
        orphans,
        lone_masters,
    } = pair_assets(&records);

    // Orphan assets we have never seen need their master from the server.
    let unknown: Vec<&AssetPart> = orphans
        .iter()
        .filter(|o| !o.deleted && cat.asset(&o.id).ok().flatten().is_none())
        .collect();
    let fetched = if unknown.is_empty() {
        HashMap::new()
    } else {
        lookup(ck, unknown.iter().map(|o| o.master_id.as_str()))?
    };

    cat.transaction(|cat| {
        let mut report = Report {
            mode: Mode::Incremental,
            assets: 0,
            removed: 0,
            albums: 0,
            fell_back: None,
        };
        for a in &paired {
            cat.upsert_asset(a)?;
            if a.deleted {
                report.removed += 1
            } else {
                report.assets += 1
            }
        }
        for m in &lone_masters {
            cat.update_master(m)?;
        }
        for o in &orphans {
            if cat.update_asset_part(o)? {
                if o.deleted {
                    report.removed += 1
                } else {
                    report.assets += 1
                }
            } else if let Some(m) = fetched.get(&o.master_id) {
                cat.upsert_asset(&o.clone().with_master(m.clone()))?;
                report.assets += 1;
            }
        }
        for r in &records {
            match r.record_type.as_deref() {
                None => {
                    cat.apply_tombstone(&r.name)?;
                    report.removed += 1;
                }
                Some("CPLAlbum") => {
                    if let Some(album) = Album::from_record(r) {
                        cat.upsert_album(&album)?;
                        report.albums += 1;
                    }
                }
                Some("CPLContainerRelation") => {
                    if let Some(rel) = Relation::from_record(r) {
                        cat.apply_relation(&rel)?;
                    }
                }
                _ if r.deleted => cat.apply_tombstone(&r.name)?,
                _ => {}
            }
        }
        cat.set_meta(SYNC_TOKEN_KEY, Some(new_token))?;
        Ok(report)
    })
}
