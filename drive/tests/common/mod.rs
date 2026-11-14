//! What the engine tests share: a real temp folder and an in-memory iCloud
//! Drive that behaves as the real one was measured to: ids survive
//! renames and content changes; a folder's etag changes when something
//! below it is added, removed or changes size, and not for a rename or
//! same-size content; a change feed names what changed; a vanished folder
//! lists as an error; and removed items go to a trash.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use icloud_drive::engine::{self, Engine, Error, Outcome};
use icloud_drive::local;
use icloud_drive::remote::{self, Kind, Node, ROOT_ID, Remote};
use icloud_drive::state::State;

#[derive(Default)]
pub struct Cloud {
    pub nodes: HashMap<String, Node>,
    pub content: HashMap<String, Vec<u8>>,
    pub trashed: Vec<String>,
    pub packages: Vec<String>,
    pub broken_listing: Option<String>,
    /// The listing right after a replace fails, once.
    pub listing_fails_after_replace: bool,
    pub listing_fails_once: bool,
    /// iCloud refuses every rename and move.
    pub refuses_moves: bool,
    /// Every rename and move fails for want of a network.
    pub moves_offline: bool,
    /// Names iCloud refuses for a new item or a rename.
    pub refused_names: Vec<String>,
    /// The next rename is refused, once.
    pub rename_fails_once: bool,
    /// Removed when a replace is refused as stale: the file went meanwhile.
    pub stale_removes: Option<PathBuf>,
    pub counter: u64,
    pub uploads: usize,
    pub listings: usize,
    /// The change feed: (is a folder, docwsid, parent docwsid).
    pub feed: Vec<(bool, String, String)>,
    pub feed_expired: bool,
    // What was not measured of the real Drive, so the model tries both ways.
    /// A rename or a move leaves the item's own etag as it was.
    pub moves_keep_etag: bool,
    /// Listings come in an order of this seed's making, not by name.
    pub shuffle: Option<u64>,
    /// How a refused move or rename is answered: 0 a status that is not
    /// `OK`, 1 an HTTP 400, 2 an answer with no status.
    pub refusal: u8,
    /// Names iCloud never takes, for anything.
    pub never_names: Vec<String>,
    /// The listing of this number (counting from 0 in `listings`) fails.
    pub fail_listing_at: Option<usize>,
    /// Done to iCloud by another device when `listings` reaches the number.
    pub mid_listing: Option<(usize, MidOp)>,
    /// What `mid_listing` did: the texts it removed, and whether it ran.
    pub mid_fired: Option<Vec<String>>,
    /// Written here by the person while a download is under way: the text
    /// and its date, at the name of the file coming down.
    pub during_download: Option<(String, i64)>,
    /// What `during_download` did: the text it replaced, if any.
    pub download_edited: Option<Option<String>>,
    /// A replace of an item in Recently Deleted is taken (not refused).
    pub replaces_trashed: bool,
    /// What such replaces sent.
    pub replaced_in_trash: Vec<Vec<u8>>,
}

/// A change another device makes while a sync is listing.
#[derive(Debug, Clone)]
pub enum MidOp {
    Edit {
        id: String,
        text: String,
        mtime_ms: i64,
    },
    Remove {
        id: String,
    },
    /// Into `parent` (none: where it is) under `name`.
    Move {
        id: String,
        parent: Option<String>,
        name: String,
    },
}

pub fn docwsid_of(id: &str) -> String {
    id.rsplit("::").next().unwrap().to_string()
}

#[derive(Default)]
pub struct Fake(pub RefCell<Cloud>);

impl Cloud {
    pub fn may_move(&self) -> remote::Result<()> {
        if self.moves_offline {
            return Err(icloud_session::Error::Offline("no network".into()).into());
        }
        if self.refuses_moves {
            return Err(self.refused("NAME_NOT_ALLOWED"));
        }
        Ok(())
    }

    /// A refused rename, move or new folder, answered as `refusal` says.
    pub fn refused(&self, why: &str) -> remote::Error {
        match self.refusal {
            1 => icloud_session::Error::Http {
                status: 400,
                body: why.into(),
            }
            .into(),
            2 => remote::Error::Api("the move answered with no status".into()),
            _ => remote::Error::Refused(format!("refused: {why}")),
        }
    }

