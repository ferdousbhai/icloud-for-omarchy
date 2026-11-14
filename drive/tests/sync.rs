//! The engine end to end, against a real temp folder and an in-memory
//! iCloud Drive that behaves as the real one was measured to: ids survive
//! renames and content changes; a folder's etag changes when something
//! below it is added, removed or changes size, and not for a rename or
//! same-size content; a change feed names what changed; a vanished folder
//! lists as an error; and removed items go to a trash.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use icloud_drive::engine::{self, Engine, Error, Outcome};
use icloud_drive::local;
use icloud_drive::remote::{self, Kind, Node, ROOT_ID, Remote};
use icloud_drive::state::State;

#[derive(Default)]
struct Cloud {
    nodes: HashMap<String, Node>,
    content: HashMap<String, Vec<u8>>,
    trashed: Vec<String>,
    packages: Vec<String>,
    broken_listing: Option<String>,
    /// The listing right after a replace fails, once.
    listing_fails_after_replace: bool,
    listing_fails_once: bool,
    counter: u64,
    uploads: usize,
    listings: usize,
    /// The change feed: (is a folder, docwsid, parent docwsid).
    feed: Vec<(bool, String, String)>,
    feed_expired: bool,
}

fn docwsid_of(id: &str) -> String {
    id.rsplit("::").next().unwrap().to_string()
}

#[derive(Default)]
struct Fake(RefCell<Cloud>);

