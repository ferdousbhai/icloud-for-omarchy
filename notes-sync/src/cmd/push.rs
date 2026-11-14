//! `push` (and the plan `status` shares). Originally derived from icloud-md.
//! Refusal strings live in `plan.rs`.
//!
//! Plan entries carry an [`Action`] that [`execute`] interprets.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

use super::plan::{
    PlanEntry, PlanEntryKind, PlanResolution, PrepareRefusal, Refusal, RetitleRefusal, SharedWriteRefusal,
    TextUpdateRefusal, count_unchanged_notes,
};
use super::remote::{Connector, DefaultConnector, Remote, resolve_folder_account};
use super::{Error, NoticeLevel, SyncNotice, is_in_trash, is_purged};
use crate::cloudkit::{
    CloudKitRecord, CreateExtras, Database, NoteZone, RecordOp, RecordUpdate, RecordUpdateResult, Transport,
    UpdateFieldValue, UpdateFields, note_zone,
};
use crate::diff3::{has_conflict_markers, merge_note_versions};
use crate::doc::attach::{MAX_NEW_ATTACHMENT_BYTES, image_dimensions};
use crate::doc::decode::{ClassifyOptions, DecodedNote, NoteDecodeResult, classify_note_record, is_deleted};
use crate::doc::document::{
    ApplyTextEditOptions, apply_text_edit, build_initial_note_document, compute_splices, delete_attachment_placeholder,
    encode_note_document, insert_attachment_placeholder, mark_attachment_placeholder, note_document_round_trips,
    parse_note_document, validate_document_invariants,
};
use crate::doc::embeds::{
    AttachmentReference, EmbedSlot, NewAttachmentLink, OBJECT_REPLACEMENT_CHARACTER, attachment_link_spellings,
    decode_note_embed_slots, has_attachment_link_to, has_attachment_reference, has_embed_marker,
    has_unknown_content_marker, plan_embed_representations, plan_new_attachment_links,
};
use crate::doc::encode::{
    DEFAULT_FOLDER_RECORD_NAME, NewAttachmentFields, TRASH_FOLDER_RECORD_NAME, build_attachment_create_fields,
    build_folder_create_fields, build_media_create_fields, build_note_create_fields, build_note_move_fields,
    build_note_trash_fields, build_note_update_fields,
};
use crate::doc::format::{FormatParagraph, decode_note_format, formats_round_trip_equal};
use crate::doc::proto::topotext::AttachmentInfo;
use crate::doc::reconcile::reconcile_note_format;
use crate::doc::table_edit::prepare_table_attachment_update;
use crate::doc::text::{compress_note_document, decode_note_body_text, decode_note_string, decompress_note_document};
use crate::js::{self, posix};
use crate::md::frontmatter::{
    NOTE_TITLE_KEY, frontmatter_readable, join_frontmatter, read_note_id, read_note_title, set_note_id,
    split_frontmatter,
};
use crate::md::parse::{ParsedNoteMarkdown, parse_note_markdown};
use crate::md::title::{
    carried_title_spelling, representability_problem, restore_title_paragraph_text, title_from_note_file_name,
    title_is_representable, title_paragraph_from_filename,
};
use crate::vault::attachments::{
    remove_attachments_for_note, remove_table_attachments_for_note, resolve_new_attachment_file, safe_unlink,
};
use crate::vault::base::{read_base_copy, remove_base_copy, write_base_copy};
use crate::vault::epoch::record_epoch;
use crate::vault::folders::{PlannedFolder, plan_folder_creates, relocate_note_attachments};
use crate::vault::history::{
    VersionSnapshotInput, history_record_names, record_version, recording_suppressed, without_recording,
};
use crate::vault::layout::{PreviousLayout, StateDirInfo, note_dir_of, state_dir_index};
use crate::vault::local::{
    LocalFileState, LocalNote, apply_note_file_times, local_file_state, modification_date_of, mtime_ms,
    read_local_note, read_text, split_options, write_note_file,
};
use crate::vault::migrate::{read_vault, require_vault};
use crate::vault::pairing::{UntrackedFile, pending_rename_target, resolve_note_ids, settle_pending_renames};
use crate::vault::rt;
use crate::vault::state::{
    AttachmentEntry, CloneState, FolderEntry, NoteEntry, TitleMode, TrashedEntry, write_clone_state,
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
    pub entry: PlanEntry,
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

impl PushResult {
    /// What each change that was tried and failed said: iCloud rejected it,
    /// or something it depends on failed. Such a push still reports its
    /// result, and exits 1.
    pub fn failures(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter_map(|e| e.outcome.as_ref())
            .filter(|o| !o.succeeded)
            .map(|o| o.message.as_str())
            .collect()
    }
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
fn shared_note_write_refusal(state: &CloneState, entry: &NoteEntry) -> Option<SharedWriteRefusal> {
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
    RenameFolder {
        rename: FolderRename,
        record: Box<CloudKitRecord>,
    },
    /// After its notes were trashed or moved out.
    DeleteFolder {
        record_name: String,
        dir: String,
        record: Option<Box<CloudKitRecord>>,
    },
    Move {
        record_name: String,
        entry: NoteEntry,
        to_file: String,
        folder_record_name: String,
        /// Into another folder: the record moves.
        relocated: bool,
        /// Into another directory (its attachment files follow).
        dir_changed: bool,
        record: Box<CloudKitRecord>,
        retitle: Option<Retitle>,
        /// The moved file's text when it was edited too: pushed right
        /// after the move, against the moved record.
        edit: Option<MoveEdit>,
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
        /// Files the note links to, created with it as its attachments.
        new_attachments: Vec<NewAttachment>,
    },
    /// A byte-level difference with nothing to send: re-sync the base copy
    /// (and, after merging a remote change, the file and the tag).
    Rebase {
        record_name: String,
        file: String,
        local_text: String,
        merged: Option<RemoteMerge>,
    },
    /// A remote change merged into the local edit that can't go up now
    /// (conflict markers, or the merged text is refused): write it locally.
    WriteMerge {
        record_name: String,
        entry: NoteEntry,
        merged: RemoteMerge,
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
        merged: Option<RemoteMerge>,
        /// Files newly linked from the note: uploaded and created as its
        /// attachments in the same request as `updates`.
        new_attachments: Vec<NewAttachment>,
        /// Attachments whose link is gone from the note: their records are
        /// marked deleted in the same request as `updates`.
        removed_attachments: Vec<RemovedAttachment>,
    },
}

/// A file attachment a push takes out of a note: its link is gone from the
/// note's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedAttachment {
    /// The attachment identifier the note's text named: the `Attachment`
    /// record's name, and its key in `state.attachments`.
    pub attachment_record_name: String,
    /// Vault-root-relative.
    pub file: String,
    pub name: String,
    /// Its `Attachment` and `Media` records as iCloud has them now (one
    /// that is gone or deleted already is left out).
    pub records: Vec<RemovedRecord>,
}

/// A record of a removed attachment, as looked up while planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedRecord {
    pub record_name: String,
    pub record_type: String,
    pub record_change_tag: String,
    pub parent_record_name: Option<String>,
}

/// What a push sends to iCloud to remove attachments from a note, beside
/// the note's own update (whose text no longer holds their placeholders):
/// an update of each attachment's `Attachment` record and of its `Media`
/// record that sets `Deleted` to 1 and touches no other field. That is what
/// Apple's own apps do (measured on a real account, 2026-11-13: neither
/// record is deleted outright, and both keep every other field;
/// docs/DESIGN.md, "Removing an attachment").
fn attachment_removal_ops(removed_attachments: &[RemovedAttachment]) -> Vec<RecordOp> {
    removed_attachments
        .iter()
        .flat_map(|removed| &removed.records)
        .map(|record| {
            let mut fields = UpdateFields::new();
            fields.insert("Deleted".into(), UpdateFieldValue::new(serde_json::json!(1)));
            RecordOp::Update(RecordUpdate {
                record_name: record.record_name.clone(),
                record_type: record.record_type.clone(),
                record_change_tag: record.record_change_tag.clone(),
                fields,
                parent_record_name: record.parent_record_name.clone(),
            })
        })
        .collect()
}

/// A file a push adds to a note as an attachment: what is planned of the
/// `Attachment` and `Media` records it becomes. Nothing is read or sent
/// until the plan executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttachment {
    /// The attachment identifier the note's text names: the `Attachment`
    /// record's name.
    pub attachment_record_name: String,
    pub media_record_name: String,
    /// Vault-root-relative.
    pub file: String,
    pub name: String,
    pub type_uti: String,
}

impl NewAttachment {
    fn reference(&self) -> AttachmentReference {
        AttachmentReference {
            attachment_identifier: self.attachment_record_name.clone(),
            type_uti: self.type_uti.clone(),
        }
    }
}

/// "`verb` attachment a" for one name, "`verb` attachments a, b" for
/// several, nothing for none.
fn attachments_phrase<'a>(verb: &str, names: impl Iterator<Item = &'a str>) -> Option<String> {
    let names: Vec<&str> = names.collect();
    match names.as_slice() {
        [] => None,
        [name] => Some(format!("{verb} attachment {name}")),
        names => Some(format!("{verb} attachments {}", names.join(", "))),
    }
}

/// What the plan entry of a note that gains attachments says of them.
fn new_attachments_remark(new_attachments: &[NewAttachment]) -> Option<String> {
    attachments_phrase("adds the", new_attachments.iter().map(|a| a.name.as_str()))
}

/// What the plan entry of a note that loses attachments says of them.
fn removed_attachments_remark(removed_attachments: &[RemovedAttachment]) -> Option<String> {
    attachments_phrase("removes the", removed_attachments.iter().map(|a| a.name.as_str()))
}

/// What a pushed entry's message says of the attachments it added and
/// removed (nothing when there were none).
fn attachment_changes_suffix(added: usize, removed: &[RemovedAttachment]) -> String {
    let added = match added {
        0 => String::new(),
        1 => ", with its new attachment".to_owned(),
        n => format!(", with its {n} new attachments"),
    };
    let removed = attachments_phrase(", without its", removed.iter().map(|a| a.name.as_str())).unwrap_or_default();
    added + &removed
}

