//! One sync: list iCloud Drive, scan the folder, plan, and carry the plan
//! out, a round at a time until nothing is left. Each operation records what
//! it did as it finishes, so a run that stops half way (no network, a sign
//! out, a crash) is picked up by the next from wherever both sides stand.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::crash;
use crate::local::{self, Entry};
use icloud_session::time::{now_ms, rfc3339};

use crate::plan::{self, Op, Plan, Snapshot, name_of, parent_of};
use crate::remote::{self, Kind, Node, Remote};
use crate::state::{Row, State};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Remote(#[from] remote::Error),
    #[error("the sync state could not be read or written: {0}")]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The folder that was in sync is not there: syncing would read that as
    /// everything having been deleted.
    #[error("{0} is missing, so nothing was synced")]
    FolderMissing(PathBuf),
    /// More removals than a sync does unasked.
    #[error("this sync would remove {deletions} of {total} items")]
    TooManyDeletions { deletions: usize, total: usize },
    /// The state belongs to another Apple account.
    #[error("{folder} was synced with another iCloud account")]
    OtherAccount { folder: PathBuf },
    /// The state belongs to another local folder: its rows say what was in
    /// step there, and would read here as everything having been removed.
    #[error("the sync state belongs to {bound}, not {folder}")]
    OtherFolder { folder: PathBuf, bound: PathBuf },
    /// The state belongs to another folder of iCloud Drive: none of its
    /// rows are below this one, which would read as iCloud having removed
    /// them all.
    #[error("the sync state belongs to the iCloud Drive folder {bound}, not {top}")]
    OtherTop { top: String, bound: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Rounds one sync may take before it leaves the rest to the next.
const MAX_ROUNDS: usize = 12;
/// Removals a sync does without being told to, whatever the folder's size.
const FREE_DELETIONS: usize = 20;
/// Folders the change feed does not name are trusted from the last listing
/// for this long, then everything is listed again.
const FULL_LISTING_EVERY_MS: i64 = 6 * 3600 * 1000;
/// Passes over the folders one listing takes at most while iCloud changes
/// under it.
const LISTING_TRIES: usize = 3;

/// What a sync did.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Outcome {
    /// One line per operation carried out (or, in a dry run, planned).
    pub done: Vec<String>,
    /// Operations that failed and were left for the next run.
    pub problems: Vec<String>,
    /// Local files still being written.
    pub pending: Vec<String>,
    /// Paths not synced, with the reason.
    pub skipped: Vec<String>,
    pub dry_run: bool,
}

pub struct Engine<'a, R: Remote> {
    pub remote: &'a R,
    pub state: State,
    /// The local folder.
    pub root: PathBuf,
    /// The folder in iCloud Drive that `root` mirrors.
    pub top: Node,
    pub trash_dir: PathBuf,
    pub host: String,
    /// Called with a line as each operation starts.
    pub progress: Box<dyn Fn(&str) + 'a>,
}

/// A move or rename that could not be carried out.
#[derive(Debug)]
struct Stuck {
    /// Where the item is and was to go, on both sides: nothing at or below
    /// these is touched while the move waits.
    paths: Vec<String>,
    /// `(path, reason)` for `status`.
    shown: (String, String),
}

/// What carrying out one plan came to, beyond the lines in the outcome.
#[derive(Debug, Default)]
struct Applied {
    /// A file was set aside as a conflicted copy, which another round
    /// sends up.
    set_aside: bool,
    /// The moves that were not carried out.
    stuck: Vec<Stuck>,
}

/// The folder in iCloud Drive with this id, as far as the id tells
/// (`FOLDER::<zone>::<docwsid>`): enough to create and upload into it.
pub fn top_node(id: &str) -> Node {
    let mut parts = id.splitn(3, "::");
    let _ = parts.next();
    let zone = parts.next().unwrap_or("com.apple.CloudDocs").to_string();
    let docwsid = parts.next().unwrap_or("root").to_string();
    Node {
        id: id.to_string(),
        docwsid,
        zone,
        parent: String::new(),
        name: String::new(),
        kind: Kind::Folder,
        etag: String::new(),
        size: 0,
        mtime_ms: 0,
    }
}

fn describe(op: &Op) -> String {
    match op {
        Op::LocalMove { from, to, .. } => format!("move {from} to {to} (as in iCloud)"),
        Op::RemoteMove { to, .. } => format!("move to {to} in iCloud"),
        Op::RemoteAside { to, name, .. } => format!("rename to {name} in iCloud, on the way to {to}"),
        Op::SetAside { path } => format!("keep {path} as a conflicted copy"),
        Op::KeepBoth { path, .. } => format!("download {path}, keeping this one as a conflicted copy if it differs"),
        Op::MkdirLocal { path, .. } => format!("create folder {path}"),
        Op::MkdirRemote { path } => format!("create folder {path} in iCloud"),
        Op::Download { path, .. } => format!("download {path}"),
        Op::Upload { path } => format!("upload {path}"),
        Op::Replace { path, .. } => format!("upload changed {path}"),
        Op::TrashLocal { path, .. } => format!("move {path} to the trash (removed in iCloud)"),
        Op::TrashRemote { path, .. } => format!("move {path} to Recently Deleted in iCloud (removed here)"),
        Op::Adopt { path, .. } => format!("already the same: {path}"),
        Op::Forget { path, .. } => format!("look again at {path}"),
    }
}