    /// iCloud refuses to put anything in a folder that is gone.
    pub fn parent_exists(&self, parent_id: &str) -> remote::Result<()> {
        if parent_id != ROOT_ID && !self.nodes.contains_key(parent_id) {
            return Err(remote::Error::Refused("refused: ID_INVALID".into()));
        }
        Ok(())
    }

    pub fn may_name(&self, name: &str) -> remote::Result<()> {
        if self.refused_names.iter().chain(&self.never_names).any(|n| n == name) {
            return Err(self.refused("NAME_NOT_ALLOWED"));
        }
        Ok(())
    }

    pub fn edit(&mut self, id: &str, bytes: &[u8], mtime_ms: i64) {
        self.content.insert(id.to_string(), bytes.to_vec());
        let node = self.nodes.get_mut(id).unwrap();
        let resized = node.size != bytes.len() as u64;
        node.size = bytes.len() as u64;
        node.mtime_ms = mtime_ms;
        if resized {
            self.touch(id)
        } else {
            self.touch_item(id)
        }
        // A content edit is a documentContent record, which names no parent.
        self.feed.last_mut().unwrap().2.clear();
    }

    pub fn rename_item(&mut self, id: &str, name: &str) {
        self.nodes.get_mut(id).unwrap().name = name.to_string();
        if self.moves_keep_etag {
            self.note(id)
        } else {
            self.touch_item(id)
        }
    }

    pub fn move_item(&mut self, id: &str, parent_id: &str) {
        let old = self.nodes[id].parent.clone();
        self.nodes.get_mut(id).unwrap().parent = parent_id.to_string();
        self.touch(&old);
        if self.moves_keep_etag {
            self.note(id);
            self.touch(parent_id);
        } else {
            self.touch(id);
        }
    }

    /// Whether `id` may go into `parent` under `name`: the folder is there,
    /// is not the item or below it, and holds nothing of that name.
    pub fn may_go(&self, id: &str, parent: &str, name: &str) -> bool {
        let mut at = parent.to_string();
        while let Some(n) = self.nodes.get(&at) {
            if n.id == id {
                return false;
            }
            at = n.parent.clone();
        }
        (parent == ROOT_ID || self.nodes.get(parent).is_some_and(|n| n.kind == Kind::Folder))
            && !self
                .nodes
                .values()
                .any(|n| n.parent == parent && n.id != id && n.name.to_lowercase() == name.to_lowercase())
    }

    /// Carries out `mid_listing` once its listing is reached.
    fn mid_listing(&mut self) {
        let Some((at, op)) = self.mid_listing.clone() else {
            return;
        };
        if self.listings < at {
            return;
        }
        self.mid_listing = None;
        let removed = match op {
            MidOp::Edit { id, text, mtime_ms } => match self.nodes.get(&id) {
                Some(n) if n.kind == Kind::File => {
                    let old = String::from_utf8(self.content[&id].clone()).unwrap();
                    self.edit(&id, text.as_bytes(), mtime_ms);
                    vec![old]
                }
                _ => return,
            },
            MidOp::Remove { id } => {
                if !self.nodes.contains_key(&id) {
                    return;
                }
                let before = self.trashed.len();
                self.remove(&id);
                self.trashed[before..]
                    .iter()
                    .filter_map(|t| self.content.get(t))
                    .map(|b| String::from_utf8(b.clone()).unwrap())
                    .collect()
            }
            MidOp::Move { id, parent, name } => {
                let Some(node) = self.nodes.get(&id) else {
                    return;
                };
                let parent = parent.unwrap_or_else(|| node.parent.clone());
                if !self.may_go(&id, &parent, &name) {
                    return;
                }
                if self.nodes[&id].parent != parent {
                    self.move_item(&id, &parent);
                }
                self.rename_item(&id, &name);
                Vec::new()
            }
        };
        self.mid_fired = Some(removed);
    }

    pub fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Every change is in the feed.
    pub fn note(&mut self, id: &str) {
        if let Some(node) = self.nodes.get(id) {
            let entry = (
                node.kind == Kind::Folder,
                node.docwsid.clone(),
                docwsid_of(&node.parent),
            );
            self.feed.push(entry);
        } else {
            self.feed
                .push((id.starts_with("FOLDER"), docwsid_of(id), String::new()));
        }
    }

