//! The decisions: given what was in step last time (`base`), what iCloud
//! has now and what the folder has now, which operations bring the two back
//! in step. Pure: nothing here touches a file or the network, so every rule
//! has a test.
//!
//! The rules, in short:
//! - A side that changed while the other did not wins.
//! - Both changed: both are kept. The local file is set aside as a
//!   "conflicted copy" (which then uploads) and iCloud's version takes the name.
//! - An edit beats a delete: the edited side is kept and comes back.
//! - A delete is only believed for something that was in step and is
//!   unchanged on the other side; it goes to the trash there.
//! - Moves and renames are done first, alone, so everything after compares
//!   like with like. A round that moved something asks for another round.
//! - When the picture is unclear the row is forgotten: both sides then meet
//!   as strangers, which can cost a duplicate but never a file.
//! - A path one side could not show properly (a link, two names that differ
//!   only by case, two items of one name in iCloud) is `unknown`: nothing
//!   at or below it is touched, since "not seen" must never read as "gone".

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::local::Entry;
use crate::remote::{Kind, Node, ROOT_ID};
use crate::state::{Row, is_under};

/// iCloud Drive now: every syncable node below the top folder.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub nodes: HashMap<String, Node>,
    /// id to path under the top folder.
    pub paths: HashMap<String, String>,
    pub by_path: BTreeMap<String, String>,
    /// Paths left out, and why (app folders, names that cannot be files here).
    pub skipped: Vec<(String, &'static str)>,
}

impl Snapshot {
    /// From a flat listing. App libraries and what is below them are left
    /// out, as are names that cannot exist or would be hidden locally.
    pub fn build(nodes: &[Node]) -> Snapshot {
        Snapshot::build_below(ROOT_ID, nodes)
    }

