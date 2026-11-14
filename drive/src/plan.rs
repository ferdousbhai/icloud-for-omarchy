//! The decisions: given what was in step last time (`base`), what iCloud
//! has now and what the folder has now, which operations bring the two back
//! in step. Pure: nothing here touches a file or the network, so every rule
//! has a test.
//!
//! The rules, in short:
//! - A side that changed while the other did not wins.
//! - Both changed: both are kept. The local file is set aside as a
//!   "conflicted copy" (which then uploads) and iCloud's version takes the
//!   name, unless the two turn out to be the same bytes.
//! - An edit beats a delete: the edited side is kept and comes back.
//! - A delete is only believed for something that was in step and is
//!   unchanged on the other side; it goes to the trash there.
//! - Moves and renames are done first, alone, so everything after compares
//!   like with like. A round that moved something asks for another round.
//! - When the picture is unclear the row is forgotten: both sides then meet
//!   as strangers, which can cost a duplicate but never a file.
//! - Names that differ only by case are one name to iCloud: a new item in
//!   iCloud and one here of such names are one item (or both kept, if they
//!   differ), and nothing new is sent beside such a name in iCloud.
//! - A path one side could not show properly (a link, two names that differ
//!   only by case, two items of one name in iCloud) is `unknown`: nothing
//!   at or below it is touched, since "not seen" must never read as "gone".

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::local::Entry;
use crate::remote::{Kind, Node};
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
    /// From a flat listing of everything below the folder `top`. App
    /// libraries and what is below them are left out, as are names that
    /// cannot exist or would be hidden locally.
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
    /// It was moved here into a new folder of its own old name, `to`: it is
    /// renamed `name` in iCloud first, and recorded at `to` with its move
    /// noted, which the next round finishes.
    RemoteAside {
        id: String,
        to: String,
        name: String,
    },
    /// Rename the local file to a conflicted copy, freeing its name.
    SetAside {
        path: String,
    },
    /// Both sides have a different-looking file at `path`: fetch iCloud's.
    /// The same bytes are only recorded as in step; different ones are
    /// both kept, the local file as a conflicted copy.
    KeepBoth {
        id: String,
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
pub(crate) const SAME_TIME_MS: i64 = 2_000;

/// The last name of a path.
pub(crate) fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether the folder in step at `path`, removed here, is as it was in
/// iCloud: everything below it in step there and unchanged, and nothing
/// there that was never synced (a hidden name, say), which removing the
/// folder here cannot have meant.
fn untouched_there(base: &[Row], remote: &Snapshot, path: &str, unknown: &[String]) -> bool {
    let below: Vec<&Row> = base.iter().filter(|r| is_under(&r.path, path)).collect();
    let known: HashSet<&str> = below.iter().map(|r| r.node.id.as_str()).collect();
    !unknown.iter().any(|u| is_under(u, path))
        && below.iter().all(|r| {
            remote
                .nodes
                .get(&r.node.id)
                .is_some_and(|n| remote.paths[&n.id] == r.path && (r.local.is_dir || !remote_changed(r, n)))
        })
        && remote
            .by_path
            .range(path.to_string()..)
            .take_while(|(p, _)| p.starts_with(path))
            .filter(|(p, _)| is_under(p, path))
            .all(|(_, id)| known.contains(id.as_str()))
}

/// A name [`Op::RemoteAside`] gave an item called `base`.
fn is_aside_of(name: &str, base: &str) -> bool {
    name.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix(" (moving "))
        .is_some_and(|rest| rest.ends_with(')'))
}

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

/// A local file and an iCloud one of the same size and date: taken as the
/// same file.
fn same_file(entry: &Entry, node: &Node) -> bool {
    !entry.is_dir && entry.size == node.size && (entry.mtime_ns / 1_000_000 - node.mtime_ms).abs() <= SAME_TIME_MS
}

fn local_changed(row: &Row, entry: &Entry) -> bool {
    entry.size != row.local.size || entry.mtime_ns != row.local.mtime_ns
}

