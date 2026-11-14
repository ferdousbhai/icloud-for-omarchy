//! Random sequences of what people do on both sides (create, edit, remove,
//! rename, move files and folders, rename by case alone, move into a new
//! folder, edit both copies), interleaved with syncs: some where iCloud
//! refuses moves or a name, some stopped at a random crash point. Every file
//! holds a text no other file ever held, so where each one went can be told.
//!
//! After every sync that ran to the end, with nothing refused:
//! - both sides hold the same paths with the same texts, no text twice;
//! - every text that was never removed or replaced by an edit is there;
//! - neither trash holds a text that was not removed or replaced;
//! - another sync has nothing to do.
//!
//! After one that was refused something or stopped, the last two hold, and
//! every such text is still on one side or the other.
//!
//! Widened (in every case but the named regressions) to what the in-memory
//! Drive cannot know of the real one: a rename or move that keeps the
//! item's etag or not, listings in any order, a refused move answered in
//! three ways; and to more of what people do: a change made by another
//! device while a sync is listing, a file saved here while it downloads, a
//! listing that fails, names iCloud never takes or that cannot be files
//! here, a folder made and removed on one side between syncs, one made or
//! removed on both sides, and many files in one folder.
//!
//! Seeded and bounded; a failure prints its seed and the steps.
//! `ICLOUD_DRIVE_MODEL_CASES=n` runs more cases, `ICLOUD_DRIVE_MODEL_SEED=s`
//! one case.
#![cfg(debug_assertions)]

mod common;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};

use common::{Bed, MidOp};
use icloud_drive::crash;
use icloud_drive::remote::{ROOT_ID, Remote};

const CASES: u64 = 1000;
const STEPS: usize = 40;

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        (!items.is_empty()).then(|| &items[self.below(items.len())])
    }
}

const FILE_NAMES: &[&str] = &["a.txt", "b.txt", "c.md", "d"];
const FOLDER_NAMES: &[&str] = &["P", "Q", "r", "s s"];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Side {
    Here,
    There,
}

