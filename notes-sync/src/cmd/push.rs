//! `push` (and the plan `status` shares). Ports icloud-md
//! `src/commands/push.ts` (plus the bits of `delete.ts` push uses).
//! Refusal strings live in `plan.rs`.
//!
//! icloud-md's plan entries carry an `execute` closure; here they carry an
//! [`Action`] that [`execute`] interprets, over the same state.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

use super::plan::{
    PlanEntry, PlanEntryKind, PlanResolution, PrepareRefusal, Refusal, RetitleRefusal, SerializedPlanEntry,
    SharedWriteRefusal, TextUpdateRefusal, count_unchanged_notes,
};
use super::remote::{Connector, DefaultConnector, Remote, resolve_folder_account};
use super::{Error, NoticeLevel, SyncNotice, is_in_trash, is_purged};
use crate::cloudkit::{
    CloudKitRecord, CreateExtras, Database, NoteZone, RecordUpdate, RecordUpdateResult, Transport, UpdateFieldValue,
    UpdateFields, note_zone,
};
use crate::diff3::{has_conflict_markers, merge_note_versions};
use crate::doc::decode::{ClassifyOptions, DecodedNote, NoteDecodeResult, classify_note_record};
use crate::doc::document::{
    ApplyTextEditOptions, apply_text_edit, build_initial_note_document, compute_splices, encode_note_document,
    note_document_round_trips, parse_note_document, validate_document_invariants,
};
use crate::doc::embeds::{
    EmbedSlot, OBJECT_REPLACEMENT_CHARACTER, decode_note_embed_slots, has_attachment_reference, has_embed_marker,
    has_unknown_content_marker, plan_embed_representations,
};
use crate::doc::encode::{
    build_folder_create_fields, build_note_create_fields, build_note_move_fields, build_note_trash_fields,
    build_note_update_fields,
};
use crate::doc::format::{FormatParagraph, decode_note_format, formats_round_trip_equal};
use crate::doc::reconcile::reconcile_note_format;
use crate::doc::table_edit::prepare_table_attachment_update;
use crate::doc::text::{compress_note_document, decode_note_body_text, decode_note_string, decompress_note_document};
use crate::js::{self, posix};
use crate::md::frontmatter::{
    NOTE_TITLE_KEY, join_frontmatter, read_note_id, read_note_title, set_note_id, split_frontmatter,
};
use crate::md::parse::{ParsedNoteMarkdown, parse_note_markdown};
use crate::md::title::{
    carried_title_spelling, representability_problem, restore_title_paragraph_text, split_title_paragraph,
    title_from_note_file_name, title_is_representable, title_paragraph_from_filename,
};
use crate::vault::attachments::{remove_attachments_for_note, remove_table_attachments_for_note, safe_unlink};
use crate::vault::base::{read_base_copy, remove_base_copy, write_base_copy};
use crate::vault::epoch::record_epoch;
use crate::vault::folders::{PlannedFolder, plan_folder_creates};
use crate::vault::history::{VersionSnapshotInput, history_record_names, record_version};
use crate::vault::layout::{PreviousLayout, StateDirInfo, note_dir_of, state_dir_index};
use crate::vault::local::{
    LocalFileState, LocalNote, apply_note_file_times, local_file_state, modification_date_of, mtime_ms,
    read_local_note, read_text, split_options,
};
use crate::vault::migrate::open_vault;
use crate::vault::pairing::{UntrackedFile, pending_rename_target, resolve_note_ids, settle_pending_renames};
use crate::vault::rt;
use crate::vault::state::{
    CloneState, FOLDER_CREATE_ORDER, FolderEntry, NOTE_CREATE_ORDER, NoteEntry, TitleMode, TrashedEntry,
    write_clone_state,
};

/// The body plus formatting a push builds a document from (`{text,
/// paragraphs}`).
pub type NoteText = ParsedNoteMarkdown;

/// `PushOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushOptions {
    pub dry_run: bool,
}

/// `ExecuteOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteOutcome {
    pub succeeded: bool,
    pub message: String,
}

impl ExecuteOutcome {
    fn ok(message: impl Into<String>) -> Self {
        ExecuteOutcome {
            succeeded: true,
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        ExecuteOutcome {
            succeeded: false,
            message: message.into(),
        }
    }
}

/// `PushEntryResult`: a serialized plan entry plus, after a real push, what
/// executing it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEntryResult {
    #[serde(flatten)]
    pub entry: SerializedPlanEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExecuteOutcome>,
}

/// `PushResult`. Key order: dryRun, pushed, entries, unchanged, notices - a
/// dry run omits `pushed`, and an empty real push has `pushed: 0` last
/// (runPush's early return spreads it after `notices`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub dry_run: bool,
    #[serde(default)]
    pub pushed: Option<usize>,
    pub entries: Vec<PushEntryResult>,
    pub unchanged: usize,
    pub notices: Vec<SyncNotice>,
}

impl Serialize for PushResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("dryRun", &self.dry_run)?;
        let pushed_last = self.entries.is_empty();
        if let (Some(pushed), false) = (self.pushed, pushed_last) {
            map.serialize_entry("pushed", &pushed)?;
        }
        map.serialize_entry("entries", &self.entries)?;
        map.serialize_entry("unchanged", &self.unchanged)?;
        map.serialize_entry("notices", &self.notices)?;
        if let (Some(pushed), true) = (self.pushed, pushed_last) {
            map.serialize_entry("pushed", &pushed)?;
        }
        map.end()
    }
}

/// `sharedNoteWriteRefusal`.
pub fn shared_note_write_refusal(state: &CloneState, entry: &NoteEntry) -> Option<SharedWriteRefusal> {
    let owner = entry.shared_zone_owner.as_ref()?;
    let folder = entry
        .folder_record_name
        .as_ref()
        .and_then(|f| state.folders.as_ref().and_then(|folders| folders.get(f)));
    match folder {
        Some(folder) if folder.shared_zone_owner.as_ref() == Some(owner) => {
            (folder.permission.as_deref() == Some("READ_ONLY")).then_some(SharedWriteRefusal::ReadOnlyFolder)
        }
        _ => Some(SharedWriteRefusal::IndividuallyShared),
    }
}

// --- the plan's executable half ---------------------------------------------------

/// The retitle payload a move carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retitle {
    pub payload_base64: String,
    pub plain_text: String,
}

/// What executing a ready (or noop) entry does - icloud-md's `execute`.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    CreateFolder(PlannedFolder),
    Move {
        record_name: String,
        entry: NoteEntry,
        to_file: String,
        folder_record_name: String,
        relocated: bool,
        record: Box<CloudKitRecord>,
        retitle: Option<Retitle>,
    },
    DeleteAlreadyGone {
        record_name: String,
        entry: NoteEntry,
    },
    DeleteAlreadyTrashed {
        record_name: String,
        entry: NoteEntry,
    },
    Delete {
        record_name: String,
        entry: NoteEntry,
        record: Box<CloudKitRecord>,
    },
    Create {
        file: String,
        local_text: String,
        payload_base64: String,
        plain_text: String,
        folder_record_name: String,
        shared_zone_owner: Option<String>,
        modification_date_ms: i64,
    },
    /// A byte-level difference with nothing to send: re-sync the base copy.
    Rebase {
        record_name: String,
        file: String,
        local_text: String,
    },
    Update {
        record_name: String,
        entry: NoteEntry,
        zone: NoteZone,
        updates: Vec<RecordUpdate>,
        note_text_updated: bool,
        local_text: String,
        requested_title: Option<String>,
        modification_date_ms: i64,
    },
}

/// `ExecutablePlanEntry`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutablePlanEntry {
    pub entry: PlanEntry,
    pub action: Option<Action>,
}

impl From<PlanEntry> for ExecutablePlanEntry {
    fn from(entry: PlanEntry) -> Self {
        ExecutablePlanEntry { entry, action: None }
    }
}

impl ExecutablePlanEntry {
    fn with(entry: PlanEntry, action: Action) -> Self {
        ExecutablePlanEntry {
            entry,
            action: Some(action),
        }
    }
}

/// `BuildPushPlanResult`, plus the connection a real push executes over.
pub struct BuildPushPlanResult {
    pub state: CloneState,
    pub entries: Vec<ExecutablePlanEntry>,
    pub notices: Vec<SyncNotice>,
    pub remote: Option<Remote>,
}

struct PushCandidate {
    record_name: String,
    entry: NoteEntry,
    local_text: String,
    frontmatter: String,
    requested_title: Option<String>,
    title_only: bool,
}