    /// A rename or same-size content: only the item's own etag changes.
    pub fn touch_item(&mut self, id: &str) {
        let n = self.tick();
        if let Some(node) = self.nodes.get_mut(id) {
            node.etag = format!("e{n}");
        }
        self.note(id);
    }

    /// Something added, removed or resized at `id`: the etag of it and of
    /// every folder above it changes.
    pub fn touch(&mut self, id: &str) {
        self.note(id);
        let mut at = id.to_string();
        while let Some(parent) = {
            let n = self.tick();
            self.nodes.get_mut(&at).map(|node| {
                node.etag = format!("e{n}");
                node.parent.clone()
            })
        } {
            at = parent;
        }
    }

    pub fn add(&mut self, parent: &str, name: &str, kind: Kind, bytes: &[u8], mtime_ms: i64) -> Node {
        let n = self.tick();
        let (prefix, docwsid) = if kind == Kind::File {
            ("FILE", format!("D{n}"))
        } else {
            ("FOLDER", format!("F{n}"))
        };
        let node = Node {
            id: format!("{prefix}::com.apple.CloudDocs::{docwsid}"),
            docwsid,
            zone: "com.apple.CloudDocs".into(),
            parent: parent.to_string(),
            name: name.to_string(),
            kind,
            etag: String::new(),
            size: bytes.len() as u64,
            mtime_ms,
        };
        self.nodes.insert(node.id.clone(), node.clone());
        if kind == Kind::File {
            self.content.insert(node.id.clone(), bytes.to_vec());
        }
        self.touch(&node.id);
        self.nodes[&node.id].clone()
    }

    pub fn path_of(&self, id: &str) -> String {
        let n = &self.nodes[id];
        let mut parts = vec![n.name.clone()];
        let mut at = n.parent.clone();
        while let Some(p) = self.nodes.get(&at) {
            parts.push(p.name.clone());
            at = p.parent.clone();
        }
        parts.reverse();
        parts.join("/")
    }

    pub fn remove(&mut self, id: &str) {
        let parent = self.nodes[id].parent.clone();
        let mut gone = vec![id.to_string()];
        while let Some(next) = gone.pop() {
            let below: Vec<String> = self
                .nodes
                .values()
                .filter(|n| n.parent == next)
                .map(|n| n.id.clone())
                .collect();
            gone.extend(below);
            self.nodes.remove(&next);
            self.note(&next);
            self.trashed.push(next);
        }
        self.touch(&parent);
    }
}

impl Fake {
    // What another device does.
    pub fn file(&self, parent: &str, name: &str, text: &str, mtime_ms: i64) -> Node {
        self.0
            .borrow_mut()
            .add(parent, name, Kind::File, text.as_bytes(), mtime_ms)
    }
    pub fn folder(&self, parent: &str, name: &str) -> Node {
        self.0.borrow_mut().add(parent, name, Kind::Folder, b"", 0)
    }
    pub fn edit(&self, id: &str, text: &str, mtime_ms: i64) {
        self.0.borrow_mut().edit(id, text.as_bytes(), mtime_ms);
    }
    pub fn rename_item(&self, id: &str, name: &str) {
        self.0.borrow_mut().rename_item(id, name);
    }
    pub fn remove(&self, id: &str) {
        self.0.borrow_mut().remove(id);
    }
    pub fn id_at(&self, path: &str) -> Option<String> {
        let c = self.0.borrow();
        let mut at = ROOT_ID.to_string();
        for name in path.split('/') {
            at = c.nodes.values().find(|n| n.parent == at && n.name == name)?.id.clone();
        }
        Some(at)
    }
    pub fn text(&self, path: &str) -> Option<String> {
        let id = self.id_at(path)?;
        Some(String::from_utf8(self.0.borrow().content[&id].clone()).unwrap())
    }
    /// Every path in iCloud with its text, `None` for a folder.
    pub fn tree(&self) -> BTreeMap<String, Option<String>> {
        let c = self.0.borrow();
        c.nodes
            .values()
            .map(|n| {
                let text = (n.kind == Kind::File).then(|| String::from_utf8(c.content[&n.id].clone()).unwrap());
                (c.path_of(&n.id), text)
            })
            .collect()
    }
    /// The text of every file in Recently Deleted, sorted.
    pub fn trashed_texts(&self) -> Vec<String> {
        let c = self.0.borrow();
        let mut out: Vec<String> = c
            .trashed
            .iter()
            .filter_map(|id| c.content.get(id))
            .map(|b| String::from_utf8(b.clone()).unwrap())
            .collect();
        out.sort();
        out
    }
    pub fn paths(&self) -> Vec<String> {
        let c = self.0.borrow();
        let mut out: Vec<String> = c
            .nodes
            .values()
            .map(|n| {
                let mut parts = vec![n.name.clone()];
                let mut at = n.parent.clone();
                while let Some(p) = c.nodes.get(&at) {
                    parts.push(p.name.clone());
                    at = p.parent.clone();
                }
                parts.reverse();
                parts.join("/")
            })
            .collect();
        out.sort();
        out
    }
}