type Tree = BTreeMap<String, Option<String>>;

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn is_under(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// Never taken by iCloud, for anything.
const NEVER: &str = "nope.txt";

/// Whether a path is one both sides are expected to hold alike: not one of
/// a name iCloud never takes, or that cannot be a file here.
fn synced(path: &str) -> bool {
    !path.split('/').any(|n| n.contains(':') || n.to_lowercase() == NEVER)
}

struct Model {
    bed: Bed,
    rng: Rng,
    /// The widened model's own choices, so `rng` makes the same ones as
    /// before for the cases it found its regressions in.
    extra: Option<Rng>,
    /// Names handed out once.
    fresh: u32,
    /// The texts a change during a sync would write.
    mid_text: Option<String>,
    saved_text: Option<String>,
    texts: u32,
    /// Every text ever written.
    written: Vec<String>,
    /// Texts removed, or replaced by an edit, by the person.
    removed: HashSet<String>,
    log: Vec<String>,
}

impl Model {
    fn tree(&self, side: Side) -> Tree {
        match side {
            // Not the temp files of a stopped download, which are no one's.
            Side::Here => {
                let mut tree = self.bed.local_tree();
                tree.retain(|p, _| !p.split('/').any(|n| n.starts_with('.')));
                tree
            }
            Side::There => self.bed.cloud.tree(),
        }
    }

    fn text(&mut self) -> String {
        let text = self.unwritten_text();
        self.written.push(text.clone());
        text
    }

    /// A new text, not yet counted as written.
    fn unwritten_text(&mut self) -> String {
        self.texts += 1;
        // Mostly one size, so edits of the same size are common.
        let pad = if self.rng.below(4) == 0 { "++" } else { "" };
        format!("t{:04}{pad}", self.texts)
    }

    fn extra(&mut self, n: usize) -> Option<usize> {
        self.extra.as_mut().map(|r| r.below(n))
    }

    fn fresh_name(&mut self, is_dir: bool) -> String {
        self.fresh += 1;
        if is_dir {
            format!("M{}", self.fresh)
        } else {
            format!("m{}.txt", self.fresh)
        }
    }

    /// Whether `paths` (to be created, from `old` if moved on `side`) would
    /// differ from something on either side by case alone: iCloud takes
    /// such names as one, which the model does not follow. (The old path is
    /// only the moving item's own on its side: the other side may hold
    /// something else there. The cases of the named regressions were found
    /// without that.)
    fn clashes(&self, side: Side, paths: &[(String, String)]) -> bool {
        [Side::Here, Side::There].into_iter().any(|s| {
            let own = s == side || self.extra.is_none();
            self.tree(s).into_keys().any(|q| {
                paths
                    .iter()
                    .any(|(new, old)| &q != new && !(own && &q == old) && q.to_lowercase() == new.to_lowercase())
            })
        })
    }

    /// A name for something new in `dir`, free on `side`.
    fn free_name(&mut self, side: Side, dir: &str, names: &[&str]) -> Option<String> {
        let tree = self.tree(side);
        let name = self.rng.pick(names)?.to_string();
        let path = join(dir, &name);
        (!tree.contains_key(&path) && !self.clashes(side, &[(path.clone(), path.clone())])).then_some(path)
    }

    fn dirs(&self, side: Side) -> Vec<String> {
        let mut dirs = vec![String::new()];
        dirs.extend(self.tree(side).into_iter().filter(|(_, t)| t.is_none()).map(|(p, _)| p));
        dirs
    }

    fn files(&self, side: Side) -> Vec<String> {
        self.tree(side)
            .into_iter()
            .filter(|(_, t)| t.is_some())
            .map(|(p, _)| p)
            .collect()
    }

    fn texts_below(&self, side: Side, path: &str) -> Vec<String> {
        self.tree(side)
            .into_iter()
            .filter(|(p, _)| is_under(p, path))
            .filter_map(|(_, t)| t)
            .collect()
    }

    fn id(&self, path: &str) -> String {
        if path.is_empty() {
            ROOT_ID.to_string()
        } else {
            self.bed.cloud.id_at(path).unwrap()
        }
    }

    fn create_file(&mut self, side: Side) -> Option<String> {
        let dir = self.rng.pick(&self.dirs(side))?.clone();
        let path = self.free_name(side, &dir, FILE_NAMES)?;
        let text = self.text();
        match side {
            Side::Here => self.bed.write(&path, &text),
            Side::There => {
                let parent = self.id(&dir);
                self.bed.cloud.file(&parent, name_of(&path), &text, self.bed.time());
            }
        }
        Some(format!("create {path} = {text}"))
    }

    fn create_folder(&mut self, side: Side) -> Option<String> {
        let dir = self.rng.pick(&self.dirs(side))?.clone();
        let path = self.free_name(side, &dir, FOLDER_NAMES)?;
        self.mkdir(side, &path);
        Some(format!("mkdir {path}"))
    }

    fn mkdir(&self, side: Side, path: &str) {
        match side {
            Side::Here => {
                fs::create_dir_all(self.bed.root()).unwrap();
                fs::create_dir(self.bed.root().join(path)).unwrap()
            }
            Side::There => {
                self.bed.cloud.folder(&self.id(parent(path)), name_of(path));
            }
        }
    }

    fn edit(&mut self, side: Side, path: &str) {
        let old = self.tree(side)[path].clone().unwrap();
        self.removed.insert(old);
        let text = self.text();
        match side {
            Side::Here => self.bed.write(path, &text),
            Side::There => self.bed.cloud.edit(&self.id(path), &text, self.bed.time()),
        }
    }

    fn edit_one(&mut self, side: Side) -> Option<String> {
        let path = self.rng.pick(&self.files(side))?.clone();
        self.edit(side, &path);
        Some(format!("edit {path}"))
    }

    fn edit_both(&mut self) -> Option<String> {
        let there: HashSet<String> = self.files(Side::There).into_iter().collect();
        let both: Vec<String> = self
            .files(Side::Here)
            .into_iter()
            .filter(|p| there.contains(p))
            .collect();
        let path = self.rng.pick(&both)?.clone();
        self.edit(Side::Here, &path);
        self.edit(Side::There, &path);
        Some(format!("edit {path} on both sides"))
    }

    fn remove(&mut self, side: Side) -> Option<String> {
        let tree = self.tree(side);
        let paths: Vec<String> = tree.keys().cloned().collect();
        let path = self.rng.pick(&paths)?.clone();
        let gone = self.texts_below(side, &path);
        self.removed.extend(gone);
        match side {
            Side::Here => {
                let full = self.bed.root().join(&path);
                if tree[&path].is_none() {
                    fs::remove_dir_all(full).unwrap()
                } else {
                    fs::remove_file(full).unwrap()
                }
            }
            Side::There => self.bed.cloud.remove(&self.id(&path)),
        }
        Some(format!("remove {path}"))
    }

    /// Renames or moves `from` to `to` on `side`, unless that cannot be.
    fn move_item(&mut self, side: Side, from: &str, to: &str) -> bool {
        let tree = self.tree(side);
        if tree.contains_key(to) || is_under(to, from) {
            return false;
        }
        let moved: Vec<(String, String)> = tree
            .keys()
            .filter(|p| is_under(p, from))
            .map(|p| (format!("{to}{}", &p[from.len()..]), p.clone()))
            .collect();
        if self.clashes(side, &moved) {
            return false;
        }
        match side {
            Side::Here => fs::rename(self.bed.root().join(from), self.bed.root().join(to)).unwrap(),
            Side::There => {
                let id = self.id(from);
                if parent(from) != parent(to) {
                    let node = self.bed.cloud.0.borrow().nodes[&id].clone();
                    self.bed.cloud.move_to(&node, &self.id(parent(to))).unwrap();
                }
                if name_of(from) != name_of(to) {
                    self.bed.cloud.rename_item(&id, name_of(to));
                }
            }
        }
        true
    }

    fn some_item(&mut self, side: Side) -> Option<(String, bool)> {
        let items: Vec<(String, bool)> = self.tree(side).into_iter().map(|(p, t)| (p, t.is_none())).collect();
        self.rng.pick(&items).cloned()
    }

    fn rename(&mut self, side: Side) -> Option<String> {
        let (from, is_dir) = self.some_item(side)?;
        let names = if is_dir { FOLDER_NAMES } else { FILE_NAMES };
        let to = join(parent(&from), self.rng.pick(names)?);
        self.move_item(side, &from, &to)
            .then(|| format!("rename {from} to {to}"))
    }

    fn rename_case(&mut self, side: Side) -> Option<String> {
        let (from, _) = self.some_item(side)?;
        let name = name_of(&from);
        let flipped: String = name
            .chars()
            .map(|c| {
                if c.is_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        let to = join(parent(&from), &flipped);
        (to != from && self.move_item(side, &from, &to)).then(|| format!("rename {from} to {to}"))
    }

    fn move_into(&mut self, side: Side, new_folder: bool) -> Option<String> {
        let (from, _) = self.some_item(side)?;
        let dir = if new_folder {
            let base = self.rng.pick(&self.dirs(side))?.clone();
            let dir = self.free_name(side, &base, FOLDER_NAMES)?;
            if is_under(&dir, &from) {
                return None;
            }
            self.mkdir(side, &dir);
            self.log.push(format!("  {side:?}: mkdir {dir}"));
            dir
        } else {
            self.rng.pick(&self.dirs(side))?.clone()
        };
        let to = join(&dir, name_of(&from));
        (parent(&from) != dir && self.move_item(side, &from, &to)).then(|| format!("move {from} to {to}"))
    }

    /// A file of a name iCloud never takes (made here), or one that cannot
    /// be a file here (either side).
    fn create_odd(&mut self, side: Side) -> Option<String> {
        let dir = self.rng.pick(&self.dirs(side))?.clone();
        let names: &[&str] = match side {
            Side::Here => &["x:y.txt", NEVER],
            Side::There => &["x:y.txt"],
        };
        let path = self.free_name(side, &dir, names)?;
        let text = self.text();
        match side {
            Side::Here => self.bed.write(&path, &text),
            Side::There => {
                let parent = self.id(&dir);
                self.bed.cloud.file(&parent, name_of(&path), &text, self.bed.time());
            }
        }
        Some(format!("create {path} = {text}"))
    }

    /// A folder with a file in it, made and removed again before a sync.
    fn transient(&mut self, side: Side) -> Option<String> {
        let dir = self.rng.pick(&self.dirs(side))?.clone();
        let path = self.free_name(side, &dir, FOLDER_NAMES)?;
        self.mkdir(side, &path);
        let file = join(&path, "a.txt");
        let text = self.text();
        match side {
            Side::Here => {
                self.bed.write(&file, &text);
                fs::remove_dir_all(self.bed.root().join(&path)).unwrap();
            }
            Side::There => {
                let folder = self.id(&path);
                self.bed.cloud.file(&folder, "a.txt", &text, self.bed.time());
                self.bed.cloud.remove(&folder);
            }
        }
        self.removed.insert(text);
        Some(format!("mkdir {path}, put {file} in it, remove {path}"))
    }

    /// A folder of one name made on both sides, a file in each maybe.
    fn mkdir_both(&mut self) -> Option<String> {
        let dir = self.rng.pick(&self.dirs(Side::Here))?.clone();
        let name = self.rng.pick(FOLDER_NAMES)?;
        let path = join(&dir, name);
        if !self.dirs(Side::There).contains(&dir)
            || [Side::Here, Side::There]
                .iter()
                .any(|s| self.tree(*s).contains_key(&path))
            || self.clashes(Side::Here, &[(path.clone(), path.clone())])
        {
            return None;
        }
        for side in [Side::Here, Side::There] {
            self.mkdir(side, &path);
        }
        if self.rng.below(2) == 0 {
            for side in [Side::Here, Side::There] {
                let file = join(&path, "a.txt");
                let text = self.text();
                match side {
                    Side::Here => self.bed.write(&file, &text),
                    Side::There => {
                        let folder = self.id(&path);
                        self.bed.cloud.file(&folder, "a.txt", &text, self.bed.time());
                    }
                }
            }
        }
        Some(format!("mkdir {path} on both sides"))
    }

    /// The same path removed on both sides.
    fn remove_both(&mut self) -> Option<String> {
        let there = self.tree(Side::There);
        let both: Vec<String> = self
            .tree(Side::Here)
            .into_keys()
            .filter(|p| there.contains_key(p))
            .collect();
        let path = self.rng.pick(&both)?.clone();
        for side in [Side::Here, Side::There] {
            let gone = self.texts_below(side, &path);
            self.removed.extend(gone);
            match side {
                Side::Here => {
                    let full = self.bed.root().join(&path);
                    if full.is_dir() {
                        fs::remove_dir_all(full).unwrap()
                    } else {
                        fs::remove_file(full).unwrap()
                    }
                }
                Side::There => self.bed.cloud.remove(&self.id(&path)),
            }
        }
        Some(format!("remove {path} on both sides"))
    }

    /// Many files in one new folder.
    fn many(&mut self, side: Side) -> Option<String> {
        let dir = self.rng.pick(&self.dirs(side))?.clone();
        let path = self.free_name(side, &dir, FOLDER_NAMES)?;
        self.mkdir(side, &path);
        let n = 21 + self.rng.below(10);
        for i in 0..n {
            let text = self.text();
            let name = format!("f{i:02}");
            match side {
                Side::Here => self.bed.write(&join(&path, &name), &text),
                Side::There => {
                    let folder = self.id(&path);
                    self.bed.cloud.file(&folder, &name, &text, self.bed.time());
                }
            }
        }
        Some(format!("mkdir {path} with {n} files"))
    }

    fn step_extra(&mut self) -> Option<String> {
        let side = if self.rng.below(2) == 0 {
            Side::Here
        } else {
            Side::There
        };
        let what = match self.extra(40)? {
            0..=3 => self.create_odd(side),
            4 | 5 => self.transient(side),
            6 | 7 => self.mkdir_both().map(|s| format!("{s} (both)")),
            8 | 9 => self.remove_both().map(|s| format!("{s} (both)")),
            10 => self.many(side),
            _ => None,
        }?;
        Some(format!("{side:?}: {what}"))
    }

    /// A change another device makes in iCloud while the next sync lists.
    fn mid_listing(&mut self) -> Option<(MidOp, String)> {
        let (path, is_dir) = self.some_item(Side::There)?;
        let id = self.id(&path);
        Some(match self.extra(3)? {
            0 if !is_dir => {
                let text = self.unwritten_text();
                let mtime_ms = self.bed.time();
                self.mid_text = Some(text.clone());
                (MidOp::Edit { id, text, mtime_ms }, format!("edit {path}"))
            }
            1 => (MidOp::Remove { id }, format!("remove {path}")),
            _ => {
                let to = self.rng.pick(&self.dirs(Side::There))?.clone();
                let parent = (self.rng.below(2) == 0).then(|| self.id(&to));
                let name = self.fresh_name(is_dir);
                let shown = match &parent {
                    Some(_) => format!("move {path} to {}", join(&to, &name)),
                    None => format!("rename {path} to {name}"),
                };
                (MidOp::Move { id, parent, name }, shown)
            }
        })
    }

    /// Sets up what may happen while the next sync runs; whether any of it
    /// did is told by [`Model::during_done`].
    fn during(&mut self, stopped: bool) {
        if self.extra(4) == Some(0)
            && let Some((op, shown)) = self.mid_listing()
        {
            let at = self.bed.cloud.0.borrow().listings + self.rng.below(6);
            self.log.push(format!("  (iCloud, while listing: {shown})"));
            self.bed.cloud.0.borrow_mut().mid_listing = Some((at, op));
        }
        if self.extra(4) == Some(0) {
            let text = self.unwritten_text();
            let mtime_ms = self.bed.time();
            self.log
                .push(format!("  (here, while a file comes down: save it as {text})"));
            self.saved_text = Some(text.clone());
            self.bed.cloud.0.borrow_mut().during_download = Some((text, mtime_ms));
        }
        if self.extra(10) == Some(0) && !stopped {
            let at = self.bed.cloud.0.borrow().listings + self.rng.below(6);
            self.log.push("  (a listing fails)".into());
            self.bed.cloud.0.borrow_mut().fail_listing_at = Some(at);
        }
    }

    /// Whether what [`Model::during`] set up happened, counted if so.
    fn during_done(&mut self) -> bool {
        let mut c = self.bed.cloud.0.borrow_mut();
        // What did not come to pass by the end of the sync never will.
        (c.mid_listing, c.during_download, c.fail_listing_at) = (None, None, None);
        let (fired, edited) = (c.mid_fired.take(), c.download_edited.take());
        drop(c);
        let mut happened = false;
        if let Some(removed) = fired {
            happened = true;
            self.log.push("  (it happened while listing)".into());
            self.removed.extend(removed);
            if let Some(text) = self.mid_text.take() {
                self.written.push(text);
            }
        }
        if let Some(old) = edited {
            happened = true;
            self.log.push("  (it was saved while it came down)".into());
            self.removed.extend(old);
            self.written.extend(self.saved_text.take());
        }
        self.mid_text = None;
        self.saved_text = None;
        happened
    }

    fn step(&mut self) -> Option<String> {
        let side = if self.rng.below(2) == 0 {
            Side::Here
        } else {
            Side::There
        };
        let what = match self.rng.below(12) {
            0 | 1 => self.create_file(side),
            2 => self.create_folder(side),
            3 | 4 => self.edit_one(side),
            5 => self.edit_both().map(|s| format!("{s} (both)")),
            6 => self.remove(side),
            7 => self.rename(side),
            8 => self.rename_case(side),
            9 => self.move_into(side, false),
            10 => self.move_into(side, true),
            _ => self.create_file(side),
        }?;
        Some(format!("{side:?}: {what}"))
    }

    fn present(&self) -> Vec<(Side, String, String)> {
        [Side::Here, Side::There]
            .into_iter()
            .flat_map(|s| self.tree(s).into_iter().filter_map(move |(p, t)| t.map(|t| (s, p, t))))
            .collect()
    }

    /// What the last sync set out to do, shown with the steps of a failure.
    fn log_started(&mut self) {
        let started = std::mem::take(&mut *self.bed.started.borrow_mut());
        self.log.extend(started.iter().map(|l| format!("    > {l}")));
    }

    /// What holds after any sync; no text twice on a side unless it was
    /// `stopped` (between sending a file and removing the old one, say).
    fn check_always(&self, stopped: bool) -> Result<(), String> {
        let present: HashSet<String> = self.present().into_iter().map(|(_, _, t)| t).collect();
        for text in &self.written {
            if !self.removed.contains(text) && !present.contains(text) {
                return Err(format!("{text} is on neither side"));
            }
        }
        for (side, trashed) in [
            ("the trash here", self.bed.trashed_here()),
            ("Recently Deleted", self.bed.cloud.trashed_texts()),
        ] {
            if let Some(text) = trashed.iter().find(|t| !self.removed.contains(*t)) {
                return Err(format!("{text} is in {side}, and was never removed"));
            }
        }
        let mut seen = HashSet::new();
        for (side, path, text) in self.present().into_iter().filter(|_| !stopped) {
            if !seen.insert((side, text.clone())) {
                return Err(format!("{text} is twice on {side:?} (once at {path})"));
            }
        }
        Ok(())
    }

    /// What holds after a sync that ran to the end with nothing refused.
    fn check_settled(&self) -> Result<(), String> {
        self.check_always(false)?;
        let only_synced = |mut tree: Tree| {
            tree.retain(|p, _| synced(p));
            tree
        };
        let (here, there) = (only_synced(self.tree(Side::Here)), only_synced(self.tree(Side::There)));
        if here != there {
            let diff: Vec<String> = here
                .iter()
                .filter(|(p, t)| there.get(*p) != Some(t))
                .map(|(p, t)| format!("here {p} = {t:?}, there {:?}", there.get(p)))
                .chain(
                    there
                        .iter()
                        .filter(|(p, _)| !here.contains_key(*p))
                        .map(|(p, t)| format!("there only {p} = {t:?}")),
                )
                .collect();
            return Err(format!("the sides differ: {diff:#?}"));
        }
        let again = self.bed.try_sync(true).map_err(|e| format!("the sync after: {e}"))?;
        self.bed.started.borrow_mut().clear();
        if !again.done.is_empty() || !never_problems(&again.problems) {
            return Err(format!(
                "the sync after did {:?}, failed {:?}",
                again.done, again.problems
            ));
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), String> {
        let kind = self.rng.below(8);
        self.during(kind == 2 || kind == 3);
        let (what, refused) = match kind {
            0 => {
                self.bed.cloud.0.borrow_mut().refuses_moves = true;
                ("sync, iCloud refusing moves", true)
            }
            1 => {
                let name = self.rng.pick(&[FILE_NAMES, FOLDER_NAMES].concat()).unwrap().to_string();
                self.bed.cloud.0.borrow_mut().refused_names = vec![name];
                ("sync, iCloud refusing a name", true)
            }
            2 | 3 => ("sync, stopped", true),
            _ => ("sync", false),
        };
        let at = self.rng.below(80);
        let result = if kind == 2 || kind == 3 {
            common::quiet();
            crash::arm(Some(at));
            let result = catch_unwind(AssertUnwindSafe(|| self.bed.try_sync(true)));
            crash::disarm();
            let fired = self.bed.cloud.0.borrow().mid_fired.is_some();
            match result {
                Ok(Ok(_)) => {
                    self.log.push(format!("sync (point {at} not reached)"));
                    Ok(false)
                }
                Ok(Err(e)) if fired => {
                    self.log.push(format!("sync failed: {e}"));
                    Ok(true)
                }
                Ok(Err(e)) => Err(format!("the sync failed: {e}")),
                Err(payload) => match payload.downcast_ref::<crash::Crash>() {
                    Some(point) => {
                        self.log.push(format!("sync, stopped at {} (point {at})", point.0));
                        Ok(true)
                    }
                    None => std::panic::resume_unwind(payload),
                },
            }
        } else {
            self.log.push(format!(
                "{what}{}",
                match kind {
                    1 => format!(" {:?}", self.bed.cloud.0.borrow().refused_names),
                    _ => String::new(),
                }
            ));
            let outcome = self.bed.try_sync(true);
            if let Ok(outcome) = &outcome {
                self.log.extend(outcome.problems.iter().map(|l| format!("    ! {l}")));
            }
            let listing_failed = {
                let c = self.bed.cloud.0.borrow();
                c.fail_listing_at.is_some_and(|at| c.listings > at)
            };
            let disturbed = listing_failed || self.bed.cloud.0.borrow().mid_fired.is_some();
            match outcome {
                Ok(outcome) if !refused && !disturbed && !never_problems(&outcome.problems) => {
                    Err(format!("problems: {:?}", outcome.problems))
                }
                Ok(_) => Ok(refused || disturbed),
                Err(_) if disturbed => Ok(true),
                Err(e) => Err(format!("the sync failed: {e}")),
            }
        };
        self.log_started();
        let result = match (self.during_done(), result) {
            (true, Ok(_)) => Ok(true),
            (_, result) => result,
        };
        {
            let mut c = self.bed.cloud.0.borrow_mut();
            c.refuses_moves = false;
            c.refused_names.clear();
        }
        let unsettled = result?;
        if !unsettled {
            return self.check_settled();
        }
        self.check_always(kind == 2 || kind == 3)?;
        Ok(())
    }
}

/// Whether every problem is with a name iCloud never takes.
fn never_problems(problems: &[String]) -> bool {
    problems.iter().all(|p| p.to_lowercase().contains(NEVER))
}

fn run_case(seed: u64, widened: bool) -> Result<(), (String, Vec<String>)> {
    let bed = Bed::new();
    let mut extra = widened.then_some(Rng(seed ^ 0x005e_ed0f_d21e));
    if let Some(extra) = &mut extra {
        let mut c = bed.cloud.0.borrow_mut();
        c.moves_keep_etag = extra.below(2) == 0;
        c.shuffle = (extra.below(2) == 0).then(|| extra.next());
        c.refusal = extra.below(3) as u8;
        c.never_names = vec![NEVER.to_string()];
        c.replaces_trashed = extra.below(2) == 0;
    }
    let mut model = Model {
        bed,
        rng: Rng(seed),
        extra,
        fresh: 0,
        mid_text: None,
        saved_text: None,
        texts: 0,
        written: Vec::new(),
        removed: HashSet::new(),
        log: Vec::new(),
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        for _ in 0..STEPS {
            if model.rng.below(4) == 0 {
                model.sync()?;
            } else if let Some(step) = model.step() {
                model.log.push(step);
            }
            if let Some(step) = model.step_extra() {
                model.log.push(step);
            }
        }
        model.log.push("sync".into());
        let last = model.bed.try_sync(true);
        model.log_started();
        last.map_err(|e| e.to_string())?;
        model.check_settled()
    }));
    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(why)) => Err((why, model.log)),
        Err(panic) => {
            let why = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "a panic".into());
            Err((format!("panicked: {why}"), model.log))
        }
    }
}