struct UntrackedNote {
    file: String,
    local_text: String,
    note_id: Option<String>,
    note_title: Option<String>,
}

struct ReadyMove {
    record_name: String,
    entry: NoteEntry,
    to_file: String,
    folder_record_name: String,
    relocated: bool,
    new_title: Option<String>,
}

struct CreateCandidate {
    file: String,
    local_text: String,
    folder_record_name: String,
    shared_zone_owner: Option<String>,
}

fn b64(bytes: &[u8]) -> String {
    js::base64_encode(bytes)
}

/// `listUntrackedMarkdownFiles`: untracked `.md` files anywhere in the vault
/// (dot-directories and `attachments/` skipped), sorted.
fn list_untracked_markdown_files(target_dir: &Path, state: &CloneState) -> Result<Vec<String>, Error> {
    let tracked: HashSet<&str> = state.notes.values().map(|e| e.file.as_str()).collect();
    let mut found = Vec::new();
    fn walk(target_dir: &Path, dir: &str, tracked: &HashSet<&str>, found: &mut Vec<String>) -> Result<(), Error> {
        let entries = match std::fs::read_dir(target_dir.join(dir)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let relative = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if name.to_lowercase() != "attachments" {
                    walk(target_dir, &relative, tracked, found)?;
                }
            } else if file_type.is_file() && name.ends_with(".md") && !tracked.contains(relative.as_str()) {
                found.push(relative);
            }
        }
        Ok(())
    }
    walk(target_dir, "", &tracked, &mut found)?;
    found.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(found)
}

/// `titleExpressedByFile`: `apple-note-title` when the file carried one,
/// else what its name spells.
pub fn title_expressed_by_file(file: &str, recorded_titles: &HashMap<String, String>) -> String {
    recorded_titles
        .get(file)
        .cloned()
        .unwrap_or_else(|| title_from_note_file_name(file))
}

fn retitle_remark(title: &str) -> String {
    if title_is_representable(title) {
        format!("retitled to \"{title}\" - the next \"pull\" renames this file to match")
    } else {
        format!(
            "retitled to \"{title}\" - {}, so it stays in \"{NOTE_TITLE_KEY}\"",
            representability_problem(title).unwrap_or_else(|| "a file name can't hold it".into())
        )
    }
}

/// `requestedRetitle`: what an `apple-note-title` asks to change, if
/// anything.
fn requested_retitle(classified: &DecodedNote, requested_title: Option<&str>) -> Option<String> {
    let requested = requested_title?;
    if !classified.title_stripped {
        return None;
    }
    let current = classified
        .format
        .as_ref()
        .and_then(|f| f.first())
        .map(|p| p.text.as_str());
    if Some(requested) == current {
        None
    } else {
        Some(requested.to_owned())
    }
}