impl Cloud {
    fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Every change is in the feed.
    fn note(&mut self, id: &str) {
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
    fn touch_item(&mut self, id: &str) {
        let n = self.tick();
        if let Some(node) = self.nodes.get_mut(id) {
            node.etag = format!("e{n}");
        }
        self.note(id);
    }

    /// Something added, removed or resized at `id`: the etag of it and of
    /// every folder above it changes.
    fn touch(&mut self, id: &str) {
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

    fn add(&mut self, parent: &str, name: &str, kind: Kind, bytes: &[u8], mtime_ms: i64) -> Node {
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

    fn remove(&mut self, id: &str) {
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
    fn file(&self, parent: &str, name: &str, text: &str, mtime_ms: i64) -> Node {
        self.0
            .borrow_mut()
            .add(parent, name, Kind::File, text.as_bytes(), mtime_ms)
    }
    fn folder(&self, parent: &str, name: &str) -> Node {
        self.0.borrow_mut().add(parent, name, Kind::Folder, b"", 0)
    }
    fn edit(&self, id: &str, text: &str, mtime_ms: i64) {
        let mut c = self.0.borrow_mut();
        c.content.insert(id.to_string(), text.as_bytes().to_vec());
        let node = c.nodes.get_mut(id).unwrap();
        let resized = node.size != text.len() as u64;
        node.size = text.len() as u64;
        node.mtime_ms = mtime_ms;
        if resized {
            c.touch(id)
        } else {
            c.touch_item(id)
        }
        // A content edit is a documentContent record, which names no parent.
        c.feed.last_mut().unwrap().2.clear();
    }
    fn rename_item(&self, id: &str, name: &str) {
        let mut c = self.0.borrow_mut();
        c.nodes.get_mut(id).unwrap().name = name.to_string();
        c.touch_item(id);
    }
    fn remove(&self, id: &str) {
        self.0.borrow_mut().remove(id);
    }
    fn id_at(&self, path: &str) -> Option<String> {
        let c = self.0.borrow();
        let mut at = ROOT_ID.to_string();
        for name in path.split('/') {
            at = c.nodes.values().find(|n| n.parent == at && n.name == name)?.id.clone();
        }
        Some(at)
    }
    fn text(&self, path: &str) -> Option<String> {
        let id = self.id_at(path)?;
        Some(String::from_utf8(self.0.borrow().content[&id].clone()).unwrap())
    }
    fn paths(&self) -> Vec<String> {
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
        c.listings += 1;
        let gone = || remote::Error::Listing {
            id: folder_id.to_string(),
            status: "ID_INVALID".into(),
        };
        if c.broken_listing.as_deref() == Some(folder_id) || std::mem::take(&mut c.listing_fails_once) {
            return Err(gone());
        }
        let folder = if folder_id == ROOT_ID {
            engine::top_node(ROOT_ID)
        } else {
            c.nodes.get(folder_id).cloned().ok_or_else(gone)?
        };
        let mut kids: Vec<Node> = c.nodes.values().filter(|n| n.parent == folder_id).cloned().collect();
        kids.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((folder, kids))
    }
    fn create_folder(&self, parent_id: &str, name: &str) -> remote::Result<Node> {
        Ok(self.0.borrow_mut().add(parent_id, name, Kind::Folder, b"", 0))
    }
    fn upload_new(&self, parent: &Node, name: &str, path: &Path, mtime_ms: i64) -> remote::Result<Node> {
        let bytes = fs::read(path)?;
        let mut c = self.0.borrow_mut();
        c.uploads += 1;
        Ok(c.add(&parent.id, name, Kind::File, &bytes, mtime_ms))
    }
    fn replace(&self, node: &Node, path: &Path, mtime_ms: i64) -> remote::Result<Node> {
        let bytes = fs::read(path)?;
        let mut c = self.0.borrow_mut();
        // `allow_conflict: false`: a stale etag is refused, as iCloud does.
        if c.nodes.get(&node.id).is_some_and(|n| n.etag != node.etag) {
            return Err(remote::Error::Stale(node.name.clone()));
        }
        c.uploads += 1;
        c.listing_fails_once = c.listing_fails_after_replace;
        c.content.insert(node.id.clone(), bytes.clone());
        let n = c
            .nodes
            .get_mut(&node.id)
            .ok_or_else(|| remote::Error::Api("gone".into()))?;
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
        c.nodes
            .get_mut(&node.id)
            .ok_or_else(|| remote::Error::Api("gone".into()))?
            .name = name.to_string();
        c.touch_item(&node.id);
        Ok(c.nodes[&node.id].clone())
    }
    fn move_to(&self, node: &Node, parent_id: &str) -> remote::Result<Node> {
        let mut c = self.0.borrow_mut();
        let old = c.nodes[&node.id].parent.clone();
        c.nodes.get_mut(&node.id).unwrap().parent = parent_id.to_string();
        c.touch(&old);
        c.touch(&node.id);
        Ok(c.nodes[&node.id].clone())
    }
    fn trash(&self, node: &Node) -> remote::Result<()> {
        self.0.borrow_mut().remove(&node.id);
        Ok(())
    }
    fn download(&self, node: &Node, dest: &Path) -> remote::Result<()> {
        let c = self.0.borrow();
        if c.packages.contains(&node.id) {
            return Err(remote::Error::Package(node.name.clone()));
        }
        fs::write(dest, &c.content[&node.id])?;
        Ok(())
    }
}

struct Bed {
    dir: tempfile::TempDir,
    cloud: Fake,
    clock: RefCell<i64>,
}

impl Bed {
    fn new() -> Bed {
        Bed {
            dir: tempfile::tempdir().unwrap(),
            cloud: Fake::default(),
            clock: RefCell::new(1_600_000_000_000),
        }
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("iCloud Drive")
    }
    /// A time in the past, later than the last one handed out.
    fn time(&self) -> i64 {
        *self.clock.borrow_mut() += 10_000;
        *self.clock.borrow()
    }
    /// Writes a local file as if some time ago.
    fn write(&self, path: &str, text: &str) {
        let full = self.root().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, text).unwrap();
        local::set_mtime(&full, self.time()).unwrap();
    }
    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.root().join(path)).ok()
    }
    fn local_paths(&self) -> Vec<String> {
        local::scan(&self.root()).unwrap().0.into_keys().collect()
    }
    fn try_sync(&self, allow_deletes: bool) -> Result<Outcome, Error> {
        self.run(false, allow_deletes)
    }
    fn run(&self, dry_run: bool, allow_deletes: bool) -> Result<Outcome, Error> {
        let mut engine = Engine {
            remote: &self.cloud,
            state: State::open(&self.dir.path().join("state")).unwrap(),
            root: self.root(),
            top: engine::top_node(ROOT_ID),
            trash_dir: self.dir.path().join("Trash"),
            host: "desk".into(),
            progress: Box::new(|_| {}),
        };
        engine.sync(dry_run, allow_deletes)
    }
    fn not_synced(&self) -> Vec<String> {
        let state = State::open(&self.dir.path().join("state")).unwrap();
        state.not_synced().unwrap().into_iter().map(|(path, _)| path).collect()
    }
    fn sync(&self) -> Outcome {
        let outcome = self.try_sync(false).unwrap();
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        outcome
    }
    /// Syncs twice, the second finding nothing to do, with iCloud left
    /// holding `cloud` (which may include what is never synced).
    fn settle_paths(&self, cloud: &[&str]) {
        self.sync();
        let again = self.sync();
        assert!(again.done.is_empty(), "not settled: {:?}", again.done);
        assert_eq!(self.cloud.paths(), cloud);
    }
    /// Syncs, then checks that a second sync finds nothing to do and that
    /// both sides hold the same paths.
    fn settle(&self) -> Outcome {
        let outcome = self.sync();
        let again = self.sync();
        assert!(again.done.is_empty(), "not settled: {:?}", again.done);
        assert_eq!(self.local_paths(), self.cloud.paths());
        outcome
    }
}

#[test]
fn the_first_sync_brings_both_sides_together() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "from icloud", bed.time());
    bed.cloud.file(ROOT_ID, "empty.txt", "", bed.time());
    bed.write("mine/b.txt", "from here");
    bed.settle();
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("from icloud"));
    assert_eq!(bed.read("empty.txt").as_deref(), Some(""));
    assert_eq!(bed.cloud.text("mine/b.txt").as_deref(), Some("from here"));
}