/// The cases of one test: every fourth seed, so four run side by side.
fn run_part(part: u64) {
    let seeds: Vec<u64> = match std::env::var("ICLOUD_DRIVE_MODEL_SEED") {
        Ok(seed) if part == 0 => vec![seed.parse().unwrap()],
        Ok(_) => Vec::new(),
        Err(_) => {
            let cases = std::env::var("ICLOUD_DRIVE_MODEL_CASES")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(CASES);
            (0..cases).filter(|s| s % 4 == part).collect()
        }
    };
    let mut failed = Vec::new();
    for seed in seeds {
        if let Err((why, log)) = run_case(seed, true) {
            eprintln!("seed {seed}: {why}\n  {}", log.join("\n  "));
            failed.push(seed);
        }
    }
    assert!(failed.is_empty(), "failing seeds: {failed:?}");
}

#[test]
fn random_changes_on_both_sides_always_sync_to_the_same_files_0() {
    run_part(0);
}

#[test]
fn random_changes_on_both_sides_always_sync_to_the_same_files_1() {
    run_part(1);
}

#[test]
fn random_changes_on_both_sides_always_sync_to_the_same_files_2() {
    run_part(2);
}

#[test]
fn random_changes_on_both_sides_always_sync_to_the_same_files_3() {
    run_part(3);
}

