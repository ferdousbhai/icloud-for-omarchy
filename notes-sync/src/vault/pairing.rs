//! Note-id pairing of moved files and deferred renames. Ports icloud-md
//! `src/notes/noteIdPairing.ts` and `pendingRename.ts`.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;

use super::layout::note_dir_of;
use super::local::{read_text, split_options};
use super::state::{NoteEntry, TitleMode};
use crate::cmd::errors::Error;
use crate::js::posix;
use crate::md::frontmatter::{read_note_id, split_frontmatter};

// --- noteIdPairing.ts ----------------------------------------------------------

/// `UntrackedFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrackedFile {
    /// Vault-root-relative POSIX path.
    pub file: String,
    pub note_id: Option<String>,
}

/// `IdMovePair`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdMovePair {
    pub record_name: String,
    pub file: String,
}

/// `AmbiguousIdClaim`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousIdClaim {
    pub record_name: String,
    pub files: Vec<String>,
}

/// `NoteIdResolution`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteIdResolution {
    pub moves: Vec<IdMovePair>,
    pub creates: Vec<String>,
    pub ambiguous: Vec<AmbiguousIdClaim>,
    pub stale_ids: Vec<String>,
}

/// `resolveNoteIds`: an id matching a tracked note whose own file is gone is
/// a move (unless several files claim it: ambiguous); an id whose note's
/// file is still present marks a copy (a create); an unknown id is stale (a
/// create); no id is a create.
pub fn resolve_note_ids<'a>(
    untracked: &[UntrackedFile],
    tracked_record_names: impl IntoIterator<Item = &'a str>,
    is_tracked_file_present: &dyn Fn(&str) -> bool,
) -> NoteIdResolution {
    let tracked: HashSet<&str> = tracked_record_names.into_iter().collect();
    let mut resolution = NoteIdResolution::default();

    let mut claims_by_id: IndexMap<&str, Vec<String>> = IndexMap::new();
    for candidate in untracked {
        if let Some(id) = &candidate.note_id {
            claims_by_id
                .entry(id.as_str())
                .or_default()
                .push(candidate.file.clone());
        }
    }

    let mut resolved_as_move: HashSet<String> = HashSet::new();
    let mut refused: HashSet<String> = HashSet::new();
    for (record_name, files) in claims_by_id {
        if !tracked.contains(record_name) {
            resolution.stale_ids.extend(files);
            continue;
        }
        if is_tracked_file_present(record_name) {
            continue;
        }
        if files.len() > 1 {
            refused.extend(files.iter().cloned());
            resolution.ambiguous.push(AmbiguousIdClaim {
                record_name: record_name.to_owned(),
                files,
            });
            continue;
        }
        if let Some(file) = files.into_iter().next() {
            resolved_as_move.insert(file.clone());
            resolution.moves.push(IdMovePair {
                record_name: record_name.to_owned(),
                file,
            });
        }
    }

    for candidate in untracked {
        if !resolved_as_move.contains(&candidate.file) && !refused.contains(&candidate.file) {
            resolution.creates.push(candidate.file.clone());
        }
    }
    resolution
}

// --- pendingRename.ts ----------------------------------------------------------

/// `pendingRenameTarget`: where a note with a rename outstanding should end
/// up (its pending name in its current directory), or `None`.
pub fn pending_rename_target(entry: &NoteEntry) -> Option<String> {
    let pending = entry.pending_rename.as_deref()?;
    if pending == posix::basename(&entry.file) {
        return None;
    }
    Some(posix::join(&[&note_dir_of(&entry.file), pending]))
}

/// A performed rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformedRename {
    pub from: String,
    pub to: String,
}

/// A rename that couldn't be performed (the target is occupied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRename {
    pub file: String,
    pub to: String,
}

/// `PendingRenameSettlement`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingRenameSettlement {
    pub changed: bool,
    pub performed: Vec<PerformedRename>,
    pub blocked: Vec<BlockedRename>,
}

pub(crate) fn file_exists(path: &Path) -> Result<bool, Error> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn note_id_at(target_dir: &Path, file: &str, title_mode: TitleMode) -> Result<Option<String>, Error> {
    let Some(raw) = read_text(&target_dir.join(file))? else {
        return Ok(None);
    };
    Ok(read_note_id(
        &split_frontmatter(&raw, split_options(title_mode)).frontmatter,
    ))
}

/// `settlePendingRenames`: clear renames that are moot or already done
/// (adopting a target that carries this note's id); with `perform`, carry
/// out the rest unless the target is occupied.
pub fn settle_pending_renames(
    target_dir: &Path,
    notes: &mut IndexMap<String, NoteEntry>,
    perform: bool,
    title_mode: TitleMode,
) -> Result<PendingRenameSettlement, Error> {
    let mut settlement = PendingRenameSettlement::default();
    let record_names: Vec<String> = notes.keys().cloned().collect();
    for record_name in record_names {
        let entry = notes[&record_name].clone();
        if entry.pending_rename.is_none() {
            continue;
        }
        let Some(target) = pending_rename_target(&entry) else {
            notes[&record_name].pending_rename = None;
            settlement.changed = true;
            continue;
        };

        if file_exists(&target_dir.join(&entry.file))? {
            if !perform {
                continue;
            }
            if file_exists(&target_dir.join(&target))? {
                settlement.blocked.push(BlockedRename {
                    file: entry.file.clone(),
                    to: target,
                });
                continue;
            }
            std::fs::rename(target_dir.join(&entry.file), target_dir.join(&target))?;
            settlement.performed.push(PerformedRename {
                from: entry.file.clone(),
                to: target.clone(),
            });
            let e = &mut notes[&record_name];
            e.file = target;
            e.pending_rename = None;
            settlement.changed = true;
            continue;
        }

        let adopted = note_id_at(target_dir, &target, title_mode)?.as_deref() == Some(record_name.as_str());
        let e = &mut notes[&record_name];
        if adopted {
            e.file = target;
        }
        e.pending_rename = None;
        settlement.changed = true;
    }
    Ok(settlement)
}
