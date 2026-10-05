//! Folder tree and note placement. Ports icloud-md `src/notes/folderTree.ts`
//! and `folderLayout.ts`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde_json::Value;

use super::state::{CloneState, FolderEntry, NoteEntry, SharerHomeEntry};
use crate::cloudkit::CloudKitRecord;
use crate::doc::encode::{DEFAULT_FOLDER_RECORD_NAME, TRASH_FOLDER_RECORD_NAME};
use crate::js::{self, posix};

/// Directory names the clone reserves at the top level.
pub const RESERVED_TOP_LEVEL_DIR_NAMES: &[&str] = &[super::state::STATE_DIR_NAME, super::state::LEGACY_STATE_DIR_NAME];
/// Directory names reserved inside every folder directory.
pub const RESERVED_SIBLING_DIR_NAMES: &[&str] = &["attachments"];

// --- folderTree.ts ---------------------------------------------------------------

/// `FolderInfo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderInfo {
    pub record_name: String,
    pub title: String,
    pub parent_record_name: Option<String>,
    pub share_record_name: Option<String>,
    pub permission: Option<String>,
}

/// `FolderTreeNode` (children as indexes into [`FolderTree::nodes`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderTreeNode {
    pub info: FolderInfo,
    pub dir_name: String,
    pub dir_path: String,
    pub children: Vec<usize>,
}

/// `FolderTree`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderTree {
    /// Every node, in input order (`byRecordName`'s iteration order).
    pub nodes: Vec<FolderTreeNode>,
    pub roots: Vec<usize>,
    pub by_record_name: IndexMap<String, usize>,
}

impl FolderTree {
    pub fn get(&self, record_name: &str) -> Option<&FolderTreeNode> {
        self.by_record_name.get(record_name).map(|&i| &self.nodes[i])
    }

    /// `byRecordName.values()`.
    pub fn values(&self) -> impl Iterator<Item = &FolderTreeNode> {
        self.by_record_name.values().map(|&i| &self.nodes[i])
    }
}

pub(crate) fn reference_record_name(value: Option<&Value>) -> Option<String> {
    value?.as_object()?.get("recordName")?.as_str().map(str::to_owned)
}

/// `decodeFolderRecord`: `None` for non-Folder records and the Trash folder.
pub fn decode_folder_record(record: &CloudKitRecord) -> Option<FolderInfo> {
    if record.record_type != "Folder" || record.record_name == TRASH_FOLDER_RECORD_NAME {
        return None;
    }
    let title = match record.fields.get("TitleEncrypted").map(|f| &f.value) {
        Some(Value::String(b64)) => String::from_utf8_lossy(&js::base64_decode(b64)).into_owned(),
        _ => String::new(),
    };
    Some(FolderInfo {
        record_name: record.record_name.clone(),
        title,
        parent_record_name: reference_record_name(record.fields.get("ParentFolder").map(|f| &f.value))
            .or_else(|| record.parent_record_name.clone()),
        share_record_name: record.share_record_name.clone(),
        permission: None,
    })
}

/// `sanitizeFolderDirName`.
pub fn sanitize_folder_dir_name(title: &str) -> String {
    let first_line = js::trim(title.split('\n').next().unwrap_or(""));
    let stripped: String = first_line
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let mut collapsed = String::with_capacity(stripped.len());
    let mut in_ws = false;
    for c in stripped.chars() {
        if js::is_whitespace(c) {
            if !in_ws {
                collapsed.push(' ');
            }
            in_ws = true;
        } else {
            collapsed.push(c);
            in_ws = false;
        }
    }
    let sliced = js::slice16(js::trim(&collapsed), 0, 80);
    let slug = sliced.trim_end_matches(['.', ' ']);
    if slug.is_empty() {
        "Untitled Folder".into()
    } else {
        slug.into()
    }
}

fn reserved_names(top_level: bool) -> HashSet<String> {
    let mut names: Vec<&str> = RESERVED_SIBLING_DIR_NAMES.to_vec();
    if top_level {
        names.extend(RESERVED_TOP_LEVEL_DIR_NAMES);
    }
    names.into_iter().map(str::to_lowercase).collect()
}