/// Seeds that found a folder taken for another of its path: one renamed
/// here to the name of a folder new in iCloud, or moved here into a new
/// folder of its own old name.
fn regression(seed: u64) {
    regression_of(seed, false)
}

fn regression_of(seed: u64, widened: bool) {
    if let Err((why, log)) = run_case(seed, widened) {
        panic!("seed {seed}: {why}\n  {}", log.join("\n  "));
    }
}

#[test]
fn seed_5216_a_folder_moved_here_into_a_new_folder_of_its_old_name() {
    regression(5216);
}

#[test]
fn seed_5507_a_folder_renamed_here_to_the_name_of_one_new_in_icloud() {
    regression(5507);
}

#[test]
fn seed_6428_a_folder_renamed_here_to_the_name_of_one_new_in_icloud() {
    regression(6428);
}

#[test]
fn seed_9476_a_folder_renamed_here_to_the_name_of_one_new_in_icloud() {
    regression(9476);
}

#[test]
fn seed_11883_a_folder_moved_here_into_one_renamed_to_its_old_name() {
    regression(11883);
}

#[test]
fn seed_20614_a_folder_moved_here_into_a_new_folder_of_its_old_name() {
    regression(20614);
}

/// Seeds of the widened model.
fn widened(seed: u64) {
    regression_of(seed, true)
}

#[test]
fn seed_5791_a_move_here_stopped_as_it_was_recorded_then_an_edit() {
    widened(5791);
}

#[test]
fn seed_16435_a_file_renamed_here_to_the_name_one_takes_in_icloud() {
    widened(16435);
}

#[test]
fn seed_22447_a_file_moved_in_icloud_right_after_its_replace_is_kept_once() {
    widened(22447);
}

#[test]
fn seed_21971_folders_moved_into_each_other_with_a_new_one_in_the_way() {
    widened(21971);
}

#[test]
fn seed_5273_a_move_here_planned_from_a_place_another_move_took() {
    widened(5273);
}