impl Remote for Fake {
    fn changes(&self, token: Option<&str>) -> remote::Result<remote::Changes> {
        let mut c = self.0.borrow_mut();
        if token.is_some() && c.feed_expired {
            c.feed_expired = false;
            return Err(remote::Error::FeedReset);
        }
        let from = token.map_or(c.feed.len(), |t| t.parse::<usize>().unwrap());
        let mut changes = remote::Changes {
            token: c.feed.len().to_string(),
            ..Default::default()
        };
        for (is_folder, id, parent) in &c.feed[from..] {
            if *is_folder {
                changes.folders.push(id.clone());
            } else {
                changes.files.push(id.clone());
            }
            if !parent.is_empty() {
                changes.folders.push(parent.clone());
            }
        }
        Ok(changes)
    }
    fn list(&self, folder_id: &str) -> remote::Result<(Node, Vec<Node>)> {
        let mut c = self.0.borrow_mut();
        c.mid_listing();
        c.listings += 1;
        let gone = || remote::Error::Listing {
            id: folder_id.to_string(),
            status: "ID_INVALID".into(),
        };
        if c.broken_listing.as_deref() == Some(folder_id)
            || std::mem::take(&mut c.listing_fails_once)
            || c.fail_listing_at.is_some_and(|at| c.listings == at + 1)
        {
            return Err(gone());
        }
        let folder = if folder_id == ROOT_ID {
            engine::top_node(ROOT_ID)
        } else {
            c.nodes.get(folder_id).cloned().ok_or_else(gone)?
        };
        let mut kids: Vec<Node> = c.nodes.values().filter(|n| n.parent == folder_id).cloned().collect();
        kids.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(seed) = c.shuffle {
            let n = c.listings as u64;
            kids.sort_by_key(|k| {
                let mut h = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&(seed, n, &k.id), &mut h);
                std::hash::Hasher::finish(&h)
            });
        }
        Ok((folder, kids))
    }
    fn create_folder(&self, parent_id: &str, name: &str) -> remote::Result<Node> {
        let mut c = self.0.borrow_mut();
        c.may_name(name)?;
        c.parent_exists(parent_id)?;
        Ok(c.add(parent_id, name, Kind::Folder, b"", 0))
    }
    fn upload_new(&self, parent: &Node, name: &str, path: &Path, mtime_ms: i64) -> remote::Result<Node> {
        let bytes = fs::read(path)?;
        let mut c = self.0.borrow_mut();
        if c.refused_names.iter().chain(&c.never_names).any(|n| n == name) {
            return Err(remote::Error::Api("upload refused: NAME_NOT_ALLOWED".into()));
        }
        c.parent_exists(&parent.id)?;
        c.uploads += 1;
        Ok(c.add(&parent.id, name, Kind::File, &bytes, mtime_ms))
    }
    fn replace(&self, node: &Node, path: &Path, mtime_ms: i64) -> remote::Result<Node> {
        let bytes = fs::read(path)?;
        let mut c = self.0.borrow_mut();
        // `allow_conflict: false`: a stale etag is refused, as iCloud does.
        if c.nodes.get(&node.id).is_some_and(|n| n.etag != node.etag) {
            if let Some(gone) = c.stale_removes.take() {
                fs::remove_file(gone)?;
            }
            return Err(remote::Error::Stale(node.name.clone()));
        }
        if !c.nodes.contains_key(&node.id) {
            if c.replaces_trashed && c.trashed.contains(&node.id) {
                // Taken, in Recently Deleted. Kept apart, so that what the
                // trash holds is still what was removed.
                c.replaced_in_trash.push(bytes);
                let n = c.tick();
                return Ok(Node {
                    etag: format!("e{n}"),
                    size: c.replaced_in_trash.last().unwrap().len() as u64,
                    mtime_ms: mtime_ms + 5_000,
                    ..node.clone()
                });
            }
            return Err(remote::Error::Api("gone".into()));
        }
        c.uploads += 1;
        c.listing_fails_once = c.listing_fails_after_replace;
        c.content.insert(node.id.clone(), bytes.clone());
        let n = c.nodes.get_mut(&node.id).unwrap();
        let resized = n.size != bytes.len() as u64;
        n.size = bytes.len() as u64;
        // A replaced file gets the time of the replacement, not the one sent.
        n.mtime_ms = mtime_ms + 5_000;
        if resized {
            c.touch(&node.id)
        } else {
            c.touch_item(&node.id)
        }
        Ok(c.nodes[&node.id].clone())
    }
    fn rename(&self, node: &Node, name: &str) -> remote::Result<Node> {
        let mut c = self.0.borrow_mut();
        c.may_move()?;
        c.may_name(name)?;
        if std::mem::take(&mut c.rename_fails_once) {
            return Err(remote::Error::Refused("the rename refused: UNKNOWN".into()));
        }
        if !c.nodes.contains_key(&node.id) {
            return Err(remote::Error::Api("gone".into()));
        }
        c.rename_item(&node.id, name);
        Ok(c.nodes[&node.id].clone())
    }
    fn move_to(&self, node: &Node, parent_id: &str) -> remote::Result<Node> {
        let mut c = self.0.borrow_mut();
        c.may_move()?;
        if parent_id != ROOT_ID && !c.nodes.contains_key(parent_id) {
            return Err(c.refused("ID_INVALID"));
        }
        // Not into itself or below it.
        let mut at = parent_id.to_string();
        while let Some(n) = c.nodes.get(&at) {
            if n.id == node.id {
                return Err(c.refused("INVALID_DESTINATION"));
            }
            at = n.parent.clone();
        }
        c.move_item(&node.id, parent_id);
        Ok(c.nodes[&node.id].clone())
    }
    fn trash(&self, node: &Node) -> remote::Result<()> {
        let mut c = self.0.borrow_mut();
        if !c.nodes.contains_key(&node.id) {
            return Err(remote::Error::Api(format!(
                "moving {} to Recently Deleted refused: ID_INVALID",
                node.name
            )));
        }
        c.remove(&node.id);
        Ok(())
    }
    fn download(&self, node: &Node, dest: &Path) -> remote::Result<()> {
        let c = self.0.borrow();
        if c.packages.contains(&node.id) {
            return Err(remote::Error::Package(node.name.clone()));
        }
        fs::write(dest, &c.content[&node.id])?;
        drop(c);
        // The person saves the file here while it comes down.
        let during = self.0.borrow_mut().during_download.take();
        if let Some((text, mtime_ms)) = during {
            let path = dest.with_file_name(&node.name);
            if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                self.0.borrow_mut().during_download = Some((text, mtime_ms));
                return Ok(());
            }
            let old = fs::read_to_string(&path).ok();
            fs::write(&path, &text)?;
            local::set_mtime(&path, mtime_ms)?;
            self.0.borrow_mut().download_edited = Some(old);
        }
        Ok(())
    }
}