/// JS `<` on strings: UTF-16 code unit order.
fn js_str_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `buildFolderTree`: missing parents and cycles promote to roots; sibling
/// directory names are uniquified case-insensitively, preferred names
/// (from previous state) first.
pub fn build_folder_tree(folders: &[FolderInfo], preferred_dir_names: Option<&HashMap<String, String>>) -> FolderTree {
    let mut tree = FolderTree::default();
    for folder in folders {
        let node = FolderTreeNode {
            info: folder.clone(),
            ..Default::default()
        };
        if let Some(&i) = tree.by_record_name.get(&folder.record_name) {
            tree.nodes[i] = node;
        } else {
            tree.nodes.push(node);
            tree.by_record_name
                .insert(folder.record_name.clone(), tree.nodes.len() - 1);
        }
    }

    let order: Vec<usize> = tree.by_record_name.values().copied().collect();
    for &i in &order {
        let parent = tree.nodes[i]
            .info
            .parent_record_name
            .as_ref()
            .and_then(|p| tree.by_record_name.get(p).copied());
        match parent {
            Some(p) if p != i => tree.nodes[p].children.push(i),
            _ => tree.roots.push(i),
        }
    }

    let mut reachable = collect_reachable(&tree, &tree.roots);
    let mut unreachable: Vec<usize> = order.iter().copied().filter(|i| !reachable.contains(i)).collect();
    unreachable.sort_by(|&a, &b| js_str_cmp(&tree.nodes[a].info.record_name, &tree.nodes[b].info.record_name));
    for i in unreachable {
        if reachable.contains(&i) {
            continue;
        }
        let parent = tree.nodes[i]
            .info
            .parent_record_name
            .as_ref()
            .and_then(|p| tree.by_record_name.get(p).copied());
        if let Some(p) = parent {
            let children = &mut tree.nodes[p].children;
            if let Some(pos) = children.iter().position(|&c| c == i) {
                children.remove(pos);
            }
        }
        tree.roots.push(i);
        reachable.extend(collect_reachable(&tree, &[i]));
    }

    let roots = tree.roots.clone();
    assign_dir_names(&mut tree, &roots, reserved_names(true), "", preferred_dir_names);
    tree
}

fn collect_reachable(tree: &FolderTree, roots: &[usize]) -> HashSet<usize> {
    let mut reachable = HashSet::new();
    let mut queue: Vec<usize> = roots.to_vec();
    while let Some(i) = queue.pop() {
        if !reachable.insert(i) {
            continue;
        }
        queue.extend(tree.nodes[i].children.iter().copied());
    }
    reachable
}

fn next_free_name(candidate: &str, claimed_lower: &HashSet<String>) -> String {
    if !claimed_lower.contains(&candidate.to_lowercase()) {
        return candidate.to_owned();
    }
    let lower = candidate.to_lowercase();
    let mut n = 2;
    while claimed_lower.contains(&format!("{lower} {n}")) {
        n += 1;
    }
    format!("{candidate} {n}")
}

fn assign_dir_names(
    tree: &mut FolderTree,
    siblings: &[usize],
    mut claimed_lower: HashSet<String>,
    parent_path: &str,
    preferred: Option<&HashMap<String, String>>,
) {
    let mut ordered = siblings.to_vec();
    ordered.sort_by(|&a, &b| {
        let (a, b) = (&tree.nodes[a].info, &tree.nodes[b].info);
        js::locale_compare(&a.title, &b.title).then_with(|| js_str_cmp(&a.record_name, &b.record_name))
    });
    let has_pref =
        |i: usize, tree: &FolderTree| preferred.is_some_and(|p| p.contains_key(&tree.nodes[i].info.record_name));
    let with: Vec<usize> = ordered.iter().copied().filter(|&i| has_pref(i, tree)).collect();
    let without: Vec<usize> = ordered.iter().copied().filter(|&i| !has_pref(i, tree)).collect();

    let claim = |tree: &mut FolderTree, i: usize, name: String, claimed: &mut HashSet<String>| {
        tree.nodes[i].dir_path = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };
        claimed.insert(name.to_lowercase());
        tree.nodes[i].dir_name = name;
    };
    for i in with {
        let preferred_name = preferred
            .and_then(|p| p.get(&tree.nodes[i].info.record_name))
            .cloned()
            .unwrap_or_default();
        let name = if claimed_lower.contains(&preferred_name.to_lowercase()) {
            next_free_name(&sanitize_folder_dir_name(&tree.nodes[i].info.title), &claimed_lower)
        } else {
            preferred_name
        };
        claim(tree, i, name, &mut claimed_lower);
    }
    for i in without {
        let name = next_free_name(&sanitize_folder_dir_name(&tree.nodes[i].info.title), &claimed_lower);
        claim(tree, i, name, &mut claimed_lower);
    }
    for i in ordered {
        let children = tree.nodes[i].children.clone();
        let path = tree.nodes[i].dir_path.clone();
        assign_dir_names(tree, &children, reserved_names(false), &path, preferred);
    }
}