/// Paths by folder and name in lower case: to iCloud a name of another
/// case is the same name.
fn case_index<'a>(paths: impl Iterator<Item = &'a String>) -> HashMap<(&'a str, String), &'a str> {
    paths.map(|p| (case_key(p), p.as_str())).collect()
}

fn case_key(path: &str) -> (&str, String) {
    (parent_of(path), name_of(path).to_lowercase())
}

/// What the planner is told beyond the three views.
#[derive(Debug, Default)]
pub struct Context {
    /// Items in iCloud to leave alone while their etag is this one.
    pub ignored: HashMap<String, String>,
    /// Paths that could not be seen properly on one side.
    pub unknown: Vec<String>,
    /// Item id to the local path a move in iCloud was heading for when a
    /// run stopped before finishing and recording it.
    pub pending_moves: HashMap<String, String>,
    /// The time, in Unix nanoseconds.
    pub now_ns: i64,
}

pub fn plan(base: &[Row], remote: &Snapshot, local: &BTreeMap<String, Entry>, context: &Context) -> Plan {
    let moves = plan_moves(base, remote, local, context);
    if !moves.plan.ops.is_empty() {
        return moves.plan;
    }
    let mut plan = plan_content(base, remote, local, context, &moves.held);
    plan.again |= moves.waiting;
    plan
}

/// What the move rules came to.
struct Moves {
    plan: Plan,
    /// Paths the content rules leave alone: of rows moved here to where
    /// nothing is touched (neither moved nor read as removed here), and
    /// where items whose move waits are here now (not sent as new).
    held: Vec<String>,
    /// A move in iCloud waits for the content rules to take what is gone
    /// from iCloud out of its way here: another round follows them.
    waiting: bool,
}

/// Moves and renames only. A row whose folder is itself moving waits for the
/// next round, when its folder's move has carried it along.
fn plan_moves(base: &[Row], remote: &Snapshot, local: &BTreeMap<String, Entry>, context: &Context) -> Moves {
    let unknown = &context.unknown;
    let unseen = |path: &str| unknown.iter().any(|u| is_under(path, u));
    let tracked_ids: HashMap<&str, &str> = base.iter().map(|r| (r.path.as_str(), r.node.id.as_str())).collect();
    let tracked: HashSet<&str> = tracked_ids.keys().copied().collect();
    // Local entries by inode that are not what is in step at their path:
    // where a tracked one may have gone (onto another's place, too).
    let ino_at: HashMap<&str, u64> = base.iter().map(|r| (r.path.as_str(), r.local.ino)).collect();
    let inodes: HashSet<u64> = ino_at.values().copied().collect();
    let mut by_ino: HashMap<u64, Vec<&str>> = HashMap::new();
    for (path, entry) in local {
        if ino_at.get(path.as_str()) != Some(&entry.ino) {
            by_ino.entry(entry.ino).or_default().push(path);
        }
    }
    let moved_here = |row: &Row| -> Option<&str> {
        by_ino.get(&row.local.ino)?.iter().copied().find(|p| {
            let e = &local[*p];
            e.is_dir == row.local.is_dir && (e.is_dir || !local_changed(row, e))
        })
    };
    // Still where it was in step here: what is at its path, unless that is
    // another file and this one is found, as it was, where it was moved to.
    let here = |row: &Row| {
        local
            .get(&row.path)
            .filter(|e| e.is_dir == row.local.is_dir && (e.ino == row.local.ino || moved_here(row).is_none()))
    };
    // Where folders are moving to, on each side, and where they were in
    // step. An item moved into one of them waits a round for the folder to
    // get there first (or away), rather than making a second folder of
    // that name or missing the one it is in.
    let mut dests_there: HashSet<&str> = HashSet::new();
    let mut dests_here: HashSet<&str> = HashSet::new();
    let mut left_here: HashSet<&str> = HashSet::new();
    let mut left_there: HashSet<&str> = HashSet::new();
    for row in base.iter().filter(|r| r.local.is_dir) {
        match remote.paths.get(&row.node.id) {
            Some(rpath) if rpath != &row.path => {
                dests_there.insert(rpath);
                left_there.insert(&row.path);
                // And here, from where it was moved to, to follow iCloud.
                if here(row).is_none() {
                    left_here.extend(moved_here(row));
                }
            }
            Some(_) if here(row).is_none() => {
                if let Some(to) = moved_here(row) {
                    dests_here.insert(to);
                    left_here.insert(&row.path);
                }
            }
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
    let mut made_here: HashSet<String> = HashSet::new();
    let by_case = case_index(local.keys().filter(|p| !tracked.contains(p.as_str())));
    let there_by_case = case_index(remote.by_path.keys());
    let all_by_case = case_index(local.keys());
    let mut held: Vec<String> = Vec::new();
    let mut waiting = false;
    let mut deletions = 0;
    // Moves made here that wait for a folder's move, and where each is now.
    let mut waited: Vec<(&Row, &str)> = Vec::new();
    for row in base {
        if moving.iter().any(|p| is_under(&row.path, p)) {
            continue;
        }
        let rpath = remote.paths.get(&row.node.id);
        if unseen(&row.path) || rpath.is_some_and(|p| unseen(p)) {
            // In or into where nothing is touched (a folder whose move
            // iCloud refused, say), and moved here: left where it is, not
            // sent as new, until that clears.
            if let Some(to) = moved_here(row).filter(|p| !unseen(p)) {
                held.push(to.to_string());
            }
            continue;
        }
        let Some(rpath) = rpath else {
            continue;
        };
        // A move made here that an earlier run did not finish in iCloud
        // (a move there is the folder, then the name, or the other way
        // round: under its old name or in its old folder it is half way, at
        // the noted destination it is done and was not
        // recorded): where iCloud has it is that move's doing, not a move
        // made in iCloud to follow here, which would undo what was done
        // here. It is finished as a move made here, to where the item is
        // here now: where it was moved to, or since moved on to, or back
        // where it was in step. Not when something else in iCloud has that
        // name: then iCloud's place is followed after all.
        let ours = rpath != &row.path
            && context.pending_moves.get(&row.node.id).is_some_and(|to| {
                rpath == to
                    || name_of(rpath) == name_of(&row.path)
                    || is_aside_of(name_of(rpath), name_of(to))
                    // Renamed first, where the folder it goes to held its name.
                    || (parent_of(rpath) == parent_of(&row.path) && name_of(rpath) == name_of(to))
            });
        let finish = if !ours {
            None
        } else if here(row).is_some() {
            Some(row.path.as_str())
        } else {
            moved_here(row)
        }
        .filter(|to| to != rpath && !remote.by_path.contains_key(*to));
        if rpath != &row.path && finish.is_none() {
            // Moved in iCloud. Find it here: where it was, or where it was moved to.
            let from = if here(row).is_some() {
                Some(row.path.as_str())
            } else {
                moved_here(row)
            };
            if let Some(from) = from.filter(|f| f != rpath && is_under(rpath, f)) {
                // Into where it is now here (another folder took its name
                // in iCloud, and it went into that one): out of the way
                // first, under a name of its own, and on from there.
                let free = |p: &String| !local.contains_key(p) && !remote.by_path.contains_key(p);
                let aside = (1..)
                    .map(|n| join(parent_of(from), &format!("{} (moving {n})", name_of(from))))
                    .find(free)
                    .unwrap();
                moving.push(&row.path);
                ops.push(Op::LocalMove {
                    id: row.node.id.clone(),
                    from: from.to_string(),
                    to: aside,
                });
                continue;
            }
            // (Not where this one itself is leaving.)
            let left: HashSet<&str> = left_here.iter().copied().filter(|p| Some(*p) != from).collect();
            if into(&dests_there, rpath) || into(&left, rpath) {
                continue;
            }
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
            // Folders it was moved into that are new in iCloud: made here
            // first, or taken from one of that name here (under another
            // case too, which the next round renames, and the move waits
            // for), so that none is made here twice.
            let mut missing = Vec::new();
            let mut later = false;
            let mut dir = parent_of(rpath);
            while !dir.is_empty() && !made_here.contains(dir) {
                let id = &remote.by_path[dir];
                if tracked_ids.get(dir) == Some(&id.as_str()) {
                    break;
                }
                match local.get(dir) {
                    // A folder in step that was moved here to that name:
                    // not the new one, which waits for it to be sorted out.
                    Some(e) if !tracked.contains(dir) && inodes.contains(&e.ino) => {
                        missing.clear();
                        later = true;
                        break;
                    }
                    Some(e) if e.is_dir && !tracked.contains(dir) => missing.push((
                        dir,
                        Op::Adopt {
                            id: id.clone(),
                            path: dir.to_string(),
                        },
                    )),
                    // Something else is there: the move is tried as it is.
                    Some(_) => {
                        missing.clear();
                        break;
                    }
                    None => match by_case.get(&case_key(dir)) {
                        Some(other) if local[*other].is_dir && !inodes.contains(&local[*other].ino) => {
                            missing = vec![(
                                dir,
                                Op::Adopt {
                                    id: id.clone(),
                                    path: other.to_string(),
                                },
                            )];
                            later = true;
                        }
                        _ => missing.push((
                            dir,
                            Op::MkdirLocal {
                                id: id.clone(),
                                path: dir.to_string(),
                            },
                        )),
                    },
                }
                dir = parent_of(dir);
            }
            for (dir, op) in missing.into_iter().rev() {
                made_here.insert(dir.to_string());
                ops.push(op);
            }
            if later {
                continue;
            }
            let other_case = all_by_case
                .get(&case_key(rpath))
                .copied()
                .filter(|p| *p != rpath.as_str() && *p != from);
            if let Some(other) = other_case {
                // Something here whose name iCloud takes as this one's: set
                // aside, unless it is another item in step (which may be
                // about to move away itself).
                let holder = tracked_ids.get(other).filter(|id| **id != row.node.id.as_str());
                if holder.is_some_and(|id| remote.paths.contains_key(*id)) {
                    stuck.push(row);
                    continue;
                }
                ops.push(Op::SetAside {
                    path: other.to_string(),
                });
            } else if !local.contains_key(rpath)
                && tracked_ids
                    .get(rpath.as_str())
                    .is_some_and(|holder| remote.paths.contains_key(*holder))
            {
                // Another item in step still holds the name (moved away
                // here, and that not recorded yet): wait for it.
                stuck.push(row);
                continue;
            } else if local.contains_key(rpath) {
                if let Some(holder) = tracked_ids.get(rpath.as_str()) {
                    if remote.paths.contains_key(*holder) {
                        // The destination is another item in step, which
                        // may be about to move away itself: wait.
                        stuck.push(row);
                    } else {
                        // That item is gone from iCloud (removed there, and
                        // this one given its name): the content rules take
                        // it out of the way first, and leave this one be.
                        waiting = true;
                        if from != row.path {
                            held.push(from.to_string());
                        }
                    }
                    continue;
                }
                ops.push(Op::SetAside { path: rpath.clone() });
            }
            ops.push(Op::LocalMove {
                id: row.node.id.clone(),
                from: from.to_string(),
                to: rpath.clone(),
            });
        } else if let Some(to) = finish.or_else(|| moved_here(row).filter(|_| here(row).is_none())) {
            // Moved here; iCloud still has it at the old place.
            if unseen(to) {
                // Moved to where nothing is touched (into a folder whose
                // own move iCloud refused, say). The file is here, so this
                // is no removal: it stays in iCloud until that clears.
                held.push(row.path.clone());
                continue;
            }
            if to != row.path && is_under(to, &row.path) {
                // Into a new folder of its own old name here: renamed in
                // iCloud first to a name of its own (or kept under the one
                // an earlier run gave it), so that its old name is free for
                // the new folder; the next round finishes the move.
                let base = name_of(&row.path);
                let name = if is_aside_of(name_of(rpath), base) {
                    name_of(rpath).to_string()
                } else {
                    (1..)
                        .map(|n| format!("{base} (moving {n})"))
                        .find(|n| !remote.by_path.contains_key(&join(parent_of(rpath), n)))
                        .unwrap()
                };
                moving.push(&row.path);
                ops.push(Op::RemoteAside {
                    id: row.node.id.clone(),
                    to: to.to_string(),
                    name,
                });
                continue;
            }
            let there = remote.by_path.get(to).or_else(|| {
                there_by_case
                    .get(&case_key(to))
                    .filter(|p| **p != to && **p != rpath.as_str())
                    .and_then(|p| remote.by_path.get(*p))
            });
            if let Some(there) = there {
                let holder = base.iter().find(|r| &r.node.id == there);
                match holder {
                    None => {
                        // Something new in iCloud took that name (in any
                        // case): this one is kept under a conflicted name,
                        // which the next round gives it in iCloud too.
                        moving.push(&row.path);
                        ops.push(Op::SetAside { path: to.to_string() });
                    }
                    Some(holder)
                        if case_key(&holder.path) == case_key(to)
                            && local.get(to).is_some_and(|e| e.ino == row.local.ino)
                            && moved_here(holder).is_none() =>
                    {
                        // Moved here over a file in step, which went: that
                        // one goes to Recently Deleted, and the next round
                        // gives this one its name in iCloud. Unless it was
                        // edited there, which beats the removal: then this
                        // one is kept under a conflicted name, and that one
                        // comes back.
                        moving.push(&row.path);
                        let changed = if holder.local.is_dir {
                            !untouched_there(base, remote, &holder.path, unknown)
                        } else {
                            remote_changed(holder, &remote.nodes[there])
                        };
                        if changed {
                            ops.push(Op::SetAside { path: to.to_string() });
                        } else {
                            deletions += base.iter().filter(|r| is_under(&r.path, &holder.path)).count();
                            ops.push(Op::TrashRemote {
                                id: there.clone(),
                                path: holder.path.clone(),
                            });
                        }
                    }
                    // Else another item in step has it, on its way there
                    // in iCloud or away from it here: this one waits for
                    // that, and is not read as removed here meanwhile.
                    Some(_) => held.push(row.path.clone()),
                }
                continue;
            }
            if into(&dests_here, to) || into(&dests_there, to) || into(&left_here, to) || into(&left_there, to) {
                waited.push((row, to));
                continue;
            }
            if tracked_ids
                .get(to)
                .is_some_and(|holder| *holder != row.node.id && remote.paths.contains_key(*holder))
            {
                // Another item in step is still recorded at that name (moved
                // away on both sides, say, which this round records): wait
                // for it, so that recording this move does not drop its row.
                stuck.push(row);
                continue;
            }
            // Folders it was moved into that are not in step yet: iCloud
            // does not have them, or has one of that name that this folder
            // is not in step with (made there too, or made there after the
            // one in step was removed), which is taken as the same folder,
            // as the content rules would. Without them the move could never
            // be done, and no round would get past it.
            let mut missing = Vec::new();
            let mut dir = parent_of(to);
            let mut blocked = false;
            while !dir.is_empty() && !made.contains(dir) {
                let in_step = tracked_ids.get(dir);
                // A folder in iCloud of that name in another case is that
                // folder to iCloud: taken as it, renamed here next round.
                let there = remote
                    .by_path
                    .get(dir)
                    .or_else(|| there_by_case.get(&case_key(dir)).and_then(|p| remote.by_path.get(*p)));
                match there {
                    Some(id) if in_step == Some(&id.as_str()) => break,
                    Some(id)
                        if remote.nodes[id].kind == Kind::Folder
                            && in_step.is_none_or(|old| !remote.paths.contains_key(*old)) =>
                    {
                        missing.push(Op::Adopt {
                            id: id.clone(),
                            path: dir.to_string(),
                        })
                    }
                    // A file of that name in iCloud, or the folder in step
                    // here has moved there: the other rules sort that out
                    // first (this one may read as deleted here).
                    Some(_) => blocked = true,
                    None => missing.push(Op::MkdirRemote { path: dir.to_string() }),
                }
                dir = parent_of(dir);
            }
            if blocked {
                continue;
            }
            for op in missing.into_iter().rev() {
                if let Op::Adopt { path, .. } | Op::MkdirRemote { path } = &op {
                    made.insert(path.clone());
                }
                ops.push(op);
            }
            moving.push(&row.path);
            ops.push(Op::RemoteMove {
                id: row.node.id.clone(),
                to: to.to_string(),
            });
        } else if here(row).is_none()
            && !row.local.is_dir
            && by_ino
                .get(&row.local.ino)
                .is_some_and(|paths| paths.iter().any(|p| !local[*p].is_dir && unseen(p)))
        {
            // Gone from its place, and its inode is a changed file where
            // nothing is touched: moved there and edited. Elsewhere that
            // is a removal and a new file; here the new file cannot be
            // sent yet, so the old one stays in iCloud until it can.
            held.push(row.path.clone());
        }
    }
    // A folder is its id in iCloud and its inode here: one at its path here
    // that is a new folder, while it is elsewhere here, is never taken for
    // it by the content rules (which would read its contents as removed
    // here and send them again from where they are now).
    for row in base.iter().filter(|r| r.local.is_dir) {
        let new_in_its_place = local
            .get(&row.path)
            .is_some_and(|e| e.is_dir && e.ino != row.local.ino && !inodes.contains(&e.ino));
        if new_in_its_place && moved_here(row).is_some() {
            if remote.paths.contains_key(&row.node.id) {
                held.push(row.path.clone());
            } else {
                // Gone from iCloud, and moved here: what it holds is kept,
                // and both folders meet iCloud as new.
                ops.push(Op::Forget {
                    id: row.node.id.clone(),
                    path: row.path.clone(),
                });
            }
        }
    }
    // Waiting on each other (two folders each moved into the other, one
    // here and one in iCloud): iCloud's move wins, as when one item was
    // moved on both sides, and the move here is undone.
    let back = waited.into_iter().find(|(row, _)| {
        let dir = parent_of(&row.path);
        !local.contains_key(&row.path) && (dir.is_empty() || local.get(dir).is_some_and(|e| e.is_dir))
    });
    if let Some((row, now)) = back.filter(|_| ops.is_empty()) {
        ops.push(Op::LocalMove {
            id: row.node.id.clone(),
            from: now.to_string(),
            to: row.path.clone(),
        });
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
    let plan = Plan {
        again: !ops.is_empty(),
        ops,
        deletions,
        ..Plan::default()
    };
    Moves { plan, held, waiting }
}

/// Everything but moves. `held` are the rows the move rules found here
/// under a path that cannot be seen properly.
fn plan_content(
    base: &[Row],
    remote: &Snapshot,
    local: &BTreeMap<String, Entry>,
    context: &Context,
    held: &[String],
) -> Plan {
    let (ignored, now_ns) = (&context.ignored, context.now_ns);
    let mut plan = Plan::default();
    let tracked: HashMap<&str, &Row> = base.iter().map(|r| (r.path.as_str(), r)).collect();
    let inodes: HashSet<u64> = base.iter().map(|r| r.local.ino).collect();
    // Subtrees to leave alone: the ones that could not be seen properly,
    // and then each one a single operation already takes care of.
    let unknown: Vec<String> = context.unknown.iter().chain(held).cloned().collect();
    let mut done: Vec<String> = unknown.clone();
    let holds_unknown = |path: &str| unknown.iter().any(|u| is_under(u, path));
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
            .filter(|n| (n.kind == Kind::Folder) == row.local.is_dir);
        let entry = local.get(&path).filter(|e| e.is_dir == row.local.is_dir);
        if local.get(&path).is_some() && entry.is_none() {
            // A file where a folder was, or the other way round.
            plan.ops.push(forget(row));
            deferred.insert(path);
            continue;
        }
        if row.local.is_dir {
            match (node, entry) {
                // Made again here under its name (the move rules found it
                // nowhere else): this one is it from now on, so that a
                // later rename of it here is seen as one.
                (Some(_), Some(e)) if e.ino != row.local.ino => plan.ops.push(Op::Adopt { id, path }),
                (Some(_), Some(_)) => {}
                (Some(_), None) => {
                    // The folder was removed here. If nothing below it changed
                    // in iCloud, it goes to Recently Deleted as one; else it
                    // comes back and its contents are judged one by one. It
                    // also comes back while iCloud holds something below it
                    // that was never synced (a hidden name, say), which
                    // removing the folder here cannot have meant.
                    let below = base.iter().filter(|r| is_under(&r.path, &path)).count();
                    let untouched = untouched_there(base, remote, &path, &unknown);
                    if untouched {
                        plan.deletions += below;
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
                        // A new folder of its name in iCloud comes next round.
                        plan.again |= remote.by_path.contains_key(&path);
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
                // Both differ from the row and agree with each other: an
                // operation that finished without being recorded.
                (true, true) if same_file(e, n) => plan.ops.push(Op::Adopt { id, path }),
                (true, true) => plan.ops.push(Op::KeepBoth { id, path }),
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
    // Local paths this plan sets aside: what is below them goes up next
    // round, under the name they are given.
    let mut aside: Vec<String> = Vec::new();
    let all_by_case = case_index(local.keys());
    let by_case = case_index(
        local
            .keys()
            .filter(|p| !tracked.contains_key(p.as_str()) && !under(&done, p) && !deferred.contains(*p)),
    );

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
        let other = all_by_case
            .get(&case_key(path))
            .copied()
            .filter(|other| other != path && entry.is_none() && !freed.contains(*other));
        if other.is_some_and(|other| by_case.get(&case_key(other)) != Some(&other)) {
            // One still in step, or on its way out: the next round.
            continue;
        }
        if let Some(other) = other {
            // Here under another case (made on both sides, or a folder a
            // stopped sync made here for a move): one item. The same kind
            // and file is recorded at its name here, which the next round
            // renames as iCloud has it; anything else is set aside.
            claimed.insert(other);
            let e = &local[other];
            let same = if e.is_dir {
                node.kind == Kind::Folder && !inodes.contains(&e.ino)
            } else {
                same_file(e, node)
            };
            if same {
                plan.ops.push(Op::Adopt {
                    id: id.clone(),
                    path: other.to_string(),
                });
                plan.again = true;
                continue;
            }
            plan.ops.push(Op::SetAside {
                path: other.to_string(),
            });
            aside.push(other.to_string());
        }
        let (id, path) = (id.clone(), path.clone());
        if node.kind == Kind::Folder {
            match entry {
                // Not a folder in step moved here to that name, which is
                // another folder: set aside, as anything else there.
                Some(e) if e.is_dir && !inodes.contains(&e.ino) => plan.ops.push(Op::Adopt { id, path: path.clone() }),
                Some(_) => {
                    plan.ops.push(Op::SetAside { path: path.clone() });
                    aside.push(path.clone());
                    plan.ops.push(Op::MkdirLocal { id, path: path.clone() });
                }
                None => plan.ops.push(Op::MkdirLocal { id, path: path.clone() }),
            }
            ready.insert(path);
        } else {
            match entry {
                Some(e) if same_file(e, node) => plan.ops.push(Op::Adopt { id, path }),
                Some(_) => plan.ops.push(Op::KeepBoth { id, path }),
                None => plan.ops.push(Op::Download { id, path }),
            }
        }
    }

    // New here.
    let there_by_case = case_index(remote.by_path.keys());
    let trashed_there: Vec<String> = plan
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::TrashRemote { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    for (path, entry) in local {
        if tracked.contains_key(path.as_str())
            || claimed.contains(path.as_str())
            || under(&aside, path)
            || under(&done, path)
            || deferred.contains(path)
            || !parent_ready(&ready, path)
        {
            continue;
        }
        if remote.by_path.contains_key(path) {
            // iCloud has something in step elsewhere under this name (half
            // way through a move here that it refused the rest of, say):
            // sent once that has moved on.
            continue;
        }
        let taken = there_by_case
            .get(&case_key(path))
            .is_some_and(|there| there != path && !under(&trashed_there, there));
        if taken {
            // iCloud has something else of this name under another case,
            // which it takes as the same name (this one renamed by case
            // and edited, say): kept as a conflicted copy, sent next round.
            plan.ops.push(Op::SetAside { path: path.clone() });
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

    plan.again |= plan
        .ops
        .iter()
        .any(|op| matches!(op, Op::Forget { .. } | Op::SetAside { .. } | Op::KeepBoth { .. }));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ROOT_ID;

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

    fn snapshot(nodes: &[Node]) -> Snapshot {
        Snapshot::build_below(ROOT_ID, nodes)
    }

    fn context() -> Context {
        Context {
            now_ns: NOW,
            ..Context::default()
        }
    }

    fn run(base: &[Row], remote: &[Node], local: &[(&str, Entry)]) -> Plan {
        let local: BTreeMap<String, Entry> = local.iter().map(|(p, e)| (p.to_string(), *e)).collect();
        plan(base, &snapshot(remote), &local, &context())
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
                Op::KeepBoth {
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
            [Op::KeepBoth {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
        assert!(p.again, "the conflicted copy uploads in the next round");
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
    fn a_folder_removed_here_comes_back_when_icloud_holds_something_never_synced_in_it() {
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "d", "a.txt", false, 5, 100);
        let hidden = node("h", "d", ".hidden", false, 1, 300);
        let base = [row(&d, "Docs", dir(1)), row(&f, "Docs/a.txt", file(5, 7, 2))];
        let remote = snapshot(&[d.clone(), f.clone(), hidden]);
        let context = Context {
            unknown: remote.skipped.iter().map(|(path, _)| path.clone()).collect(),
            ..context()
        };
        assert_eq!(context.unknown, ["Docs/.hidden"]);
        let p = plan(&base, &remote, &BTreeMap::new(), &context);
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
            ]
        );
    }

    #[test]
    fn both_changed_to_the_same_file_is_adopted_not_a_conflict() {
        // A download or an upload that finished but was not recorded.
        let old = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&old, "a.txt", file(5, 7, 1))];
        let newer = node("f", "", "a.txt", false, 6, 200);
        let p = run(&base, &[newer], &[("a.txt", file(6, 200_000_000, 1))]);
        assert_eq!(
            p.ops,
            [Op::Adopt {
                id: "f".into(),
                path: "a.txt".into()
            }]
        );
        assert!(!p.again);
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
    fn a_move_here_into_a_folder_icloud_also_made_adopts_that_folder_first() {
        let f = node("f", "", "a.txt", false, 5, 100);
        let trip = node("t", "", "Trip", true, 0, 0);
        let base = [row(&f, "a.txt", file(5, 7, 2))];
        let local = [("Trip", dir(9)), ("Trip/a.txt", file(5, 7, 2))];
        let p = run(&base, &[f.clone(), trip], &local);
        assert_eq!(
            p.ops,
            [
                Op::Adopt {
                    id: "t".into(),
                    path: "Trip".into()
                },
                Op::RemoteMove {
                    id: "f".into(),
                    to: "Trip/a.txt".into()
                }
            ]
        );
        // A file of that name in iCloud is not a folder to move into.
        let not_a_folder = node("t", "", "Trip", false, 1, 1);
        let p = run(&base, &[f, not_a_folder], &local);
        assert!(
            !p.ops.iter().any(|op| matches!(op, Op::RemoteMove { .. })),
            "{:?}",
            p.ops
        );
    }

    #[test]
    fn a_move_here_left_half_done_in_icloud_is_finished_not_undone() {
        // a.txt was moved here to Docs/b.txt; iCloud got as far as Docs/a.txt.
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&d, "Docs", dir(1)), row(&f, "a.txt", file(5, 7, 2))];
        let half_way = node("f", "d", "a.txt", false, 5, 100);
        let remote = snapshot(&[d.clone(), half_way]);
        let local: BTreeMap<String, Entry> =
            [("Docs".to_string(), dir(1)), ("Docs/b.txt".to_string(), file(5, 7, 2))].into();
        let mut context = context();
        context.pending_moves.insert("f".into(), "Docs/b.txt".into());
        assert_eq!(
            plan(&base, &remote, &local, &context).ops,
            [Op::RemoteMove {
                id: "f".into(),
                to: "Docs/b.txt".into()
            }]
        );
        // With no note of it, that is a move made in iCloud, followed here.
        context.pending_moves.clear();
        assert_eq!(
            plan(&base, &remote, &local, &context).ops,
            [Op::LocalMove {
                id: "f".into(),
                from: "Docs/b.txt".into(),
                to: "Docs/a.txt".into()
            }]
        );
    }

    #[test]
    fn an_unfinished_move_of_a_file_put_back_here_is_sent_back_in_icloud() {
        // a.txt was moved here to Docs/b.txt, which iCloud did in full or
        // in part without it being recorded; the file is back at a.txt.
        let d = node("d", "", "Docs", true, 0, 0);
        let f = node("f", "", "a.txt", false, 5, 100);
        let base = [row(&d, "Docs", dir(1)), row(&f, "a.txt", file(5, 7, 2))];
        let local: BTreeMap<String, Entry> =
            [("Docs".to_string(), dir(1)), ("a.txt".to_string(), file(5, 7, 2))].into();
        let mut context = context();
        context.pending_moves.insert("f".into(), "Docs/b.txt".into());
        let back = [Op::RemoteMove {
            id: "f".into(),
            to: "a.txt".into(),
        }];
        for there in ["a.txt", "b.txt"] {
            let remote = snapshot(&[d.clone(), node("f", "d", there, false, 5, 100)]);
            assert_eq!(plan(&base, &remote, &local, &context).ops, back, "{there}");
        }
        // Another file has taken the name in iCloud: iCloud's place is followed.
        let taken = node("g", "", "a.txt", false, 9, 300);
        let remote = snapshot(&[d.clone(), node("f", "d", "a.txt", false, 5, 100), taken]);
        assert_eq!(
            plan(&base, &remote, &local, &context).ops,
            [Op::LocalMove {
                id: "f".into(),
                from: "a.txt".into(),
                to: "Docs/a.txt".into()
            }]
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
        let snapshot = snapshot(&[lib, inside, hidden]);
        assert!(snapshot.nodes.is_empty());
        assert_eq!(snapshot.skipped.len(), 2);
    }

    #[test]
    fn an_ignored_item_is_left_alone_until_it_changes() {
        let pkg = node("p", "", "doc.pages", false, 5, 100);
        let local = BTreeMap::new();
        let mut context = context();
        context.ignored.insert("p".to_string(), pkg.etag.clone());
        let p = plan(&[], &snapshot(std::slice::from_ref(&pkg)), &local, &context);
        assert!(p.ops.is_empty());
        context.ignored.insert("p".to_string(), "older".to_string());
        let p = plan(&[], &snapshot(&[pkg]), &local, &context);
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
        let p = plan(&base, &snapshot(&[a, d.clone(), inside]), &local, &context);
        assert_eq!(p, Plan::default());
        // And a folder iCloud removed is kept while it holds such a path.
        let p = plan(&base[..2], &snapshot(&[]), &local, &context);
        assert!(
            !p.ops.iter().any(|op| matches!(op, Op::TrashLocal { .. })),
            "{:?}",
            p.ops
        );
    }
}