pub struct Bed {
    pub dir: tempfile::TempDir,
    pub cloud: Fake,
    pub clock: RefCell<i64>,
    /// Each operation a sync started, in order.
    pub started: RefCell<Vec<String>>,
}

impl Bed {
    pub fn new() -> Bed {
        Bed {
            dir: tempfile::tempdir().unwrap(),
            cloud: Fake::default(),
            clock: RefCell::new(1_600_000_000_000),
            started: RefCell::default(),
        }
    }
    pub fn root(&self) -> PathBuf {
        self.dir.path().join("iCloud Drive")
    }
    /// A time in the past, later than the last one handed out.
    pub fn time(&self) -> i64 {
        *self.clock.borrow_mut() += 10_000;
        *self.clock.borrow()
    }
    /// Writes a local file as if some time ago.
    pub fn write(&self, path: &str, text: &str) {
        let full = self.root().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, text).unwrap();
        local::set_mtime(&full, self.time()).unwrap();
    }
    /// Everything in the folder, hidden names too, with each file's text
    /// (`None` for a folder).
    pub fn local_tree(&self) -> BTreeMap<String, Option<String>> {
        let mut out = BTreeMap::new();
        walk_texts(&self.root(), "", &mut out);
        out
    }
    /// The text of every file in the local trash, sorted.
    pub fn trashed_here(&self) -> Vec<String> {
        let mut out = BTreeMap::new();
        walk_texts(&self.dir.path().join("Trash/files"), "", &mut out);
        let mut texts: Vec<String> = out.into_values().flatten().collect();
        texts.sort();
        texts
    }
    pub fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.root().join(path)).ok()
    }
    pub fn local_paths(&self) -> Vec<String> {
        local::scan(&self.root()).unwrap().0.into_keys().collect()
    }
    pub fn try_sync(&self, allow_deletes: bool) -> Result<Outcome, Error> {
        self.run(false, allow_deletes)
    }
    pub fn run(&self, dry_run: bool, allow_deletes: bool) -> Result<Outcome, Error> {
        let mut engine = Engine {
            remote: &self.cloud,
            state: State::open(&self.dir.path().join("state")).unwrap(),
            root: self.root(),
            top: engine::top_node(ROOT_ID),
            trash_dir: self.dir.path().join("Trash"),
            host: "desk".into(),
            progress: Box::new(|line| self.started.borrow_mut().push(line.to_string())),
        };
        engine.sync(dry_run, allow_deletes)
    }
    pub fn not_synced(&self) -> Vec<String> {
        let state = State::open(&self.dir.path().join("state")).unwrap();
        state.not_synced().unwrap().into_iter().map(|(path, _)| path).collect()
    }
    pub fn sync(&self) -> Outcome {
        let outcome = self.try_sync(false).unwrap();
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        outcome
    }
    /// Syncs twice, the second finding nothing to do, with iCloud left
    /// holding `cloud` (which may include what is never synced).
    pub fn settle_paths(&self, cloud: &[&str]) {
        self.sync();
        let again = self.sync();
        assert!(again.done.is_empty(), "not settled: {:?}", again.done);
        assert_eq!(self.cloud.paths(), cloud);
    }
    /// Syncs, then checks that a second sync finds nothing to do and that
    /// both sides hold the same paths.
    pub fn settle(&self) -> Outcome {
        let outcome = self.sync();
        let again = self.sync();
        assert!(again.done.is_empty(), "not settled: {:?}", again.done);
        assert_eq!(self.local_paths(), self.cloud.paths());
        outcome
    }
}

fn walk_texts(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Option<String>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type().unwrap().is_dir() {
            out.insert(path.clone(), None);
            walk_texts(&entry.path(), &path, out);
        } else {
            out.insert(path, Some(fs::read_to_string(entry.path()).unwrap_or_default()));
        }
    }
}

/// A stop at a crash point is a panic; it is expected, so it is not printed.
pub fn quiet() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<icloud_drive::crash::Crash>().is_none() {
                default(info)
            }
        }));
    });
}

/// Runs a sync with the crash hook `arm`ed; the point it stopped at.
pub fn crash_at(bed: &Bed, allow_deletes: bool, arm: impl FnOnce()) -> Option<&'static str> {
    quiet();
    arm();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bed.try_sync(allow_deletes)));
    icloud_drive::crash::disarm();
    match result {
        Ok(done) => {
            done.unwrap();
            None
        }
        Err(payload) => Some(
            payload
                .downcast_ref::<icloud_drive::crash::Crash>()
                .expect("a stop at a crash point, not a failure")
                .0,
        ),
    }
}