// --- folderLayout.ts -------------------------------------------------------------

/// `SharedZoneRecords`.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedZoneRecords {
    pub owner_record_name: String,
    pub records: Vec<CloudKitRecord>,
}

/// `VaultLayout`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultLayout {
    /// Folder recordName → vault-root-relative dirPath (own and shared).
    pub folder_dirs: IndexMap<String, String>,
    /// Sharer ownerRecordName → the sharer's top-level home dirPath.
    pub sharer_home_dirs: IndexMap<String, String>,
    pub state_folders: IndexMap<String, FolderEntry>,
    pub state_sharer_homes: IndexMap<String, SharerHomeEntry>,
    /// Every directory the layout implies.
    pub all_dirs: Vec<String>,
}

/// The previous state `buildVaultLayout` carries forward.
#[derive(Debug, Clone, Copy, Default)]
pub struct PreviousLayout<'a> {
    pub folders: Option<&'a IndexMap<String, FolderEntry>>,
    pub sharer_homes: Option<&'a IndexMap<String, SharerHomeEntry>>,
}

impl<'a> PreviousLayout<'a> {
    pub fn of(state: &'a CloneState) -> Self {
        PreviousLayout {
            folders: state.folders.as_ref(),
            sharer_homes: state.sharer_homes.as_ref(),
        }
    }
}

fn merge_folder_infos(carried: Vec<FolderInfo>, records: &[CloudKitRecord]) -> Vec<FolderInfo> {
    let mut merged: IndexMap<String, FolderInfo> = IndexMap::new();
    for info in carried {
        merged.insert(info.record_name.clone(), info);
    }
    for record in records {
        if record.record_type != "Folder" {
            continue;
        }
        if record.is_deleted() {
            merged.shift_remove(&record.record_name);
            continue;
        }
        if let Some(decoded) = decode_folder_record(record) {
            merged.insert(decoded.record_name.clone(), decoded);
        }
    }
    merged.into_values().collect()
}

/// `sharerDisplayName`: the OWNER participant's full name, else email, else
/// phone.
pub fn sharer_display_name(zone: &SharedZoneRecords) -> Option<String> {
    for record in &zone.records {
        if record.record_type != "cloudkit.share" {
            continue;
        }
        let Some(owner) = record
            .participants
            .as_ref()
            .and_then(|ps| ps.iter().find(|p| p.type_.as_deref() == Some("OWNER")))
        else {
            continue;
        };
        let full_name = [&owner.given_name, &owner.family_name]
            .iter()
            .filter_map(|p| p.as_deref())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let name = [Some(full_name), owner.email_address.clone(), owner.phone_number.clone()]
            .into_iter()
            .flatten()
            .find(|n| !n.is_empty());
        if name.is_some() {
            return name;
        }
    }
    None
}

fn claim_top_level_name(candidate: &str, claimed_lower: &mut HashSet<String>) -> String {
    let mut name = candidate.to_owned();
    let mut n = 2;
    while claimed_lower.contains(&name.to_lowercase()) {
        name = format!("{candidate} {n}");
        n += 1;
    }
    claimed_lower.insert(name.to_lowercase());
    name
}

