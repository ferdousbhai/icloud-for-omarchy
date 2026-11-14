//! What the last sync left in step: one row per file or folder that is the
//! same here and in iCloud, with both sides as they were then. A later
//! difference from the row is a change; a side with no row is new. Kept in
//! SQLite under the state folder, with the last listing of iCloud Drive (so
//! unchanged folders are not listed again) and a few settings.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::local::Entry;
use crate::remote::{Kind, Node};

/// One item in step on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The item in iCloud when last in step.
    pub node: Node,
    /// Its path under the local folder.
    pub path: String,
    /// The local file or folder when last in step.
    pub local: Entry,
}

pub struct State {
    db: Connection,
}

/// An item in iCloud that is left alone until it changes there (a package,
/// say).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ignored {
    pub id: String,
    /// The etag it had when it was set aside.
    pub etag: String,
    pub path: String,
    pub reason: String,
}

/// The SQL test for "`path` is ?1 or below it" (see [`is_under`]).
const UNDER_1: &str = "(path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/')";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS items (
  id TEXT PRIMARY KEY, docwsid TEXT NOT NULL, zone TEXT NOT NULL, parent TEXT NOT NULL,
  name TEXT NOT NULL, is_dir INTEGER NOT NULL, etag TEXT NOT NULL, size INTEGER NOT NULL,
  mtime_ms INTEGER NOT NULL, path TEXT NOT NULL UNIQUE,
  l_size INTEGER NOT NULL, l_mtime_ns INTEGER NOT NULL, l_ino INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS remote_cache (
  id TEXT PRIMARY KEY, docwsid TEXT NOT NULL, zone TEXT NOT NULL, parent TEXT NOT NULL,
  name TEXT NOT NULL, kind INTEGER NOT NULL, etag TEXT NOT NULL, size INTEGER NOT NULL,
  mtime_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ignored (id TEXT PRIMARY KEY, etag TEXT NOT NULL, path TEXT NOT NULL, reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS not_synced (path TEXT NOT NULL, reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pending_moves (id TEXT PRIMARY KEY, dest TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

fn kind_code(kind: Kind) -> i64 {
    match kind {
        Kind::File => 0,
        Kind::Folder => 1,
        Kind::AppLibrary => 2,
    }
}

fn kind_of(code: i64) -> Kind {
    match code {
        0 => Kind::File,
        1 => Kind::Folder,
        _ => Kind::AppLibrary,
    }
}

/// `path` itself or anything below it.
pub fn is_under(path: &str, prefix: &str) -> bool {
    path == prefix || (path.len() > prefix.len() && path.starts_with(prefix) && path.as_bytes()[prefix.len()] == b'/')
}

impl State {
    pub fn open(dir: &Path) -> rusqlite::Result<State> {
        std::fs::create_dir_all(dir).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let db = Connection::open(dir.join("state.db"))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(SCHEMA)?;
        Ok(State { db })
    }

    #[cfg(test)]
    pub fn in_memory() -> State {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        State { db }
    }

    /// Every row, parents before children.
    pub fn rows(&self) -> rusqlite::Result<Vec<Row>> {
        let mut rows = self.select("", [])?;
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(rows)
    }

    pub fn row_at(&self, path: &str) -> rusqlite::Result<Option<Row>> {
        Ok(self.select("WHERE path = ?1", [path])?.pop())
    }

    pub fn row(&self, id: &str) -> rusqlite::Result<Option<Row>> {
        Ok(self.select("WHERE id = ?1", [id])?.pop())
    }

    fn select(&self, filter: &str, params: impl rusqlite::Params) -> rusqlite::Result<Vec<Row>> {
        let mut stmt = self.db.prepare(&format!(
            "SELECT id, docwsid, zone, parent, name, is_dir, etag, size, mtime_ms, path, l_size, l_mtime_ns, l_ino FROM items {filter}"
        ))?;
        stmt.query_map(params, |r| {
            let is_dir = r.get::<_, i64>(5)? != 0;
            Ok(Row {
                node: Node {
                    id: r.get(0)?,
                    docwsid: r.get(1)?,
                    zone: r.get(2)?,
                    parent: r.get(3)?,
                    name: r.get(4)?,
                    kind: if is_dir { Kind::Folder } else { Kind::File },
                    etag: r.get(6)?,
                    size: r.get::<_, i64>(7)? as u64,
                    mtime_ms: r.get(8)?,
                },
                path: r.get(9)?,
                local: Entry {
                    is_dir,
                    size: r.get::<_, i64>(10)? as u64,
                    mtime_ns: r.get(11)?,
                    ino: r.get::<_, i64>(12)? as u64,
                },
            })
        })?
        .collect()
    }

    /// Records an item as in step. A row that held the path for another id
    /// (the file was replaced by a new one in iCloud) gives way.
    pub fn put(&self, row: &Row) -> rusqlite::Result<()> {
        self.db.execute(
            "DELETE FROM items WHERE path = ?1 AND id != ?2",
            params![row.path, row.node.id],
        )?;
        self.db.execute(
            "INSERT OR REPLACE INTO items VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                row.node.id,
                row.node.docwsid,
                row.node.zone,
                row.node.parent,
                row.node.name,
                row.local.is_dir as i64,
                row.node.etag,
                row.node.size as i64,
                row.node.mtime_ms,
                row.path,
                row.local.size as i64,
                row.local.mtime_ns,
                row.local.ino as i64,
            ],
        )?;
        Ok(())
    }

    /// Drops one row (its children keep theirs).
    pub fn forget(&self, id: &str) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM items WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Drops the row at `path` and every row below it.
    pub fn forget_under(&self, path: &str) -> rusqlite::Result<()> {
        self.db.execute(&format!("DELETE FROM items WHERE {UNDER_1}"), [path])?;
        Ok(())
    }

    /// After a move: the rows at and below `from` now live at and below `to`.
    pub fn move_paths(&self, from: &str, to: &str) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        // Anything a stale row still claims at the destination gives way.
        let under_from = UNDER_1.replace("?1", "?2");
        tx.execute(
            &format!("DELETE FROM items WHERE {UNDER_1} AND NOT {under_from}"),
            [to, from],
        )?;
        // Parents first, so no row takes a path another still holds.
        for row in self.rows()? {
            if is_under(&row.path, from) {
                let moved = format!("{to}{}", &row.path[from.len()..]);
                tx.execute("UPDATE items SET path = ?1 WHERE id = ?2", params![moved, row.node.id])?;
            }
        }
        tx.commit()
    }

    pub fn meta(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.db
            .execute("INSERT OR REPLACE INTO meta VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    pub fn clear_meta(&self, key: &str) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    /// The last listing of iCloud Drive, every node below the top folder.
    pub fn cached_remote(&self) -> rusqlite::Result<Vec<Node>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, docwsid, zone, parent, name, kind, etag, size, mtime_ms FROM remote_cache")?;
        stmt.query_map([], |r| {
            Ok(Node {
                id: r.get(0)?,
                docwsid: r.get(1)?,
                zone: r.get(2)?,
                parent: r.get(3)?,
                name: r.get(4)?,
                kind: kind_of(r.get(5)?),
                etag: r.get(6)?,
                size: r.get::<_, i64>(7)? as u64,
                mtime_ms: r.get(8)?,
            })
        })?
        .collect()
    }

    pub fn store_remote(&mut self, nodes: &[Node]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM remote_cache", [])?;
        {
            let mut stmt =
                tx.prepare("INSERT OR REPLACE INTO remote_cache VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
            for n in nodes {
                stmt.execute(params![
                    n.id,
                    n.docwsid,
                    n.zone,
                    n.parent,
                    n.name,
                    kind_code(n.kind),
                    n.etag,
                    n.size as i64,
                    n.mtime_ms
                ])?;
            }
        }
        tx.commit()
    }

    /// The items in iCloud that are left alone until they change there, by
    /// path.
    pub fn ignored(&self) -> rusqlite::Result<Vec<Ignored>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, etag, path, reason FROM ignored ORDER BY path")?;
        stmt.query_map([], |r| {
            Ok(Ignored {
                id: r.get(0)?,
                etag: r.get(1)?,
                path: r.get(2)?,
                reason: r.get(3)?,
            })
        })?
        .collect()
    }

    /// Stops ignoring every item whose id `keep` does not want (it is gone
    /// from iCloud).
    pub fn keep_ignored(&self, keep: impl Fn(&str) -> bool) -> rusqlite::Result<()> {
        for item in self.ignored()? {
            if !keep(&item.id) {
                self.db.execute("DELETE FROM ignored WHERE id = ?1", [&item.id])?;
            }
        }
        Ok(())
    }

    /// `(path, reason)` of everything the last sync left out, for `status`.
    pub fn not_synced(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.db.prepare("SELECT path, reason FROM not_synced ORDER BY path")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    pub fn set_not_synced(&mut self, items: &[(String, String)]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM not_synced", [])?;
        for (path, reason) in items {
            tx.execute("INSERT INTO not_synced VALUES (?1, ?2)", params![path, reason])?;
        }
        tx.commit()
    }

    /// Notes that the item is being moved in iCloud to the local path
    /// `dest`: a move there takes two requests (the folder, then the name),
    /// and a run can stop between them.
    pub fn set_pending_move(&self, id: &str, dest: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO pending_moves VALUES (?1, ?2)",
            params![id, dest],
        )?;
        Ok(())
    }

    pub fn clear_pending_move(&self, id: &str) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM pending_moves WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Item id to the local path its move in iCloud was heading for, for
    /// the moves not finished (those of items no longer in step are dropped).
    pub fn pending_moves(&self) -> rusqlite::Result<std::collections::HashMap<String, String>> {
        self.db
            .execute("DELETE FROM pending_moves WHERE id NOT IN (SELECT id FROM items)", [])?;
        let mut stmt = self.db.prepare("SELECT id, dest FROM pending_moves")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    pub fn ignore(&self, node: &Node, path: &str, reason: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO ignored VALUES (?1, ?2, ?3, ?4)",
            params![node.id, node.etag, path, reason],
        )?;
        Ok(())
    }

    pub fn count(&self) -> rusqlite::Result<(usize, usize)> {
        let files: i64 = self
            .db
            .query_row("SELECT count(*) FROM items WHERE is_dir = 0", [], |r| r.get(0))?;
        let folders: i64 = self
            .db
            .query_row("SELECT count(*) FROM items WHERE is_dir = 1", [], |r| r.get(0))?;
        Ok((files as usize, folders as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, path: &str, is_dir: bool) -> Row {
        Row {
            node: Node {
                id: id.into(),
                docwsid: id.into(),
                zone: "z".into(),
                parent: "p".into(),
                name: path.rsplit('/').next().unwrap().into(),
                kind: if is_dir { Kind::Folder } else { Kind::File },
                etag: "e".into(),
                size: 1,
                mtime_ms: 2,
            },
            path: path.into(),
            local: Entry {
                is_dir,
                size: 1,
                mtime_ns: 3,
                ino: 4,
            },
        }
    }

    #[test]
    fn rows_round_trip_and_sort_parents_first() {
        let state = State::in_memory();
        state.put(&row("2", "a/b.txt", false)).unwrap();
        state.put(&row("1", "a", true)).unwrap();
        let rows = state.rows().unwrap();
        assert_eq!(rows, [row("1", "a", true), row("2", "a/b.txt", false)]);
    }

    #[test]
    fn a_move_takes_everything_below_and_nothing_beside() {
        let state = State::in_memory();
        for (id, path, dir) in [("1", "a", true), ("2", "a/b.txt", false), ("3", "ab.txt", false)] {
            state.put(&row(id, path, dir)).unwrap();
        }
        state.move_paths("a", "z/y").unwrap();
        let paths: Vec<_> = state.rows().unwrap().into_iter().map(|r| r.path).collect();
        assert_eq!(paths, ["ab.txt", "z/y", "z/y/b.txt"]);
        state.forget_under("z/y").unwrap();
        assert_eq!(state.rows().unwrap().len(), 1);
    }

    #[test]
    fn a_move_up_into_the_parent_replaces_what_the_parent_held() {
        let state = State::in_memory();
        for (id, path, dir) in [
            ("1", "a", true),
            ("2", "a/b", true),
            ("3", "a/b/b", false),
            ("4", "a/other.txt", false),
            ("5", "a%", false),
        ] {
            state.put(&row(id, path, dir)).unwrap();
        }
        state.move_paths("a/b", "a").unwrap();
        let rows: Vec<_> = state.rows().unwrap().into_iter().map(|r| (r.node.id, r.path)).collect();
        let want = [("2", "a"), ("5", "a%"), ("3", "a/b")].map(|(id, path)| (id.to_string(), path.to_string()));
        assert_eq!(rows, want);
        state.forget_under("a").unwrap();
        let paths: Vec<_> = state.rows().unwrap().into_iter().map(|r| r.path).collect();
        assert_eq!(paths, ["a%"]);
    }

    #[test]
    fn ignored_items_and_the_not_synced_list_round_trip() {
        let mut state = State::in_memory();
        state
            .ignore(&row("1", "b.pages", false).node, "b.pages", "a package")
            .unwrap();
        state
            .ignore(&row("2", "a.pages", false).node, "a.pages", "a package")
            .unwrap();
        state.keep_ignored(|id| id == "2").unwrap();
        let left: Vec<_> = state.ignored().unwrap().into_iter().map(|i| (i.id, i.path)).collect();
        assert_eq!(left, [("2".to_string(), "a.pages".to_string())]);
        let items = [("link".to_string(), "it is a link".to_string())];
        state.set_not_synced(&items).unwrap();
        state.set_not_synced(&items).unwrap();
        assert_eq!(state.not_synced().unwrap(), items);
    }

    #[test]
    fn a_pending_move_is_kept_until_cleared_or_its_item_is_forgotten() {
        let state = State::in_memory();
        state.put(&row("1", "a.txt", false)).unwrap();
        state.put(&row("2", "b.txt", false)).unwrap();
        state.set_pending_move("1", "Docs/a2.txt").unwrap();
        state.set_pending_move("2", "Docs/b2.txt").unwrap();
        assert_eq!(state.pending_moves().unwrap().len(), 2);
        state.clear_pending_move("1").unwrap();
        state.forget("2").unwrap();
        assert!(state.pending_moves().unwrap().is_empty());
    }

    #[test]
    fn a_new_id_at_a_path_replaces_the_old_row() {
        let state = State::in_memory();
        state.put(&row("1", "a.txt", false)).unwrap();
        state.put(&row("2", "a.txt", false)).unwrap();
        assert_eq!(state.rows().unwrap(), [row("2", "a.txt", false)]);
    }
}