#[test]
fn a_downloaded_file_keeps_iclouds_date() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "x", 1_700_000_000_000);
    bed.settle();
    let entry = local::stat(&bed.root().join("a.txt")).unwrap().unwrap();
    assert_eq!(entry.mtime_ns, 1_700_000_000_000 * 1_000_000);
}

#[test]
fn an_edit_on_either_side_reaches_the_other_and_keeps_the_id() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "two from icloud", bed.time());
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("two from icloud"));
    bed.write("a.txt", "three from here");
    bed.settle();
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("three from here"));
    assert_eq!(bed.cloud.id_at("a.txt").as_deref(), Some(a.id.as_str()));
}

#[test]
fn edits_on_both_sides_keep_both_files() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "icloud's", bed.time());
    bed.write("a.txt", "mine");
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("icloud's"));
    let copy = bed
        .local_paths()
        .into_iter()
        .find(|p| p.contains("conflicted copy desk"))
        .expect("a conflicted copy");
    assert!(copy.starts_with("a (") && copy.ends_with(").txt"), "{copy}");
    assert_eq!(bed.read(&copy).as_deref(), Some("mine"));
    assert_eq!(bed.cloud.text(&copy).as_deref(), Some("mine"));
}

#[test]
fn an_icloud_edit_its_size_and_date_hide_still_keeps_both_files() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // Same size, same date (a copy that kept its time): only the etag shows it.
    bed.cloud.edit(&a.id, "ONE", a.mtime_ms);
    bed.write("a.txt", "mine");
    bed.settle();
    assert_eq!(bed.read("a.txt").as_deref(), Some("ONE"));
    let copy = bed
        .local_paths()
        .into_iter()
        .find(|p| p.contains("conflicted copy desk"))
        .expect("a conflicted copy");
    assert_eq!(bed.read(&copy).as_deref(), Some("mine"));
    assert_eq!(bed.cloud.text(&copy).as_deref(), Some("mine"));
}

#[test]
fn a_removal_goes_to_the_other_sides_trash() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let b = bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    bed.cloud.remove(&b.id);
    bed.settle();
    assert!(bed.cloud.0.borrow().trashed.contains(&a.id));
    assert_eq!(
        fs::read_to_string(bed.dir.path().join("Trash/files/b.txt")).unwrap(),
        "two"
    );
    assert!(bed.local_paths().is_empty());
}

#[test]
fn an_edit_beats_a_removal() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let b = bed.cloud.file(ROOT_ID, "b.txt", "one", bed.time());
    bed.settle();
    // Removed in iCloud, edited here: it goes back up.
    bed.cloud.remove(&a.id);
    bed.write("a.txt", "edited here");
    // Removed here, edited in iCloud: it comes back down.
    fs::remove_file(bed.root().join("b.txt")).unwrap();
    bed.cloud.edit(&b.id, "edited there", bed.time());
    bed.settle();
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("edited here"));
    assert_eq!(bed.read("b.txt").as_deref(), Some("edited there"));
}