/// `buildPushPlan`.
pub fn build_push_plan(
    connector: &dyn Connector,
    target_dir: &Path,
    on_status: &mut dyn FnMut(&str),
) -> Result<BuildPushPlanResult, Error> {
    let Some(mut state) = open_vault(target_dir, on_status)? else {
        return Err(Error::NotClonedDirectory {
            target_dir: target_dir.display().to_string(),
        });
    };
    let title_mode = state.mode();
    let mut planning_mutated_state = settle_pending_renames(target_dir, &mut state.notes, false, title_mode)?.changed;

    let mut entries: Vec<ExecutablePlanEntry> = Vec::new();
    let dir_index = state_dir_index(PreviousLayout::of(&state));
    let filename_as_title = title_mode == TitleMode::Filename;
    let mut notices: Vec<SyncNotice> = Vec::new();

    for entry in state.notes.values() {
        let Some(target) = pending_rename_target(entry) else {
            continue;
        };
        let mut plan_entry = PlanEntry::refused(PlanEntryKind::Rename, entry.file.clone(), Refusal::PendingRename);
        plan_entry.pending_rename = Some(target);
        entries.push(plan_entry.into());
    }

    let mut untracked: Vec<UntrackedNote> = Vec::new();
    for file in list_untracked_markdown_files(target_dir, &state)? {
        let text = read_text(&target_dir.join(&file))?.unwrap_or_default();
        let envelope = split_frontmatter(&text, split_options(title_mode));
        untracked.push(UntrackedNote {
            note_id: read_note_id(&envelope.frontmatter),
            note_title: read_note_title(&envelope.frontmatter),
            file,
            local_text: envelope.body,
        });
    }
    let recorded_titles: HashMap<String, String> = untracked
        .iter()
        .filter_map(|u| u.note_title.clone().map(|t| (u.file.clone(), t)))
        .collect();

    let mut update_candidates: Vec<PushCandidate> = Vec::new();
    let mut missing_candidates: Vec<(String, NoteEntry)> = Vec::new();

    for (record_name, entry) in &state.notes {
        let (local_state, frontmatter, local_text) = match read_local_note(target_dir, entry, record_name, title_mode)?
        {
            LocalNote::Missing => {
                missing_candidates.push((record_name.clone(), entry.clone()));
                continue;
            }
            LocalNote::Present {
                state,
                frontmatter,
                body,
            } => (state, frontmatter, body),
        };
        let requested_title = if filename_as_title {
            read_note_title(&frontmatter)
        } else {
            None
        };
        let title_only = local_state == LocalFileState::Clean;
        if title_only && (requested_title.is_none() || requested_title == entry.frontmatter_title) {
            continue;
        }
        let shared_refusal = shared_note_write_refusal(&state, entry);
        if let Some(r) = shared_refusal
            && !(title_only && entry.frontmatter_title.is_none())
        {
            entries.push(
                PlanEntry::refused(PlanEntryKind::Update, entry.file.clone(), Refusal::UpdateSharedNote(r)).into(),
            );
            continue;
        }
        if has_conflict_markers(&local_text) {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::Update,
                    entry.file.clone(),
                    Refusal::UpdateConflictMarkers,
                )
                .into(),
            );
            continue;
        }
        if local_text.is_empty() && title_mode != TitleMode::Filename {
            entries.push(PlanEntry::refused(PlanEntryKind::Update, entry.file.clone(), Refusal::UpdateEmptied).into());
            continue;
        }
        if has_unknown_content_marker(&local_text) {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::Update,
                    entry.file.clone(),
                    Refusal::UpdateUnknownContent {
                        file: entry.file.clone(),
                    },
                )
                .into(),
            );
            continue;
        }
        let previously_had_attachments = state
            .attachments
            .as_ref()
            .is_some_and(|a| a.values().any(|a| a.note_record_name == *record_name));
        if !previously_had_attachments && has_attachment_reference(&local_text) {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::Update,
                    entry.file.clone(),
                    Refusal::UpdateNewAttachmentReference {
                        file: entry.file.clone(),
                    },
                )
                .into(),
            );
            continue;
        }
        update_candidates.push(PushCandidate {
            record_name: record_name.clone(),
            entry: entry.clone(),
            local_text,
            frontmatter,
            requested_title,
            title_only,
        });
    }

    // --- local-move pairing
    let mut move_pairs: Vec<(String, NoteEntry, String)> = Vec::new();
    let mut claimed_by_move: HashSet<String> = HashSet::new();
    let mut refused_by_id_claim: HashSet<String> = HashSet::new();
    let mut ambiguous_record_names: HashSet<String> = HashSet::new();
    {
        let missing: IndexMap<&str, &NoteEntry> = missing_candidates.iter().map(|(rn, e)| (rn.as_str(), e)).collect();
        let files: Vec<UntrackedFile> = untracked
            .iter()
            .map(|u| UntrackedFile {
                file: u.file.clone(),
                note_id: u.note_id.clone(),
            })
            .collect();
        let resolution = resolve_note_ids(&files, state.notes.keys().map(String::as_str), &|rn| {
            !missing.contains_key(rn)
        });
        for mv in &resolution.moves {
            if let Some(entry) = missing.get(mv.record_name.as_str()) {
                move_pairs.push((mv.record_name.clone(), (*entry).clone(), mv.file.clone()));
                claimed_by_move.insert(mv.file.clone());
            }
        }
        for claim in &resolution.ambiguous {
            let tracked_file = state.notes.get(&claim.record_name).map(|e| e.file.clone());
            ambiguous_record_names.insert(claim.record_name.clone());
            for file in &claim.files {
                refused_by_id_claim.insert(file.clone());
                entries.push(
                    PlanEntry::refused(
                        PlanEntryKind::Create,
                        file.clone(),
                        Refusal::AmbiguousNoteId {
                            other_count: claim.files.len() - 1,
                            others: claim.files.iter().filter(|o| *o != file).cloned().collect(),
                            tracked_file: tracked_file.clone(),
                        },
                    )
                    .into(),
                );
            }
        }
        for file in &resolution.stale_ids {
            notices.push(SyncNotice {
                level: NoticeLevel::Warn,
                message: format!(
                    "{file} carries an {} for a note this clone doesn't track - pushing it as a new note",
                    crate::md::frontmatter::NOTE_ID_KEY
                ),
            });
        }
    }
    {
        let paired = |pairs: &Vec<(String, NoteEntry, String)>, rn: &str| pairs.iter().any(|(p, _, _)| p == rn);
        let pairable: Vec<(String, NoteEntry)> = missing_candidates
            .iter()
            .filter(|(rn, _)| !paired(&move_pairs, rn))
            .cloned()
            .collect();
        for (record_name, entry) in &pairable {
            let Some(base) = read_base_copy(target_dir, record_name)? else {
                continue;
            };
            if let Some(found) = untracked.iter().find(|u| {
                !claimed_by_move.contains(&u.file) && !refused_by_id_claim.contains(&u.file) && u.local_text == base
            }) {
                move_pairs.push((record_name.clone(), entry.clone(), found.file.clone()));
                claimed_by_move.insert(found.file.clone());
            }
        }
        let unpaired: Vec<(String, NoteEntry)> = pairable
            .into_iter()
            .filter(|(rn, _)| !paired(&move_pairs, rn))
            .collect();
        for (record_name, entry) in &unpaired {
            let base_name = posix::basename(&entry.file);
            let files: Vec<&UntrackedNote> = untracked
                .iter()
                .filter(|u| {
                    !claimed_by_move.contains(&u.file)
                        && !refused_by_id_claim.contains(&u.file)
                        && posix::basename(&u.file) == base_name
                })
                .collect();
            let rivals = unpaired
                .iter()
                .filter(|(_, o)| posix::basename(&o.file) == base_name)
                .count();
            if files.len() == 1 && rivals == 1 {
                let file = files[0].file.clone();
                move_pairs.push((record_name.clone(), entry.clone(), file.clone()));
                claimed_by_move.insert(file);
            }
        }
    }
    let mut delete_candidates: Vec<(String, NoteEntry)> = Vec::new();
    for (record_name, entry) in &missing_candidates {
        if move_pairs.iter().any(|(p, _, _)| p == record_name) || ambiguous_record_names.contains(record_name) {
            continue;
        }
        if entry.shared_zone_owner.is_some() {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::Delete,
                    entry.file.clone(),
                    Refusal::DeleteSharedNote {
                        file: entry.file.clone(),
                    },
                )
                .into(),
            );
            continue;
        }
        delete_candidates.push((record_name.clone(), entry.clone()));
    }

    // --- folders the account doesn't have yet
    let wanted: Vec<String> = move_pairs
        .iter()
        .map(|(_, _, to)| note_dir_of(to))
        .chain(untracked.iter().map(|u| note_dir_of(&u.file)))
        .collect();
    let folder_plan = plan_folder_creates(wanted.iter().map(String::as_str), &dir_index, &mut rt::random_uuid);
    let folder_refusals: HashMap<String, super::plan::FolderRefusal> = folder_plan
        .refusals
        .iter()
        .map(|r| (r.dir_path.clone(), r.reason.clone()))
        .collect();
    let resolve_dir = |dir: &str| -> Option<StateDirInfo> {
        if let Some(existing) = dir_index.get(dir) {
            return Some(existing.clone());
        }
        folder_plan.dir_to_record_name.get(dir).map(|rn| StateDirInfo::Folder {
            folder_record_name: rn.clone(),
            shared_zone_owner: None,
            permission: None,
        })
    };

    let mut ready_moves: Vec<ReadyMove> = Vec::new();
    for (record_name, entry, to_file) in &move_pairs {
        let to_dir = note_dir_of(to_file);
        let relocated = to_dir != note_dir_of(&entry.file);
        let info = resolve_dir(&to_dir);
        let refuse = |refusal: Refusal| -> ExecutablePlanEntry {
            let mut e = PlanEntry::refused(PlanEntryKind::Move, to_file.clone(), refusal);
            e.previous_file = Some(entry.file.clone());
            e.into()
        };
        if entry.shared_zone_owner.is_some() {
            entries.push(refuse(Refusal::MoveSharedNote {
                previous_file: entry.file.clone(),
            }));
            continue;
        }
        if to_dir.is_empty() {
            entries.push(refuse(Refusal::MoveToTopLevel));
            continue;
        }
        let Some(info) = info else {
            entries.push(refuse(Refusal::MoveIntoUnfolderableDir {
                dir: to_dir.clone(),
                folder_refusal: folder_refusals.get(&to_dir).cloned(),
            }));
            continue;
        };
        if matches!(info, StateDirInfo::SharerHome { .. }) || info.shared_zone_owner().is_some() {
            entries.push(refuse(Refusal::MoveIntoSharerArea));
            continue;
        }
        let has_tracked_attachments = state
            .attachments
            .as_ref()
            .is_some_and(|a| a.values().any(|a| a.note_record_name == *record_name));
        if relocated && has_tracked_attachments {
            entries.push(refuse(Refusal::MoveWithAttachments));
            continue;
        }
        let previous_title = title_from_note_file_name(&entry.file);
        let new_title = title_expressed_by_file(to_file, &recorded_titles);
        let retitled = title_mode == TitleMode::Filename && new_title != previous_title;
        ready_moves.push(ReadyMove {
            record_name: record_name.clone(),
            entry: entry.clone(),
            to_file: to_file.clone(),
            folder_record_name: info.folder_record_name().unwrap_or_default().to_owned(),
            relocated,
            new_title: retitled.then_some(new_title),
        });
    }

    let mut create_candidates: Vec<CreateCandidate> = Vec::new();
    for u in &untracked {
        if claimed_by_move.contains(&u.file) || refused_by_id_claim.contains(&u.file) {
            continue;
        }
        let file = &u.file;
        let local_text = &u.local_text;
        let dir = note_dir_of(file);
        let info = resolve_dir(&dir);
        let refuse = |refusal: Refusal| -> ExecutablePlanEntry {
            PlanEntry::refused(PlanEntryKind::Create, file.clone(), refusal).into()
        };
        // Port only (docs/PORT_PLAN.md §7): an id claimed by a move or an
        // ambiguous claim was handled above, so a tracked id here means that
        // note's own file is still present - this file is a duplicate of it.
        if let Some(tracked) = u.note_id.as_deref().and_then(|id| state.notes.get(id)) {
            entries.push(refuse(Refusal::CreateDuplicatesTrackedNote {
                tracked_file: tracked.file.clone(),
            }));
            continue;
        }
        if dir.is_empty() {
            entries.push(refuse(Refusal::CreateAtTopLevel));
            continue;
        }
        let Some(info) = info else {
            entries.push(refuse(Refusal::CreateInUnfolderableDir {
                dir: dir.clone(),
                folder_refusal: folder_refusals.get(&dir).cloned(),
            }));
            continue;
        };
        if matches!(info, StateDirInfo::SharerHome { .. }) {
            entries.push(refuse(Refusal::CreateLooseInSharerHome));
            continue;
        }
        if info.shared_zone_owner().is_some() && info.permission() == Some("READ_ONLY") {
            entries.push(refuse(Refusal::CreateInReadOnlyShare));
            continue;
        }
        if local_text.is_empty() && title_mode != TitleMode::Filename {
            entries.push(refuse(Refusal::CreateEmptyFile));
            continue;
        }
        if has_conflict_markers(local_text) {
            entries.push(refuse(Refusal::CreateConflictMarkers));
            continue;
        }
        if has_unknown_content_marker(local_text) {
            entries.push(refuse(Refusal::CreateUnknownContent));
            continue;
        }
        if has_embed_marker(local_text) {
            entries.push(refuse(Refusal::CreateEmbedMarker));
            continue;
        }
        if has_attachment_reference(local_text) {
            entries.push(refuse(Refusal::CreateAttachmentReference));
            continue;
        }
        create_candidates.push(CreateCandidate {
            file: file.clone(),
            local_text: local_text.clone(),
            folder_record_name: info.folder_record_name().unwrap_or_default().to_owned(),
            shared_zone_owner: info.shared_zone_owner().map(str::to_owned),
        });
    }

    if update_candidates.is_empty()
        && delete_candidates.is_empty()
        && create_candidates.is_empty()
        && ready_moves.is_empty()
    {
        // icloud-md returns before its state write here, even when settling
        // changed something: a later run re-derives it.
        return Ok(BuildPushPlanResult {
            state,
            entries,
            notices,
            remote: None,
        });
    }

    let remote = resolve_folder_account(connector, target_dir, state.account.as_ref())?;
    let db = &remote.db;

    // --- folder creates, ahead of every note write
    let needed_dirs: Vec<String> = create_candidates
        .iter()
        .map(|c| note_dir_of(&c.file))
        .chain(ready_moves.iter().map(|m| note_dir_of(&m.to_file)))
        .collect::<indexmap::IndexSet<_>>()
        .into_iter()
        .collect();
    let folder_entries: Vec<ExecutablePlanEntry> = folder_plan
        .folders
        .iter()
        .filter(|f| {
            needed_dirs
                .iter()
                .any(|d| *d == f.dir_path || d.starts_with(&format!("{}/", f.dir_path)))
        })
        .map(|f| {
            let mut e = PlanEntry::new(PlanEntryKind::CreateFolder, f.dir_path.clone(), PlanResolution::Ready);
            e.folder_title = Some(f.title.clone());
            ExecutablePlanEntry::with(e, Action::CreateFolder(f.clone()))
        })
        .collect();
    entries.splice(0..0, folder_entries);

    // --- fresh lookup of every candidate, one per zone
    let mut lookup_groups: IndexMap<Option<String>, Vec<String>> = IndexMap::new();
    for c in &update_candidates {
        lookup_groups
            .entry(c.entry.shared_zone_owner.clone())
            .or_default()
            .push(c.record_name.clone());
    }
    for (record_name, _) in &delete_candidates {
        lookup_groups.entry(None).or_default().push(record_name.clone());
    }
    for m in &ready_moves {
        lookup_groups.entry(None).or_default().push(m.record_name.clone());
    }
    let mut records_by_name: HashMap<String, CloudKitRecord> = HashMap::new();
    for (owner, names) in &lookup_groups {
        for record in db.lookup_records(&note_zone(owner.as_deref()), names)? {
            records_by_name.insert(record.record_name.clone(), record);
        }
    }

    let replica_id = match &state.replica_id {
        Some(id) => id.clone(),
        None => b64(&rt::random_bytes(16)),
    };
    state.replica_id = Some(replica_id.clone());
    let replica_bytes: [u8; 16] = js::base64_decode(&replica_id).try_into().map_err(|_| {
        Error::CorruptStateFile("state.json has a malformed replicaId (expected 16 bytes, base64-encoded)".into())
    })?;

    // --- moves
    for m in &ready_moves {
        let refuse = |refusal: Refusal| -> ExecutablePlanEntry {
            let mut e = PlanEntry::refused(PlanEntryKind::Move, m.to_file.clone(), refusal);
            e.previous_file = Some(m.entry.file.clone());
            e.into()
        };
        let record = match records_by_name.get(&m.record_name) {
            Some(r) if !r.is_deleted() && !is_purged(r) && !is_in_trash(r) => r,
            _ => {
                entries.push(refuse(Refusal::MoveGoneRemotely));
                continue;
            }
        };
        if record.record_change_tag.clone().unwrap_or_default() != m.entry.record_change_tag {
            entries.push(refuse(Refusal::MoveChangedRemotely));
            continue;
        }
        let mut retitle = None;
        if let Some(new_title) = &m.new_title {
            match prepare_retitle(record, &m.entry, &m.to_file, new_title, &replica_bytes, title_mode)? {
                Ok(r) => retitle = r,
                Err(refusal) => {
                    entries.push(refuse(Refusal::MoveRetitle {
                        refusal,
                        previous_file: m.entry.file.clone(),
                    }));
                    continue;
                }
            }
        }
        let mut e = PlanEntry::new(PlanEntryKind::Move, m.to_file.clone(), PlanResolution::Ready);
        e.previous_file = Some(m.entry.file.clone());
        entries.push(ExecutablePlanEntry::with(
            e,
            Action::Move {
                record_name: m.record_name.clone(),
                entry: m.entry.clone(),
                to_file: m.to_file.clone(),
                folder_record_name: m.folder_record_name.clone(),
                relocated: m.relocated,
                record: Box::new(record.clone()),
                retitle,
            },
        ));
    }

    // --- deletes
    for (record_name, entry) in &delete_candidates {
        let ready = |action: Action| {
            ExecutablePlanEntry::with(
                PlanEntry::new(PlanEntryKind::Delete, entry.file.clone(), PlanResolution::Ready),
                action,
            )
        };
        let record = match records_by_name.get(record_name) {
            Some(r) if !r.is_deleted() && !is_purged(r) => r,
            _ => {
                entries.push(ready(Action::DeleteAlreadyGone {
                    record_name: record_name.clone(),
                    entry: entry.clone(),
                }));
                continue;
            }
        };
        if is_in_trash(record) {
            entries.push(ready(Action::DeleteAlreadyTrashed {
                record_name: record_name.clone(),
                entry: entry.clone(),
            }));
            continue;
        }
        if record.record_change_tag.clone().unwrap_or_default() != entry.record_change_tag {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::Delete,
                    entry.file.clone(),
                    Refusal::DeleteChangedRemotely,
                )
                .into(),
            );
            continue;
        }
        entries.push(ready(Action::Delete {
            record_name: record_name.clone(),
            entry: entry.clone(),
            record: Box::new(record.clone()),
        }));
    }

    // --- creates
    for c in &create_candidates {
        let refuse = |refusal: Refusal| -> ExecutablePlanEntry {
            PlanEntry::refused(PlanEntryKind::Create, c.file.clone(), refusal).into()
        };
        let built = build_create_payload(&c.file, &c.local_text, title_mode, &recorded_titles, &replica_bytes);
        let (payload_base64, plain_text) = match built {
            Ok(built) => built,
            Err(refusal) => {
                entries.push(refuse(refusal));
                continue;
            }
        };
        let modification_date_ms = mtime_ms(&target_dir.join(&c.file))?;
        entries.push(ExecutablePlanEntry::with(
            PlanEntry::new(PlanEntryKind::Create, c.file.clone(), PlanResolution::Ready),
            Action::Create {
                file: c.file.clone(),
                local_text: c.local_text.clone(),
                payload_base64,
                plain_text,
                folder_record_name: c.folder_record_name.clone(),
                shared_zone_owner: c.shared_zone_owner.clone(),
                modification_date_ms,
            },
        ));
    }

    // --- updates
    for c in &update_candidates {
        let file = c.entry.file.clone();
        let refuse = |refusal: Refusal| -> ExecutablePlanEntry {
            PlanEntry::refused(PlanEntryKind::Update, file.clone(), refusal).into()
        };
        let Some(record) = records_by_name.get(&c.record_name).filter(|r| !r.is_deleted()) else {
            entries.push(refuse(Refusal::UpdateGoneRemotely));
            continue;
        };

        if let Some(text) = record.fields.get("TextDataEncrypted").and_then(|f| f.value.as_str()) {
            record_version(
                target_dir,
                &VersionSnapshotInput::note(
                    &record.record_name,
                    record.record_change_tag.as_deref().unwrap_or(""),
                    text,
                ),
            )?;
        }

        if record.record_change_tag.clone().unwrap_or_default() != c.entry.record_change_tag {
            let classified = classify_note_record(record, &ClassifyOptions { title_mode });
            let remote_text = match &classified {
                NoteDecodeResult::Ok(d) if d.embed_slots.is_empty() => d.markdown_text.clone(),
                _ => {
                    entries.push(refuse(Refusal::UpdateChangedRemotelyUnmergeable));
                    continue;
                }
            };
            let remote_tag = record
                .record_change_tag
                .clone()
                .unwrap_or_else(|| c.entry.record_change_tag.clone());
            entries.push(
                plan_remote_changed_merge(
                    target_dir,
                    &mut state,
                    &c.record_name,
                    &c.entry,
                    &c.frontmatter,
                    &c.local_text,
                    &remote_text,
                    &remote_tag,
                )?
                .into(),
            );
            planning_mutated_state = true;
            continue;
        }

        if c.title_only {
            let classified = classify_note_record(record, &ClassifyOptions { title_mode });
            if let NoteDecodeResult::Ok(d) = &classified
                && requested_retitle(d, c.requested_title.as_deref()).is_none()
            {
                let mut entry = c.entry.clone();
                entry.frontmatter_title = c.requested_title.clone();
                state.notes.insert(c.record_name.clone(), entry);
                planning_mutated_state = true;
                continue;
            }
            if let Some(r) = shared_note_write_refusal(&state, &c.entry) {
                entries.push(refuse(Refusal::TitleOnlySharedNote(r)));
                continue;
            }
        }

        let modification_date_ms = mtime_ms(&target_dir.join(&c.entry.file))?;
        let zone = note_zone(c.entry.shared_zone_owner.as_deref());
        let tracked_ids: HashSet<String> = state
            .attachments
            .as_ref()
            .map(|a| {
                a.iter()
                    .filter(|(_, att)| att.note_record_name == c.record_name)
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        let prepared = prepare_update(
            db,
            &zone,
            target_dir,
            record,
            &c.entry,
            &c.local_text,
            &tracked_ids,
            &replica_bytes,
            modification_date_ms,
            title_mode,
            c.requested_title.as_deref(),
        )?;
        let prepared = match prepared {
            Ok(p) => p,
            Err(refusal) => {
                entries.push(refuse(Refusal::UpdatePrepare(refusal)));
                continue;
            }
        };
        if prepared.updates.is_empty() {
            entries.push(ExecutablePlanEntry::with(
                PlanEntry::new(PlanEntryKind::Update, file.clone(), PlanResolution::Noop),
                Action::Rebase {
                    record_name: c.record_name.clone(),
                    file: file.clone(),
                    local_text: c.local_text.clone(),
                },
            ));
            continue;
        }
        let mut e = PlanEntry::new(PlanEntryKind::Update, file.clone(), PlanResolution::Ready);
        e.remark = prepared.retitled_to.as_deref().map(retitle_remark);
        entries.push(ExecutablePlanEntry::with(
            e,
            Action::Update {
                record_name: c.record_name.clone(),
                entry: c.entry.clone(),
                zone,
                updates: prepared.updates,
                note_text_updated: prepared.note_text_updated,
                local_text: c.local_text.clone(),
                requested_title: c.requested_title.clone(),
                modification_date_ms,
            },
        ));
    }

    if planning_mutated_state {
        write_clone_state(target_dir, &state)?;
    }
    Ok(BuildPushPlanResult {
        state,
        entries,
        notices,
        remote: Some(remote),
    })
}

/// The create path's document build and verification (push.ts 1000-1049):
/// `Ok((payloadBase64, plainText))` or the refusal.
fn build_create_payload(
    file: &str,
    local_text: &str,
    title_mode: TitleMode,
    recorded_titles: &HashMap<String, String>,
    replica: &[u8; 16],
) -> Result<(String, String), Refusal> {
    let parsed = parse_note_markdown(local_text).map_err(|r| Refusal::CreateMarkdown { reason: r.reason })?;
    let desired = if title_mode == TitleMode::Filename {
        restore_title_paragraph_text(
            &title_paragraph_from_filename(&title_expressed_by_file(file, recorded_titles)),
            &parsed.paragraphs,
        )
    } else {
        parsed
    };
    // A thrown error anywhere below is TS's `catch (cause)`: the message is
    // the refusal reason.
    let thrown = |e: crate::doc::DocError| Refusal::CreateBuildError { message: e.to_string() };
    let mut doc = build_initial_note_document(&desired.text, replica).map_err(thrown)?;
    reconcile_note_format(&mut doc, &desired.paragraphs, replica)
        .map_err(thrown)?
        .map_err(|reason| Refusal::CreateReconcile { reason })?;
    let compressed = compress_note_document(&encode_note_document(&doc).map_err(thrown)?);
    let rebuilt = decode_note_string(&compressed).map_err(thrown)?;
    let rebuilt_text = rebuilt.string();
    let format = decode_note_format(rebuilt_text, &rebuilt.attribute_run);
    let verified = rebuilt_text == desired.text
        && format
            .as_ref()
            .is_ok_and(|f| formats_round_trip_equal(f, &desired.paragraphs));
    if !verified {
        return Err(Refusal::CreateVerificationFailed);
    }
    Ok((b64(&compressed), desired.text))
}

/// `planRemoteChangedMerge`: diff3 the remote text into the local edit
/// during planning; the tag advances, the base copy only on a clean merge.
#[allow(clippy::too_many_arguments)]
pub fn plan_remote_changed_merge(
    target_dir: &Path,
    state: &mut CloneState,
    record_name: &str,
    entry: &NoteEntry,
    frontmatter: &str,
    local_text: &str,
    remote_text: &str,
    remote_tag: &str,
) -> Result<PlanEntry, Error> {
    let base = read_base_copy(target_dir, record_name)?.unwrap_or_default();
    let outcome = merge_note_versions(&base, local_text, remote_text);
    std::fs::write(
        target_dir.join(&entry.file),
        join_frontmatter(frontmatter, &outcome.text),
    )?;
    let mut updated = entry.clone();
    updated.record_change_tag = remote_tag.to_owned();
    if outcome.has_conflict {
        state.notes.insert(record_name.to_owned(), updated);
        return Ok(PlanEntry::refused(
            PlanEntryKind::Update,
            entry.file.clone(),
            Refusal::MergedWithConflicts,
        ));
    }
    write_base_copy(target_dir, record_name, remote_text)?;
    state.notes.insert(record_name.to_owned(), updated);
    Ok(PlanEntry::refused(
        PlanEntryKind::Update,
        entry.file.clone(),
        Refusal::MergedCleanly,
    ))
}

/// `PreparedCandidate`.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCandidate {
    pub updates: Vec<RecordUpdate>,
    pub note_text_updated: bool,
    pub retitled_to: Option<String>,
}