    /// [`Snapshot::build`] with another folder as the top.
    pub fn build_below(top: &str, nodes: &[Node]) -> Snapshot {
        let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
        for node in nodes {
            children.entry(node.parent.as_str()).or_default().push(node);
        }
        let mut snapshot = Snapshot::default();
        let mut stack: Vec<(&str, String)> = vec![(top, String::new())];
        while let Some((parent, prefix)) = stack.pop() {
            let mut kids = children.get(parent).cloned().unwrap_or_default();
            kids.sort_by(|a, b| a.id.cmp(&b.id));
            for node in kids {
                let path = if prefix.is_empty() {
                    node.name.clone()
                } else {
                    format!("{prefix}/{}", node.name)
                };
                if node.kind == Kind::AppLibrary {
                    snapshot.skipped.push((path, "an app's own folder"));
                    continue;
                }
                if node.name.starts_with('.') || node.name.contains('/') || node.name.contains('\0') {
                    snapshot.skipped.push((path, "its name cannot be a visible file here"));
                    continue;
                }
                if crate::local::unsyncable_name(&node.name).is_some() {
                    snapshot.skipped.push((path, "its name cannot be a file here"));
                    continue;
                }
                if snapshot.by_path.contains_key(&path) {
                    snapshot
                        .skipped
                        .push((path, "another item in iCloud has the same name"));
                    continue;
                }
                snapshot.by_path.insert(path.clone(), node.id.clone());
                snapshot.paths.insert(node.id.clone(), path.clone());
                snapshot.nodes.insert(node.id.clone(), node.clone());
                if node.kind == Kind::Folder {
                    stack.push((node.id.as_str(), path));
                }
            }
        }
        snapshot
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// iCloud moved or renamed it: do the same here.
    LocalMove {
        id: String,
        from: String,
        to: String,
    },
    /// It was moved or renamed here: do the same in iCloud.
    RemoteMove {
        id: String,
        to: String,
    },
    /// Rename the local file to a conflicted copy, freeing its name.
    SetAside {
        path: String,
    },
    MkdirLocal {
        id: String,
        path: String,
    },
    MkdirRemote {
        path: String,
    },
    Download {
        id: String,
        path: String,
    },
    Upload {
        path: String,
    },
    /// Send the local file as the new content of the iCloud one.
    Replace {
        id: String,
        path: String,
    },
    TrashLocal {
        id: String,
        path: String,
    },
    TrashRemote {
        id: String,
        path: String,
    },
    /// Both sides already have it: record it as in step.
    Adopt {
        id: String,
        path: String,
    },
    /// Drop the row; the two sides are compared afresh next round.
    Forget {
        id: String,
        path: String,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub ops: Vec<Op>,
    /// Run another round after this one (moves were done alone, or
    /// something was forgotten or set aside).
    pub again: bool,
    /// How many items in step this plan sends to a trash.
    pub deletions: usize,
    /// Local files still being written; left for the next run.
    pub pending: Vec<String>,
}

/// A file that changed less than this long ago may still be changing.
const SETTLE_NS: i64 = 2_000_000_000;
/// Clocks and file systems round: within this, two times are one time.
const SAME_TIME_MS: i64 = 2_000;

/// The folder a path is in; empty at the top.
pub(crate) fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Changed so recently that it may still be being written. A date in the
/// future (another clock, an unpacked archive) is not "recent".
fn settling(entry: &Entry, now_ns: i64) -> bool {
    (0..SETTLE_NS).contains(&(now_ns - entry.mtime_ns))
}

fn remote_changed(row: &Row, node: &Node) -> bool {
    node.size != row.node.size || node.mtime_ms != row.node.mtime_ms
}

fn local_changed(row: &Row, entry: &Entry) -> bool {
    entry.size != row.local.size || entry.mtime_ns != row.local.mtime_ns
}

/// What the planner is told beyond the three views.
#[derive(Debug, Default)]
pub struct Context {
    /// Items in iCloud to leave alone while their etag is this one.
    pub ignored: HashMap<String, String>,
    /// Paths that could not be seen properly on one side.
    pub unknown: Vec<String>,
    /// The time, in Unix nanoseconds.
    pub now_ns: i64,
}

pub fn plan(base: &[Row], remote: &Snapshot, local: &BTreeMap<String, Entry>, context: &Context) -> Plan {
    let moves = plan_moves(base, remote, local, &context.unknown);
    if !moves.ops.is_empty() {
        return moves;
    }
    plan_content(base, remote, local, context)
}

/// Moves and renames only. A row whose folder is itself moving waits for the
/// next round, when its folder's move has carried it along.
fn plan_moves(base: &[Row], remote: &Snapshot, local: &BTreeMap<String, Entry>, unknown: &[String]) -> Plan {
    let unseen = |path: &str| unknown.iter().any(|u| is_under(path, u));
    let tracked: HashSet<&str> = base.iter().map(|r| r.path.as_str()).collect();
    // Untracked local entries by inode: where a tracked one may have gone.
    let mut by_ino: HashMap<u64, Vec<&str>> = HashMap::new();
    for (path, entry) in local {
        if !tracked.contains(path.as_str()) {
            by_ino.entry(entry.ino).or_default().push(path);
        }
    }
    let moved_here = |row: &Row| -> Option<&str> {
        by_ino.get(&row.local.ino)?.iter().copied().find(|p| {
            let e = &local[*p];
            e.is_dir == row.local.is_dir && (e.is_dir || !local_changed(row, e))
        })
    };
    let here = |row: &Row| local.get(&row.path).filter(|e| e.is_dir == row.local.is_dir);
    // Where folders are moving to, on each side. An item moved into one of
    // them waits a round for the folder to get there first, rather than
    // making a second folder of that name.
    let mut dests_there: HashSet<&str> = HashSet::new();
    let mut dests_here: HashSet<&str> = HashSet::new();
    for row in base.iter().filter(|r| r.local.is_dir) {
        match remote.paths.get(&row.node.id) {
            Some(rpath) if rpath != &row.path => {
                dests_there.insert(rpath);
            }
            Some(_) if here(row).is_none() => dests_here.extend(moved_here(row)),
            _ => {}
        }
    }
    let into = |dests: &HashSet<&str>, path: &str| {
        let mut dir = parent_of(path);
        while !dir.is_empty() {
            if dests.contains(dir) {
                return true;
            }
            dir = parent_of(dir);
        }
        false
    };

    let mut ops = Vec::new();
    let mut moving: Vec<&str> = Vec::new();
    let mut stuck: Vec<&Row> = Vec::new();
    let mut made: HashSet<String> = HashSet::new();
    for row in base {
        if moving.iter().any(|p| is_under(&row.path, p)) || unseen(&row.path) {
            continue;
        }
        let Some(rpath) = remote.paths.get(&row.node.id) else {
            continue;
        };
        if unseen(rpath) {
            continue;
        }
        if rpath != &row.path {
            if into(&dests_there, rpath) {
                continue;
            }
            // Moved in iCloud. Find it here: where it was, or where it was moved to.
            let from = if here(row).is_some() {
                Some(row.path.as_str())
            } else {
                moved_here(row)
            };
            moving.push(&row.path);
            let Some(from) = from else {
                // Gone here and moved there: meet again as strangers.
                ops.push(Op::Forget {
                    id: row.node.id.clone(),
                    path: row.path.clone(),
                });
                continue;
            };
            if from == rpath {
                // Moved the same way on both sides already.
                ops.push(Op::LocalMove {
                    id: row.node.id.clone(),
                    from: from.to_string(),
                    to: rpath.clone(),
                });
                continue;
            }
            if local.contains_key(rpath) {
                if tracked.contains(rpath.as_str()) {
                    // The destination is another item in step, which may be
                    // about to move away itself: wait.
                    stuck.push(row);
                    continue;
                }
                ops.push(Op::SetAside { path: rpath.clone() });
            }
            ops.push(Op::LocalMove {
                id: row.node.id.clone(),
                from: from.to_string(),
                to: rpath.clone(),
            });
        } else if here(row).is_none()
            && let Some(to) = moved_here(row)
        {
            // Moved here; iCloud still has it at the old place.
            if remote.by_path.contains_key(to) || unseen(to) {
                // Something else took that name in iCloud: leave it to the
                // content rules (this one reads as deleted here).
                continue;
            }
            if into(&dests_here, to) {
                continue;
            }
            // Folders it was moved into that iCloud does not have yet.
            let mut missing = Vec::new();
            let mut dir = parent_of(to);
            while !dir.is_empty() && !remote.by_path.contains_key(dir) && !made.contains(dir) {
                missing.push(dir.to_string());
                dir = parent_of(dir);
            }
            for dir in missing.into_iter().rev() {
                made.insert(dir.clone());
                ops.push(Op::MkdirRemote { path: dir });
            }
            moving.push(&row.path);
            ops.push(Op::RemoteMove {
                id: row.node.id.clone(),
                to: to.to_string(),
            });
        }
    }
    if ops.is_empty() {
        // Nothing can move and something is waiting on something else
        // (two items that swapped names, say).
        for row in stuck {
            ops.push(Op::Forget {
                id: row.node.id.clone(),
                path: row.path.clone(),
            });
        }
    }
    Plan {
        again: !ops.is_empty(),
        ops,
        ..Plan::default()
    }
}

fn plan_content(base: &[Row], remote: &Snapshot, local: &BTreeMap<String, Entry>, context: &Context) -> Plan {
    let (ignored, now_ns) = (&context.ignored, context.now_ns);
    let mut plan = Plan::default();
    let tracked: HashMap<&str, &Row> = base.iter().map(|r| (r.path.as_str(), r)).collect();
    // Subtrees to leave alone: the ones that could not be seen properly,
    // and then each one a single operation already takes care of.
    let mut done: Vec<String> = context.unknown.clone();
    let holds_unknown = |path: &str| context.unknown.iter().any(|u| is_under(u, path));
    // Paths whose row was forgotten: left alone until the next round.
    let mut deferred: HashSet<String> = HashSet::new();
    // Local paths this plan removes or sets aside, so their name is free.
    let mut freed: HashSet<String> = HashSet::new();
    let under = |list: &[String], path: &str| list.iter().any(|p| is_under(path, p));
    let forget = |row: &Row| Op::Forget {
        id: row.node.id.clone(),
        path: row.path.clone(),
    };

    for row in base {
        if under(&done, &row.path) {
            continue;
        }
        let id = row.node.id.clone();
        let path = row.path.clone();
        if remote.paths.get(&id).is_some_and(|p| p != &path) {
            // Moved in iCloud and not yet followed here (the move rules
            // come first and should have dealt with it): never read that
            // as a deletion.
            done.push(path);
            continue;
        }
        let node = remote
            .nodes
            .get(&id)
            .filter(|n| remote.paths[&n.id] == row.path && (n.kind == Kind::Folder) == row.local.is_dir);
        let entry = local.get(&path).filter(|e| e.is_dir == row.local.is_dir);
        if local.get(&path).is_some() && entry.is_none() {
            // A file where a folder was, or the other way round.
            plan.ops.push(forget(row));
            deferred.insert(path);
            continue;
        }
        if row.local.is_dir {
            match (node, entry) {
                (Some(_), Some(_)) => {}
                (Some(_), None) => {
                    // The folder was removed here. If nothing below it changed
                    // in iCloud, it goes to Recently Deleted as one; else it
                    // comes back and its contents are judged one by one.
                    let below: Vec<&Row> = base.iter().filter(|r| is_under(&r.path, &path)).collect();
                    let known: HashSet<&str> = below.iter().map(|r| r.node.id.as_str()).collect();
                    let untouched = below.iter().all(|r| {
                        remote
                            .nodes
                            .get(&r.node.id)
                            .is_some_and(|n| remote.paths[&n.id] == r.path && (r.local.is_dir || !remote_changed(r, n)))
                    }) && remote
                        .by_path
                        .range(path.clone()..)
                        .take_while(|(p, _)| p.starts_with(&path))
                        .filter(|(p, _)| is_under(p, &path))
                        .all(|(_, id)| known.contains(id.as_str()));
                    if untouched {
                        plan.deletions += below.len();
                        plan.ops.push(Op::TrashRemote { id, path: path.clone() });
                        done.push(path);
                    } else {
                        plan.ops.push(Op::MkdirLocal { id, path });
                    }
                }
                (None, Some(_)) => {
                    // The folder was removed in iCloud. If everything in it
                    // here is as it was, it goes to the trash as one; else it
                    // stays, and what is new or changed in it goes back up.
                    let untouched = !holds_unknown(&path)
                        && local
                            .range(path.clone()..)
                            .take_while(|(p, _)| p.starts_with(&path))
                            .filter(|(p, _)| is_under(p, &path))
                            .all(|(p, e)| {
                                tracked.get(p.as_str()).is_some_and(|r| {
                                    r.local.is_dir == e.is_dir
                                        && (e.is_dir || !local_changed(r, e))
                                        && !remote.nodes.contains_key(&r.node.id)
                                })
                            });
                    if untouched {
                        plan.deletions += base.iter().filter(|r| is_under(&r.path, &path)).count();
                        plan.ops.push(Op::TrashLocal { id, path: path.clone() });
                        freed.insert(path.clone());
                        done.push(path);
                    } else {
                        plan.ops.push(forget(row));
                        deferred.insert(path);
                    }
                }
                (None, None) => plan.ops.push(forget(row)),
            }
            continue;
        }
        match (node, entry) {
            (Some(n), Some(e)) => match (remote_changed(row, n), local_changed(row, e)) {
                (false, false) => {}
                (true, false) => plan.ops.push(Op::Download { id, path }),
                (false, true) => {
                    if settling(e, now_ns) {
                        plan.pending.push(path);
                    } else {
                        plan.ops.push(Op::Replace { id, path });
                    }
                }
                (true, true) => {
                    plan.ops.push(Op::SetAside { path: path.clone() });
                    plan.ops.push(Op::Download { id, path });
                }
            },
            (Some(n), None) => {
                if remote_changed(row, n) {
                    plan.ops.push(Op::Download { id, path });
                } else {
                    plan.deletions += 1;
                    plan.ops.push(Op::TrashRemote { id, path });
                }
            }
            (None, Some(e)) => {
                if local_changed(row, e) {
                    plan.ops.push(forget(row));
                    deferred.insert(path);
                } else {
                    plan.deletions += 1;
                    plan.ops.push(Op::TrashLocal { id, path: path.clone() });
                    freed.insert(path);
                }
            }
            (None, None) => plan.ops.push(forget(row)),
        }
    }

    // Folders that will exist on both sides once this plan has run: a new
    // item needs its parent to be one of them.
    let mut ready: HashSet<String> = base
        .iter()
        .filter(|r| r.local.is_dir && remote.nodes.contains_key(&r.node.id) && !under(&done, &r.path))
        .map(|r| r.path.clone())
        .collect();
    let parent_ready = |ready: &HashSet<String>, path: &str| {
        let parent = parent_of(path);
        parent.is_empty() || ready.contains(parent)
    };
    let known: HashSet<&str> = base.iter().map(|r| r.node.id.as_str()).collect();
    let mut claimed: HashSet<&str> = HashSet::new();

    // New in iCloud.
    for (path, id) in &remote.by_path {
        if known.contains(id.as_str()) || under(&done, path) || deferred.contains(path) {
            continue;
        }
        let node = &remote.nodes[id];
        if ignored.get(id).is_some_and(|etag| etag == &node.etag) {
            claimed.insert(path);
            continue;
        }
        if !parent_ready(&ready, path) {
            continue;
        }
        claimed.insert(path);
        // What is at that name here, unless this plan is taking it away. A
        // row still holding the name is on its way out (its item is gone
        // from iCloud); wait for that unless its file is being trashed.
        let entry = if freed.contains(path) { None } else { local.get(path) };
        if entry.is_some() && tracked.contains_key(path.as_str()) {
            continue;
        }
        let (id, path) = (id.clone(), path.clone());
        if node.kind == Kind::Folder {
            match entry {
                Some(e) if e.is_dir => plan.ops.push(Op::Adopt { id, path: path.clone() }),
                Some(_) => {
                    plan.ops.push(Op::SetAside { path: path.clone() });
                    plan.ops.push(Op::MkdirLocal { id, path: path.clone() });
                }
                None => plan.ops.push(Op::MkdirLocal { id, path: path.clone() }),
            }
            ready.insert(path);
        } else {
            match entry {
                Some(e)
                    if !e.is_dir
                        && e.size == node.size
                        && (e.mtime_ns / 1_000_000 - node.mtime_ms).abs() <= SAME_TIME_MS =>
                {
                    plan.ops.push(Op::Adopt { id, path })
                }
                Some(_) => {
                    plan.ops.push(Op::SetAside { path: path.clone() });
                    plan.ops.push(Op::Download { id, path });
                }
                None => plan.ops.push(Op::Download { id, path }),
            }
        }
    }

    // New here.
    for (path, entry) in local {
        if tracked.contains_key(path.as_str())
            || claimed.contains(path.as_str())
            || under(&done, path)
            || deferred.contains(path)
            || !parent_ready(&ready, path)
        {
            continue;
        }
        if entry.is_dir {
            plan.ops.push(Op::MkdirRemote { path: path.clone() });
            ready.insert(path.clone());
        } else if settling(entry, now_ns) {
            plan.pending.push(path.clone());
        } else {
            plan.ops.push(Op::Upload { path: path.clone() });
        }
    }

    plan.again = plan
        .ops
        .iter()
        .any(|op| matches!(op, Op::Forget { .. } | Op::SetAside { .. }));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000_000_000_000;

    fn node(id: &str, parent: &str, name: &str, dir: bool, size: u64, mtime_ms: i64) -> Node {
        Node {
            id: id.into(),
            docwsid: id.into(),
            zone: "z".into(),
            parent: if parent.is_empty() {
                ROOT_ID.into()
            } else {
                parent.into()
            },
            name: name.into(),
            kind: if dir { Kind::Folder } else { Kind::File },
            etag: format!("e{size}{mtime_ms}"),
            size,
            mtime_ms,
        }
    }

    fn file(size: u64, mtime_ns: i64, ino: u64) -> Entry {
        Entry {
            is_dir: false,
            size,
            mtime_ns,
            ino,
        }
    }

    fn dir(ino: u64) -> Entry {
        Entry {
            is_dir: true,
            size: 0,
            mtime_ns: 0,
            ino,
        }
    }

    fn row(node: &Node, path: &str, local: Entry) -> Row {
        Row {
            node: node.clone(),
            path: path.into(),
            local,
        }
    }

    fn context() -> Context {
        Context {
            now_ns: NOW,
            ..Context::default()
        }
    }

    fn run(base: &[Row], remote: &[Node], local: &[(&str, Entry)]) -> Plan {
        let local: BTreeMap<String, Entry> = local.iter().map(|(p, e)| (p.to_string(), *e)).collect();
        plan(base, &Snapshot::build(remote), &local, &context())
    }

    #[test]
    fn nothing_changed_means_nothing_to_do() {
        let n = node("f", "", "a.txt", false, 5, 100);
        let p = run(
            &[row(&n, "a.txt", file(5, 7, 1))],
            std::slice::from_ref(&n),
            &[("a.txt", file(5, 7, 1))],
        );
        assert_eq!(p, Plan::default());
    }

    #[test]
    fn first_run_downloads_what_is_only_in_icloud_and_uploads_what_is_only_here() {
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "d", "a.txt", false, 5, 100);
        let p = run(&[], &[d, f], &[("mine", dir(1)), ("mine/b.txt", file(3, 9, 2))]);
        assert_eq!(
            p.ops,
            [
                Op::MkdirLocal {
                    id: "d".into(),
                    path: "Docs".into()
                },
                Op::Download {
                    id: "f".into(),
                    path: "Docs/a.txt".into()
                },
                Op::MkdirRemote { path: "mine".into() },
                Op::Upload {
                    path: "mine/b.txt".into()
                },
            ]
        );
        assert_eq!(p.deletions, 0);
    }

    #[test]
    fn first_run_adopts_identical_files_and_keeps_both_of_differing_ones() {
        let same = node("s", "", "same.txt", false, 5, 100);
        let differ = node("x", "", "differ.txt", false, 5, 100);
        let p = run(
            &[],
            &[same, differ],
            &[
                ("differ.txt", file(6, 100_000_000, 1)),
                ("same.txt", file(5, 100_000_000, 2)),
            ],
        );
        assert_eq!(
            p.ops,
            [
                Op::SetAside {
                    path: "differ.txt".into()
                },
                Op::Download {
                    id: "x".into(),
                    path: "differ.txt".into()
                },
                Op::Adopt {
                    id: "s".into(),
                    path: "same.txt".into()
                },
            ]
        );
        assert!(p.again, "the conflicted copy uploads in the next round");
    }

    #[test]
    fn the_side_that_changed_wins() {
        let old = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&old, "a.txt", file(5, 7, 1))];
        let newer = node("f", "", "a.txt", false, 6, 200);
        assert_eq!(
            run(&base, &[newer], &[("a.txt", file(5, 7, 1))]).ops,
            [Op::Download {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
        assert_eq!(
            run(&base, std::slice::from_ref(&old), &[("a.txt", file(9, 8, 1))]).ops,
            [Op::Replace {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
    }

    #[test]
    fn both_changed_keeps_both() {
        let old = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&old, "a.txt", file(5, 7, 1))];
        let newer = node("f", "", "a.txt", false, 6, 200);
        let p = run(&base, &[newer], &[("a.txt", file(9, 8, 1))]);
        assert_eq!(
            p.ops,
            [
                Op::SetAside { path: "a.txt".into() },
                Op::Download {
                    id: "f".into(),
                    path: "a.txt".into()
                }
            ]
        );
    }

    #[test]
    fn a_file_still_being_written_waits() {
        let old = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&old, "a.txt", file(5, 7, 1))];
        let p = run(
            &base,
            &[old],
            &[("a.txt", file(9, NOW - 1, 1)), ("new.txt", file(1, NOW, 2))],
        );
        assert!(p.ops.is_empty());
        assert_eq!(p.pending, ["a.txt", "new.txt"]);
    }

    #[test]
    fn deletes_go_to_the_other_sides_trash_when_it_is_unchanged() {
        let n = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&n, "a.txt", file(5, 7, 1))];
        let p = run(&base, std::slice::from_ref(&n), &[]);
        assert_eq!(
            p.ops,
            [Op::TrashRemote {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
        assert_eq!(p.deletions, 1);
        let p = run(&base, &[], &[("a.txt", file(5, 7, 1))]);
        assert_eq!(
            p.ops,
            [Op::TrashLocal {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
    }

    #[test]
    fn an_edit_beats_a_delete() {
        let n = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&n, "a.txt", file(5, 7, 1))];
        // Deleted here, edited in iCloud: it comes back.
        let newer = node("f", "", "a.txt", false, 6, 200);
        assert_eq!(
            run(&base, &[newer], &[]).ops,
            [Op::Download {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
        // Deleted in iCloud, edited here: forgotten, then uploaded as new.
        let p = run(&base, &[], &[("a.txt", file(9, 8, 1))]);
        assert_eq!(
            p.ops,
            [Op::Forget {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
        assert!(p.again);
        assert_eq!(p.deletions, 0);
        let p = run(&[], &[], &[("a.txt", file(9, 8, 1))]);
        assert_eq!(p.ops, [Op::Upload { path: "a.txt".into() }]);
    }

    #[test]
    fn a_folder_removed_here_goes_to_recently_deleted_as_one() {
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "d", "a.txt", false, 5, 100);
        let base = [row(&d, "Docs", dir(1)), row(&f, "Docs/a.txt", file(5, 7, 2))];
        let p = run(&base, &[d.clone(), f.clone()], &[]);
        assert_eq!(
            p.ops,
            [Op::TrashRemote {
                id: "d".into(),
                path: "Docs".into()
            }]
        );
        assert_eq!(p.deletions, 2);
    }

    #[test]
    fn a_folder_removed_here_comes_back_when_icloud_added_to_it() {
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "d", "a.txt", false, 5, 100);
        let new = node("n", "d", "new.txt", false, 1, 300);
        let base = [row(&d, "Docs", dir(1)), row(&f, "Docs/a.txt", file(5, 7, 2))];
        let p = run(&base, &[d.clone(), f.clone(), new], &[]);
        assert_eq!(
            p.ops,
            [
                Op::MkdirLocal {
                    id: "d".into(),
                    path: "Docs".into()
                },
                Op::TrashRemote {
                    id: "f".into(),
                    path: "Docs/a.txt".into()
                },
                Op::Download {
                    id: "n".into(),
                    path: "Docs/new.txt".into()
                },
            ]
        );
    }

    #[test]
    fn a_folder_removed_in_icloud_goes_to_the_trash_unless_something_in_it_is_new() {
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "d", "a.txt", false, 5, 100);
        let base = [row(&d, "Docs", dir(1)), row(&f, "Docs/a.txt", file(5, 7, 2))];
        let p = run(&base, &[], &[("Docs", dir(1)), ("Docs/a.txt", file(5, 7, 2))]);
        assert_eq!(
            p.ops,
            [Op::TrashLocal {
                id: "d".into(),
                path: "Docs".into()
            }]
        );
        assert_eq!(p.deletions, 2);

        let p = run(
            &base,
            &[],
            &[
                ("Docs", dir(1)),
                ("Docs/a.txt", file(5, 7, 2)),
                ("Docs/mine.txt", file(1, 9, 3)),
            ],
        );
        assert_eq!(
            p.ops,
            [
                Op::Forget {
                    id: "d".into(),
                    path: "Docs".into()
                },
                Op::TrashLocal {
                    id: "f".into(),
                    path: "Docs/a.txt".into()
                },
            ]
        );
        assert!(p.again, "the folder and the new file go up in the next round");
    }

    #[test]
    fn a_rename_in_icloud_is_a_rename_here_done_alone() {
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "d", "a.txt", false, 5, 100);
        let base = [row(&d, "Docs", dir(1)), row(&f, "Docs/a.txt", file(5, 7, 2))];
        let renamed = node("d", "", "Papers", true, 0, 0);
        let newer = node("f", "d", "a.txt", false, 6, 200);
        let p = run(
            &base,
            &[renamed, newer],
            &[("Docs", dir(1)), ("Docs/a.txt", file(5, 7, 2))],
        );
        assert_eq!(
            p.ops,
            [Op::LocalMove {
                id: "d".into(),
                from: "Docs".into(),
                to: "Papers".into()
            }]
        );
        assert!(p.again, "the changed file inside waits for the next round");
    }

    #[test]
    fn a_rename_here_is_a_rename_in_icloud_found_by_inode() {
        let f = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&f, "a.txt", file(5, 7, 2))];
        let p = run(
            &base,
            std::slice::from_ref(&f),
            &[("new", dir(9)), ("new/b.txt", file(5, 7, 2))],
        );
        assert_eq!(
            p.ops,
            [
                Op::MkdirRemote { path: "new".into() },
                Op::RemoteMove {
                    id: "f".into(),
                    to: "new/b.txt".into()
                }
            ]
        );
        // A different file that happens to reuse the inode is not a move.
        let p = run(&base, std::slice::from_ref(&f), &[("b.txt", file(6, 8, 2))]);
        assert_eq!(
            p.ops,
            [
                Op::TrashRemote {
                    id: "f".into(),
                    path: "a.txt".into()
                },
                Op::Upload { path: "b.txt".into() }
            ]
        );
    }

    #[test]
    fn a_move_onto_an_untracked_local_file_sets_it_aside() {
        let f = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&f, "a.txt", file(5, 7, 2))];
        let renamed = node("f", "", "b.txt", false, 5, 100);
        let p = run(&base, &[renamed], &[("a.txt", file(5, 7, 2)), ("b.txt", file(1, 1, 3))]);
        assert_eq!(
            p.ops,
            [
                Op::SetAside { path: "b.txt".into() },
                Op::LocalMove {
                    id: "f".into(),
                    from: "a.txt".into(),
                    to: "b.txt".into()
                }
            ]
        );
    }

    #[test]
    fn two_files_that_swapped_names_are_forgotten_not_looped_on() {
        let a = node("a", "", "a.txt", false, 5, 100);
        let b = node("b", "", "b.txt", false, 6, 100);
        let base = [row(&a, "a.txt", file(5, 7, 1)), row(&b, "b.txt", file(6, 7, 2))];
        let a2 = node("a", "", "b.txt", false, 5, 100);
        let b2 = node("b", "", "a.txt", false, 6, 100);
        let p = run(&base, &[a2, b2], &[("a.txt", file(5, 7, 1)), ("b.txt", file(6, 7, 2))]);
        assert!(p.ops.iter().all(|op| matches!(op, Op::Forget { .. })), "{:?}", p.ops);
        assert_eq!(p.ops.len(), 2);
    }

    #[test]
    fn a_file_replaced_by_a_new_one_in_icloud_is_trashed_then_fetched() {
        let old = node("old", "", "a.txt", false, 5, 100);
        let base = [row(&old, "a.txt", file(5, 7, 1))];
        let new = node("new", "", "a.txt", false, 8, 300);
        let p = run(&base, &[new], &[("a.txt", file(5, 7, 1))]);
        assert_eq!(
            p.ops,
            [
                Op::TrashLocal {
                    id: "old".into(),
                    path: "a.txt".into()
                },
                Op::Download {
                    id: "new".into(),
                    path: "a.txt".into()
                }
            ]
        );
    }

    #[test]
    fn app_folders_and_hidden_names_are_left_out() {
        let mut lib = node("lib", "", "Pages", true, 0, 0);
        lib.kind = Kind::AppLibrary;
        let inside = node("in", "lib", "doc.pages", false, 5, 1);
        let hidden = node("h", "", ".secret", false, 1, 1);
        let snapshot = Snapshot::build(&[lib, inside, hidden]);
        assert!(snapshot.nodes.is_empty());
        assert_eq!(snapshot.skipped.len(), 2);
    }

    #[test]
    fn an_ignored_item_is_left_alone_until_it_changes() {
        let pkg = node("p", "", "doc.pages", false, 5, 100);
        let local = BTreeMap::new();
        let mut context = context();
        context.ignored.insert("p".to_string(), pkg.etag.clone());
        let p = plan(&[], &Snapshot::build(std::slice::from_ref(&pkg)), &local, &context);
        assert!(p.ops.is_empty());
        context.ignored.insert("p".to_string(), "older".to_string());
        let p = plan(&[], &Snapshot::build(&[pkg]), &local, &context);
        assert_eq!(p.ops.len(), 1);
    }

    #[test]
    fn what_could_not_be_seen_is_never_read_as_gone() {
        // a.txt is in step, then turns into something the scan leaves out
        // (a link, or the loser of two names that differ only by case).
        let a = node("f", "", "a.txt", false, 5, 100);
        let d = node("d", "", "Docs", true, 0, 0);
        let inside = node("i", "d", "in.txt", false, 5, 100);
        let base = [
            row(&d, "Docs", dir(1)),
            row(&inside, "Docs/in.txt", file(5, 7, 3)),
            row(&a, "a.txt", file(5, 7, 2)),
        ];
        let local: BTreeMap<String, Entry> = [("Docs".to_string(), dir(1))].into();
        let mut context = context();
        context.unknown = vec!["a.txt".into(), "Docs/in.txt".into()];
        let p = plan(&base, &Snapshot::build(&[a, d.clone(), inside]), &local, &context);
        assert_eq!(p, Plan::default());
        // And a folder iCloud removed is kept while it holds such a path.
        let p = plan(&base[..2], &Snapshot::build(&[]), &local, &context);
        assert!(
            !p.ops.iter().any(|op| matches!(op, Op::TrashLocal { .. })),
            "{:?}",
            p.ops
        );
    }
}