fn join_remarks<const N: usize>(remarks: [Option<String>; N]) -> Option<String> {
    let remarks: Vec<String> = remarks.into_iter().flatten().collect();
    (!remarks.is_empty()).then(|| remarks.join("; "))
}

#[derive(Debug, Clone, PartialEq)]
pub struct MoveEdit {
    pub replica: [u8; 16],
}

/// A remote change diff3-merged into the local edit during planning; nothing
/// is written until the plan executes, so a preview changes no file.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMerge {
    /// The whole merged file (frontmatter kept).
    pub file_text: String,
    /// The merged body.
    pub text: String,
    /// The remote body: the base copy after a clean merge.
    pub remote_text: String,
    pub remote_tag: String,
    pub has_conflict: bool,
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
    dir_changed: bool,
    new_title: Option<String>,
    /// The moved file's body differs from the base copy.
    edited: bool,
}

struct CreateCandidate {
    file: String,
    local_text: String,
    /// `local_text` with a placeholder in place of each new attachment's
    /// link.
    document_text: String,
    new_attachments: Vec<NewAttachment>,
    folder_record_name: String,
    shared_zone_owner: Option<String>,
}

/// `listUntrackedMarkdownFiles`: untracked `.md` files anywhere in the vault
/// (dot-directories and `attachments/` skipped), sorted.
pub(crate) fn list_untracked_markdown_files(
    target_dir: &Path,
    notes: &IndexMap<String, NoteEntry>,
) -> Result<Vec<String>, Error> {
    Ok(untracked_files(walk_vault(target_dir)?.files, notes))
}

/// `files` without the tracked ones, sorted.
fn untracked_files(mut files: Vec<String>, notes: &IndexMap<String, NoteEntry>) -> Vec<String> {
    let tracked: HashSet<&str> = notes.values().map(|e| e.file.as_str()).collect();
    files.retain(|file| !tracked.contains(file.as_str()));
    files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    files
}

/// What [`walk_vault`] finds, relative to the vault's directory.
#[derive(Default)]
struct VaultWalk {
    /// Every directory that could be a folder's.
    dirs: HashSet<String>,
    /// `attachments/` directories that hold `.md` files (which are not
    /// notes there).
    attachments_with_notes: Vec<String>,
    /// `.md` files.
    files: Vec<String>,
    /// Syncthing conflict copies ([`is_sync_conflict_name`]), of any
    /// extension: never note files.
    conflicts: Vec<String>,
}

/// Whether `name` is a copy Syncthing made of a file two devices changed at
/// once: `<name>.sync-conflict-<YYYYMMDD>-<HHMMSS>-<device>.<ext>` (the
/// device is 7 letters or digits; the extension may be absent). Such a copy
/// of a note carries the note's id, so read as a note file it would be
/// pushed as a new note, or taken for the note itself once its file is gone.
pub(crate) fn is_sync_conflict_name(name: &str) -> bool {
    const MARKER: &str = ".sync-conflict-";
    name.match_indices(MARKER).any(|(at, _)| {
        let rest = &name.as_bytes()[at + MARKER.len()..];
        let digits = |range: std::ops::Range<usize>| rest[range].iter().all(u8::is_ascii_digit);
        rest.len() >= 23
            && digits(0..8)
            && rest[8] == b'-'
            && digits(9..15)
            && rest[15] == b'-'
            && rest[16..23].iter().all(u8::is_ascii_alphanumeric)
            && matches!(rest.get(23), None | Some(b'.'))
    })
}

/// The vault's directories and `.md` files, relative to `target_dir`, with
/// dot-entries and `attachments/` directories skipped, and Syncthing's
/// conflict copies kept apart.
fn walk_vault(target_dir: &Path) -> Result<VaultWalk, Error> {
    fn walk(target_dir: &Path, dir: &str, found: &mut VaultWalk) -> Result<(), Error> {
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
            let file_type = entry.file_type()?;
            let attachments = name.to_lowercase() == "attachments";
            let relative = if dir.is_empty() { name } else { format!("{dir}/{name}") };
            if attachments && entry.path().is_dir() {
                let holds_notes = std::fs::read_dir(entry.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|f| f.file_name().to_string_lossy().ends_with(".md"));
                if holds_notes {
                    found.attachments_with_notes.push(relative);
                }
            } else if file_type.is_dir() {
                walk(target_dir, &relative, found)?;
                found.dirs.insert(relative);
            } else if file_type.is_file() {
                if is_sync_conflict_name(posix::basename(&relative)) {
                    found.conflicts.push(relative);
                } else if relative.ends_with(".md") {
                    found.files.push(relative);
                }
            }
        }
        Ok(())
    }
    let mut found = VaultWalk::default();
    walk(target_dir, "", &mut found)?;
    Ok(found)
}

/// A folder directory renamed here (`from` gone, `to` new beside it).
#[derive(Debug, Clone, PartialEq)]
pub struct FolderRename {
    pub record_name: String,
    pub from: String,
    pub to: String,
}

