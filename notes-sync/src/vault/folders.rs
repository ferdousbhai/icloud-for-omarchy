//! Folder reconciliation on pull and folder creation on push. Ports
//! icloud-md `src/notes/folderReconcile.ts` and `folderCreate.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::{IndexMap, IndexSet};

use super::base::{read_base_copy, write_base_copy};
use super::layout::{
    RESERVED_SIBLING_DIR_NAMES, RESERVED_TOP_LEVEL_DIR_NAMES, StateDirInfo, VaultLayout, expected_note_dir, note_dir_of,
};
use super::local::read_text;
use super::state::{AttachmentEntry, NoteEntry};
use crate::cmd::errors::Error;
use crate::cmd::plan::FolderRefusal;
use crate::js::{self, posix};
use crate::md::filename::unique_file_name;

// --- folderReconcile.ts ----------------------------------------------------------

/// `Relocation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocation {
    pub from: String,
    pub to: String,
}

fn names<'a>(by_dir: &'a mut HashMap<String, HashSet<String>>, dir: &str) -> &'a mut HashSet<String> {
    by_dir.entry(dir.to_owned()).or_default()
}

fn try_rename(from: &Path, to: &Path) -> Result<bool, Error> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn parent_dir_of(target_dir: &Path, file: &str) -> std::path::PathBuf {
    target_dir
        .join(file)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| target_dir.to_path_buf())
}

/// `reconcileNotePlacements`: move every tracked note (and its attachments)
/// whose directory no longer matches the layout. Remote wins.
pub fn reconcile_note_placements(
    target_dir: &Path,
    layout: &VaultLayout,
    notes: &mut IndexMap<String, NoteEntry>,
    attachments: &mut IndexMap<String, AttachmentEntry>,
) -> Result<Vec<Relocation>, Error> {
    let mut moves: Vec<(String, String)> = Vec::new();
    let mut claimed_names: HashMap<String, HashSet<String>> = HashMap::new();
    let mut claimed_attachment_names: HashMap<String, HashSet<String>> = HashMap::new();

    let mut sorted: Vec<(&String, &NoteEntry)> = notes.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| js::locale_compare(a, b));
    for (record_name, entry) in sorted {
        let to_dir = expected_note_dir(layout, entry);
        match to_dir {
            Some(to_dir) if to_dir != note_dir_of(&entry.file) => moves.push((record_name.clone(), to_dir)),
            _ => {
                names(&mut claimed_names, &note_dir_of(&entry.file)).insert(posix::basename(&entry.file).to_owned());
            }
        }
    }
    let moving: HashSet<&str> = moves.iter().map(|(rn, _)| rn.as_str()).collect();
    for attachment in attachments.values() {
        if !moving.contains(attachment.note_record_name.as_str()) {
            names(
                &mut claimed_attachment_names,
                &note_dir_of(&posix::dirname(&attachment.file)),
            )
            .insert(posix::basename(&attachment.file).to_owned());
        }
    }

    let mut relocations = Vec::new();
    for (record_name, to_dir) in &moves {
        let Some(entry) = notes.get(record_name) else {
            continue;
        };
        let from_file = entry.file.clone();
        let base_name = unique_file_name(posix::basename(&from_file), names(&mut claimed_names, to_dir));
        let to_file = posix::join(&[to_dir, &base_name]);

        std::fs::create_dir_all(parent_dir_of(target_dir, &to_file))?;
        if !try_rename(&target_dir.join(&from_file), &target_dir.join(&to_file))? {
            names(&mut claimed_names, &note_dir_of(&from_file)).insert(posix::basename(&from_file).to_owned());
            continue;
        }
        names(&mut claimed_names, to_dir).insert(base_name);
        notes[record_name].file = to_file.clone();
        relocations.push(Relocation {
            from: from_file,
            to: to_file.clone(),
        });

        let attachment_names: Vec<String> = attachments.keys().cloned().collect();
        for attachment_record_name in attachment_names {
            let attachment = attachments[&attachment_record_name].clone();
            if attachment.note_record_name != *record_name {
                continue;
            }
            let old_base = posix::basename(&attachment.file).to_owned();
            let new_base = unique_file_name(&old_base, names(&mut claimed_attachment_names, to_dir));
            let to_attachment = posix::join(&[to_dir, "attachments", &new_base]);
            std::fs::create_dir_all(parent_dir_of(target_dir, &to_attachment))?;
            if !try_rename(&target_dir.join(&attachment.file), &target_dir.join(&to_attachment))? {
                continue;
            }
            names(&mut claimed_attachment_names, to_dir).insert(new_base.clone());
            attachments[&attachment_record_name].file = to_attachment;
            if new_base != old_base {
                rewrite_attachment_link(target_dir, &to_file, record_name, &old_base, &new_base)?;
            }
        }
    }
    Ok(relocations)
}

/// `removeStaleDirs`: best-effort rmdir of directories the previous layout
/// used and the current one doesn't (deepest first; only if empty).
pub fn remove_stale_dirs(target_dir: &Path, previous_dirs: &[String], current_dirs: &HashSet<String>) {
    let mut stale: Vec<&String> = previous_dirs
        .iter()
        .filter(|d| !d.is_empty() && !current_dirs.contains(*d))
        .collect();
    stale.sort_by_key(|d| std::cmp::Reverse(d.split('/').count()));
    for dir in stale {
        let _ = std::fs::remove_dir(target_dir.join(dir).join("attachments"));
        let _ = std::fs::remove_dir(target_dir.join(dir));
    }
}