/// `buildVaultLayout`.
pub fn build_vault_layout(
    private_records: &[CloudKitRecord],
    shared_zones: &[SharedZoneRecords],
    previous: PreviousLayout<'_>,
) -> VaultLayout {
    let empty = IndexMap::new();
    let prev_folders = previous.folders.unwrap_or(&empty);

    let own_folders = merge_folder_infos(
        prev_folders
            .iter()
            .filter(|(_, e)| e.shared_zone_owner.is_none())
            .map(|(rn, e)| FolderInfo {
                record_name: rn.clone(),
                title: e.name.clone(),
                parent_record_name: e.parent_record_name.clone(),
                ..Default::default()
            })
            .collect(),
        private_records,
    );

    let mut current_titles: HashMap<String, String> = HashMap::new();
    for info in &own_folders {
        current_titles.insert(info.record_name.clone(), info.title.clone());
    }
    for zone in shared_zones {
        for record in &zone.records {
            if !record.is_deleted()
                && let Some(decoded) = decode_folder_record(record)
            {
                current_titles.insert(decoded.record_name, decoded.title);
            }
        }
    }
    let mut preferred: HashMap<String, String> = HashMap::new();
    for (rn, entry) in prev_folders {
        match current_titles.get(rn) {
            Some(title) if *title != entry.name => {}
            _ => {
                preferred.insert(rn.clone(), entry.dir_name.clone());
            }
        }
    }

    let own_tree = build_folder_tree(&own_folders, Some(&preferred));
    let mut layout = VaultLayout::default();
    for node in own_tree.values() {
        layout
            .folder_dirs
            .insert(node.info.record_name.clone(), node.dir_path.clone());
        layout.state_folders.insert(
            node.info.record_name.clone(),
            FolderEntry {
                name: node.info.title.clone(),
                parent_record_name: node.info.parent_record_name.clone(),
                dir_name: node.dir_name.clone(),
                ..Default::default()
            },
        );
        layout.all_dirs.push(node.dir_path.clone());
    }

    let mut top_level_claimed: HashSet<String> = RESERVED_TOP_LEVEL_DIR_NAMES
        .iter()
        .chain(RESERVED_SIBLING_DIR_NAMES)
        .map(|n| n.to_lowercase())
        .collect();
    for &root in &own_tree.roots {
        top_level_claimed.insert(own_tree.nodes[root].dir_name.to_lowercase());
    }

    let mut ordered_zones: Vec<&SharedZoneRecords> = shared_zones.iter().collect();
    ordered_zones.sort_by(|a, b| js::locale_compare(&a.owner_record_name, &b.owner_record_name));

    for prefer_previous in [true, false] {
        for zone in &ordered_zones {
            let previous_home = previous.sharer_homes.and_then(|h| h.get(&zone.owner_record_name));
            if previous_home.is_some() != prefer_previous {
                continue;
            }
            let fresh = sharer_display_name(zone);
            let renamed = matches!((previous_home, &fresh), (Some(prev), Some(fresh)) if *fresh != prev.name);
            let name = if renamed {
                fresh.clone().unwrap_or_default()
            } else {
                previous_home
                    .map(|h| h.name.clone())
                    .or_else(|| fresh.clone())
                    .unwrap_or_else(|| zone.owner_record_name.clone())
            };
            let candidate = match previous_home {
                Some(prev) if !renamed => prev.dir_name.clone(),
                _ => sanitize_folder_dir_name(&name),
            };
            let dir_name = claim_top_level_name(&candidate, &mut top_level_claimed);
            layout
                .sharer_home_dirs
                .insert(zone.owner_record_name.clone(), dir_name.clone());
            layout.state_sharer_homes.insert(
                zone.owner_record_name.clone(),
                SharerHomeEntry {
                    name,
                    dir_name: dir_name.clone(),
                },
            );
            layout.all_dirs.push(dir_name);
        }
    }

    for zone in &ordered_zones {
        let home_dir = layout
            .sharer_home_dirs
            .get(&zone.owner_record_name)
            .cloned()
            .unwrap_or_default();
        let zone_folders = merge_folder_infos(
            prev_folders
                .iter()
                .filter(|(_, e)| e.shared_zone_owner.as_deref() == Some(zone.owner_record_name.as_str()))
                .map(|(rn, e)| FolderInfo {
                    record_name: rn.clone(),
                    title: e.name.clone(),
                    parent_record_name: e.parent_record_name.clone(),
                    share_record_name: None,
                    permission: e.permission.clone(),
                })
                .collect(),
            &zone.records,
        );
        let zone_tree = build_folder_tree(&zone_folders, Some(&preferred));

        let mut share_permissions: HashMap<String, String> = HashMap::new();
        for record in &zone.records {
            if record.record_type == "cloudkit.share"
                && !record.is_deleted()
                && let Some(p) = &record.current_user_permission
            {
                share_permissions.insert(record.record_name.clone(), p.clone());
            }
        }
        let previous_permission = |rn: &str| -> Option<String> {
            let entry = prev_folders.get(rn)?;
            if entry.shared_zone_owner.as_deref() == Some(zone.owner_record_name.as_str()) {
                entry.permission.clone()
            } else {
                None
            }
        };

        let mut stack: Vec<(usize, Option<String>)> = zone_tree.roots.iter().rev().map(|&r| (r, None)).collect();
        while let Some((i, inherited)) = stack.pop() {
            let node = &zone_tree.nodes[i];
            let permission = node
                .info
                .share_record_name
                .as_ref()
                .and_then(|s| share_permissions.get(s).cloned())
                .or_else(|| previous_permission(&node.info.record_name))
                .or(inherited);
            let dir_path = format!("{home_dir}/{}", node.dir_path);
            layout
                .folder_dirs
                .insert(node.info.record_name.clone(), dir_path.clone());
            layout.state_folders.insert(
                node.info.record_name.clone(),
                FolderEntry {
                    name: node.info.title.clone(),
                    parent_record_name: node.info.parent_record_name.clone(),
                    dir_name: node.dir_name.clone(),
                    shared_zone_owner: Some(zone.owner_record_name.clone()),
                    permission: permission.clone(),
                },
            );
            layout.all_dirs.push(dir_path);
            for &child in node.children.iter().rev() {
                stack.push((child, permission.clone()));
            }
        }
    }
    layout
}

