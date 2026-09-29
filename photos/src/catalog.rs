//! Local catalog: `~/.local/share/icloud-photos/catalog.db`.
//!
//! One connection per thread (SQLite WAL); the UI reads, the sync and
//! download threads write.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::cloudkit::{Album, Asset, AssetPart, Kind, MasterInfo, Relation, Resource};
use crate::transport::Result;

pub const SYNC_TOKEN_KEY: &str = "zone_sync_token";
pub const LAST_SYNC_KEY: &str = "last_sync";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS assets(
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
CREATE INDEX IF NOT EXISTS assets_by_date ON assets(deleted, created DESC);
CREATE INDEX IF NOT EXISTS assets_by_master ON assets(master_id);
CREATE TABLE IF NOT EXISTS albums(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS album_assets(
  album_id TEXT NOT NULL,
  asset_id TEXT NOT NULL,
  relation_id TEXT NULL,
  PRIMARY KEY(album_id, asset_id)
);
CREATE INDEX IF NOT EXISTS album_assets_by_asset ON album_assets(asset_id);
CREATE INDEX IF NOT EXISTS album_assets_by_relation ON album_assets(relation_id);
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
";

/// A catalog row, as the UI and downloaders see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub master_id: String,
    pub filename: String,
    pub created: i64,
    pub size: i64,
    pub w: i64,
    pub h: i64,
    pub kind: Kind,
    pub is_live: bool,
    pub local_path: Option<PathBuf>,
    pub live_path: Option<PathBuf>,
    pub thumb_path: Option<PathBuf>,
    pub medium_path: Option<PathBuf>,
    pub deleted: bool,
    pub change_tag: Option<String>,
    pub orig_url: Option<String>,
    pub orig_type: Option<String>,
    pub thumb_url: Option<String>,
    pub medium_url: Option<String>,
    pub live_url: Option<String>,
    pub live_type: Option<String>,
}

const ROW_COLUMNS: &str = "id, master_id, filename, created, size, w, h, kind, is_live, local_path, live_path, \
    thumb_path, medium_path, deleted, change_tag, orig_url, orig_type, thumb_url, medium_url, live_url, live_type";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let path = |i: usize| r.get::<_, Option<String>>(i).map(|p| p.map(PathBuf::from));
    Ok(Row {
        id: r.get(0)?,
        master_id: r.get(1)?,
        filename: r.get(2)?,
        created: r.get(3)?,
        size: r.get(4)?,
        w: r.get(5)?,
        h: r.get(6)?,
        kind: Kind::parse(&r.get::<_, String>(7)?),
        is_live: r.get::<_, i64>(8)? != 0,
        local_path: path(9)?,
        live_path: path(10)?,
        thumb_path: path(11)?,
        medium_path: path(12)?,
        deleted: r.get::<_, i64>(13)? != 0,
        change_tag: r.get(14)?,
        orig_url: r.get(15)?,
        orig_type: r.get(16)?,
        thumb_url: r.get(17)?,
        medium_url: r.get(18)?,
        live_url: r.get(19)?,
        live_type: r.get(20)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRow {
    pub id: String,
    pub name: String,
    pub count: i64,
}

pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Catalog> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Catalog { conn })
    }

    pub fn open_in_memory() -> Result<Catalog> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Catalog { conn })
    }

    /// Run `f` in one transaction (a full sync writes thousands of rows).
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Catalog) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, v],
            )?,
            None => self.conn.execute("DELETE FROM meta WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    /// Insert or refresh an asset from CloudKit, keeping local paths.
    pub fn upsert_asset(&self, a: &Asset) -> Result<()> {
        let m = &a.master;
        self.conn.execute(
            "INSERT INTO assets(id, master_id, filename, created, size, w, h, kind, is_live, deleted, change_tag,
                                orig_url, orig_type, thumb_url, medium_url, live_url, live_type)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET master_id = excluded.master_id, filename = excluded.filename,
               created = excluded.created, size = excluded.size, w = excluded.w, h = excluded.h,
               kind = excluded.kind, is_live = excluded.is_live, deleted = excluded.deleted,
               change_tag = excluded.change_tag, orig_url = excluded.orig_url, orig_type = excluded.orig_type,
               thumb_url = excluded.thumb_url, medium_url = excluded.medium_url,
               live_url = excluded.live_url, live_type = excluded.live_type",
            params![
                a.id,
                m.master_id,
                m.filename,
                a.created,
                m.size,
                m.width,
                m.height,
                m.kind.as_str(),
                a.is_live() as i64,
                a.deleted as i64,
                a.change_tag,
                url(&m.original),
                file_type(&m.original),
                url(&m.thumb),
                url(&m.medium),
                url(&m.live),
                file_type(&m.live),
            ],
        )?;
        Ok(())
    }

    /// Apply the asset-side of a change whose master did not come with it.
    /// Returns false when the asset is unknown (the caller must look the master up).
    pub fn update_asset_part(&self, p: &AssetPart) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE assets SET master_id = ?2, created = ?3, deleted = ?4, change_tag = ?5 WHERE id = ?1",
            params![p.id, p.master_id, p.created, p.deleted as i64, p.change_tag],
        )?;
        Ok(n > 0)
    }

    /// Apply a master change (new URLs, renamed file) to every asset using it.
    pub fn update_master(&self, m: &MasterInfo) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE assets SET filename = ?2, size = ?3, w = ?4, h = ?5, kind = ?6, is_live = ?7,
               orig_url = ?8, orig_type = ?9, thumb_url = ?10, medium_url = ?11, live_url = ?12, live_type = ?13
             WHERE master_id = ?1",
            params![
                m.master_id,
                m.filename,
                m.size,
                m.width,
                m.height,
                m.kind.as_str(),
                m.live.is_some() as i64,
                url(&m.original),
                file_type(&m.original),
                url(&m.thumb),
                url(&m.medium),
                url(&m.live),
                file_type(&m.live),
            ],
        )?)
    }

    pub fn mark_deleted(&self, asset_id: &str, change_tag: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET deleted = 1, change_tag = COALESCE(?2, change_tag) WHERE id = ?1",
            params![asset_id, change_tag],
        )?;
        Ok(())
    }

    /// A CloudKit tombstone: the record is gone for good. Downloaded files stay
    /// on disk; the catalog forgets the record.
    pub fn apply_tombstone(&self, record_name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET deleted = 1 WHERE id = ?1 OR master_id = ?1",
            [record_name],
        )?;
        self.conn.execute("DELETE FROM albums WHERE id = ?1", [record_name])?;
        self.conn.execute(
            "DELETE FROM album_assets WHERE album_id = ?1 OR relation_id = ?1",
            [record_name],
        )?;
        Ok(())
    }

    /// After a full listing: anything not seen is no longer in the library.
    pub fn mark_missing_deleted(&self, seen: &std::collections::HashSet<String>) -> Result<usize> {
        let ids: Vec<String> = {
            let mut st = self.conn.prepare("SELECT id FROM assets WHERE deleted = 0")?;
            st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut n = 0;
        for id in ids.iter().filter(|id| !seen.contains(*id)) {
            n += self.conn.execute("UPDATE assets SET deleted = 1 WHERE id = ?1", [id])?;
        }
        Ok(n)
    }

    pub fn upsert_album(&self, a: &Album) -> Result<()> {
        if a.deleted || a.is_folder {
            self.conn.execute("DELETE FROM albums WHERE id = ?1", [&a.id])?;
            self.conn
                .execute("DELETE FROM album_assets WHERE album_id = ?1", [&a.id])?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO albums(id, name, position) VALUES(?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, position = excluded.position",
            params![a.id, a.name, a.position],
        )?;
        Ok(())
    }

    /// Replace the album list (full sync).
    pub fn replace_albums(&self, albums: &[Album]) -> Result<()> {
        self.conn.execute("DELETE FROM albums", [])?;
        for a in albums {
            self.upsert_album(a)?;
        }
        self.conn.execute(
            "DELETE FROM album_assets WHERE album_id NOT IN (SELECT id FROM albums)",
            [],
        )?;
        Ok(())
    }

    pub fn set_album_members(&self, album_id: &str, members: &[Relation]) -> Result<()> {
        self.conn
            .execute("DELETE FROM album_assets WHERE album_id = ?1", [album_id])?;
        for m in members {
            self.apply_relation(m)?;
        }
        Ok(())
    }

    pub fn apply_relation(&self, r: &Relation) -> Result<()> {
        let relation_id = (!r.id.is_empty()).then_some(r.id.as_str());
        if r.deleted {
            self.conn.execute(
                "DELETE FROM album_assets WHERE album_id = ?1 AND asset_id = ?2",
                [&r.album_id, &r.asset_id],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO album_assets(album_id, asset_id, relation_id) VALUES(?1, ?2, ?3)
                 ON CONFLICT(album_id, asset_id) DO UPDATE SET relation_id = COALESCE(excluded.relation_id, relation_id)",
                params![r.album_id, r.asset_id, relation_id],
            )?;
        }
        Ok(())
    }

    pub fn albums(&self) -> Result<Vec<AlbumRow>> {
        let mut st = self.conn.prepare(
            "SELECT al.id, al.name, (SELECT COUNT(*) FROM album_assets aa JOIN assets a ON a.id = aa.asset_id
                                      WHERE aa.album_id = al.id AND a.deleted = 0)
             FROM albums al ORDER BY al.position, al.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| {
            Ok(AlbumRow {
                id: r.get(0)?,
                name: r.get(1)?,
                count: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Visible assets, newest first; `album` narrows to one album.
    pub fn assets(&self, album: Option<&str>) -> Result<Vec<Row>> {
        let rows = match album {
            None => {
                let mut st = self.conn.prepare(&format!(
                    "SELECT {ROW_COLUMNS} FROM assets WHERE deleted = 0 ORDER BY created DESC, id"
                ))?;
                st.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>()?
            }
            Some(album) => {
                let mut st = self.conn.prepare(&format!(
                    "SELECT {ROW_COLUMNS} FROM assets WHERE deleted = 0
                       AND id IN (SELECT asset_id FROM album_assets WHERE album_id = ?1)
                     ORDER BY created DESC, id"
                ))?;
                st.query_map([album], row)?.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    pub fn asset(&self, id: &str) -> Result<Option<Row>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {ROW_COLUMNS} FROM assets WHERE id = ?1"), [id], row)
            .optional()?)
    }

    /// The albums an asset is in: (id, name), in sidebar order.
    pub fn albums_of(&self, asset_id: &str) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare(
            "SELECT al.id, al.name FROM albums al JOIN album_assets aa ON aa.album_id = al.id
             WHERE aa.asset_id = ?1 ORDER BY al.position, al.name COLLATE NOCASE",
        )?;
        Ok(st
            .query_map([asset_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM assets WHERE deleted = 0", [], |r| r.get(0))?)
    }

    /// Assets with no downloaded original ("download all" mode).
    pub fn missing_originals(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT id FROM assets WHERE deleted = 0 AND local_path IS NULL AND orig_url IS NOT NULL ORDER BY created DESC")?;
        Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn missing_thumbs(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT id FROM assets WHERE deleted = 0 AND thumb_path IS NULL AND thumb_url IS NOT NULL ORDER BY created DESC")?;
        Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_path(&self, id: &str, which: PathKind, path: Option<&Path>) -> Result<()> {
        let col = match which {
            PathKind::Original => "local_path",
            PathKind::Live => "live_path",
            PathKind::Thumb => "thumb_path",
            PathKind::Medium => "medium_path",
        };
        let p = path.map(|p| p.to_string_lossy().into_owned());
        self.conn
            .execute(&format!("UPDATE assets SET {col} = ?2 WHERE id = ?1"), params![id, p])?;
        Ok(())
    }

    /// Cached thumb/medium files of assets no longer in the library:
    /// (asset id, thumb, medium).
    pub fn cached_renditions_of_removed(&self) -> Result<Vec<CachedRenditions>> {
        let mut st = self.conn.prepare(
            "SELECT id, thumb_path, medium_path FROM assets
             WHERE deleted = 1 AND (thumb_path IS NOT NULL OR medium_path IS NOT NULL)",
        )?;
        let rows = st.query_map([], |r| {
            let path = |i: usize| r.get::<_, Option<String>>(i).map(|p| p.map(PathBuf::from));
            Ok((r.get(0)?, path(1)?, path(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A medium JPEG was evicted from the cache.
    pub fn forget_medium(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET medium_path = NULL WHERE medium_path = ?1",
            [path.to_string_lossy()],
        )?;
        Ok(())
    }

    /// Is `path` already the original of an asset other than `id`?
    pub fn path_taken(&self, path: &Path, id: &str) -> Result<bool> {
        let p = path.to_string_lossy();
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM assets WHERE (local_path = ?1 OR live_path = ?1) AND id != ?2",
                params![p, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

pub type CachedRenditions = (String, Option<PathBuf>, Option<PathBuf>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Original,
    Live,
    Thumb,
    Medium,
}

fn url(r: &Option<Resource>) -> Option<&str> {
    r.as_ref().map(|r| r.url.as_str())
}

fn file_type(r: &Option<Resource>) -> Option<&str> {
    r.as_ref().and_then(|r| r.file_type.as_deref())
}