fn rewrite_attachment_link(
    target_dir: &Path,
    note_file: &str,
    record_name: &str,
    old_base: &str,
    new_base: &str,
) -> Result<(), Error> {
    let old_link = format!("attachments/{}", js::encode_uri_component(old_base));
    let new_link = format!("attachments/{}", js::encode_uri_component(new_base));
    let note_path = target_dir.join(note_file);
    if let Some(content) = read_text(&note_path)? {
        std::fs::write(&note_path, content.replace(&old_link, &new_link))?;
    }
    if let Some(base) = read_base_copy(target_dir, record_name)? {
        write_base_copy(target_dir, record_name, &base.replace(&old_link, &new_link))?;
    }
    Ok(())
}

// --- folderCreate.ts -------------------------------------------------------------

/// `PlannedFolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFolder {
    pub record_name: String,
    pub title: String,
    pub dir_path: String,
    pub parent_record_name: Option<String>,
}

/// `RefusedFolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedFolder {
    pub dir_path: String,
    pub reason: FolderRefusal,
}

/// `FolderCreatePlan`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderCreatePlan {
    /// Parent before child.
    pub folders: Vec<PlannedFolder>,
    pub refusals: Vec<RefusedFolder>,
    /// Directory → folder record name, planned or existing.
    pub dir_to_record_name: IndexMap<String, String>,
}

fn reserved_reason(segment: &str, is_top_level: bool) -> Option<FolderRefusal> {
    if segment.starts_with('.') {
        return Some(FolderRefusal::Hidden {
            segment: segment.to_owned(),
        });
    }
    if RESERVED_SIBLING_DIR_NAMES.contains(&segment) {
        return Some(FolderRefusal::ReservedAttachments {
            segment: segment.to_owned(),
        });
    }
    if is_top_level && RESERVED_TOP_LEVEL_DIR_NAMES.contains(&segment) {
        return Some(FolderRefusal::ReservedStateDir {
            segment: segment.to_owned(),
        });
    }
    None
}

/// `planFolderCreates`: which directories a note is going into must become
/// Folder records, walked root-down; nothing inside a sharer's area.
pub fn plan_folder_creates<'a>(
    wanted_dirs: impl IntoIterator<Item = &'a str>,
    dir_index: &IndexMap<String, StateDirInfo>,
    new_record_name: &mut dyn FnMut() -> String,
) -> FolderCreatePlan {
    let mut plan = FolderCreatePlan::default();
    let mut refused_dirs: HashSet<String> = HashSet::new();
    for (dir, info) in dir_index {
        if let StateDirInfo::Folder { folder_record_name, .. } = info {
            plan.dir_to_record_name.insert(dir.clone(), folder_record_name.clone());
        }
    }

    let unique: IndexSet<&str> = wanted_dirs.into_iter().collect();
    let mut sorted: Vec<String> = unique
        .into_iter()
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect();
    sorted.sort_by_key(|d| d.split('/').count());

    for dir in &sorted {
        if plan.dir_to_record_name.contains_key(dir) || refused_dirs.contains(dir) {
            continue;
        }
        let mut parent_record_name: Option<String> = None;
        let mut refusal: Option<FolderRefusal> = None;
        let mut walked = String::new();
        for (depth, segment) in dir.split('/').enumerate() {
            walked = if walked.is_empty() {
                segment.to_owned()
            } else {
                format!("{walked}/{segment}")
            };
            match dir_index.get(&walked) {
                Some(StateDirInfo::SharerHome { .. }) => {
                    refusal = Some(FolderRefusal::SharerHome { walked: walked.clone() });
                    break;
                }
                Some(StateDirInfo::Folder {
                    shared_zone_owner: Some(_),
                    ..
                }) => {
                    refusal = Some(FolderRefusal::SharedFolder { walked: walked.clone() });
                    break;
                }
                _ => {}
            }
            if let Some(known) = plan.dir_to_record_name.get(&walked) {
                parent_record_name = Some(known.clone());
                continue;
            }
            if let Some(reserved) = reserved_reason(segment, depth == 0) {
                refusal = Some(reserved);
                break;
            }
            let planned = PlannedFolder {
                record_name: new_record_name(),
                title: segment.to_owned(),
                dir_path: walked.clone(),
                parent_record_name: parent_record_name.clone(),
            };
            plan.dir_to_record_name
                .insert(walked.clone(), planned.record_name.clone());
            parent_record_name = Some(planned.record_name.clone());
            plan.folders.push(planned);
        }

        if let Some(reason) = refusal {
            plan.refusals.push(RefusedFolder {
                dir_path: dir.clone(),
                reason,
            });
            refused_dirs.insert(dir.clone());
            drop_unreachable(&mut plan, &sorted, &refused_dirs);
        }
    }
    plan
}

fn drop_unreachable(plan: &mut FolderCreatePlan, wanted: &[String], refused_dirs: &HashSet<String>) {
    let live: Vec<&String> = wanted.iter().filter(|d| !refused_dirs.contains(*d)).collect();
    let mut i = plan.folders.len();
    while i > 0 {
        i -= 1;
        let folder_dir = plan.folders[i].dir_path.clone();
        let needed = live
            .iter()
            .any(|d| **d == folder_dir || d.starts_with(&format!("{folder_dir}/")));
        if !needed {
            plan.folders.remove(i);
            plan.dir_to_record_name.shift_remove(&folder_dir);
        }
    }
}