#[test]
fn a_rename_or_move_here_is_not_an_upload() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    bed.settle();
    let uploads = bed.cloud.0.borrow().uploads;
    fs::create_dir_all(bed.root().join("new/deeper")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("new/deeper/b.txt")).unwrap();
    fs::rename(bed.root().join("Docs"), bed.root().join("Papers")).unwrap();
    bed.settle();
    assert_eq!(
        bed.cloud.0.borrow().uploads,
        uploads,
        "moves must not send content again"
    );
    assert_eq!(bed.cloud.id_at("new/deeper/b.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.id_at("Papers").as_deref(), Some(docs.id.as_str()));
    assert_eq!(bed.cloud.text("Papers/in.txt").as_deref(), Some("inside"));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}

#[test]
fn a_folder_renamed_here_with_a_file_moved_into_it_stays_one_folder() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "zDocs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::rename(bed.root().join("zDocs"), bed.root().join("Papers")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Papers/a.txt")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.id_at("Papers").as_deref(), Some(docs.id.as_str()));
    assert_eq!(bed.cloud.id_at("Papers/a.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.paths(), ["Papers", "Papers/a.txt", "Papers/in.txt"]);
}

#[test]
fn a_folder_renamed_in_icloud_with_a_file_moved_into_it_stays_one_folder() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "in.txt", "inside", bed.time());
    let apple = bed.cloud.file(ROOT_ID, "Apple.txt", "one", bed.time());
    bed.settle();
    bed.cloud.rename_item(&docs.id, "Zed");
    let node = bed.cloud.0.borrow().nodes[&apple.id].clone();
    bed.cloud.move_to(&node, &docs.id).unwrap();
    bed.settle();
    assert_eq!(bed.local_paths(), ["Zed", "Zed/Apple.txt", "Zed/in.txt"]);
    assert_eq!(bed.read("Zed/Apple.txt").as_deref(), Some("one"));
}

#[test]
fn a_rename_in_icloud_is_a_rename_here() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.rename_item(&docs.id, "Papers");
    bed.cloud.edit(&a.id, "two", bed.time());
    bed.settle();
    assert_eq!(bed.read("Papers/a.txt").as_deref(), Some("two"));
    assert!(!bed.root().join("Docs").exists());
    assert!(!bed.dir.path().join("Trash").exists(), "nothing was trashed");
}

#[test]
fn a_listing_that_fails_stops_the_sync_and_removes_nothing() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    // Something changes below Docs, and its listing comes back broken.
    bed.cloud.file(&docs.id, "b.txt", "two", bed.time());
    bed.cloud.0.borrow_mut().broken_listing = Some(docs.id.clone());
    let result = bed.try_sync(false);
    assert!(
        matches!(result, Err(Error::Remote(remote::Error::Listing { .. }))),
        "{result:?}"
    );
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("one"));
    bed.cloud.0.borrow_mut().broken_listing = None;
    bed.settle();
    assert_eq!(bed.read("Docs/b.txt").as_deref(), Some("two"));
}

#[test]
fn many_removals_at_once_wait_to_be_allowed() {
    let bed = Bed::new();
    for n in 0..30 {
        bed.cloud.file(ROOT_ID, &format!("f{n}.txt"), "x", bed.time());
    }
    bed.settle();
    for n in 0..30 {
        fs::remove_file(bed.root().join(format!("f{n}.txt"))).unwrap();
    }
    let result = bed.try_sync(false);
    assert!(
        matches!(
            result,
            Err(Error::TooManyDeletions {
                deletions: 30,
                total: 30
            })
        ),
        "{result:?}"
    );
    assert_eq!(bed.cloud.paths().len(), 30, "nothing was removed");
    // A dry run is how they are looked at: it lists them and removes nothing.
    let looked = bed.run(true, false).unwrap();
    assert_eq!(looked.done.len(), 30, "{:?}", looked.done);
    assert_eq!(bed.cloud.paths().len(), 30);
    bed.try_sync(true).unwrap();
    assert!(bed.cloud.paths().is_empty());
}

#[test]
fn a_missing_folder_is_not_read_as_everything_deleted() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    fs::remove_dir_all(bed.root()).unwrap();
    assert!(matches!(bed.try_sync(true), Err(Error::FolderMissing(_))));
    assert_eq!(bed.cloud.paths(), ["a.txt"]);
}