fn classify_for_edit(record: &CloudKitRecord, title_mode: TitleMode) -> Result<Box<DecodedNote>, String> {
    match classify_note_record(record, &ClassifyOptions { title_mode }) {
        NoteDecodeResult::Ok(d) => Ok(d),
        NoteDecodeResult::Deleted => Err("deleted".into()),
        NoteDecodeResult::Unsyncable(r) => Err(r.as_str().into()),
    }
}

/// `prepareUpdate`.
#[allow(clippy::too_many_arguments)]
fn prepare_update<T: Transport>(
    db: &Database<T>,
    zone: &NoteZone,
    target_dir: &Path,
    record: &CloudKitRecord,
    entry: &NoteEntry,
    local_text: &str,
    tracked_ids: &HashSet<String>,
    replica: &[u8; 16],
    modification_date_ms: i64,
    title_mode: TitleMode,
    requested_title: Option<&str>,
) -> Result<Result<PreparedCandidate, PrepareRefusal>, Error> {
    let file = entry.file.clone();
    let classified = match classify_for_edit(record, title_mode) {
        Ok(c) => c,
        Err(detail) => return Ok(Err(PrepareRefusal::NoLongerEditable { detail })),
    };
    if !classified.publishable {
        return Ok(Err(PrepareRefusal::Unpublishable {
            unpublishable_reason: classified.unpublishable_reason.clone(),
            file,
        }));
    }
    if !classified.embed_slots.is_empty() {
        return prepare_embed_candidate(
            db,
            zone,
            target_dir,
            record,
            &classified,
            entry,
            local_text,
            tracked_ids,
            replica,
            modification_date_ms,
            requested_title,
        );
    }
    let parsed = match parse_note_markdown(local_text) {
        Ok(p) => p,
        Err(r) => return Ok(Err(PrepareRefusal::Markdown { reason: r.reason, file })),
    };
    let desired = match restore_stripped_title(&classified, parsed, requested_title) {
        Ok(d) => d,
        Err(r) => return Ok(Err(r)),
    };
    let text_update = match prepare_note_text_update(
        record,
        &classified.body_text,
        &desired,
        &classified.embed_slots,
        replica,
        &file,
    )? {
        Ok(t) => t,
        Err(r) => return Ok(Err(PrepareRefusal::TextUpdate(r))),
    };
    let Some(payload) = text_update else {
        return Ok(Ok(PreparedCandidate {
            updates: Vec::new(),
            note_text_updated: false,
            retitled_to: None,
        }));
    };
    let fields = build_note_update_fields(record, &payload, &desired.text, modification_date_ms);
    Ok(Ok(PreparedCandidate {
        updates: vec![note_record_update(record, entry, fields)],
        note_text_updated: true,
        retitled_to: requested_retitle(&classified, requested_title),
    }))
}

