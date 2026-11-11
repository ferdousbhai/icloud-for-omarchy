//! One sync: list iCloud Drive, scan the folder, plan, and carry the plan
//! out, a round at a time until nothing is left. Each operation records what
//! it did as it finishes, so a run that stops half way (no network, a sign
//! out, a crash) is picked up by the next from wherever both sides stand.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::local::{self, Entry};
use crate::plan::{self, Op, Plan, Snapshot};
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
}

pub type Result<T> = std::result::Result<T, Error>;

/// Rounds one sync may take before it leaves the rest to the next.
const MAX_ROUNDS: usize = 12;
/// Removals a sync does without being told to, whatever the folder's size.
const FREE_DELETIONS: usize = 20;
/// Folders the change feed does not name are trusted from the last listing
/// for this long, then everything is listed again.
const FULL_LISTING_EVERY_MS: i64 = 6 * 3600 * 1000;

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

fn now_ms() -> i64 {
    icloud_session::time::now_ms()
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn describe(op: &Op) -> String {
    match op {
        Op::LocalMove { from, to, .. } => format!("move {from} to {to} (as in iCloud)"),
        Op::RemoteMove { to, .. } => format!("move to {to} in iCloud"),
        Op::SetAside { path } => format!("keep {path} as a conflicted copy"),
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
        Error::Remote(remote::Error::Api(_) | remote::Error::Package(_) | remote::Error::Listing { .. }) => true,
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
    fn crawl(&mut self) -> Result<Vec<Node>> {
        let fresh_enough = self
            .state
            .meta("full_listing_ms")?
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|at| now_ms() - at < FULL_LISTING_EVERY_MS);
        let token = self.state.meta("feed_token")?;
        // Asked before listing, so a change made while listing is named
        // again next time rather than missed.
        let (changes, trust_cache) = match self.remote.changes(token.as_deref()) {
            Ok(changes) => (changes, fresh_enough && token.is_some()),
            Err(remote::Error::FeedReset) => (self.remote.changes(None)?, false),
            Err(e) => return Err(e.into()),
        };
        let cached = if trust_cache {
            self.state.cached_remote()?
        } else {
            Vec::new()
        };
        let by_id: HashMap<&str, &Node> = cached.iter().map(|n| (n.id.as_str(), n)).collect();
        let by_docwsid: HashMap<&str, &Node> = cached.iter().map(|n| (n.docwsid.as_str(), n)).collect();
        let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
        for node in &cached {
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
        if !fresh_enough {
            self.state.set_meta("full_listing_ms", &now_ms().to_string())?;
        }
        self.state.store_remote(&out)?;
        self.state.set_meta("feed_token", &changes.token)?;
        Ok(out)
    }

    /// Brings the folder and iCloud Drive in step. With `dry_run`, only says
    /// what the first round would do.
    pub fn sync(&mut self, dry_run: bool, allow_deletes: bool) -> Result<Outcome> {
        let mut outcome = Outcome {
            dry_run,
            ..Outcome::default()
        };
        let had_rows = !self.state.rows()?.is_empty();
        if !self.root.is_dir() {
            if had_rows {
                return Err(Error::FolderMissing(self.root.clone()));
            }
            if !dry_run {
                fs::create_dir_all(&self.root)?;
            }
        }
        for _ in 0..MAX_ROUNDS {
            let nodes = self.crawl()?;
            let snapshot = Snapshot::build_below(&self.top.id, &nodes);
            let (entries, skipped) = if self.root.is_dir() {
                local::scan(&self.root)?
            } else {
                Default::default()
            };
            let base = self.state.rows()?;
            let context = plan::Context {
                ignored: self.state.ignored()?,
                unknown: skipped
                    .iter()
                    .map(|s| s.path.clone())
                    .chain(snapshot.skipped.iter().map(|(path, _)| path.clone()))
                    .collect(),
                now_ns: now_ms() * 1_000_000,
            };
            let plan = plan::plan(&base, &snapshot, &entries, &context);

            outcome.pending = plan.pending.clone();
            outcome.skipped = skipped
                .iter()
                .map(|s| format!("{}: {}", s.path, s.reason))
                .chain(snapshot.skipped.iter().map(|(p, why)| format!("{p}: {why}")))
                .chain(
                    self.state
                        .ignored_list()?
                        .into_iter()
                        .map(|(p, why)| format!("{p}: {why}")),
                )
                .collect();

            let allowed = FREE_DELETIONS.max(base.len() / 4);
            if plan.deletions > allowed && !allow_deletes {
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
            self.apply(&plan, &snapshot, &mut outcome)?;
            // Another round only if this one got somewhere.
            if !plan.again || outcome.done.len() == before {
                break;
            }
        }
        self.state.set_meta("last_sync_ms", &now_ms().to_string())?;
        Ok(outcome)
    }

    fn apply(&mut self, plan: &Plan, snapshot: &Snapshot, outcome: &mut Outcome) -> Result<()> {
        for op in &plan.ops {
            let line = describe(op);
            (self.progress)(&line);
            match self.apply_one(op, snapshot) {
                Ok(true) => outcome.done.push(line),
                Ok(false) => {}
                Err(e) if is_item_problem(&e) => outcome.problems.push(format!("{line}: {e}")),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn abs(&self, path: &str) -> PathBuf {
        self.root.join(path)
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

    /// The item as its folder's listing has it now: the sizes and dates
    /// every later comparison is made against.
    fn listed(&self, parent_id: &str, id: &str) -> Result<Node> {
        let (_, kids) = self.remote.list(parent_id)?;
        kids.into_iter()
            .find(|k| k.id == id)
            .ok_or_else(|| Error::Remote(remote::Error::Api("the item is not in its folder's listing".into())))
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
                    if self.stat(to)?.is_some() || self.stat(from)?.is_none() {
                        return Ok(false);
                    }
                    if let Some(parent) = self.abs(to).parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(self.abs(from), self.abs(to))?;
                }
                self.state.move_paths(&row.path, to)?;
                self.state.put(&Row {
                    node,
                    path: to.clone(),
                    local: row.local,
                })?;
            }
            Op::RemoteMove { id, to } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                let entry = self.need_stat(to)?;
                let parent = self.parent_node(to)?;
                let mut node = row.node.clone();
                if node.parent != parent.id {
                    node = self.remote.move_to(&node, &parent.id)?;
                }
                if node.name != name_of(to) {
                    node = self.remote.rename(&node, name_of(to))?;
                }
                self.state.move_paths(&row.path, to)?;
                self.state.put(&Row {
                    node,
                    path: to.clone(),
                    // A file keeps the stamp it was in step with, so an edit
                    // made along with the move still shows as one.
                    local: if entry.is_dir {
                        entry
                    } else {
                        Entry {
                            ino: entry.ino,
                            ..row.local
                        }
                    },
                })?;
            }
            Op::SetAside { path } => {
                let from = self.abs(path);
                if !from.exists() {
                    return Ok(false);
                }
                let date = &icloud_session::time::rfc3339(now_ms() / 1000)[..10];
                fs::rename(&from, local::conflict_path(&from, &self.host, date))?;
            }
            Op::MkdirLocal { id, path } => {
                let node = node_of(id)?;
                fs::create_dir_all(self.abs(path))?;
                let local = self.need_stat(path)?;
                self.state.put(&Row {
                    node,
                    path: path.clone(),
                    local,
                })?;
            }
            Op::MkdirRemote { path } => {
                let local = self.need_stat(path)?;
                let parent = self.parent_node(path)?;
                let node = self.remote.create_folder(&parent.id, name_of(path))?;
                self.state.put(&Row {
                    node,
                    path: path.clone(),
                    local,
                })?;
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
                if !safe {
                    return Ok(false);
                }
                let dest = self.abs(path);
                let temp = dest.with_file_name(format!(".icloud-drive-{}.part", uuid::Uuid::new_v4().simple()));
                let fetched = self.remote.download(&node, &temp).and_then(|()| {
                    if node.mtime_ms > 0 {
                        local::set_mtime(&temp, node.mtime_ms)?;
                    }
                    Ok(())
                });
                match fetched {
                    Ok(()) => {}
                    Err(remote::Error::Package(_)) => {
                        let _ = fs::remove_file(&temp);
                        self.state
                            .ignore(&node, path, "a package (a Pages document, say), which is not synced")?;
                        return Ok(false);
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&temp);
                        return Err(e.into());
                    }
                }
                fs::rename(&temp, &dest)?;
                let local = self.need_stat(path)?;
                self.state.put(&Row {
                    node,
                    path: path.clone(),
                    local,
                })?;
            }
            Op::Upload { path } => {
                let before = self.need_stat(path)?;
                let parent = self.parent_node(path)?;
                let sent = self.remote.upload_new(
                    &parent,
                    name_of(path),
                    &self.abs(path),
                    before.mtime_ns / 1_000_000_000 * 1000,
                )?;
                let node = self.listed(&parent.id, &sent.id)?;
                // Recorded as it was before the upload: a change made while
                // it was going up shows as a change next time.
                self.state.put(&Row {
                    node,
                    path: path.clone(),
                    local: before,
                })?;
            }
            Op::Replace { id, path } => {
                let Some(row) = self.state.row(id)? else {
                    return Ok(false);
                };
                let before = self.need_stat(path)?;
                let sent = self
                    .remote
                    .replace(&row.node, &self.abs(path), before.mtime_ns / 1_000_000_000 * 1000)?;
                let node = self.listed(&row.node.parent, &sent.id)?;
                self.state.put(&Row {
                    node,
                    path: path.clone(),
                    local: before,
                })?;
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
                if self.stat(path)?.is_some() {
                    // It is back.
                    return Ok(false);
                }
                self.remote.trash(&node_of(id).unwrap_or(row.node))?;
                self.state.forget_under(path)?;
            }
            Op::Adopt { id, path } => {
                let node = node_of(id)?;
                let local = self.need_stat(path)?;
                self.state.put(&Row {
                    node,
                    path: path.clone(),
                    local,
                })?;
            }
            Op::Forget { id, .. } => self.state.forget(id)?,
        }
        Ok(true)
    }

    fn trash(&self, path: &str) -> Result<()> {
        let stamp = icloud_session::time::rfc3339(now_ms() / 1000);
        match local::trash(&self.abs(path), &self.trash_dir, &stamp) {
            Ok(_) => Ok(()),
            // The trash is on another file system: keep it beside the folder.
            Err(e) if e.raw_os_error() == Some(18) => {
                let beside = self.root.with_file_name(format!(
                    ".{} removed",
                    self.root.file_name().unwrap_or_default().to_string_lossy()
                ));
                local::trash(&self.abs(path), &beside, &stamp)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Where the state for a local folder lives when nothing says otherwise.
pub fn default_state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/state"));
    base.join("icloud-drive")
}

pub fn default_trash_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/share"));
    base.join("Trash")
}

pub fn default_folder() -> PathBuf {
    home().join("iCloud Drive")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Refuses a state that belongs to another account; claims a new one.
pub fn bind_account(state: &State, dsid: &str, folder: &Path) -> Result<()> {
    match state.meta("dsid")? {
        Some(owner) if owner != dsid => Err(Error::OtherAccount {
            folder: folder.to_path_buf(),
        }),
        Some(_) => Ok(()),
        None => Ok(state.set_meta("dsid", dsid)?),
    }
}