#[test]
fn an_emptied_folder_is_not_read_as_everything_deleted_unless_allowed() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    // What an unmounted disk's mount point looks like.
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    fs::remove_file(bed.root().join("b.txt")).unwrap();
    assert!(matches!(bed.try_sync(false), Err(Error::TooManyDeletions { .. })));
    assert_eq!(bed.cloud.paths(), ["a.txt", "b.txt"]);
    bed.try_sync(true).unwrap();
    assert!(bed.cloud.paths().is_empty());
}

#[test]
fn a_file_dated_in_the_future_is_still_sent() {
    let bed = Bed::new();
    bed.write("a.txt", "from another clock");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let later = now.as_millis() as i64 + 3_600_000;
    local::set_mtime(&bed.root().join("a.txt"), later).unwrap();
    let outcome = bed.sync();
    assert!(outcome.pending.is_empty(), "{:?}", outcome.pending);
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("from another clock"));
}

#[test]
fn unchanged_folders_are_not_listed_again() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let other = bed.cloud.folder(ROOT_ID, "Other");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.cloud.file(&other.id, "b.txt", "two", bed.time());
    bed.settle();
    let before = bed.cloud.0.borrow().listings;
    bed.sync();
    assert_eq!(bed.cloud.0.borrow().listings - before, 1, "only the top folder");
    bed.cloud.file(&docs.id, "c.txt", "three", bed.time());
    let before = bed.cloud.0.borrow().listings;
    bed.sync();
    assert_eq!(bed.cloud.0.borrow().listings - before, 2, "the top folder and Docs");
    assert_eq!(bed.read("Docs/c.txt").as_deref(), Some("three"));
}

#[test]
fn a_package_is_left_alone_and_reported() {
    let bed = Bed::new();
    let doc = bed.cloud.file(ROOT_ID, "letter.pages", "bundle", bed.time());
    bed.cloud.0.borrow_mut().packages.push(doc.id.clone());
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    let outcome = bed.sync();
    assert!(outcome.problems.is_empty());
    let again = bed.sync();
    assert!(again.done.is_empty(), "{:?}", again.done);
    assert!(
        again.skipped.iter().any(|s| s.starts_with("letter.pages")),
        "{:?}",
        again.skipped
    );
    assert_eq!(bed.local_paths(), ["a.txt"]);
    assert_eq!(bed.cloud.paths(), ["a.txt", "letter.pages"]);
}

#[test]
fn a_folder_removed_here_takes_its_contents_to_recently_deleted_as_one() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.cloud.file(ROOT_ID, "keep.txt", "k", bed.time());
    bed.settle();
    fs::remove_dir_all(bed.root().join("Docs")).unwrap();
    bed.settle();
    assert_eq!(bed.cloud.paths(), ["keep.txt"]);
}

#[test]
fn a_file_changed_while_a_removal_was_planned_is_kept() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.remove(&docs.id);
    bed.write("Docs/new.txt", "made here meanwhile");
    bed.settle();
    assert_eq!(bed.cloud.text("Docs/new.txt").as_deref(), Some("made here meanwhile"));
    assert_eq!(bed.local_paths(), ["Docs", "Docs/new.txt"]);
}

#[test]
fn a_same_size_edit_or_a_rename_in_icloud_is_seen_though_no_folder_etag_changes() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let deep = bed.cloud.folder(&docs.id, "deep");
    let a = bed.cloud.file(&deep.id, "a.txt", "one", bed.time());
    let b = bed.cloud.file(&deep.id, "b.txt", "bee", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "two", bed.time());
    bed.cloud.rename_item(&b.id, "c.txt");
    bed.settle();
    assert_eq!(bed.read("Docs/deep/a.txt").as_deref(), Some("two"));
    assert_eq!(bed.read("Docs/deep/c.txt").as_deref(), Some("bee"));
}

#[test]
fn when_the_feed_starts_over_everything_is_listed_again() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    let a = bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.settle();
    bed.cloud.edit(&a.id, "two", bed.time());
    bed.cloud.0.borrow_mut().feed_expired = true;
    bed.settle();
    assert_eq!(bed.read("Docs/a.txt").as_deref(), Some("two"));
}

#[test]
fn a_synced_file_that_becomes_unsyncable_is_left_alone_on_both_sides() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.cloud.file(ROOT_ID, "b.txt", "two", bed.time());
    bed.settle();
    // a.txt becomes a link; "B.txt" appears beside b.txt (one name to iCloud).
    fs::remove_file(bed.root().join("a.txt")).unwrap();
    std::os::unix::fs::symlink("b.txt", bed.root().join("a.txt")).unwrap();
    bed.write("B.txt", "capital");
    let outcome = bed.sync();
    assert!(outcome.done.is_empty(), "{:?}", outcome.done);
    assert_eq!(bed.cloud.paths(), ["a.txt", "b.txt"]);
    assert!(bed.cloud.0.borrow().trashed.is_empty());
    assert_eq!(outcome.skipped.len(), 3, "{:?}", outcome.skipped);
}