/// `prepareEmbedCandidate`.
#[allow(clippy::too_many_arguments)]
fn prepare_embed_candidate<T: Transport>(
    db: &Database<T>,
    zone: &NoteZone,
    target_dir: &Path,
    record: &CloudKitRecord,
    classified: &DecodedNote,
    entry: &NoteEntry,
    local_text: &str,
    tracked_ids: &HashSet<String>,
    replica: &[u8; 16],
    modification_date_ms: i64,
    requested_title: Option<&str>,
) -> Result<Result<PreparedCandidate, PrepareRefusal>, Error> {
    let file = entry.file.clone();
    let plan = match plan_embed_representations(local_text, &classified.embed_slots, tracked_ids) {
        Ok(p) => p,
        Err(reason) => return Ok(Err(PrepareRefusal::EmbedPlan { reason, file })),
    };
    let parsed = match parse_note_markdown(&plan.reconstructed_body_text) {
        Ok(p) => p,
        Err(r) => return Ok(Err(PrepareRefusal::EmbedMarkdown { reason: r.reason, file })),
    };
    let desired = match restore_stripped_title(classified, parsed, requested_title) {
        Ok(d) => d,
        Err(r) => return Ok(Err(r)),
    };

    let attachment_records = if plan.tables.is_empty() {
        Vec::new()
    } else {
        let names: Vec<String> = plan
            .tables
            .iter()
            .map(|t| t.reference.attachment_identifier.clone())
            .collect();
        db.lookup_records(zone, &names)?
    };
    for attachment in &attachment_records {
        if attachment.is_deleted() {
            continue;
        }
        if let Some(value) = attachment
            .fields
            .get("MergeableDataEncrypted")
            .and_then(|f| f.value.as_str())
        {
            record_version(
                target_dir,
                &VersionSnapshotInput::table(
                    &attachment.record_name,
                    attachment.record_change_tag.as_deref().unwrap_or(""),
                    value,
                    &record.record_name,
                ),
            )?;
        }
    }

    let mut updates: Vec<RecordUpdate> = Vec::new();
    for table in &plan.tables {
        let Some(attachment) = attachment_records
            .iter()
            .rev()
            .find(|r| r.record_name == table.reference.attachment_identifier)
            .filter(|r| !r.is_deleted())
        else {
            return Ok(Err(PrepareRefusal::TableGoneRemotely));
        };
        let result = match prepare_table_attachment_update(attachment, &table.block.grid, replica) {
            Ok(r) => r,
            Err(reason) => return Ok(Err(PrepareRefusal::TableUpdate { reason, file })),
        };
        if result.changed {
            let mut fields = UpdateFields::new();
            fields.insert(
                "MergeableDataEncrypted".into(),
                UpdateFieldValue::new(result.mergeable_data_base64.into()),
            );
            updates.push(RecordUpdate {
                record_name: attachment.record_name.clone(),
                record_type: "Attachment".into(),
                record_change_tag: attachment.record_change_tag.clone().unwrap_or_default(),
                fields,
                parent_record_name: attachment.parent_record_name.clone(),
            });
        }
    }

    let retitled_to = requested_retitle(classified, requested_title);
    let mut note_text_updated = false;
    if plan.reconstructed_body_text != classified.markdown_text || retitled_to.is_some() {
        let text_update = match prepare_note_text_update(
            record,
            &classified.body_text,
            &desired,
            &classified.embed_slots,
            replica,
            &file,
        )? {
            Ok(t) => t,
            Err(r) => return Ok(Err(PrepareRefusal::TextUpdate(r))),
        };
        if let Some(payload) = text_update {
            let fields = build_note_update_fields(record, &payload, &desired.text, modification_date_ms);
            updates.insert(0, note_record_update(record, entry, fields));
            note_text_updated = true;
        }
    }
    if updates.is_empty() {
        return Ok(Ok(PreparedCandidate {
            updates: Vec::new(),
            note_text_updated: false,
            retitled_to: None,
        }));
    }
    Ok(Ok(PreparedCandidate {
        updates,
        note_text_updated,
        retitled_to,
    }))
}