/// `NotePlacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePlacement {
    pub dir: String,
    pub folder_record_name: Option<String>,
}

/// `placeNote`.
pub fn place_note(layout: &VaultLayout, record: &CloudKitRecord, shared_zone_owner: Option<&str>) -> NotePlacement {
    let folder_record_name = reference_record_name(record.fields.get("Folder").map(|f| &f.value));
    match shared_zone_owner {
        None => {
            if let Some(dir) = folder_record_name.as_ref().and_then(|f| layout.folder_dirs.get(f)) {
                return NotePlacement {
                    dir: dir.clone(),
                    folder_record_name,
                };
            }
            NotePlacement {
                dir: layout
                    .folder_dirs
                    .get(DEFAULT_FOLDER_RECORD_NAME)
                    .cloned()
                    .unwrap_or_default(),
                folder_record_name: None,
            }
        }
        Some(owner) => {
            if let Some(f) = &folder_record_name
                && let Some(dir) = layout.folder_dirs.get(f)
                && layout.state_folders.get(f).and_then(|e| e.shared_zone_owner.as_deref()) == Some(owner)
            {
                return NotePlacement {
                    dir: dir.clone(),
                    folder_record_name,
                };
            }
            NotePlacement {
                dir: layout.sharer_home_dirs.get(owner).cloned().unwrap_or_default(),
                folder_record_name: None,
            }
        }
    }
}

/// `noteDirOf`: the directory of a vault-relative file ("" at the root).
pub fn note_dir_of(state_file: &str) -> String {
    let dir = posix::dirname(state_file);
    if dir == "." { String::new() } else { dir }
}