#[test]
fn a_replace_whose_listing_fails_is_still_recorded_and_makes_no_conflict() {
    let bed = Bed::new();
    bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    bed.write("a.txt", "two from here");
    bed.cloud.0.borrow_mut().listing_fails_after_replace = true;
    let outcome = bed.try_sync(false).unwrap();
    assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
    assert_eq!(bed.cloud.text("a.txt").as_deref(), Some("two from here"));
    bed.cloud.0.borrow_mut().listing_fails_after_replace = false;
    let uploads = bed.cloud.0.borrow().uploads;
    bed.settle();
    assert_eq!(bed.local_paths(), ["a.txt"], "no conflicted copy");
    assert_eq!(bed.cloud.0.borrow().uploads, uploads, "not sent again");
}

#[test]
fn a_download_that_was_not_recorded_makes_no_conflict() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    let at = bed.time();
    bed.cloud.edit(&a.id, "two from icloud", at);
    // What a sync stopped between putting the file in place and recording it leaves.
    fs::write(bed.root().join("a.txt"), "two from icloud").unwrap();
    local::set_mtime(&bed.root().join("a.txt"), at).unwrap();
    bed.settle();
    assert_eq!(bed.local_paths(), ["a.txt"], "no conflicted copy");
    assert_eq!(bed.read("a.txt").as_deref(), Some("two from icloud"));
}

#[test]
fn what_is_not_synced_is_kept_for_status_and_a_package_that_goes_is_dropped() {
    let bed = Bed::new();
    let doc = bed.cloud.file(ROOT_ID, "letter.pages", "bundle", bed.time());
    bed.cloud.0.borrow_mut().packages.push(doc.id.clone());
    bed.cloud.file(ROOT_ID, ".hidden", "h", bed.time());
    bed.write("a.txt", "one");
    std::os::unix::fs::symlink("a.txt", bed.root().join("link")).unwrap();
    bed.sync();
    assert_eq!(bed.not_synced(), [".hidden", "letter.pages", "link"]);
    bed.cloud.remove(&doc.id);
    fs::remove_file(bed.root().join("link")).unwrap();
    bed.sync();
    assert_eq!(bed.not_synced(), [".hidden"]);
}

#[test]
fn a_folder_removed_here_stays_in_icloud_while_it_holds_something_never_synced() {
    let bed = Bed::new();
    let docs = bed.cloud.folder(ROOT_ID, "Docs");
    bed.cloud.file(&docs.id, "a.txt", "one", bed.time());
    bed.cloud.file(&docs.id, ".hidden", "h", bed.time());
    bed.cloud.file(ROOT_ID, "keep.txt", "k", bed.time());
    bed.settle_paths(&["Docs", "Docs/.hidden", "Docs/a.txt", "keep.txt"]);
    fs::remove_dir_all(bed.root().join("Docs")).unwrap();
    bed.settle_paths(&["Docs", "Docs/.hidden", "keep.txt"]);
    assert_eq!(bed.local_paths(), ["Docs", "keep.txt"]);
}

#[test]
fn a_file_moved_into_a_folder_both_sides_made_does_not_stop_the_sync() {
    let bed = Bed::new();
    let a = bed.cloud.file(ROOT_ID, "a.txt", "one", bed.time());
    bed.settle();
    // Offline: a phone makes "Trip", and so does this computer, which also
    // moves a synced file into it.
    bed.cloud.folder(ROOT_ID, "Trip");
    fs::create_dir(bed.root().join("Trip")).unwrap();
    fs::rename(bed.root().join("a.txt"), bed.root().join("Trip/a.txt")).unwrap();
    bed.write("other.txt", "unrelated");
    bed.settle();
    assert_eq!(bed.cloud.id_at("Trip/a.txt").as_deref(), Some(a.id.as_str()));
    assert_eq!(bed.cloud.text("other.txt").as_deref(), Some("unrelated"));
    assert!(bed.cloud.0.borrow().trashed.is_empty());
}