/// `prepareRetitle`: the note-text payload carrying a renamed file's new
/// title (`Ok(None)`: the title doesn't actually change).
pub fn prepare_retitle(
    record: &CloudKitRecord,
    entry: &NoteEntry,
    to_file: &str,
    new_title: &str,
    replica: &[u8; 16],
    title_mode: TitleMode,
) -> Result<Result<Option<Retitle>, RetitleRefusal>, Error> {
    let classified = match classify_for_edit(record, title_mode) {
        Ok(c) => c,
        Err(detail) => return Ok(Err(RetitleRefusal::NoLongerEditable { detail })),
    };
    if !classified.publishable {
        return Ok(Err(RetitleRefusal::Unpublishable {
            unpublishable_reason: classified.unpublishable_reason.clone(),
        }));
    }
    let format = match (&classified.format, classified.title_stripped) {
        (Some(format), true) => format,
        _ => return Ok(Err(RetitleRefusal::TitleHasEmbed)),
    };
    let split = split_title_paragraph(format);
    if let Some(title) = &split.title
        && (title.text == new_title || carried_title_spelling(&title.text) == new_title)
    {
        return Ok(Ok(None));
    }
    let desired = restore_title_paragraph_text(&title_paragraph_from_filename(new_title), &split.body);
    let _ = entry;
    match prepare_note_text_update(
        record,
        &classified.body_text,
        &desired,
        &classified.embed_slots,
        replica,
        to_file,
    )? {
        Err(r) => Ok(Err(RetitleRefusal::TextUpdate(r))),
        Ok(None) => Ok(Ok(None)),
        Ok(Some(payload_base64)) => Ok(Ok(Some(Retitle {
            payload_base64,
            plain_text: desired.text,
        }))),
    }
}

/// `restoreStrippedTitle`: put the live title paragraph (or the one an
/// `apple-note-title` asks for) back in front of a body-only parse.
pub fn restore_stripped_title(
    classified: &DecodedNote,
    parsed: NoteText,
    requested_title: Option<&str>,
) -> Result<NoteText, PrepareRefusal> {
    if !classified.title_stripped {
        if requested_title.is_some() {
            return Err(PrepareRefusal::TitleHasEmbedCannotRetitle);
        }
        return Ok(parsed);
    }
    let Some(current) = classified.format.as_ref().and_then(|f| f.first()) else {
        return Err(PrepareRefusal::NoTitleParagraph);
    };
    let title: FormatParagraph = match requested_retitle(classified, requested_title) {
        Some(retitle) => title_paragraph_from_filename(&retitle),
        None => current.clone(),
    };
    Ok(restore_title_paragraph_text(&title, &parsed.paragraphs))
}

fn note_record_update(record: &CloudKitRecord, entry: &NoteEntry, fields: UpdateFields) -> RecordUpdate {
    RecordUpdate {
        record_name: record.record_name.clone(),
        record_type: "Note".into(),
        record_change_tag: entry.record_change_tag.clone(),
        fields,
        parent_record_name: if entry.shared_zone_owner.is_none() {
            record.parent_record_name.clone()
        } else {
            None
        },
    }
}