/// `expectedNoteDir`: `None` = leave it where it is.
pub fn expected_note_dir(layout: &VaultLayout, entry: &NoteEntry) -> Option<String> {
    match &entry.shared_zone_owner {
        None => {
            let dir = entry
                .folder_record_name
                .as_ref()
                .and_then(|f| layout.folder_dirs.get(f));
            Some(
                dir.or_else(|| layout.folder_dirs.get(DEFAULT_FOLDER_RECORD_NAME))
                    .cloned()
                    .unwrap_or_default(),
            )
        }
        Some(owner) => {
            if let Some(f) = &entry.folder_record_name
                && let Some(dir) = layout.folder_dirs.get(f)
                && layout.state_folders.get(f).and_then(|e| e.shared_zone_owner.as_ref()) == Some(owner)
            {
                return Some(dir.clone());
            }
            layout.sharer_home_dirs.get(owner).cloned()
        }
    }
}

/// `StateDirInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateDirInfo {
    Folder {
        folder_record_name: String,
        shared_zone_owner: Option<String>,
        permission: Option<String>,
    },
    SharerHome {
        shared_zone_owner: String,
    },
}

impl StateDirInfo {
    pub fn shared_zone_owner(&self) -> Option<&str> {
        match self {
            StateDirInfo::Folder { shared_zone_owner, .. } => shared_zone_owner.as_deref(),
            StateDirInfo::SharerHome { shared_zone_owner } => Some(shared_zone_owner),
        }
    }

    pub fn folder_record_name(&self) -> Option<&str> {
        match self {
            StateDirInfo::Folder { folder_record_name, .. } => Some(folder_record_name),
            StateDirInfo::SharerHome { .. } => None,
        }
    }

    pub fn permission(&self) -> Option<&str> {
        match self {
            StateDirInfo::Folder { permission, .. } => permission.as_deref(),
            StateDirInfo::SharerHome { .. } => None,
        }
    }
}

/// `stateDirIndex`: dirPath → what the carried state says is there.
pub fn state_dir_index(previous: PreviousLayout<'_>) -> IndexMap<String, StateDirInfo> {
    let empty = IndexMap::new();
    let folders = previous.folders.unwrap_or(&empty);
    let mut memo: HashMap<String, Option<String>> = HashMap::new();

    fn resolve(
        record_name: &str,
        seen: &mut HashSet<String>,
        memo: &mut HashMap<String, Option<String>>,
        folders: &IndexMap<String, FolderEntry>,
        previous: PreviousLayout<'_>,
    ) -> Option<String> {
        if let Some(known) = memo.get(record_name) {
            return known.clone();
        }
        let entry = folders.get(record_name)?;
        if seen.contains(record_name) {
            return None;
        }
        seen.insert(record_name.to_owned());
        let home_prefix = entry
            .shared_zone_owner
            .as_ref()
            .and_then(|owner| previous.sharer_homes.and_then(|h| h.get(owner)))
            .map(|h| h.dir_name.clone());
        if entry.shared_zone_owner.is_some() && home_prefix.is_none() {
            memo.insert(record_name.to_owned(), None);
            return None;
        }
        let parent = entry
            .parent_record_name
            .as_ref()
            .and_then(|p| resolve(p, seen, memo, folders, previous));
        let dir = match (parent, home_prefix) {
            (Some(parent), _) => format!("{parent}/{}", entry.dir_name),
            (None, Some(home)) => format!("{home}/{}", entry.dir_name),
            (None, None) => entry.dir_name.clone(),
        };
        memo.insert(record_name.to_owned(), Some(dir.clone()));
        Some(dir)
    }

    let mut index = IndexMap::new();
    if let Some(homes) = previous.sharer_homes {
        for (owner, home) in homes {
            index.insert(
                home.dir_name.clone(),
                StateDirInfo::SharerHome {
                    shared_zone_owner: owner.clone(),
                },
            );
        }
    }
    for (record_name, entry) in folders {
        let mut seen = HashSet::new();
        if let Some(dir) = resolve(record_name, &mut seen, &mut memo, folders, previous) {
            index.insert(
                dir,
                StateDirInfo::Folder {
                    folder_record_name: record_name.clone(),
                    shared_zone_owner: entry.shared_zone_owner.clone(),
                    permission: entry.permission.clone(),
                },
            );
        }
    }
    index
}

/// `previousLayoutDirs`.
pub fn previous_layout_dirs(previous: PreviousLayout<'_>) -> Vec<String> {
    state_dir_index(previous).into_keys().collect()
}