/// Own folders whose directory is gone: renamed, when its notes all turn up
/// in one new directory beside it (or, emptied, when it and one new empty
/// directory are the only candidates there), else deleted. Renames come
/// shallowest first; deletes deepest first.
/// `move_pairs` are (record name, entry, file it moved to); `untracked_dirs`
/// the directories holding untracked note files.
pub fn plan_folder_dir_changes(
    dir_index: &IndexMap<String, StateDirInfo>,
    dirs_on_disk: &HashSet<String>,
    notes: &IndexMap<String, NoteEntry>,
    move_pairs: &[(String, NoteEntry, String)],
    untracked_dirs: &HashSet<String>,
) -> (Vec<FolderRename>, Vec<(String, String)>) {
    let mut own: Vec<(&String, &String)> = dir_index
        .iter()
        .filter_map(|(dir, info)| match info {
            StateDirInfo::Folder {
                folder_record_name,
                shared_zone_owner: None,
                ..
            } if folder_record_name != DEFAULT_FOLDER_RECORD_NAME && folder_record_name != TRASH_FOLDER_RECORD_NAME => {
                Some((dir, folder_record_name))
            }
            _ => None,
        })
        .collect();
    own.sort_by_key(|(dir, _)| dir.split('/').count());
    let mut new_dirs: HashSet<&String> = dirs_on_disk.iter().filter(|d| !dir_index.contains_key(*d)).collect();
    let parent_of = |dir: &str| posix::dirname(dir).trim_start_matches('.').to_owned();
    let has_notes = |dir: &str| untracked_dirs.contains(dir);
    let missing_empty =
        |dir: &&String| !dirs_on_disk.contains(*dir) && !notes.values().any(|n| note_dir_of(&n.file) == **dir);

    let mut renames: Vec<FolderRename> = Vec::new();
    let mut deletes: Vec<(String, String)> = Vec::new();
    for (dir, record_name) in &own {
        // Under a folder renamed here, it went along.
        let mapped = renames.iter().find_map(|r| {
            dir.strip_prefix(&format!("{}/", r.from))
                .map(|rest| format!("{}/{rest}", r.to))
        });
        if dirs_on_disk.contains(*dir) || mapped.as_ref().is_some_and(|m| dirs_on_disk.contains(m)) {
            continue;
        }
        let parent = parent_of(dir);
        let parent = renames
            .iter()
            .find_map(|r| {
                (parent == r.from).then(|| r.to.clone()).or_else(|| {
                    parent
                        .strip_prefix(&format!("{}/", r.from))
                        .map(|rest| format!("{}/{rest}", r.to))
                })
            })
            .unwrap_or(parent);
        let notes_here: Vec<&String> = notes
            .iter()
            .filter(|(_, n)| note_dir_of(&n.file) == **dir)
            .map(|(rn, _)| rn)
            .collect();
        let targets: HashSet<String> = move_pairs
            .iter()
            .filter(|(_, entry, _)| note_dir_of(&entry.file) == **dir)
            .map(|(_, _, to)| note_dir_of(to))
            .collect();
        let to = if notes_here.is_empty() {
            let gone = own
                .iter()
                .map(|(d, _)| d)
                .filter(|d| parent_of(d) == parent_of(dir) && missing_empty(d))
                .count();
            let fresh: Vec<&&String> = new_dirs
                .iter()
                .filter(|d| parent_of(d) == parent && !has_notes(d))
                .collect();
            (gone == 1 && fresh.len() == 1).then(|| (*fresh[0]).clone())
        } else if targets.len() == 1 {
            let to = targets.into_iter().next().unwrap_or_default();
            let all_moved_there = notes_here.iter().all(|rn| {
                move_pairs.iter().any(|(m, _, t)| m == *rn && note_dir_of(t) == to)
                    || !move_pairs.iter().any(|(m, _, _)| m == *rn)
            });
            (all_moved_there && new_dirs.contains(&to) && parent_of(&to) == parent).then_some(to)
        } else {
            None
        };
        match to {
            Some(to) => {
                new_dirs.retain(|d| **d != to);
                renames.push(FolderRename {
                    record_name: (*record_name).clone(),
                    from: (*dir).clone(),
                    to,
                });
            }
            None => deletes.push(((*dir).clone(), (*record_name).clone())),
        }
    }
    deletes.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.split('/').count()));
    (renames, deletes)
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
    // A preview (status, push --dry-run) writes nothing, a migration included:
    // it reads a layout 3 vault where it is.
    let mut state = if recording_suppressed() {
        read_vault(target_dir)?
    } else {
        require_vault(target_dir, on_status)?
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

    let VaultWalk {
        dirs: dirs_on_disk,
        attachments_with_notes: mut attachments_dirs,
        files,
        conflicts: mut conflict_copies,
    } = walk_vault(target_dir)?;
    // `attachments/` holds attachment files, never notes: say so rather than
    // skip notes put there in silence.
    attachments_dirs.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    for at in attachments_dirs {
        notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!(
                "{at}/ holds note files, but \"attachments\" is where attachment files live, so they're \
                 ignored - move them to a folder with another name"
            ),
        });
    }

    // A Syncthing conflict copy is never a note file: say so, so that it
    // gets merged by hand and deleted.
    conflict_copies.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    for file in conflict_copies {
        notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!(
                "{file} is a Syncthing conflict copy, so it is not synced - merge what you want from it into the \
                 note and delete it"
            ),
        });
    }

    let mut untracked: Vec<UntrackedNote> = Vec::new();
    for file in untracked_files(files, &state.notes) {
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
    let mut emptied: Vec<(String, NoteEntry)> = Vec::new();

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
            // Removing the title key asks for the title the file name spells;
            // YAML too broken to read says nothing about the key either way.
            let readable = frontmatter_readable(&frontmatter);
            if !readable && entry.frontmatter_title.is_some() {
                notices.push(SyncNotice {
                    level: NoticeLevel::Warn,
                    message: format!(
                        "{}: the frontmatter isn't valid YAML, so {NOTE_TITLE_KEY} can't be read - the note's \
                         title is left as it is",
                        entry.file
                    ),
                });
            }
            read_note_title(&frontmatter).or_else(|| {
                (readable && entry.frontmatter_title.is_some()).then(|| title_from_note_file_name(&entry.file))
            })
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
            // Notes discards a note left empty: so does emptying its file.
            emptied.push((record_name.clone(), entry.clone()));
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
                        // A file carrying another tracked note's id is that
                        // note's copy, whatever it is called.
                        && !u
                            .note_id
                            .as_deref()
                            .is_some_and(|id| id != record_name && state.notes.contains_key(id))
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
    let missing = missing_candidates.iter().filter(|(record_name, _)| {
        !move_pairs.iter().any(|(p, _, _)| p == record_name) && !ambiguous_record_names.contains(record_name)
    });
    // A file gone and a file emptied both ask for the note's deletion, and
    // neither can delete a note someone else shares.
    for (record_name, entry) in missing.chain(&emptied) {
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

    // --- folder directories renamed or deleted here
    let (folder_renames, folder_deletes) = plan_folder_dir_changes(
        &dir_index,
        &dirs_on_disk,
        &state.notes,
        &move_pairs,
        &untracked.iter().map(|u| note_dir_of(&u.file)).collect(),
    );
    // A renamed directory (and everything under it) answers for its folder.
    let mut dir_index = dir_index;
    for rename in &folder_renames {
        let aliases: Vec<(String, StateDirInfo)> = dir_index
            .iter()
            .filter_map(|(dir, info)| {
                let rest = if *dir == rename.from {
                    Some("")
                } else {
                    dir.strip_prefix(&format!("{}/", rename.from))
                        .map(|_| &dir[rename.from.len()..])
                }?;
                Some((format!("{}{rest}", rename.to), info.clone()))
            })
            .collect();
        dir_index.extend(aliases);
    }

    // --- folders the account doesn't have yet: every new directory, empty
    // or not, as a folder made in Notes
    let wanted: Vec<String> = move_pairs
        .iter()
        .map(|(_, _, to)| note_dir_of(to))
        .chain(untracked.iter().map(|u| note_dir_of(&u.file)))
        .chain(dirs_on_disk.iter().cloned())
        .collect();
    let folder_plan = plan_folder_creates(wanted.iter().map(String::as_str), &dir_index, &mut rt::random_uuid);
    let folder_refusals: HashMap<String, super::plan::FolderRefusal> = folder_plan
        .refusals
        .iter()
        .map(|r| (r.dir_path.clone(), r.reason.clone()))
        .collect();
    let resolve_dir = |dir: &str| -> Option<StateDirInfo> {
        // Every note lives in a folder: one at the top level of the vault
        // goes in the default one, where Notes puts a note made outside any.
        if dir.is_empty() {
            return Some(StateDirInfo::Folder {
                folder_record_name: DEFAULT_FOLDER_RECORD_NAME.to_owned(),
                shared_zone_owner: None,
                permission: None,
            });
        }
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
        let dir_changed = to_dir != note_dir_of(&entry.file);
        let info = resolve_dir(&to_dir);
        let from_folder =
            resolve_dir(&note_dir_of(&entry.file)).and_then(|i| i.folder_record_name().map(str::to_owned));
        let relocated = dir_changed && info.as_ref().and_then(|i| i.folder_record_name()) != from_folder.as_deref();
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
        let edited = match untracked.iter().find(|u| u.file == *to_file) {
            Some(u) => read_base_copy(target_dir, record_name)?.as_deref() != Some(u.local_text.as_str()),
            None => false,
        };
        let previous_title = title_from_note_file_name(&entry.file);
        let new_title = title_expressed_by_file(to_file, &recorded_titles);
        let retitled = title_mode == TitleMode::Filename && new_title != previous_title;
        ready_moves.push(ReadyMove {
            record_name: record_name.clone(),
            entry: entry.clone(),
            to_file: to_file.clone(),
            folder_record_name: info.folder_record_name().unwrap_or_default().to_owned(),
            relocated,
            dir_changed,
            new_title: retitled.then_some(new_title),
            edited,
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
        // A tracked id here means that note's own file is still present:
        // this file is a copy, and a copy is a new note (the create gives it
        // its own id).
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
        // A link to a file in the folder's attachments/ is a file to attach.
        let (document_text, new_attachments) = if has_attachment_reference(local_text) {
            let planned = match plan_new_attachment_links(local_text) {
                Ok(plan) => resolve_new_attachments(
                    target_dir,
                    file,
                    info.shared_zone_owner().is_some(),
                    &TrackedAttachments::of(&state, "", file),
                    &plan.new_links,
                )?
                .map(|new_attachments| (plan.reconstructed_body_text, new_attachments)),
                Err(reason) => Err(reason),
            };
            match planned {
                Ok(planned) => planned,
                Err(reason) => {
                    entries.push(refuse(Refusal::CreateNewAttachment { reason }));
                    continue;
                }
            }
        } else {
            (local_text.clone(), Vec::new())
        };
        create_candidates.push(CreateCandidate {
            file: file.clone(),
            local_text: local_text.clone(),
            document_text,
            new_attachments,
            folder_record_name: info.folder_record_name().unwrap_or_default().to_owned(),
            shared_zone_owner: info.shared_zone_owner().map(str::to_owned),
        });
    }

    if update_candidates.is_empty()
        && delete_candidates.is_empty()
        && create_candidates.is_empty()
        && ready_moves.is_empty()
        && folder_plan.folders.is_empty()
        && folder_renames.is_empty()
        && folder_deletes.is_empty()
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
    let folder_entries: Vec<ExecutablePlanEntry> = folder_plan
        .folders
        .iter()
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
    for record_name in folder_renames
        .iter()
        .map(|r| &r.record_name)
        .chain(folder_deletes.iter().map(|(_, rn)| rn))
    {
        lookup_groups.entry(None).or_default().push(record_name.clone());
    }
    let mut records_by_name: HashMap<String, CloudKitRecord> = HashMap::new();
    // Records iCloud answered an error for (other than "not found"): it
    // didn't say they are gone.
    let mut lookup_failures: HashMap<String, String> = HashMap::new();
    for (owner, names) in &lookup_groups {
        let (records, failures) = db.lookup_records_and_failures(&note_zone(owner.as_deref()), names)?;
        for record in records {
            records_by_name.insert(record.record_name.clone(), record);
        }
        lookup_failures.extend(failures);
    }

    // A record iCloud answered an error for is not one it said is gone.
    let gone_or_lookup_failed = |record_name: &str, gone: Refusal| match lookup_failures.get(record_name) {
        Some(code) => Refusal::LookupFailed {
            server_error_code: code.clone(),
        },
        None => gone,
    };

    let replica_id = match &state.replica_id {
        Some(id) => id.clone(),
        None => js::base64_encode(&rt::random_bytes(16)),
    };
    state.replica_id = Some(replica_id.clone());
    let replica_bytes: [u8; 16] = js::base64_decode(&replica_id).try_into().map_err(|_| {
        Error::CorruptStateFile("state.json has a malformed replicaId (expected 16 bytes, base64-encoded)".into())
    })?;

    // --- folder renames, ahead of the notes moving with them
    for rename in &folder_renames {
        let mut e = PlanEntry::new(PlanEntryKind::RenameFolder, rename.to.clone(), PlanResolution::Ready);
        e.previous_file = Some(rename.from.clone());
        match records_by_name.get(&rename.record_name).filter(|r| !r.is_deleted()) {
            Some(record) => entries.push(ExecutablePlanEntry::with(
                e,
                Action::RenameFolder {
                    rename: rename.clone(),
                    record: Box::new(record.clone()),
                },
            )),
            None => entries.push(
                PlanEntry::refused(
                    PlanEntryKind::RenameFolder,
                    rename.to.clone(),
                    gone_or_lookup_failed(&rename.record_name, Refusal::FolderGoneRemotely),
                )
                .into(),
            ),
        }
    }

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
                entries.push(refuse(gone_or_lookup_failed(&m.record_name, Refusal::MoveGoneRemotely)));
                continue;
            }
        };
        if record.record_change_tag.clone().unwrap_or_default() != m.entry.record_change_tag {
            entries.push(refuse(Refusal::MoveChangedRemotely));
            continue;
        }
        let mut retitle = None;
        if let Some(new_title) = &m.new_title {
            match prepare_retitle(record, &m.to_file, new_title, &replica_bytes, title_mode)? {
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
                dir_changed: m.dir_changed,
                record: Box::new(record.clone()),
                retitle,
                edit: m.edited.then_some(MoveEdit { replica: replica_bytes }),
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
            // Not known to be gone: untracking it now would leave the note
            // in iCloud with nothing here to delete it by.
            None if lookup_failures.contains_key(record_name) => {
                entries.push(
                    PlanEntry::refused(
                        PlanEntryKind::Delete,
                        entry.file.clone(),
                        Refusal::DeleteLookupFailed {
                            server_error_code: lookup_failures[record_name].clone(),
                        },
                    )
                    .into(),
                );
                continue;
            }
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
        // Deleting the file deletes the note, as deleting it in Notes would,
        // even when another device changed it since the last pull: the trash
        // is written against the live record, and Recently Deleted keeps that
        // change. (icloud-md refuses, and its pull then writes the file back.)
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
        let built = build_create_payload(
            &c.file,
            &c.document_text,
            &c.new_attachments,
            title_mode,
            &recorded_titles,
            &replica_bytes,
        );
        let (payload_base64, plain_text) = match built {
            Ok(built) => built,
            Err(refusal) => {
                entries.push(refuse(refusal));
                continue;
            }
        };
        let modification_date_ms = mtime_ms(&target_dir.join(&c.file))?;
        let mut e = PlanEntry::new(PlanEntryKind::Create, c.file.clone(), PlanResolution::Ready);
        e.remark = new_attachments_remark(&c.new_attachments);
        entries.push(ExecutablePlanEntry::with(
            e,
            Action::Create {
                file: c.file.clone(),
                local_text: c.local_text.clone(),
                payload_base64,
                plain_text,
                folder_record_name: c.folder_record_name.clone(),
                shared_zone_owner: c.shared_zone_owner.clone(),
                modification_date_ms,
                new_attachments: c.new_attachments.clone(),
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
            entries.push(refuse(gone_or_lookup_failed(
                &c.record_name,
                Refusal::UpdateGoneRemotely,
            )));
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

        // Changed remotely since the last pull: merge the remote text into the
        // local edit, and push a clean merge straight away against the live
        // record, as Notes would apply both edits.
        let mut merged: Option<RemoteMerge> = None;
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
            let merge = merge_remote_change(
                target_dir,
                &c.record_name,
                &c.frontmatter,
                &c.local_text,
                &remote_text,
                &remote_tag,
            )?;
            if merge.has_conflict {
                entries.push(ExecutablePlanEntry::with(
                    PlanEntry::refused(PlanEntryKind::Update, file.clone(), Refusal::MergedWithConflicts),
                    Action::WriteMerge {
                        record_name: c.record_name.clone(),
                        entry: c.entry.clone(),
                        merged: merge,
                    },
                ));
                continue;
            }
            merged = Some(merge);
        }
        let local_text = merged.as_ref().map_or(c.local_text.as_str(), |m| m.text.as_str());

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
        let tracked = TrackedAttachments::of(&state, &c.record_name, &c.entry.file);
        let prepared = prepare_update(
            db,
            &zone,
            target_dir,
            record,
            &c.entry,
            local_text,
            &tracked,
            &replica_bytes,
            modification_date_ms,
            title_mode,
            c.requested_title.as_deref(),
        )?;
        let prepared = match prepared {
            Ok(p) => p,
            Err(refusal) => {
                let refused = PlanEntry::refused(PlanEntryKind::Update, file.clone(), Refusal::UpdatePrepare(refusal));
                // The remote change still lands in the file.
                entries.push(match merged {
                    Some(merged) => ExecutablePlanEntry::with(
                        refused,
                        Action::WriteMerge {
                            record_name: c.record_name.clone(),
                            entry: c.entry.clone(),
                            merged,
                        },
                    ),
                    None => refused.into(),
                });
                continue;
            }
        };
        if prepared.updates.is_empty() {
            entries.push(ExecutablePlanEntry::with(
                PlanEntry::new(PlanEntryKind::Update, file.clone(), PlanResolution::Noop),
                Action::Rebase {
                    record_name: c.record_name.clone(),
                    file: file.clone(),
                    local_text: local_text.to_owned(),
                    merged,
                },
            ));
            continue;
        }
        let mut e = PlanEntry::new(PlanEntryKind::Update, file.clone(), PlanResolution::Ready);
        e.remark = join_remarks([
            prepared.retitled_to.as_deref().map(retitle_remark),
            new_attachments_remark(&prepared.new_attachments),
            removed_attachments_remark(&prepared.removed_attachments),
        ]);
        entries.push(ExecutablePlanEntry::with(
            e,
            Action::Update {
                record_name: c.record_name.clone(),
                entry: c.entry.clone(),
                zone,
                updates: prepared.updates,
                note_text_updated: prepared.note_text_updated,
                local_text: local_text.to_owned(),
                requested_title: c.requested_title.clone(),
                modification_date_ms,
                merged,
                new_attachments: prepared.new_attachments,
                removed_attachments: prepared.removed_attachments,
            },
        ));
    }

    // --- folders deleted here, once their notes are gone. Not one still
    // holding a note or folder this vault doesn't track: deleting it would
    // orphan that. Another device may have put one there since the last
    // pull, and pull never tracks locked or unreadable notes, so a folder of
    // only those looks empty here. (One walk of the whole private zone, only
    // when deleting.)
    let mut kept_folders: HashSet<String> = HashSet::new();
    let mut unsyncable_in: HashMap<String, usize> = HashMap::new();
    if !folder_deletes.is_empty() {
        match state.sync_token.as_deref() {
            Some(_) => {
                let zone = db.fetch_all_note_records(None, &mut |_| {})?;
                // Only what this vault doesn't track: tracked notes there are
                // being trashed or moved by this push.
                let known = |rn: &String| {
                    state.notes.contains_key(rn) || state.folders.as_ref().is_some_and(|f| f.contains_key(rn))
                };
                for record in zone
                    .records
                    .iter()
                    .filter(|r| !r.is_deleted() && !is_in_trash(r) && !known(&r.record_name))
                {
                    let field = if record.record_type == "Folder" {
                        "ParentFolder"
                    } else {
                        "Folder"
                    };
                    let Some(folder) =
                        crate::vault::layout::reference_record_name(record.fields.get(field).map(|f| &f.value))
                    else {
                        continue;
                    };
                    let syncable = match record.record_type.as_str() {
                        "Folder" => true,
                        "Note" => match classify_note_record(record, &ClassifyOptions { title_mode }) {
                            NoteDecodeResult::Deleted => continue,
                            NoteDecodeResult::Ok(_) => true,
                            NoteDecodeResult::Unsyncable(_) => false,
                        },
                        _ => false,
                    };
                    if syncable {
                        kept_folders.insert(folder);
                    } else {
                        *unsyncable_in.entry(folder).or_default() += 1;
                    }
                }
            }
            None => kept_folders.extend(folder_deletes.iter().map(|(_, rn)| rn.clone())),
        }
    }
    for (dir, record_name) in &folder_deletes {
        if kept_folders.contains(record_name) {
            entries.push(
                PlanEntry::refused(PlanEntryKind::DeleteFolder, dir.clone(), Refusal::FolderChangedRemotely).into(),
            );
            continue;
        }
        // Untracking a folder iCloud didn't say is gone would leave it there.
        if let Some(code) = lookup_failures.get(record_name) {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::DeleteFolder,
                    dir.clone(),
                    Refusal::LookupFailed {
                        server_error_code: code.clone(),
                    },
                )
                .into(),
            );
            continue;
        }
        if let Some(&count) = unsyncable_in.get(record_name) {
            entries.push(
                PlanEntry::refused(
                    PlanEntryKind::DeleteFolder,
                    dir.clone(),
                    Refusal::FolderHoldsUnsyncableNotes { count },
                )
                .into(),
            );
            continue;
        }
        entries.push(ExecutablePlanEntry::with(
            PlanEntry::new(PlanEntryKind::DeleteFolder, dir.clone(), PlanResolution::Ready),
            Action::DeleteFolder {
                record_name: record_name.clone(),
                dir: dir.clone(),
                record: records_by_name
                    .get(record_name)
                    .filter(|r| !r.is_deleted())
                    .map(|r| Box::new(r.clone())),
            },
        ));
    }

    // A preview (status, push --dry-run) writes nothing; a real push writes
    // the state after executing anyway.
    if planning_mutated_state && !recording_suppressed() {
        write_clone_state(target_dir, &state)?;
    }
    Ok(BuildPushPlanResult {
        state,
        entries,
        notices,
        remote: Some(remote),
    })
}

/// The create path's document build and verification:
/// `Ok((payloadBase64, plainText))` or the refusal.
fn build_create_payload(
    file: &str,
    local_text: &str,
    new_attachments: &[NewAttachment],
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
    // Each new attachment's placeholder, in the order of their links.
    let expected_slots: Vec<EmbedSlot> = new_attachments
        .iter()
        .map(|a| EmbedSlot::Attachment(a.reference()))
        .collect();
    if !new_attachments.is_empty() {
        let placeholders = placeholder_offsets(&desired.text);
        if placeholders.len() != new_attachments.len() {
            return Err(Refusal::CreateVerificationFailed);
        }
        for (attachment, offset) in new_attachments.iter().zip(placeholders) {
            mark_attachment_placeholder(&mut doc, offset, attachment_info(&attachment.reference())).map_err(thrown)?;
        }
    }
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
            .is_ok_and(|f| formats_round_trip_equal(f, &desired.paragraphs))
        && (new_attachments.is_empty()
            || crate::doc::embeds::embed_slots_of(&rebuilt).is_some_and(|slots| slots == expected_slots));
    if !verified {
        return Err(Refusal::CreateVerificationFailed);
    }
    Ok((js::base64_encode(&compressed), desired.text))
}

/// diff3 the remote text into the local edit, in memory.
pub fn merge_remote_change(
    target_dir: &Path,
    record_name: &str,
    frontmatter: &str,
    local_text: &str,
    remote_text: &str,
    remote_tag: &str,
) -> Result<RemoteMerge, Error> {
    let base = read_base_copy(target_dir, record_name)?.unwrap_or_default();
    let outcome = merge_note_versions(&base, local_text, remote_text);
    Ok(RemoteMerge {
        file_text: join_frontmatter(frontmatter, &outcome.text),
        text: outcome.text,
        remote_text: remote_text.to_owned(),
        remote_tag: remote_tag.to_owned(),
        has_conflict: outcome.has_conflict,
    })
}

/// Writes a merge that isn't going up: the merged file, the remote tag, and
/// the remote text as the base copy unless it conflicted (so the local half
/// stays a pending edit).
pub fn apply_remote_merge(
    target_dir: &Path,
    state: &mut CloneState,
    record_name: &str,
    entry: &NoteEntry,
    merged: &RemoteMerge,
) -> Result<(), Error> {
    write_note_file(&target_dir.join(&entry.file), &merged.file_text)?;
    if !merged.has_conflict {
        write_base_copy(target_dir, record_name, &merged.remote_text)?;
    }
    let mut updated = entry.clone();
    updated.record_change_tag = merged.remote_tag.clone();
    state.notes.insert(record_name.to_owned(), updated);
    Ok(())
}

/// `PreparedCandidate`.
#[derive(Debug, Clone, PartialEq)]
struct PreparedCandidate {
    pub updates: Vec<RecordUpdate>,
    note_text_updated: bool,
    retitled_to: Option<String>,
    /// Files to add as attachments along with `updates`.
    new_attachments: Vec<NewAttachment>,
    /// Attachments to remove along with `updates`.
    removed_attachments: Vec<RemovedAttachment>,
}

impl PreparedCandidate {
    fn unchanged() -> Self {
        PreparedCandidate {
            updates: Vec::new(),
            note_text_updated: false,
            retitled_to: None,
            new_attachments: Vec::new(),
            removed_attachments: Vec::new(),
        }
    }
}

/// The attachment files this vault tracks, as a note being pushed sees them.
#[derive(Debug, Default)]
struct TrackedAttachments {
    /// The note's own file attachments, each with its link path from the
    /// note's directory (what pull wrote into the note for it).
    links: HashMap<String, String>,
    /// The note's own file attachments as state tracks them.
    own: HashMap<String, AttachmentEntry>,
    /// Every tracked attachment file, of any note (vault-root-relative).
    files: HashSet<String>,
}

impl TrackedAttachments {
    fn of(state: &CloneState, record_name: &str, note_file: &str) -> Self {
        let note_dir = note_dir_of(note_file);
        let all = || state.attachments.iter().flatten();
        let own: HashMap<String, AttachmentEntry> = all()
            .filter(|(_, att)| att.note_record_name == record_name)
            .map(|(k, att)| (k.clone(), att.clone()))
            .collect();
        TrackedAttachments {
            links: own
                .iter()
                .map(|(k, att)| (k.clone(), posix::relative(&note_dir, &att.file)))
                .collect(),
            own,
            files: all().map(|(_, att)| att.file.clone()).collect(),
        }
    }
}

/// The UTF-16 offsets of the U+FFFC placeholders in `text`, in order.
fn placeholder_offsets(text: &str) -> Vec<usize> {
    text.encode_utf16()
        .enumerate()
        .filter(|(_, unit)| *unit == OBJECT_REPLACEMENT_CHARACTER as u16)
        .map(|(offset, _)| offset)
        .collect()
}

/// The placeholder of an attachment an edit removes from a note.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemovedPlaceholder {
    /// UTF-16 offset in the note's text as it is before the edit.
    offset: usize,
    attachment_identifier: String,
}

/// What a new attachment's placeholder carries in the note's text.
fn attachment_info(reference: &AttachmentReference) -> AttachmentInfo {
    AttachmentInfo {
        attachment_identifier: Some(reference.attachment_identifier.clone()),
        type_uti: Some(reference.type_uti.clone()),
        ..Default::default()
    }
}

/// The files the note's new attachment links name, each with the record
/// names it will be created under, or why one can't be added. The file has
/// to be in the note folder's own `attachments/` (see
/// [`resolve_new_attachment_file`]), attached to no note yet, and linked
/// the way pull writes an attachment's link, so that the link stands for
/// the attachment from then on.
fn resolve_new_attachments(
    target_dir: &Path,
    note_file: &str,
    shared: bool,
    tracked: &TrackedAttachments,
    links: &[NewAttachmentLink],
) -> Result<Result<Vec<NewAttachment>, String>, Error> {
    if links.is_empty() {
        return Ok(Ok(Vec::new()));
    }
    if shared {
        return Ok(Err(
            "attachments can't be added to a shared note from here - add them in Notes itself".into(),
        ));
    }
    let note_dir = note_dir_of(note_file);
    let mut new_attachments: Vec<NewAttachment> = Vec::new();
    for link in links {
        let file = match resolve_new_attachment_file(target_dir, &note_dir, &link.href)? {
            Ok(file) => file,
            Err(reason) => return Ok(Err(reason)),
        };
        if tracked.files.contains(&file.file) {
            let whose = if tracked.links.values().any(|path| *path == file.link_path) {
                "this note"
            } else {
                "another note"
            };
            return Ok(Err(format!(
                "\"{}\" is already an attachment of {whose} - to attach the file again, copy it under another name",
                file.link_path
            )));
        }
        if new_attachments.iter().any(|a| a.file == file.file) {
            return Ok(Err(format!(
                "\"{}\" is linked more than once - a file is attached once",
                file.link_path
            )));
        }
        let mut attachment = NewAttachment {
            attachment_record_name: String::new(),
            media_record_name: String::new(),
            file: file.file,
            name: file.name,
            type_uti: file.type_uti.to_owned(),
        };
        let spellings = attachment_link_spellings(&attachment.reference(), &file.link_path);
        if !spellings.contains(&link.text) {
            return Ok(Err(format!(
                "the link to \"{}\" isn't written the way this tool writes an attachment's link - write it as {}",
                file.link_path, spellings[0]
            )));
        }
        // Record names as Notes makes them: uppercase UUIDs.
        attachment.attachment_record_name = rt::random_uuid().to_uppercase();
        attachment.media_record_name = rt::random_uuid().to_uppercase();
        new_attachments.push(attachment);
    }
    Ok(Ok(new_attachments))
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
    tracked: &TrackedAttachments,
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
    // A note with embeds, or one linking to a file to attach.
    if !classified.embed_slots.is_empty() || has_attachment_reference(local_text) {
        return prepare_embed_candidate(
            db,
            zone,
            target_dir,
            record,
            &classified,
            entry,
            local_text,
            tracked,
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
        &[],
        &[],
        replica,
        &file,
    )? {
        Ok(t) => t,
        Err(r) => return Ok(Err(PrepareRefusal::TextUpdate(r))),
    };
    let Some(payload) = text_update else {
        return Ok(Ok(PreparedCandidate::unchanged()));
    };
    let fields = build_note_update_fields(record, &payload, &desired.text, modification_date_ms);
    Ok(Ok(PreparedCandidate {
        updates: vec![note_record_update(record, entry, fields)],
        note_text_updated: true,
        retitled_to: requested_retitle(&classified, requested_title),
        new_attachments: Vec::new(),
        removed_attachments: Vec::new(),
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
    tracked: &TrackedAttachments,
    replica: &[u8; 16],
    modification_date_ms: i64,
    requested_title: Option<&str>,
) -> Result<Result<PreparedCandidate, PrepareRefusal>, Error> {
    let file = entry.file.clone();
    let plan = match plan_embed_representations(local_text, &classified.embed_slots, &tracked.links) {
        Ok(p) => p,
        Err(reason) => return Ok(Err(PrepareRefusal::EmbedPlan { reason, file })),
    };
    let new_attachments = match resolve_new_attachments(
        target_dir,
        &file,
        entry.shared_zone_owner.is_some(),
        tracked,
        &plan.new_links,
    )? {
        Ok(new_attachments) => new_attachments,
        Err(reason) => return Ok(Err(PrepareRefusal::NewAttachment { reason, file })),
    };
    // The attachments whose link is gone: each one's placeholder in the
    // note's text as it is now, and what state tracks of it.
    let current_placeholders = placeholder_offsets(&classified.body_text);
    let mut removed_placeholders: Vec<RemovedPlaceholder> = Vec::new();
    let mut removed_attachments: Vec<RemovedAttachment> = Vec::new();
    for &slot in &plan.removed_slots {
        let (Some(EmbedSlot::Attachment(reference)), Some(&offset)) =
            (classified.embed_slots.get(slot), current_placeholders.get(slot))
        else {
            return Ok(Err(PrepareRefusal::TextUpdate(TextUpdateRefusal::RebuiltEmbedFailed)));
        };
        let Some(tracked_entry) = tracked.own.get(&reference.attachment_identifier) else {
            return Ok(Err(PrepareRefusal::TextUpdate(TextUpdateRefusal::RebuiltEmbedFailed)));
        };
        removed_placeholders.push(RemovedPlaceholder {
            offset,
            attachment_identifier: reference.attachment_identifier.clone(),
        });
        removed_attachments.push(RemovedAttachment {
            attachment_record_name: reference.attachment_identifier.clone(),
            file: tracked_entry.file.clone(),
            name: posix::basename(&tracked_entry.file).to_owned(),
            records: Vec::new(),
        });
    }
    if !removed_attachments.is_empty() && entry.shared_zone_owner.is_some() {
        return Ok(Err(PrepareRefusal::RemovedAttachment {
            reason: "attachments can't be removed from a shared note from here - remove them in Notes itself".into(),
            file,
        }));
    }
    // The placeholders the pushed text has to hold, in order: the note's
    // embeds that stay, as they are, and each new attachment where its link
    // is.
    let mut expected_slots: Vec<EmbedSlot> = classified
        .embed_slots
        .iter()
        .enumerate()
        .filter(|(slot, _)| !plan.removed_slots.contains(slot))
        .map(|(_, slot)| slot.clone())
        .collect();
    let mut new_slots: Vec<usize> = Vec::new();
    for (link, attachment) in plan.new_links.iter().zip(&new_attachments) {
        let at = link.slot_index.min(expected_slots.len());
        expected_slots.insert(at, EmbedSlot::Attachment(attachment.reference()));
        new_slots.push(at);
    }
    let parsed = match parse_note_markdown(&plan.reconstructed_body_text) {
        Ok(p) => p,
        Err(r) => return Ok(Err(PrepareRefusal::EmbedMarkdown { reason: r.reason, file })),
    };
    let desired = match restore_stripped_title(classified, parsed, requested_title) {
        Ok(d) => d,
        Err(r) => return Ok(Err(r)),
    };

    // One lookup: the tables to edit, and the records of the attachments to
    // remove (their change tags go with the removal).
    let mut names: Vec<String> = plan
        .tables
        .iter()
        .map(|t| t.reference.attachment_identifier.clone())
        .collect();
    for removed in &removed_attachments {
        names.push(removed.attachment_record_name.clone());
        if let Some(tracked_entry) = tracked.own.get(&removed.attachment_record_name)
            && !tracked_entry.media_record_name.is_empty()
        {
            names.push(tracked_entry.media_record_name.clone());
        }
    }
    let attachment_records = if names.is_empty() {
        Vec::new()
    } else {
        let (records, failures) = db.lookup_records_and_failures(zone, &names)?;
        // Not known to be gone: neither skipped as removed already, nor
        // reported as a table that no longer exists.
        if let Some((_, code)) = failures.first() {
            return Ok(Err(PrepareRefusal::LookupFailed {
                server_error_code: code.clone(),
            }));
        }
        records
    };
    for removed in &mut removed_attachments {
        let media = tracked
            .own
            .get(&removed.attachment_record_name)
            .map(|tracked_entry| tracked_entry.media_record_name.as_str())
            .unwrap_or_default();
        for (record_name, record_type) in [
            (removed.attachment_record_name.as_str(), "Attachment"),
            (media, "Media"),
        ] {
            // Gone or deleted already (another device removed it too):
            // nothing to send for it.
            let Some(found) = attachment_records
                .iter()
                .rev()
                .find(|r| r.record_name == record_name)
                .filter(|r| !is_deleted(r))
            else {
                continue;
            };
            let names_another_note = record_type == "Attachment"
                && found
                    .fields
                    .get("Note")
                    .and_then(|f| f.value.get("recordName"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|note| note != record.record_name);
            if found.record_type != record_type || names_another_note {
                return Ok(Err(PrepareRefusal::RemovedAttachment {
                    reason: format!(
                        "the attachment \"{}\" can't be removed from here: its record in iCloud is not this note's \
                         file attachment - run \"pull\" first, or put the link back as it was",
                        removed.name
                    ),
                    file,
                }));
            }
            removed.records.push(RemovedRecord {
                record_name: found.record_name.clone(),
                record_type: record_type.to_owned(),
                record_change_tag: found.record_change_tag.clone().unwrap_or_default(),
                parent_record_name: found.parent_record_name.clone(),
            });
        }
    }
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
            &expected_slots,
            &new_slots,
            &removed_placeholders,
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
    if !(new_attachments.is_empty() && removed_attachments.is_empty()) && !note_text_updated {
        // A placeholder added or removed always changes the text.
        return Ok(Err(PrepareRefusal::TextUpdate(TextUpdateRefusal::RebuiltEmbedFailed)));
    }
    if updates.is_empty() {
        return Ok(Ok(PreparedCandidate::unchanged()));
    }
    Ok(Ok(PreparedCandidate {
        updates,
        note_text_updated,
        retitled_to,
        new_attachments,
        removed_attachments,
    }))
}

/// `prepareRetitle`: the note-text payload carrying a renamed file's new
/// title (`Ok(None)`: the title doesn't actually change).
pub fn prepare_retitle(
    record: &CloudKitRecord,
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
    let (title, body) = match format.split_first() {
        Some((title, body)) => (Some(title), body),
        None => (None, &[][..]),
    };
    if let Some(title) = title
        && (title.text == new_title || carried_title_spelling(&title.text) == new_title)
    {
        return Ok(Ok(None));
    }
    let desired = restore_title_paragraph_text(&title_paragraph_from_filename(new_title), body);
    match prepare_note_text_update(
        record,
        &classified.body_text,
        &desired,
        &classified.embed_slots,
        &[],
        &[],
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
        // The live record's: after merging a remote change it is newer than
        // the tracked one.
        record_change_tag: record
            .record_change_tag
            .clone()
            .unwrap_or_else(|| entry.record_change_tag.clone()),
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
///
/// `expected_slots` are the embeds `desired.text` holds a placeholder for,
/// in order; those at the indexes in `new_slots` (ascending) are new
/// attachments, whose placeholders the note doesn't have yet. The text is
/// edited as if they weren't there, so that edit may neither add nor remove
/// a placeholder, and each is then inserted at its place carrying its
/// attachment: the placeholders the note has keep their characters, and
/// the rebuilt document must decode to exactly `expected_slots`.
///
/// `removed` are the placeholders of the attachments the edit removes
/// (ascending offsets in `current_body_text`). Each is deleted first, by
/// itself, and the text edit is then made against what is left under the
/// same rule, so no other placeholder can be taken for a removed one.
#[allow(clippy::too_many_arguments)]
fn prepare_note_text_update(
    record: &CloudKitRecord,
    current_body_text: &str,
    desired: &NoteText,
    expected_slots: &[EmbedSlot],
    new_slots: &[usize],
    removed: &[RemovedPlaceholder],
    replica: &[u8; 16],
    file: &str,
) -> Result<Result<Option<String>, TextUpdateRefusal>, Error> {
    let Some(text_b64) = record.fields.get("TextDataEncrypted").and_then(|f| f.value.as_str()) else {
        return Ok(Err(TextUpdateRefusal::NoTextData));
    };
    let Ok(raw) = decompress_note_document(&js::base64_decode(text_b64)) else {
        return Ok(Err(TextUpdateRefusal::NoTextData));
    };
    if !note_document_round_trips(&raw) {
        return Ok(Err(TextUpdateRefusal::NotRoundTrip));
    }
    let placeholders = placeholder_offsets(&desired.text);
    if !new_slots.is_empty() && placeholders.len() != expected_slots.len() {
        return Ok(Err(TextUpdateRefusal::RebuiltEmbedFailed));
    }
    // The text before the new attachments' placeholders go in.
    let edited_text: String = if new_slots.is_empty() {
        desired.text.clone()
    } else {
        let skipped: HashSet<usize> = new_slots
            .iter()
            .filter_map(|&slot| placeholders.get(slot).copied())
            .collect();
        let units: Vec<u16> = desired
            .text
            .encode_utf16()
            .enumerate()
            .filter(|(offset, _)| !skipped.contains(offset))
            .map(|(_, unit)| unit)
            .collect();
        js::from_utf16(&units)
    };
    // The note's text once the removed attachments' placeholders are out.
    let remaining_text: String = if removed.is_empty() {
        current_body_text.to_owned()
    } else {
        let units: Vec<u16> = current_body_text
            .encode_utf16()
            .enumerate()
            .filter(|(offset, _)| !removed.iter().any(|r| r.offset == *offset))
            .map(|(_, unit)| unit)
            .collect();
        js::from_utf16(&units)
    };
    let touches_placeholder = compute_splices(&remaining_text, &edited_text).iter().any(|splice| {
        js::slice16(&remaining_text, splice.start, splice.start + splice.delete_length)
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
    let edit_options = ApplyTextEditOptions { replica_id: *replica };
    // Last first: the offsets before a deleted one stay as they are.
    for placeholder in removed.iter().rev() {
        if let Err(e) = delete_attachment_placeholder(
            &mut doc,
            placeholder.offset,
            &placeholder.attachment_identifier,
            &edit_options,
        ) {
            return Ok(Err(exception(e)));
        }
    }
    if doc.text != remaining_text {
        return Ok(Err(TextUpdateRefusal::RebuiltDecodeFailed));
    }
    let mut text_changed = match apply_text_edit(&mut doc, &edited_text, &edit_options) {
        Ok(changed) => changed || !removed.is_empty(),
        Err(e) => return Ok(Err(exception(e))),
    };
    for &slot in new_slots {
        let (Some(EmbedSlot::Attachment(reference)), Some(&offset)) =
            (expected_slots.get(slot), placeholders.get(slot))
        else {
            return Ok(Err(TextUpdateRefusal::RebuiltEmbedFailed));
        };
        if let Err(e) = insert_attachment_placeholder(&mut doc, offset, attachment_info(reference), &edit_options) {
            return Ok(Err(exception(e)));
        }
        text_changed = true;
    }
    if doc.text != desired.text {
        return Ok(Err(TextUpdateRefusal::RebuiltDecodeFailed));
    }
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
    Ok(Ok(Some(js::base64_encode(&compressed))))
}

// --- execution --------------------------------------------------------------------

/// A note deleted locally: drop tracking; delete a clean file.
fn apply_local_note_deletion(
    target_dir: &Path,
    record_name: &str,
    entry: &NoteEntry,
    state: &mut CloneState,
) -> Result<LocalFileState, Error> {
    let mut attachments = state.attachments.take().unwrap_or_default();
    let mut table_attachments = state.table_attachments.take().unwrap_or_default();
    let local = local_file_state(target_dir, entry, record_name, state.mode())?;
    let path = target_dir.join(&entry.file);
    let emptied = || -> Result<bool, Error> {
        Ok(read_text(&path)?.is_some_and(|text| split_frontmatter(&text, split_options(state.mode())).body.is_empty()))
    };
    if local == LocalFileState::Clean || (local == LocalFileState::Modified && emptied()?) {
        safe_unlink(&path)?;
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

/// Records a note moved to Recently Deleted in the trash registry.
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

/// The rejection to report of a `records/modify` answer: in an atomic
/// request every record is refused when one is, the others with
/// `ATOMIC_ERROR`, so the one that says why comes first.
fn first_rejection(results: &[RecordUpdateResult]) -> Option<&RecordUpdateResult> {
    let rejected = |r: &&RecordUpdateResult| !r.is_ok();
    let cause = |r: &&RecordUpdateResult| matches!(r, RecordUpdateResult::Rejected { server_error_code, .. } if server_error_code != "ATOMIC_ERROR");
    results.iter().find(cause).or_else(|| results.iter().find(rejected))
}

/// What a write that carries new attachments did.
struct SentWrite {
    /// One result per record sent: the note's first, as in `updates`.
    results: Vec<RecordUpdateResult>,
    /// The state entry of each attachment created, by `Attachment` record
    /// name: to track once every result is ok.
    attachments: Vec<(String, AttachmentEntry)>,
}

/// Sends a note's write (`note_ops`: its update or create first, then any
/// table updates) together with the attachments it removes
/// ([`attachment_removal_ops`]) and its new attachments. Each new file is read and
/// uploaded for the `Asset` field of the `Media` record it will be
/// (CloudKit's asset upload, which names the record the asset is for), and
/// then one atomic `records/modify` carries the note, and a `Media` and an
/// `Attachment` record per file, after the removals: all of it is applied,
/// or none.
///
/// `Ok(Err(why))` when a file can no longer be read as planned: nothing was
/// sent for the note. An upload that fails is an error like any other
/// request's; iCloud keeps nothing of an upload no record names.
fn send_with_attachment_changes<T: Transport>(
    db: &Database<T>,
    zone: &NoteZone,
    target_dir: &Path,
    note_record_name: &str,
    note_ops: Vec<RecordOp>,
    new_attachments: &[NewAttachment],
    removed_attachments: &[RemovedAttachment],
) -> Result<Result<SentWrite, String>, Error> {
    let mut ops = note_ops;
    ops.extend(attachment_removal_ops(removed_attachments));
    let mut attachments = Vec::with_capacity(new_attachments.len());
    let now_ms = rt::now_ms();
    for attachment in new_attachments {
        let path = target_dir.join(&attachment.file);
        let link = format!("attachments/{}", attachment.name);
        // As planned: still a regular file of a size this tool uploads.
        let regular = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
        let bytes = match std::fs::read(&path) {
            Ok(bytes) if regular && !bytes.is_empty() && bytes.len() as u64 <= MAX_NEW_ATTACHMENT_BYTES => bytes,
            Ok(_) => return Ok(Err(format!("{link} changed while it was being pushed"))),
            Err(e) => return Ok(Err(format!("{link} couldn't be read: {e}"))),
        };
        let asset = db.upload_asset(zone, "Media", &attachment.media_record_name, "Asset", &bytes)?;
        let checksum = asset
            .get("fileChecksum")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let parent = CreateExtras {
            parent_record_name: Some(note_record_name.to_owned()),
            create_short_guid: false,
        };
        ops.push(RecordOp::Create {
            record_name: attachment.media_record_name.clone(),
            record_type: "Media".into(),
            fields: build_media_create_fields(asset, &attachment.name),
            extras: parent.clone(),
        });
        ops.push(RecordOp::Create {
            record_name: attachment.attachment_record_name.clone(),
            record_type: "Attachment".into(),
            fields: build_attachment_create_fields(&NewAttachmentFields {
                type_uti: &attachment.type_uti,
                file_name: &attachment.name,
                media_record_name: &attachment.media_record_name,
                note_record_name,
                file_size: bytes.len() as u64,
                now_ms,
                dimensions: image_dimensions(&bytes),
            }),
            extras: parent,
        });
        attachments.push((
            attachment.attachment_record_name.clone(),
            AttachmentEntry {
                file: attachment.file.clone(),
                media_record_name: attachment.media_record_name.clone(),
                media_file_checksum: checksum,
                note_record_name: note_record_name.to_owned(),
            },
        ));
    }
    let results = db.modify_records_atomically(zone, &ops)?;
    Ok(Ok(SentWrite { results, attachments }))
}

/// Sends a note's updates (its own first, then any table updates): by
/// themselves, or with the attachments the note gains and loses
/// ([`send_with_attachment_changes`]).
fn send_updates<T: Transport>(
    db: &Database<T>,
    zone: &NoteZone,
    target_dir: &Path,
    note_record_name: &str,
    updates: &[RecordUpdate],
    new_attachments: &[NewAttachment],
    removed_attachments: &[RemovedAttachment],
) -> Result<Result<SentWrite, String>, Error> {
    if new_attachments.is_empty() && removed_attachments.is_empty() {
        return Ok(Ok(SentWrite {
            results: db.update_records(zone, updates)?,
            attachments: Vec::new(),
        }));
    }
    let ops = updates.iter().cloned().map(RecordOp::Update).collect();
    send_with_attachment_changes(
        db,
        zone,
        target_dir,
        note_record_name,
        ops,
        new_attachments,
        removed_attachments,
    )
}

/// Tracks the attachments a write created: from here on each one's link
/// stands for it, and its file is the download pull would have made.
fn track_new_attachments(state: &mut CloneState, attachments: Vec<(String, AttachmentEntry)>) {
    if !attachments.is_empty() {
        state.attachments.get_or_insert_with(IndexMap::new).extend(attachments);
    }
}

/// After a write that removed attachments went through: stops tracking
/// each, and deletes its file as pull does when an attachment disappears
/// remotely (`safe_unlink`), unless the file is still someone's: tracked
/// for another attachment, or linked from another note of the folder
/// (`attachments/` is shared by the notes of a folder).
fn forget_removed_attachments(
    target_dir: &Path,
    state: &mut CloneState,
    note_file: &str,
    removed_attachments: &[RemovedAttachment],
) -> Result<(), Error> {
    for removed in removed_attachments {
        let Some(attachments) = state.attachments.as_mut() else {
            return Ok(());
        };
        attachments.shift_remove(&removed.attachment_record_name);
        if attachments.values().any(|other| other.file == removed.file) {
            continue;
        }
        if !linked_from_another_note(target_dir, note_file, &removed.file)? {
            safe_unlink(&target_dir.join(&removed.file))?;
        }
    }
    Ok(())
}

/// Whether a note file beside `note_file` (in its directory) links to the
/// attachment file `file` (vault-root-relative).
fn linked_from_another_note(target_dir: &Path, note_file: &str, file: &str) -> Result<bool, Error> {
    let note_dir = note_dir_of(note_file);
    let link_path = posix::relative(&note_dir, file);
    let entries = match std::fs::read_dir(target_dir.join(&note_dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_lowercase().ends_with(".md") || name == posix::basename(note_file) || !entry.path().is_file() {
            continue;
        }
        // A file that can't be read as text links to nothing.
        if let Ok(text) = std::fs::read_to_string(entry.path())
            && has_attachment_link_to(&text, &link_path)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Sends every trash and every single-record update of the plan together,
/// one `records/modify` per zone (non-atomic, so each note stands alone),
/// when there are several: their results, by record name, for `execute`.
fn batch_writes<T: Transport>(
    entries: &[ExecutablePlanEntry],
    db: &Database<T>,
) -> Result<HashMap<String, Vec<RecordUpdateResult>>, Error> {
    let mut by_zone: IndexMap<NoteZone, Vec<RecordUpdate>> = IndexMap::new();
    for action in entries.iter().filter_map(|e| e.action.as_ref()) {
        match action {
            Action::Delete {
                record_name, record, ..
            } => {
                by_zone
                    .entry(note_zone(None))
                    .or_default()
                    .push(trash_update(record_name, record));
            }
            Action::Update {
                zone,
                updates,
                new_attachments,
                removed_attachments,
                ..
            } if updates.len() == 1 && new_attachments.is_empty() && removed_attachments.is_empty() => {
                by_zone.entry(zone.clone()).or_default().push(updates[0].clone());
            }
            _ => {}
        }
    }
    let mut results = HashMap::new();
    for (zone, updates) in by_zone.into_iter().filter(|(_, u)| u.len() > 1) {
        let answers = db.update_records_independently(&zone, &updates)?;
        for (update, answer) in updates.into_iter().zip(answers) {
            results.insert(update.record_name, vec![answer]);
        }
    }
    Ok(results)
}

fn trash_update(record_name: &str, record: &CloudKitRecord) -> RecordUpdate {
    RecordUpdate {
        record_name: record_name.to_owned(),
        record_type: "Note".into(),
        record_change_tag: record.record_change_tag.clone().unwrap_or_default(),
        fields: build_note_trash_fields(record, rt::now_ms()),
        parent_record_name: record.parent_record_name.clone(),
    }
}

/// Runs one entry's action - icloud-md's `execute()`.
fn execute<T: Transport>(
    action: &Action,
    db: &Database<T>,
    target_dir: &Path,
    state: &mut CloneState,
    failed_folders: &mut HashSet<String>,
    batched: &mut HashMap<String, Vec<RecordUpdateResult>>,
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
                },
            );
            Ok(ExecuteOutcome::ok(format!("Created folder {}/", folder.dir_path)))
        }
        Action::RenameFolder { rename, record } => {
            let title = posix::basename(&rename.to).to_owned();
            let mut fields = UpdateFields::new();
            fields.insert(
                "TitleEncrypted".into(),
                UpdateFieldValue::new(serde_json::json!(js::base64_encode(title.as_bytes()))),
            );
            let result = db.update_records(
                &private,
                &[RecordUpdate {
                    record_name: rename.record_name.clone(),
                    record_type: "Folder".into(),
                    record_change_tag: record.record_change_tag.clone().unwrap_or_default(),
                    fields,
                    parent_record_name: record.parent_record_name.clone(),
                }],
            )?;
            if let Some(failure) = result.iter().find_map(rejection) {
                failed_folders.insert(rename.record_name.clone());
                return Ok(ExecuteOutcome::failed(format!(
                    "{}/: server rejected the rename: {failure}",
                    rename.from
                )));
            }
            if let Some(folder) = state.folders.as_mut().and_then(|f| f.get_mut(&rename.record_name)) {
                folder.name = title;
                folder.dir_name = posix::basename(&rename.to).to_owned();
            }
            Ok(ExecuteOutcome::ok(format!(
                "Renamed folder {}/ -> {}/",
                rename.from, rename.to
            )))
        }
        Action::DeleteFolder {
            record_name,
            dir,
            record,
        } => {
            let still_holds = state
                .notes
                .values()
                .filter(|n| n.folder_record_name.as_deref() == Some(record_name.as_str()))
                .count()
                + state
                    .folders
                    .iter()
                    .flat_map(|folders| folders.values())
                    .filter(|f| f.parent_record_name.as_deref() == Some(record_name.as_str()))
                    .count();
            if still_holds > 0 {
                return Ok(ExecuteOutcome::failed(format!(
                    "{dir}/: kept - {still_holds} note(s) or folder(s) in it couldn't be removed"
                )));
            }
            if let Some(record) = record
                && let Some(failure) =
                    db.delete_record(&private, record_name, record.record_change_tag.as_deref().unwrap_or(""))?
            {
                return Ok(ExecuteOutcome::failed(format!(
                    "{dir}/: server rejected the delete: {failure}"
                )));
            }
            if let Some(folders) = state.folders.as_mut() {
                folders.shift_remove(record_name);
            }
            Ok(ExecuteOutcome::ok(format!("Deleted folder {dir}/")))
        }
        Action::Move {
            record_name,
            entry,
            to_file,
            folder_record_name,
            relocated,
            dir_changed,
            record,
            retitle,
            edit,
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
            // Renamed within its folder (and not retitled): nothing changes in
            // iCloud, only where the file is.
            if *relocated {
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
            state.notes.insert(record_name.clone(), updated.clone());
            apply_note_file_times(&target_dir.join(to_file), &current)?;
            if *dir_changed && let Some(attachments) = state.attachments.as_mut() {
                relocate_note_attachments(target_dir, record_name, to_file, attachments)?;
            }
            let what = match (retitle.is_some(), *relocated) {
                (false, _) => "Moved",
                (true, true) => "Moved and retitled",
                (true, false) => "Retitled",
            };
            let moved = format!("{what} {} -> {to_file}", entry.file);
            let Some(edit) = edit else {
                return Ok(ExecuteOutcome::ok(moved));
            };
            // Read again: relocating attachments may have renamed links.
            let text = read_text(&target_dir.join(to_file))?.unwrap_or_default();
            let local_text = split_frontmatter(&text, split_options(state.mode())).body;
            // The edit made along with the move goes up now, not a sync later.
            let tracked = TrackedAttachments::of(state, record_name, to_file);
            let prepared = prepare_update(
                db,
                &private,
                target_dir,
                &current,
                &updated,
                &local_text,
                &tracked,
                &edit.replica,
                mtime_ms(&target_dir.join(to_file))?,
                state.mode(),
                None,
            )?;
            let prepared = match prepared {
                Ok(p) => p,
                Err(refusal) => {
                    return Ok(ExecuteOutcome::ok(format!(
                        "{moved} (its edits weren't pushed: {})",
                        Refusal::UpdatePrepare(refusal).reason()
                    )));
                }
            };
            if !prepared.updates.is_empty() {
                let sent = match send_updates(
                    db,
                    &private,
                    target_dir,
                    record_name,
                    &prepared.updates,
                    &prepared.new_attachments,
                    &prepared.removed_attachments,
                )? {
                    Ok(sent) => sent,
                    Err(failure) => {
                        return Ok(ExecuteOutcome::ok(format!(
                            "{moved} (its edits weren't pushed: {failure})"
                        )));
                    }
                };
                let results = sent.results;
                if let Some(failure) = first_rejection(&results).and_then(rejection) {
                    return Ok(ExecuteOutcome::ok(format!(
                        "{moved} (its edits weren't pushed: {failure})"
                    )));
                }
                track_new_attachments(state, sent.attachments);
                forget_removed_attachments(target_dir, state, to_file, &prepared.removed_attachments)?;
                if let Some(RecordUpdateResult::Ok(note)) = results.first()
                    && note.record_name == *record_name
                {
                    updated.record_change_tag = note.record_change_tag.clone().unwrap_or_default();
                    if let ms @ 1.. = modification_date_of(note) {
                        updated.modification_date = ms;
                    }
                    apply_note_file_times(&target_dir.join(to_file), note)?;
                    state.notes.insert(record_name.clone(), updated);
                }
            }
            write_base_copy(target_dir, record_name, &local_text)?;
            record_epoch(target_dir, record_name, &history_record_names(state, record_name))?;
            Ok(ExecuteOutcome::ok(format!("{moved}, with its edits")))
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
            let result = match batched.remove(record_name).and_then(|mut r| r.pop()) {
                Some(result) => result,
                None => db.update_note_record(&private, &trash_update(record_name, record))?,
            };
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
            new_attachments,
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
            let (result, attachments) = if new_attachments.is_empty() {
                (
                    db.create_note_record(&zone, &record_name, &fields, &extras)?,
                    Vec::new(),
                )
            } else {
                // The note and its attachments are created together, or not
                // at all.
                let create = RecordOp::Create {
                    record_name: record_name.clone(),
                    record_type: "Note".into(),
                    fields,
                    extras,
                };
                let sent = match send_with_attachment_changes(
                    db,
                    &zone,
                    target_dir,
                    &record_name,
                    vec![create],
                    new_attachments,
                    &[],
                )? {
                    Ok(sent) => sent,
                    Err(failure) => return Ok(ExecuteOutcome::failed(format!("{file}: {failure}"))),
                };
                match first_rejection(&sent.results) {
                    Some(rejected) => (rejected.clone(), Vec::new()),
                    None => match sent.results.into_iter().next() {
                        Some(note) => (note, sent.attachments),
                        None => return Ok(ExecuteOutcome::failed(format!("{file}: the server gave no answer"))),
                    },
                }
            };
            let created = match result {
                RecordUpdateResult::Ok(r) => *r,
                rejected => {
                    return Ok(ExecuteOutcome::failed(format!(
                        "{file}: server rejected the create: {}",
                        rejection(&rejected).unwrap_or_default()
                    )));
                }
            };
            track_new_attachments(state, attachments);
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
                    ..Default::default()
                },
            );
            write_base_copy(target_dir, &record_name, local_text)?;
            let path = target_dir.join(file);
            let text = read_text(&path)?.unwrap_or_default();
            let envelope = split_frontmatter(&text, split_options(state.mode()));
            write_note_file(
                &path,
                &join_frontmatter(&set_note_id(&envelope.frontmatter, &record_name), &envelope.body),
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
            Ok(ExecuteOutcome::ok(match new_attachments.len() {
                0 => format!("Created {file}"),
                1 => format!("Created {file}, with its attachment"),
                n => format!("Created {file}, with its {n} attachments"),
            }))
        }
        Action::WriteMerge {
            record_name,
            entry,
            merged,
        } => {
            apply_remote_merge(target_dir, state, record_name, entry, merged)?;
            Ok(ExecuteOutcome::ok(format!(
                "{}: merged the remote change into your edit",
                entry.file
            )))
        }
        Action::Rebase {
            record_name,
            file,
            local_text,
            merged,
        } => {
            if let Some(merged) = merged {
                write_note_file(&target_dir.join(file), &merged.file_text)?;
                if let Some(entry) = state.notes.get_mut(record_name) {
                    entry.record_change_tag = merged.remote_tag.clone();
                }
            }
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
            merged,
            new_attachments,
            removed_attachments,
        } => {
            let sent = match batched.remove(record_name) {
                Some(results) => SentWrite {
                    results,
                    attachments: Vec::new(),
                },
                None => match send_updates(
                    db,
                    zone,
                    target_dir,
                    record_name,
                    updates,
                    new_attachments,
                    removed_attachments,
                )? {
                    Ok(sent) => sent,
                    Err(failure) => return Ok(ExecuteOutcome::failed(format!("{}: {failure}", entry.file))),
                },
            };
            let results = sent.results;
            if let Some(failed) = first_rejection(&results)
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
            track_new_attachments(state, sent.attachments);
            forget_removed_attachments(target_dir, state, &entry.file, removed_attachments)?;
            if let Some(merged) = merged {
                write_note_file(&target_dir.join(&entry.file), &merged.file_text)?;
                if let Some(tracked) = state.notes.get_mut(record_name) {
                    tracked.record_change_tag = merged.remote_tag.clone();
                }
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
            Ok(ExecuteOutcome::ok(format!(
                "Pushed {}{}",
                entry.file,
                attachment_changes_suffix(new_attachments.len(), removed_attachments)
            )))
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
    } = if dry_run {
        without_recording(|| build_push_plan(connector, target_dir, on_status))?
    } else {
        build_push_plan(connector, target_dir, on_status)?
    };
    let serialized: Vec<PlanEntry> = entries.iter().map(|e| e.entry.clone()).collect();
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
    let mut batched = match &remote {
        Some(remote) => batch_writes(&entries, &remote.db)?,
        None => HashMap::new(),
    };
    for plan_entry in &entries {
        let serialized = plan_entry.entry.clone();
        let (Some(action), Some(remote)) = (&plan_entry.action, &remote) else {
            results.push(PushEntryResult {
                entry: serialized,
                outcome: None,
            });
            continue;
        };
        let outcome = match execute(
            action,
            &remote.db,
            target_dir,
            &mut state,
            &mut failed_folders,
            &mut batched,
        ) {
            Ok(outcome) => outcome,
            Err(e) => {
                // What the entries before this one did is in iCloud and in
                // the vault already (a created note's file carries its new
                // id): record it, or the next push creates those notes again.
                let _ = write_clone_state(target_dir, &state);
                return Err(e);
            }
        };
        // Saved entry by entry (an unchanged state is not rewritten): a run
        // that is killed never gets to the write below, and a note it
        // created would be created again by the next push.
        write_clone_state(target_dir, &state)?;
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