/// `prepareNoteTextUpdate`: `Ok(Ok(Some(payload)))`, `Ok(Ok(None))` for
/// "unchanged", or the refusal. `file` is the entry's file as the caller
/// sees it (the move target for a retitle).
pub fn prepare_note_text_update(
    record: &CloudKitRecord,
    current_body_text: &str,
    desired: &NoteText,
    expected_slots: &[EmbedSlot],
    replica: &[u8; 16],
    file: &str,
) -> Result<Result<Option<String>, TextUpdateRefusal>, Error> {
    let Some(text_b64) = record.fields.get("TextDataEncrypted").and_then(|f| f.value.as_str()) else {
        return Ok(Err(TextUpdateRefusal::NoTextData));
    };
    if record.fields.get("TextDataAsset").is_some_and(|f| !f.value.is_null()) {
        return Ok(Err(TextUpdateRefusal::TextAsAsset));
    }
    let raw = decompress_note_document(&js::base64_decode(text_b64)).map_err(|e| Error::Internal(e.to_string()))?;
    if !note_document_round_trips(&raw) {
        return Ok(Err(TextUpdateRefusal::NotRoundTrip));
    }
    let touches_placeholder = compute_splices(current_body_text, &desired.text).iter().any(|splice| {
        js::slice16(current_body_text, splice.start, splice.start + splice.delete_length)
            .contains(OBJECT_REPLACEMENT_CHARACTER)
            || splice.insert_text.contains(OBJECT_REPLACEMENT_CHARACTER)
    });
    if touches_placeholder {
        return Ok(Err(TextUpdateRefusal::TouchesEmbed { file: file.to_owned() }));
    }

    let exception = |e: crate::doc::DocError| TextUpdateRefusal::Exception { message: e.to_string() };
    let mut doc = match parse_note_document(&raw) {
        Ok(doc) => doc,
        Err(e) => return Ok(Err(exception(e))),
    };
    if doc.text != current_body_text {
        return Ok(Err(TextUpdateRefusal::DecoderDisagreement));
    }
    let text_changed = match apply_text_edit(&mut doc, &desired.text, &ApplyTextEditOptions { replica_id: *replica }) {
        Ok(changed) => changed,
        Err(e) => return Ok(Err(exception(e))),
    };
    let format_changed = match reconcile_note_format(&mut doc, &desired.paragraphs, replica) {
        Ok(Ok(changed)) => changed,
        Err(e) => return Ok(Err(exception(e))),
        Ok(Err(reason)) => {
            return Ok(Err(TextUpdateRefusal::Reconcile {
                reason,
                file: file.to_owned(),
            }));
        }
    };
    if !text_changed && !format_changed {
        return Ok(Ok(None));
    }
    if let Err(e) = validate_document_invariants(&doc) {
        return Ok(Err(exception(e)));
    }
    let raw = match encode_note_document(&doc) {
        Ok(raw) => raw,
        Err(e) => return Ok(Err(exception(e))),
    };
    let compressed = compress_note_document(&raw);
    match decode_note_body_text(&compressed) {
        Ok(text) if text == desired.text => {}
        Ok(_) => return Ok(Err(TextUpdateRefusal::RebuiltDecodeFailed)),
        Err(e) => return Ok(Err(exception(e))),
    }
    match decode_note_embed_slots(&compressed) {
        Ok(Some(slots)) if slots == expected_slots => {}
        Ok(_) => return Ok(Err(TextUpdateRefusal::RebuiltEmbedFailed)),
        Err(e) => return Ok(Err(exception(e))),
    }
    let rebuilt = match decode_note_string(&compressed) {
        Ok(s) => s,
        Err(e) => return Ok(Err(exception(e))),
    };
    match decode_note_format(rebuilt.string(), &rebuilt.attribute_run) {
        Ok(format) if formats_round_trip_equal(&format, &desired.paragraphs) => {}
        _ => return Ok(Err(TextUpdateRefusal::RebuiltFormatFailed)),
    }
    Ok(Ok(Some(b64(&compressed))))
}

// --- execution --------------------------------------------------------------------

/// `applyLocalNoteDeletion` (delete.ts): drop tracking; delete a clean file.
fn apply_local_note_deletion(
    target_dir: &Path,
    record_name: &str,
    entry: &NoteEntry,
    state: &mut CloneState,
) -> Result<LocalFileState, Error> {
    let mut attachments = state.attachments.take().unwrap_or_default();
    let mut table_attachments = state.table_attachments.take().unwrap_or_default();
    let local = local_file_state(target_dir, entry, record_name, state.mode())?;
    if local == LocalFileState::Clean {
        safe_unlink(&target_dir.join(&entry.file))?;
    }
    state.notes.shift_remove(record_name);
    remove_base_copy(target_dir, record_name)?;
    for removed in remove_attachments_for_note(target_dir, record_name, &attachments)? {
        attachments.shift_remove(&removed);
    }
    for removed in remove_table_attachments_for_note(record_name, &table_attachments) {
        table_attachments.shift_remove(&removed);
    }
    state.attachments = Some(attachments);
    state.table_attachments = Some(table_attachments);
    Ok(local)
}

/// `rememberTrashedNote` (delete.ts).
fn remember_trashed_note(state: &mut CloneState, record_name: &str, file: &str) {
    let trashed = state.trashed.get_or_insert_with(IndexMap::new);
    trashed.insert(
        record_name.to_owned(),
        TrashedEntry {
            file: file.to_owned(),
            trashed_at: rt::now_ms(),
        },
    );
}

fn rejection(result: &RecordUpdateResult) -> Option<String> {
    match result {
        RecordUpdateResult::Ok(_) => None,
        RecordUpdateResult::Rejected {
            server_error_code,
            reason,
        } => Some(match reason {
            Some(r) if !r.is_empty() => format!("{server_error_code} ({r})"),
            _ => server_error_code.clone(),
        }),
    }
}