/// A failure that concerns one item: the sync goes on with the rest. Any
/// other (no network, signed out, iCloud in trouble) ends the run.
fn is_item_problem(e: &Error) -> bool {
    match e {
        Error::Remote(
            remote::Error::Api(_)
            | remote::Error::Refused(_)
            | remote::Error::Stale(_)
            | remote::Error::Package(_)
            | remote::Error::Listing { .. },
        ) => true,
        Error::Remote(remote::Error::Session(icloud_session::Error::Http { status, .. })) => {
            (400..500).contains(status) && !matches!(status, 401 | 421 | 423 | 429)
        }
        Error::Remote(remote::Error::Io(_)) | Error::Io(_) => true,
        _ => false,
    }
}

impl<R: Remote> Engine<'_, R> {
    /// Everything below the top folder. Only folders that changed are
    /// listed again: the ones iCloud's change feed names (and every folder
    /// above them), and any whose etag differs from the last listing. The
    /// rest come from that last listing. Every few hours, and whenever the
    /// feed starts over, everything is listed.
    ///
    /// The folders are listed one by one, so a change another device makes
    /// meanwhile can show half (an item moved out of a folder not listed
    /// yet into one that was is in neither, and would read as removed):
    /// the feed is asked again after, and while it names anything new the
    /// folders are listed again, a few times at most.
    fn crawl(&mut self) -> Result<Vec<Node>> {
        let fresh_enough = self
            .state
            .meta("full_listing_ms")?
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|at| now_ms() - at < FULL_LISTING_EVERY_MS);
        let token = self.state.meta("feed_token")?;
        // Asked before listing, so a change made while listing is named
        // again next time rather than missed.
        let (mut changes, trust_cache) = match self.remote.changes(token.as_deref()) {
            Ok(changes) => (changes, fresh_enough && token.is_some()),
            Err(remote::Error::FeedReset) => (self.remote.changes(None)?, false),
            Err(e) => return Err(e.into()),
        };
        let cached = if trust_cache {
            self.state.cached_remote()?
        } else {
            Vec::new()
        };
        let mut tries = 0;
        let out = loop {
            tries += 1;
            let listed = self.list_below(&cached, &changes);
            if tries == LISTING_TRIES {
                break listed?;
            }
            // A feed that cannot say leaves the listing as it is (the next
            // sync starts from the token kept below, or over).
            let Ok(after) = self.remote.changes(Some(&changes.token)) else {
                break listed?;
            };
            if after.folders.is_empty() && after.files.is_empty() {
                break listed?;
            }
            changes.folders.extend(after.folders);
            changes.files.extend(after.files);
            changes.token = after.token;
        };
        if !trust_cache {
            self.state.set_meta("full_listing_ms", &now_ms().to_string())?;
        }
        self.state.store_remote(&out)?;
        self.state.set_meta("feed_token", &changes.token)?;
        Ok(out)
    }

    /// One pass over the folders below the top, listing again those
    /// `changes` names (and all above them) or whose etag is not the one in
    /// `cached`, and taking the rest from `cached`.
    fn list_below(&self, cached: &[Node], changes: &remote::Changes) -> Result<Vec<Node>> {
        let by_id: HashMap<&str, &Node> = cached.iter().map(|n| (n.id.as_str(), n)).collect();
        let by_docwsid: HashMap<&str, &Node> = cached.iter().map(|n| (n.docwsid.as_str(), n)).collect();
        let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
        for node in cached {
            children.entry(node.parent.as_str()).or_default().push(node);
        }
        // Folders to list whatever their etag says: each one the feed
        // names, the folder of each file it names, and all above those.
        let mut must_list: HashSet<String> = HashSet::new();
        let mut dirty: Vec<String> = changes.folders.clone();
        for file in &changes.files {
            if let Some(node) = by_docwsid.get(file.as_str()) {
                dirty.push(node.parent.clone());
            }
        }
        for start in dirty {
            // A drivewsid or a bare docwsid; climb the last listing's tree.
            let mut at = start.rsplit("::").next().unwrap_or_default().to_string();
            while must_list.insert(at.clone()) {
                match by_docwsid.get(at.as_str()) {
                    Some(node) => at = node.parent.rsplit("::").next().unwrap_or_default().to_string(),
                    None => break,
                }
            }
        }

        let mut out = Vec::new();
        let mut to_list = vec![self.top.id.clone()];
        while let Some(folder) = to_list.pop() {
            let (_, kids) = self.remote.list(&folder)?;
            for kid in kids {
                if kid.kind == Kind::Folder {
                    let unchanged = by_id.get(kid.id.as_str()).is_some_and(|c| c.etag == kid.etag)
                        && !must_list.contains(&kid.docwsid);
                    if unchanged {
                        let mut reuse = vec![kid.id.as_str()];
                        while let Some(id) = reuse.pop() {
                            for node in children.get(id).into_iter().flatten() {
                                out.push((*node).clone());
                                if node.kind == Kind::Folder {
                                    reuse.push(node.id.as_str());
                                }
                            }
                        }
                    } else {
                        to_list.push(kid.id.clone());
                    }
                }
                out.push(kid);
            }
        }
        Ok(out)
    }

    /// Brings the folder and iCloud Drive in step. With `dry_run`, only says
    /// what the first round would do.
    pub fn sync(&mut self, dry_run: bool, allow_deletes: bool) -> Result<Outcome> {
        let mut outcome = Outcome {
            dry_run,
            ..Outcome::default()
        };
        // What neither side could show properly in the last round.
        let mut unseen: Vec<(String, String)> = Vec::new();
        // Moves and renames this sync could not carry out. Their paths are
        // left alone for the rest of it, so everything else still syncs;
        // the next sync tries them again.
        let mut stuck: Vec<Stuck> = Vec::new();
        for _ in 0..MAX_ROUNDS {
            let nodes = self.crawl()?;
            let snapshot = Snapshot::build_below(&self.top.id, &nodes);
            // A package that is gone from iCloud is no longer left out.
            self.state.keep_ignored(|id| snapshot.nodes.contains_key(id))?;
            let base = self.state.rows()?;
            if !self.root.is_dir() {
                // Missing (unmounted, renamed), now or since the last round.
                if !base.is_empty() {
                    return Err(Error::FolderMissing(self.root.clone()));
                }
                if !dry_run {
                    fs::create_dir_all(&self.root)?;
                    crash::point("local.create_root");
                }
            }
            let (entries, skipped) = if self.root.is_dir() {
                local::scan(&self.root)?
            } else {
                Default::default()
            };
            let base = if self.recover_placed(&entries)? | self.recover_sending(&base, &snapshot, &entries)? {
                self.state.rows()?
            } else {
                base
            };
            unseen = skipped
                .iter()
                .map(|s| (s.path.clone(), s.reason.to_string()))
                .chain(
                    snapshot
                        .skipped
                        .iter()
                        .map(|(path, why)| (path.clone(), why.to_string())),
                )
                .collect();
            let ignored = self.state.ignored()?;
            let context = plan::Context {
                ignored: ignored.iter().map(|i| (i.id.clone(), i.etag.clone())).collect(),
                unknown: unseen
                    .iter()
                    .map(|(path, _)| path.clone())
                    .chain(stuck.iter().flat_map(|s| s.paths.iter().cloned()))
                    .collect(),
                pending_moves: self.pending_moves(&base, &snapshot, &entries)?,
                now_ns: now_ms() * 1_000_000,
            };
            unseen.extend(stuck.iter().map(|s| s.shown.clone()));
            let plan = plan::plan(&base, &snapshot, &entries, &context);

            outcome.pending = plan.pending.clone();
            outcome.skipped = unseen
                .iter()
                .cloned()
                .chain(ignored.into_iter().map(|i| (i.path, i.reason)))
                .map(|(path, why)| format!("{path}: {why}"))
                .collect();

            let allowed = FREE_DELETIONS.max(base.len() / 4);
            // An emptied folder is never read as removals without being told:
            // it is as likely an unmounted disk's mount point as a clear-out.
            let emptied = entries.is_empty() && plan.ops.iter().any(|op| matches!(op, Op::TrashRemote { .. }));
            // A dry run is how the removals are looked at, so it lists them.
            if (plan.deletions > allowed || emptied) && !allow_deletes && !dry_run {
                return Err(Error::TooManyDeletions {
                    deletions: plan.deletions,
                    total: base.len(),
                });
            }
            if dry_run {
                outcome.done.extend(plan.ops.iter().map(describe));
                return Ok(outcome);
            }
            let before = outcome.done.len();
            let applied = self.apply(&plan, &snapshot, &mut outcome)?;
            if outcome.done.len() == before && !applied.stuck.is_empty() {
                // Moves are done alone, before anything else. One that
                // iCloud or the folder refuses would be planned again every
                // round and nothing else would ever sync: set it aside and
                // go on with the rest.
                stuck.extend(applied.stuck);
                continue;
            }
            // Another round only if this one got somewhere.
            if !(plan.again || applied.set_aside) || outcome.done.len() == before {
                break;
            }
        }
        // For `status`: everything left out, with what this sync found to
        // be a package. (A move set aside in the last round is not in the
        // list that round's plan was made from.)
        for s in &stuck {
            if !unseen.contains(&s.shown) {
                unseen.push(s.shown.clone());
            }
        }
        unseen.extend(self.state.ignored()?.into_iter().map(|i| (i.path, i.reason)));
        self.state.set_not_synced(&unseen)?;
        self.state.set_meta("last_sync_ms", &now_ms().to_string())?;
        Ok(outcome)
    }

    /// The moves in iCloud noted as not finished. A note whose item is
    /// where it was in step on both sides is void (the run stopped before
    /// iCloud was asked, and the file here was since put back): it is
    /// dropped, so it cannot later claim a move made in iCloud.
    fn pending_moves(
        &self,
        base: &[Row],
        snapshot: &Snapshot,
        entries: &std::collections::BTreeMap<String, Entry>,
    ) -> Result<HashMap<String, String>> {
        let mut pending = self.state.pending_moves()?;
        for row in base {
            let at_rest = snapshot.paths.get(&row.node.id) == Some(&row.path)
                && entries.get(&row.path).is_some_and(|e| e.is_dir == row.local.is_dir);
            if at_rest && pending.remove(&row.node.id).is_some() {
                self.state.clear_pending_move(&row.node.id)?;
            }
        }
        Ok(pending)
    }

    fn apply(&mut self, plan: &Plan, snapshot: &Snapshot, outcome: &mut Outcome) -> Result<Applied> {
        let mut applied = Applied::default();
        for op in &plan.ops {
            let line = describe(op);
            (self.progress)(&line);
            let failed = match (self.apply_one(op, snapshot), op) {
                (Ok(true), _) => {
                    outcome.done.push(line);
                    continue;
                }
                (Ok(false), _) => "something else is in the way for now".to_string(),
                // Changed in iCloud in a way its size and date did not show:
                // both are kept, ours as a conflicted copy, and the next
                // round downloads iCloud's and uploads the copy.
                (Err(Error::Remote(remote::Error::Stale(_))), Op::Replace { id, path }) => {
                    // The row goes first. While it stood, a file no longer
                    // at its name (set aside and the run stopped there, or
                    // gone meanwhile) would read as removed here, and take
                    // to Recently Deleted a version this computer never saw.
                    self.state.forget(id)?;
                    match self.set_aside(path) {
                        Ok(()) => {
                            outcome.done.push(format!(
                                "keep {path} as a conflicted copy (it changed in iCloud meanwhile)"
                            ));
                            applied.set_aside = true;
                        }
                        // The file went meanwhile: this file's problem, and
                        // the next sync brings iCloud's version back.
                        Err(e) => outcome.problems.push(format!("{line}: {e}")),
                    }
                    continue;
                }
                (Err(e), _) if is_item_problem(&e) => {
                    outcome.problems.push(format!("{line}: {e}"));
                    e.to_string()
                }
                // No network, signed out, iCloud in trouble: the run ends,
                // and the next one starts from wherever both sides stand.
                (Err(e), _) => return Err(e),
            };
            match op {
                Op::RemoteMove { id, to } | Op::RemoteAside { id, to, .. } => {
                    let from = self.state.row(id)?.map(|row| row.path);
                    applied.stuck.push(Stuck {
                        paths: from.into_iter().chain([to.clone()]).collect(),
                        shown: (
                            to.clone(),
                            format!("it could not be moved or renamed in iCloud: {failed}"),
                        ),
                    });
                }
                Op::LocalMove { id, from, to } => {
                    let row = self.state.row(id)?.map(|row| row.path);
                    applied.stuck.push(Stuck {
                        paths: row.into_iter().chain([from.clone(), to.clone()]).collect(),
                        shown: (
                            from.clone(),
                            format!("it could not be moved to {to} as in iCloud: {failed}"),
                        ),
                    });
                }
                _ => {}
            }
        }
        Ok(applied)
    }

    fn abs(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    /// Renames the local file to a conflicted copy, freeing its name.
    fn set_aside(&self, path: &str) -> io::Result<()> {
        let from = self.abs(path);
        let date = &rfc3339(now_ms() / 1000)[..10];
        fs::rename(&from, local::conflict_path(&from, &self.host, date))?;
        crash::point("local.set_aside");
        Ok(())
    }

    /// Records the item as in step at `path`.
    fn put(&self, node: Node, path: &str, local: Entry) -> Result<()> {
        Ok(self.state.put(&Row {
            node,
            path: path.to_string(),
            local,
        })?)
    }

    /// Records a moved item at `to`, with what was below it: named and
    /// placed as iCloud has it now, with the content both sides were in
    /// step with before the move (the size and date in iCloud, the stamp
    /// here), so an edit made on either side since still shows as one in
    /// the next round.
    fn put_moved(&self, node: Node, to: &str, row: &Row) -> Result<()> {
        let node = Node {
            size: row.node.size,
            mtime_ms: row.node.mtime_ms,
            ..node
        };
        let moved = Row {
            node,
            path: to.to_string(),
            local: row.local,
        };
        Ok(self.state.move_paths(&row.path, &moved)?)
    }

    /// The answer to one request of a move. When iCloud refused it and the
    /// move has `untouched` the item so far, the note of the move is dropped.
    fn unless_refused(&self, answer: remote::Result<Node>, id: &str, untouched: bool) -> Result<Node> {
        if untouched && matches!(answer, Err(remote::Error::Refused(_))) {
            self.state.clear_pending_move(id)?;
        }
        Ok(answer?)
    }

    /// The answer to a request noted as sending `path`: when iCloud said no,
    /// the note goes (nothing of it is in iCloud to find later). It stays
    /// for an answer lost on the way, which may have been a yes.
    fn unsent(&self, answer: remote::Result<Node>, path: &str) -> Result<Node> {
        if let Err(remote::Error::Refused(_) | remote::Error::Api(_) | remote::Error::Stale(_)) = &answer {
            self.state.clear_sending(path)?;
        }
        Ok(answer?)
    }

    /// The folder in iCloud that holds `path`, once it is in step.
    fn parent_node(&self, path: &str) -> Result<Node> {
        let parent = parent_of(path);
        if parent.is_empty() {
            return Ok(self.top.clone());
        }
        match self.state.row_at(parent)? {
            Some(row) if row.local.is_dir => Ok(row.node),
            _ => Err(Error::Remote(remote::Error::Api(format!(
                "its folder {parent} is not in iCloud yet"
            )))),
        }
    }

    /// The file just `sent` as its folder's listing has it now: the size
    /// and date every later comparison is made against. Unless the listing
    /// shows another version than the one iCloud answered for (another
    /// device changed it right after): then with the size, date and etag
    /// of that answer, so that the next round sees the change and fetches
    /// it, rather than take it for what is here.
    fn listed(&self, parent_id: &str, sent: &Node) -> Result<Node> {
        self.listed_if_there(parent_id, sent)?
            .ok_or_else(|| Error::Remote(remote::Error::Api("the item is not in its folder's listing".into())))
    }

    /// As [`Engine::listed`], with `None` when the listing does not hold
    /// the item.
    fn listed_if_there(&self, parent_id: &str, sent: &Node) -> Result<Option<Node>> {
        let (_, kids) = self.remote.list(parent_id)?;
        let Some(listed) = kids.into_iter().find(|k| k.id == sent.id) else {
            return Ok(None);
        };
        // An answer's etag or date may not be spelled as a listing's: the
        // same size and date, or the same etag, is the same version.
        let same_content = listed.size == sent.size && (listed.mtime_ms - sent.mtime_ms).abs() <= plan::SAME_TIME_MS;
        if listed.etag == sent.etag || same_content {
            return Ok(Some(listed));
        }
        Ok(Some(Node {
            size: sent.size,
            mtime_ms: sent.mtime_ms,
            etag: sent.etag.clone(),
            ..listed
        }))
    }

    /// Whether something beside `path` (but `except`) has its name in
    /// another case, which iCloud takes as the same name: nothing is put
    /// there then, and the next round sorts the two out.
    fn other_case_here(&self, path: &str, except: Option<&str>) -> Result<bool> {
        let except = except.filter(|f| parent_of(f) == parent_of(path)).map(name_of);
        let name = name_of(path);
        let dir = self.abs(parent_of(path));
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let other = entry?.file_name();
            let other = other.to_string_lossy();
            if other != name && Some(&*other) != except && other.to_lowercase() == name.to_lowercase() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn stat(&self, path: &str) -> Result<Option<Entry>> {
        Ok(local::stat(&self.abs(path))?)
    }

    fn need_stat(&self, path: &str) -> Result<Entry> {
        self.stat(path)?
            .ok_or_else(|| Error::Io(io::Error::new(io::ErrorKind::NotFound, format!("{path} is gone"))))
    }

    /// `Ok(true)` when the operation was carried out, `Ok(false)` when the
    /// world had moved on and it was left for the next round.
    fn apply_one(&mut self, op: &Op, snapshot: &Snapshot) -> Result<bool> {
        let node_of = |id: &str| {
            snapshot
                .nodes
                .get(id)
                .cloned()
                .ok_or_else(|| Error::Remote(remote::Error::Api("the item is no longer listed".into())))
        };
        match op {
            Op::LocalMove { id, from, to } => {
                let node = node_of(id)?;
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                if from != to {
                    if self.stat(to)?.is_some() || self.stat(from)?.is_none() || self.other_case_here(to, Some(from))? {
                        return Ok(false);
                    }
                    if let Some(parent) = self.abs(to).parent() {
                        fs::create_dir_all(parent)?;
                        crash::point("local.mkdir");
                    }
                    // Never onto something that appeared there meanwhile.
                    if !local::rename_no_replace(&self.abs(from), &self.abs(to))? {
                        return Ok(false);
                    }
                    crash::point("local.rename");
                }
                self.put_moved(node, to, &row)?;
                self.state.clear_pending_move(id)?;
            }
            Op::RemoteMove { id, to } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                self.need_stat(to)?;
                let parent = self.parent_node(to)?;
                // As iCloud has it now, so a move an earlier run left half
                // done (in the new folder, under the old name) is finished
                // from where it stands.
                let mut node = snapshot.nodes.get(id).cloned().unwrap_or_else(|| row.node.clone());
                // Noted first: until both halves are done and recorded,
                // where iCloud has it is this move's doing, not a move made
                // in iCloud to be followed here. The note goes again when
                // iCloud refuses the first request with the item still
                // where it was in step: then nothing is this move's doing.
                let mut untouched = node.parent == row.node.parent && node.name == row.node.name;
                self.state.set_pending_move(id, to)?;
                // Renamed first when the folder it goes to holds something
                // of its old name, so that it is never moved onto that.
                let taken = |node: &Node| {
                    snapshot.nodes.values().any(|n| {
                        n.parent == parent.id && n.id != node.id && n.name.to_lowercase() == node.name.to_lowercase()
                    })
                };
                if node.parent != parent.id && node.name != name_of(to) && taken(&node) {
                    node = self.unless_refused(self.remote.rename(&node, name_of(to)), id, untouched)?;
                    crash::point("remote.rename");
                    untouched = false;
                }
                if node.parent != parent.id {
                    node = self.unless_refused(self.remote.move_to(&node, &parent.id), id, untouched)?;
                    crash::point("remote.move_to");
                    untouched = false;
                }
                if node.name != name_of(to) {
                    node = self.unless_refused(self.remote.rename(&node, name_of(to)), id, untouched)?;
                    crash::point("remote.rename");
                }
                self.put_moved(node, to, &row)?;
                self.state.clear_pending_move(id)?;
            }
            Op::RemoteAside { id, to, name } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                self.need_stat(to)?;
                let mut node = snapshot.nodes.get(id).cloned().unwrap_or_else(|| row.node.clone());
                self.state.set_pending_move(id, to)?;
                if &node.name != name {
                    let untouched = node.parent == row.node.parent && node.name == row.node.name;
                    node = self.unless_refused(self.remote.rename(&node, name), id, untouched)?;
                    crash::point("remote.rename");
                }
                // Where it is here, with the move to finish noted.
                self.put_moved(node, to, &row)?;
            }
            Op::SetAside { path } => {
                if !self.abs(path).exists() {
                    return Ok(false);
                }
                self.set_aside(path)?;
            }
            Op::MkdirLocal { id, path } => {
                let node = node_of(id)?;
                if self.other_case_here(path, None)? {
                    return Ok(false);
                }
                fs::create_dir_all(self.abs(path))?;
                let local = self.need_stat(path)?;
                let row = Row {
                    node,
                    path: path.to_string(),
                    local,
                };
                self.state.set_placed(&row)?;
                crash::point("local.mkdir");
                self.state.put(&row)?;
                self.state.clear_placed(&row.node.id)?;
            }
            Op::MkdirRemote { path } => {
                let local = self.need_stat(path)?;
                let parent = self.parent_node(path)?;
                self.state.set_sending(path, "", &local)?;
                let node = self.unsent(self.remote.create_folder(&parent.id, name_of(path)), path)?;
                crash::point("remote.create_folder");
                self.put(node, path, local)?;
                self.state.clear_sending(path)?;
            }
            Op::Download { id, path } => {
                let node = node_of(id)?;
                // Only over what was in step, or nothing: anything else is
                // somebody's work that the next round must look at first.
                let current = self.stat(path)?;
                let expected = self.state.row(id)?.filter(|r| &r.path == path).map(|r| r.local);
                let safe = match (current, expected) {
                    (None, _) => true,
                    (Some(now), Some(then)) => !now.is_dir && now.size == then.size && now.mtime_ns == then.mtime_ns,
                    (Some(_), None) => false,
                };
                if !safe || self.other_case_here(path, None)? {
                    return Ok(false);
                }
                let Some(temp) = self.fetch(&node, path)? else {
                    return Ok(false);
                };
                // Looked at again: an edit saved while it came down is kept,
                // and the next round sees both sides changed.
                if self.stat(path)? != current {
                    let _ = fs::remove_file(&temp);
                    return Ok(false);
                }
                self.place(&temp, path, node)?;
            }
            Op::KeepBoth { id, path } => {
                let node = node_of(id)?;
                let current = self.stat(path)?;
                if !current.is_some_and(|e| !e.is_dir) {
                    // Gone or a folder meanwhile: the next round looks again.
                    return Ok(false);
                }
                let Some(temp) = self.fetch(&node, path)? else {
                    return Ok(false);
                };
                if self.stat(path)? != current {
                    let _ = fs::remove_file(&temp);
                    return Ok(false);
                }
                // The same bytes (a replace or a download that finished
                // without being recorded, two copies of one file): in step.
                if same_bytes(&temp, &self.abs(path))? {
                    let _ = fs::remove_file(&temp);
                    self.put(node, path, current.unwrap())?;
                    return Ok(true);
                }
                if let Err(e) = self.set_aside(path) {
                    let _ = fs::remove_file(&temp);
                    return Err(e.into());
                }
                self.place(&temp, path, node)?;
            }
            Op::Upload { path } => {
                let before = self.need_stat(path)?;
                let parent = self.parent_node(path)?;
                self.state.set_sending(path, "", &before)?;
                let sent = self.unsent(
                    self.remote.upload_new(
                        &parent,
                        name_of(path),
                        &self.abs(path),
                        before.mtime_ns / 1_000_000_000 * 1000,
                    ),
                    path,
                )?;
                crash::point("remote.upload_new");
                let node = self.listed(&parent.id, &sent)?;
                // Recorded as it was before the upload: a change made while
                // it was going up shows as a change next time.
                self.put(node, path, before)?;
                self.state.clear_sending(path)?;
            }
            Op::Replace { id, path } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                let before = self.need_stat(path)?;
                self.state.set_sending(path, id, &before)?;
                let sent = self.unsent(
                    self.remote
                        .replace(&row.node, &self.abs(path), before.mtime_ns / 1_000_000_000 * 1000),
                    path,
                )?;
                crash::point("remote.replace");
                match self.listed_if_there(&row.node.parent, &sent) {
                    Ok(Some(node)) => self.put(node, path, before)?,
                    // Not where it was (in Recently Deleted, say, where a
                    // replace may still land): nothing of it is in step, and
                    // the next round sends the file here as new.
                    Ok(None) => {
                        self.state.forget(id)?;
                        self.state.clear_sending(path)?;
                        return Err(Error::Remote(remote::Error::Api(
                            "it is no longer in its folder in iCloud".into(),
                        )));
                    }
                    // Recorded from the answer, so a listing that fails now
                    // does not leave the old row to read as a conflict.
                    Err(e) => {
                        let answered = Node {
                            etag: sent.etag.clone(),
                            size: sent.size,
                            mtime_ms: sent.mtime_ms,
                            ..row.node.clone()
                        };
                        self.put(answered, path, before)?;
                        self.state.clear_sending(path)?;
                        return Err(e);
                    }
                }
                self.state.clear_sending(path)?;
            }
            Op::TrashLocal { id, path } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                match self.stat(path)? {
                    None => {}
                    Some(now) if now.is_dir => {
                        // Only a folder whose every file is still as it was.
                        let (entries, _) = local::scan(&self.abs(path))?;
                        let rows = self.state.rows()?;
                        let intact = entries.iter().all(|(p, e)| {
                            let full = format!("{path}/{p}");
                            rows.iter().any(|r| {
                                r.path == full
                                    && (e.is_dir || (e.size == r.local.size && e.mtime_ns == r.local.mtime_ns))
                            })
                        });
                        if !intact {
                            return Ok(false);
                        }
                        self.trash(path)?;
                    }
                    Some(now) => {
                        if now.size != row.local.size || now.mtime_ns != row.local.mtime_ns {
                            return Ok(false);
                        }
                        self.trash(path)?;
                    }
                }
                self.state.forget_under(path)?;
            }
            Op::TrashRemote { id, path } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                if self.stat(path)?.is_some_and(|e| e.ino == row.local.ino) {
                    // It is back.
                    return Ok(false);
                }
                // As iCloud has it now, not when it was listed: an edit made
                // there since beats the removal, which the next round sees.
                // So does a move. A file's etag tells; a folder's does not
                // change for a rename or same-size content below it, so for
                // a folder the change feed is asked whether anything in it
                // changed since the listing.
                let node = node_of(id).unwrap_or(row.node);
                let (_, kids) = self.remote.list(&node.parent)?;
                if !kids.iter().any(|k| k.id == node.id && k.etag == node.etag) {
                    return Ok(false);
                }
                if node.kind == Kind::Folder && self.changed_below(snapshot, &node)? {
                    return Ok(false);
                }
                self.remote.trash(&node)?;
                crash::point("remote.trash");
                self.state.forget_under(path)?;
            }
            Op::Adopt { id, path } => {
                let node = node_of(id)?;
                let local = self.need_stat(path)?;
                self.put(node, path, local)?;
            }
            Op::Forget { id, .. } => self.state.forget(id)?,
        }
        Ok(true)
    }

    /// Whether the change feed names the folder `node`, or anything below
    /// it in `snapshot`, as changed since the listing (or cannot say).
    fn changed_below(&self, snapshot: &Snapshot, node: &Node) -> Result<bool> {
        let Some(token) = self.state.meta("feed_token")? else {
            return Ok(true);
        };
        let changes = match self.remote.changes(Some(&token)) {
            Ok(changes) => changes,
            Err(remote::Error::FeedReset) => return Ok(true),
            Err(e) => return Err(e.into()),
        };
        let Some(top) = snapshot.paths.get(&node.id) else {
            return Ok(true);
        };
        let below: HashSet<&str> = snapshot
            .paths
            .iter()
            .filter(|(_, p)| crate::state::is_under(p, top))
            .filter_map(|(id, _)| snapshot.nodes.get(id).map(|n| n.docwsid.as_str()))
            .collect();
        Ok(changes
            .files
            .iter()
            .chain(&changes.folders)
            .any(|d| below.contains(d.rsplit("::").next().unwrap_or_default())))
    }

    /// iCloud's file, dated as there, in a temp file beside `path`. `None`
    /// for a package, which is set aside as not synced.
    fn fetch(&self, node: &Node, path: &str) -> Result<Option<PathBuf>> {
        let temp = self
            .abs(path)
            .with_file_name(format!(".icloud-drive-{}.part", uuid::Uuid::new_v4().simple()));
        let fetched = self.remote.download(node, &temp).and_then(|()| {
            if node.mtime_ms > 0 {
                local::set_mtime(&temp, node.mtime_ms)?;
            }
            Ok(())
        });
        match fetched {
            Ok(()) => {
                crash::point("local.download_temp");
                Ok(Some(temp))
            }
            Err(remote::Error::Package(_)) => {
                let _ = fs::remove_file(&temp);
                self.state
                    .ignore(node, path, "a package (a Pages document, say), which is not synced")?;
                Ok(None)
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(e.into())
            }
        }
    }

    /// Gives a fetched temp file its name and records it, noted first as
    /// placed (a rename keeps its inode, size and date).
    fn place(&self, temp: &Path, path: &str, node: Node) -> Result<()> {
        let row = Row {
            node,
            path: path.to_string(),
            local: local::stat(temp)?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the download is gone"))?,
        };
        self.state.set_placed(&row)?;
        if let Err(e) = fs::rename(temp, self.abs(path)) {
            let _ = fs::remove_file(temp);
            self.state.clear_placed(&row.node.id)?;
            return Err(e.into());
        }
        crash::point("local.download_rename");
        self.state.put(&row)?;
        self.state.clear_placed(&row.node.id)?;
        Ok(())
    }

    /// What a run that stopped sent to iCloud as new but did not record:
    /// the one new item in iCloud of its size and date (a folder: of its
    /// name) is it, recorded where it was sent from if the item here is
    /// found, as it was, anywhere here; moved since on either side, the next
    /// plan follows the moves. Anything less certain is dropped, and the two
    /// sides meet again as strangers.
    fn recover_sending(
        &self,
        base: &[Row],
        snapshot: &Snapshot,
        entries: &std::collections::BTreeMap<String, Entry>,
    ) -> Result<bool> {
        let sending = self.state.sending()?;
        let known: std::collections::HashSet<&str> = base.iter().map(|r| r.node.id.as_str()).collect();
        let found = |sent: &Entry| {
            entries
                .iter()
                .find(|(_, e)| {
                    e.ino == sent.ino
                        && e.is_dir == sent.is_dir
                        && (e.is_dir || (e.size, e.mtime_ns) == (sent.size, sent.mtime_ns))
                })
                .map(|(p, _)| p.clone())
        };
        for (path, id, sent) in &sending {
            if !id.is_empty() {
                // New content of an item in step, and the file moved here
                // since: recorded as sent if iCloud has those very bytes
                // (else the next plan keeps both, as for any edit).
                let row = base.iter().find(|r| &r.node.id == id);
                if let (Some(row), Some(now), Some(node)) = (row, found(sent), snapshot.nodes.get(id))
                    && row.local != *sent
                    && now != row.path
                    && let Some(temp) = self.fetch(node, &now)?
                {
                    let same = same_bytes(&temp, &self.abs(&now));
                    let _ = fs::remove_file(&temp);
                    if same? {
                        self.state.put(&Row {
                            node: node.clone(),
                            path: row.path.clone(),
                            local: *sent,
                        })?;
                    }
                }
                self.state.clear_sending(path)?;
                continue;
            }
            let recorded = base
                .iter()
                .any(|r| r.local.ino == sent.ino && r.local.is_dir == sent.is_dir);
            let here = found(sent).is_some();
            let candidates: Vec<&Node> = snapshot
                .nodes
                .values()
                .filter(|n| !known.contains(n.id.as_str()) && (n.kind == Kind::Folder) == sent.is_dir)
                // Renamed or moved in iCloud since (by another device), a
                // file is still known by its size and date.
                .filter(|n| !sent.is_dir || n.name == name_of(path))
                .filter(|n| {
                    sent.is_dir || (n.size == sent.size && (n.mtime_ms - sent.mtime_ns / 1_000_000).abs() <= 2_000)
                })
                .collect();
            if let ([node], false, true) = (candidates.as_slice(), recorded, here) {
                self.state.put(&Row {
                    node: (*node).clone(),
                    path: path.clone(),
                    local: *sent,
                })?;
            }
            self.state.clear_sending(path)?;
        }
        Ok(!sending.is_empty())
    }

    /// What a run that stopped put in place here but did not record: each
    /// is recorded where it was put if it is found, as it was, anywhere
    /// here (moved since, the next plan moves it in iCloud too); one not
    /// found is dropped, and the two sides meet again as strangers.
    fn recover_placed(&self, entries: &std::collections::BTreeMap<String, Entry>) -> Result<bool> {
        let placed = self.state.placed()?;
        for row in &placed {
            let found = entries.values().any(|e| {
                e.ino == row.local.ino
                    && e.is_dir == row.local.is_dir
                    && (e.is_dir || (e.size == row.local.size && e.mtime_ns == row.local.mtime_ns))
            });
            if found {
                self.state.put(row)?;
            }
            self.state.clear_placed(&row.node.id)?;
        }
        Ok(!placed.is_empty())
    }

    fn trash(&self, path: &str) -> Result<()> {
        let stamp = rfc3339(now_ms() / 1000);
        match local::trash(&self.abs(path), &self.trash_dir, &stamp) {
            Ok(_) => Ok(()),
            // The trash is on another file system (the folder is a disk of
            // its own): keep a trash inside the folder, which is on the same
            // one. Hidden, so it is not synced.
            Err(e) if e.raw_os_error() == Some(18) => {
                local::trash(&self.abs(path), &self.root.join(".icloud-drive-trash"), &stamp)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Whether two files hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read;
    let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let n = a.read(&mut x)?;
        if n == 0 {
            return Ok(true);
        }
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

/// An XDG base folder: `var` when it holds an absolute path, else
/// `fallback` under the home folder.
pub(crate) fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

/// Where the state for a local folder lives when nothing says otherwise.
pub fn default_state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("icloud-drive")
}

pub fn default_trash_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("Trash")
}

pub fn default_folder() -> PathBuf {
    home().join("iCloud Drive")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Refuses a state that belongs to another local folder, another folder of
/// iCloud Drive (`top`, its id) or another account; claims a new one for
/// all three, and one from before a binding was recorded for that binding.
pub fn bind(state: &State, dsid: &str, folder: &Path, top: &str) -> Result<()> {
    let here = std::path::absolute(folder)?;
    match state.meta("folder")? {
        Some(bound) if bound != here.to_string_lossy() => {
            return Err(Error::OtherFolder {
                folder: here,
                bound: PathBuf::from(bound),
            });
        }
        Some(_) => {}
        None => state.set_meta("folder", &here.to_string_lossy())?,
    }
    match state.meta("top")? {
        Some(bound) if bound != top => {
            return Err(Error::OtherTop {
                top: top.to_string(),
                bound,
            });
        }
        Some(_) => {}
        None => state.set_meta("top", top)?,
    }
    match state.meta("dsid")? {
        Some(owner) if owner != dsid => Err(Error::OtherAccount {
            folder: folder.to_path_buf(),
        }),
        Some(_) => Ok(()),
        None => Ok(state.set_meta("dsid", dsid)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_is_bound_to_one_folder_and_one_account() {
        use crate::remote::ROOT_ID;
        let folder = Path::new("/home/me/iCloud Drive");
        let state = State::in_memory();
        bind(&state, "1", folder, ROOT_ID).unwrap();
        bind(&state, "1", folder, ROOT_ID).unwrap();
        assert!(matches!(
            bind(&state, "1", Path::new("/home/me/Other"), ROOT_ID),
            Err(Error::OtherFolder { .. })
        ));
        assert!(matches!(
            bind(&state, "1", folder, "FOLDER::com.apple.CloudDocs::F1"),
            Err(Error::OtherTop { .. })
        ));
        assert!(matches!(
            bind(&state, "2", folder, ROOT_ID),
            Err(Error::OtherAccount { .. })
        ));
        // A state from before the iCloud folder was recorded is claimed by
        // the first sync, as one from before the local folder was.
        let older = State::in_memory();
        older.set_meta("folder", &folder.to_string_lossy()).unwrap();
        older.set_meta("dsid", "1").unwrap();
        bind(&older, "1", folder, "FOLDER::com.apple.CloudDocs::F1").unwrap();
        assert!(matches!(
            bind(&older, "1", folder, ROOT_ID),
            Err(Error::OtherTop { .. })
        ));
    }
}