/// Runs one entry's action - icloud-md's `execute()`.
fn execute<T: Transport>(
    action: &Action,
    db: &Database<T>,
    target_dir: &Path,
    state: &mut CloneState,
    failed_folders: &mut HashSet<String>,
) -> Result<ExecuteOutcome, Error> {
    let private = note_zone(None);
    match action {
        Action::CreateFolder(folder) => {
            if let Some(parent) = &folder.parent_record_name
                && failed_folders.contains(parent)
            {
                failed_folders.insert(folder.record_name.clone());
                return Ok(ExecuteOutcome::failed(format!(
                    "{}/: skipped - its parent folder could not be created",
                    folder.dir_path
                )));
            }
            let fields = build_folder_create_fields(&folder.title, folder.parent_record_name.as_deref());
            let extras = CreateExtras {
                parent_record_name: fields.parent_record_name.clone(),
                create_short_guid: false,
            };
            let result = db.create_folder_record(&private, &folder.record_name, &fields.fields, &extras)?;
            if let RecordUpdateResult::Rejected {
                server_error_code,
                reason,
            } = &result
            {
                failed_folders.insert(folder.record_name.clone());
                return Ok(ExecuteOutcome::failed(format!(
                    "{}/: {}",
                    folder.dir_path,
                    reason.clone().unwrap_or_else(|| server_error_code.clone())
                )));
            }
            state.folders.get_or_insert_with(IndexMap::new).insert(
                folder.record_name.clone(),
                FolderEntry {
                    name: folder.title.clone(),
                    parent_record_name: folder.parent_record_name.clone(),
                    dir_name: posix::basename(&folder.dir_path).to_owned(),
                    shared_zone_owner: None,
                    permission: None,
                    key_order: Some(FOLDER_CREATE_ORDER.to_vec()),
                },
            );
            Ok(ExecuteOutcome::ok(format!("Created folder {}/", folder.dir_path)))
        }
        Action::Move {
            record_name,
            entry,
            to_file,
            folder_record_name,
            relocated,
            record,
            retitle,
        } => {
            if failed_folders.contains(folder_record_name) {
                return Ok(ExecuteOutcome::failed(format!(
                    "{to_file}: skipped - the folder it moved into could not be created"
                )));
            }
            let modify = |fields: UpdateFields, tag: String| -> Result<Result<CloudKitRecord, String>, Error> {
                let result = db.update_note_record(
                    &private,
                    &RecordUpdate {
                        record_name: record_name.clone(),
                        record_type: "Note".into(),
                        record_change_tag: tag,
                        fields,
                        parent_record_name: record.parent_record_name.clone(),
                    },
                )?;
                Ok(match result {
                    RecordUpdateResult::Ok(r) => Ok(*r),
                    rejected => Err(rejection(&rejected).unwrap_or_default()),
                })
            };
            let mut current: CloudKitRecord = (**record).clone();
            if *relocated || retitle.is_none() {
                let fields = build_note_move_fields(&current, folder_record_name, rt::now_ms());
                match modify(fields, current.record_change_tag.clone().unwrap_or_default())? {
                    Ok(r) => current = r,
                    Err(failure) => {
                        return Ok(ExecuteOutcome::failed(format!(
                            "{to_file}: server rejected the move: {failure}"
                        )));
                    }
                }
            }
            if let Some(retitle) = retitle {
                let fields =
                    build_note_update_fields(&current, &retitle.payload_base64, &retitle.plain_text, rt::now_ms());
                match modify(fields, current.record_change_tag.clone().unwrap_or_default())? {
                    Ok(r) => current = r,
                    Err(failure) => {
                        return Ok(ExecuteOutcome::failed(format!(
                            "{to_file}: server rejected the retitle: {failure}"
                        )));
                    }
                }
            }
            let mut updated = entry.clone();
            updated.file = to_file.clone();
            updated.folder_record_name = Some(folder_record_name.clone());
            updated.record_change_tag = current.record_change_tag.clone().unwrap_or_default();
            updated.modification_date = match modification_date_of(&current) {
                0 => rt::now_ms(),
                ms => ms,
            };
            state.notes.insert(record_name.clone(), updated);
            apply_note_file_times(&target_dir.join(to_file), &current)?;
            let what = match (retitle.is_some(), *relocated) {
                (false, _) => "Moved",
                (true, true) => "Moved and retitled",
                (true, false) => "Retitled",
            };
            Ok(ExecuteOutcome::ok(format!("{what} {} -> {to_file}", entry.file)))
        }
        Action::DeleteAlreadyGone { record_name, entry } => {
            apply_local_note_deletion(target_dir, record_name, entry, state)?;
            if let Some(trashed) = &mut state.trashed {
                trashed.shift_remove(record_name);
            }
            Ok(ExecuteOutcome::ok(format!(
                "{}: already deleted remotely - removed from tracking",
                entry.file
            )))
        }
        Action::DeleteAlreadyTrashed { record_name, entry } => {
            apply_local_note_deletion(target_dir, record_name, entry, state)?;
            remember_trashed_note(state, record_name, &entry.file);
            Ok(ExecuteOutcome::ok(format!(
                "{}: already in Recently Deleted - removed from tracking",
                entry.file
            )))
        }
        Action::Delete {
            record_name,
            entry,
            record,
        } => {
            let result = db.update_note_record(
                &private,
                &RecordUpdate {
                    record_name: record_name.clone(),
                    record_type: "Note".into(),
                    record_change_tag: record.record_change_tag.clone().unwrap_or_default(),
                    fields: build_note_trash_fields(record, rt::now_ms()),
                    parent_record_name: record.parent_record_name.clone(),
                },
            )?;
            if let Some(failure) = rejection(&result) {
                return Ok(ExecuteOutcome::failed(format!(
                    "{}: server rejected the delete: {failure}",
                    entry.file
                )));
            }
            apply_local_note_deletion(target_dir, record_name, entry, state)?;
            remember_trashed_note(state, record_name, &entry.file);
            Ok(ExecuteOutcome::ok(format!("Moved {} to Recently Deleted", entry.file)))
        }
        Action::Create {
            file,
            local_text,
            payload_base64,
            plain_text,
            folder_record_name,
            shared_zone_owner,
            modification_date_ms,
        } => {
            if failed_folders.contains(folder_record_name) {
                return Ok(ExecuteOutcome::failed(format!(
                    "{file}: skipped - the folder it belongs in could not be created"
                )));
            }
            let record_name = rt::random_uuid();
            let zone = note_zone(shared_zone_owner.as_deref());
            let fields = build_note_create_fields(
                payload_base64,
                plain_text,
                *modification_date_ms,
                folder_record_name,
                shared_zone_owner.as_deref(),
            );
            let extras = if shared_zone_owner.is_some() {
                CreateExtras {
                    parent_record_name: Some(folder_record_name.clone()),
                    create_short_guid: true,
                }
            } else {
                CreateExtras::default()
            };
            let created = match db.create_note_record(&zone, &record_name, &fields, &extras)? {
                RecordUpdateResult::Ok(r) => *r,
                rejected => {
                    return Ok(ExecuteOutcome::failed(format!(
                        "{file}: server rejected the create: {}",
                        rejection(&rejected).unwrap_or_default()
                    )));
                }
            };
            state.notes.insert(
                record_name.clone(),
                NoteEntry {
                    file: file.clone(),
                    record_change_tag: created.record_change_tag.clone().unwrap_or_default(),
                    modification_date: match modification_date_of(&created) {
                        0 => *modification_date_ms,
                        ms => ms,
                    },
                    folder_record_name: Some(folder_record_name.clone()),
                    shared_zone_owner: shared_zone_owner.clone(),
                    key_order: Some(NOTE_CREATE_ORDER.to_vec()),
                    ..Default::default()
                },
            );
            write_base_copy(target_dir, &record_name, local_text)?;
            let path = target_dir.join(file);
            let text = read_text(&path)?.unwrap_or_default();
            let envelope = split_frontmatter(&text, split_options(state.mode()));
            std::fs::write(
                &path,
                join_frontmatter(&set_note_id(&envelope.frontmatter, &record_name), &envelope.body),
            )?;
            apply_note_file_times(&path, &created)?;
            if let Some(value) = created.fields.get("TextDataEncrypted").and_then(|f| f.value.as_str()) {
                record_version(
                    target_dir,
                    &VersionSnapshotInput::note(
                        &record_name,
                        created.record_change_tag.as_deref().unwrap_or(""),
                        value,
                    ),
                )?;
            }
            record_epoch(target_dir, &record_name, &history_record_names(state, &record_name))?;
            Ok(ExecuteOutcome::ok(format!("Created {file}")))
        }
        Action::Rebase {
            record_name,
            file,
            local_text,
        } => {
            write_base_copy(target_dir, record_name, local_text)?;
            Ok(ExecuteOutcome::ok(format!("{file}: no server-side change needed")))
        }
        Action::Update {
            record_name,
            entry,
            zone,
            updates,
            note_text_updated,
            local_text,
            requested_title,
            modification_date_ms,
        } => {
            let results = db.update_records(zone, updates)?;
            if let Some(failed) = results.iter().find(|r| !r.is_ok())
                && let RecordUpdateResult::Rejected {
                    server_error_code,
                    reason,
                } = failed
            {
                let detail = reason
                    .as_deref()
                    .filter(|r| !r.is_empty())
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                let message = if server_error_code == "CONFLICT" {
                    format!(
                        "{}: rejected by the server as a conflicting change{detail} - run \"pull\" first",
                        entry.file
                    )
                } else {
                    format!(
                        "{}: server rejected the update: {server_error_code}{detail}",
                        entry.file
                    )
                };
                return Ok(ExecuteOutcome::failed(message));
            }
            if *note_text_updated
                && let Some(RecordUpdateResult::Ok(note)) = results.first()
                && note.record_name == *record_name
            {
                let mut updated = entry.clone();
                updated.record_change_tag = note.record_change_tag.clone().unwrap_or_default();
                updated.modification_date = match modification_date_of(note) {
                    0 => *modification_date_ms,
                    ms => ms,
                };
                if requested_title.is_some() {
                    updated.frontmatter_title = requested_title.clone();
                }
                state.notes.insert(record_name.clone(), updated);
                apply_note_file_times(&target_dir.join(&entry.file), note)?;
            }
            write_base_copy(target_dir, record_name, local_text)?;
            record_epoch(target_dir, record_name, &history_record_names(state, record_name))?;
            Ok(ExecuteOutcome::ok(format!("Pushed {}", entry.file)))
        }
    }
}

/// `runPush`.
pub fn run_push(
    target_dir: &Path,
    on_status: &mut dyn FnMut(&str),
    options: &PushOptions,
) -> Result<PushResult, Error> {
    run_push_with(&DefaultConnector, target_dir, on_status, options)
}

/// `runPush` over an explicit connector.
pub fn run_push_with(
    connector: &dyn Connector,
    target_dir: &Path,
    on_status: &mut dyn FnMut(&str),
    options: &PushOptions,
) -> Result<PushResult, Error> {
    let dry_run = options.dry_run;
    let BuildPushPlanResult {
        mut state,
        entries,
        notices,
        remote,
    } = build_push_plan(connector, target_dir, on_status)?;
    let serialized: Vec<SerializedPlanEntry> = entries.iter().map(|e| e.entry.serialize()).collect();
    let unchanged = count_unchanged_notes(&serialized, state.notes.len());

    if entries.is_empty() {
        return Ok(PushResult {
            dry_run,
            pushed: (!dry_run).then_some(0),
            entries: Vec::new(),
            unchanged,
            notices,
        });
    }
    if dry_run {
        return Ok(PushResult {
            dry_run,
            pushed: None,
            entries: serialized
                .into_iter()
                .map(|entry| PushEntryResult { entry, outcome: None })
                .collect(),
            unchanged,
            notices,
        });
    }

    let mut pushed = 0;
    let mut results = Vec::with_capacity(entries.len());
    let mut failed_folders: HashSet<String> = HashSet::new();
    for plan_entry in &entries {
        let serialized = plan_entry.entry.serialize();
        let (Some(action), Some(remote)) = (&plan_entry.action, &remote) else {
            results.push(PushEntryResult {
                entry: serialized,
                outcome: None,
            });
            continue;
        };
        let outcome = execute(action, &remote.db, target_dir, &mut state, &mut failed_folders)?;
        if plan_entry.entry.resolution == PlanResolution::Ready && outcome.succeeded {
            pushed += 1;
        }
        results.push(PushEntryResult {
            entry: serialized,
            outcome: Some(outcome),
        });
    }
    write_clone_state(target_dir, &state)?;
    Ok(PushResult {
        dry_run,
        pushed: Some(pushed),
        entries: results,
        unchanged,
        notices,
    })
}
