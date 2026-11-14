//! Commands, their output and errors. Originally derived from icloud-md.

pub mod clone;
pub mod diff;
pub mod errors;
pub mod history;
pub mod lock;
pub mod output;
pub mod plan;
pub mod pull;
pub mod push;
pub mod remote;
pub mod report;
pub mod restore;
pub mod status;
pub mod sync;
pub mod vault_info;

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub use errors::Error;

use crate::cloudkit::{CloudKitRecord, NoteZone, SharedZoneChanges, SkippedSharedZone, note_zone};
use crate::doc::encode::TRASH_FOLDER_RECORD_NAME;
use crate::vault::layout::SharedZoneRecords;

/// `SyncNotice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncNotice {
    pub level: NoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Warn,
}

/// `SyncProgress`: progress callbacks for clone/pull (a machine mode renders them
/// as `icloud-notes:progress:...` lines on stderr).
pub trait SyncProgress {
    fn on_fetch_page(&mut self, records_so_far: usize) {
        let _ = records_so_far;
    }
    fn on_process_start(&mut self, total_records: usize) {
        let _ = total_records;
    }
    fn on_record_processed(&mut self) {}
    fn on_process_complete(&mut self) {}
}

/// A `SyncProgress` that ignores everything.
pub struct NoProgress;
impl SyncProgress for NoProgress {}

/// `usedNamesFor`: the per-directory used-names set, created on first use.
pub(crate) fn used_names_for<'a>(
    by_dir: &'a mut HashMap<String, HashSet<String>>,
    dir: &str,
) -> &'a mut HashSet<String> {
    by_dir.entry(dir.to_owned()).or_default()
}

/// The zone a note of `shared_zone_owner` lives in (`source.sharedZoneOwner
/// ? {zoneName, ownerRecordName} : PRIVATE_NOTES_ZONE`).
pub(crate) fn zone_for_owner(shared_zone_owner: Option<&str>) -> NoteZone {
    note_zone(shared_zone_owner.filter(|o| !o.is_empty()))
}

/// Each fetched shared zone's sync token and records, keyed by its owner
/// (a zone with no owner has neither).
pub(crate) fn shared_zone_tokens_and_records(
    zones: &[SharedZoneChanges],
) -> (IndexMap<String, String>, Vec<SharedZoneRecords>) {
    let mut tokens = IndexMap::new();
    let mut records = Vec::new();
    for zone in zones {
        let Some(owner) = zone.zone_id.owner_record_name.as_ref().filter(|o| !o.is_empty()) else {
            continue;
        };
        if let Some(token) = zone.sync_token.as_ref().filter(|t| !t.is_empty()) {
            tokens.insert(owner.clone(), token.clone());
        }
        records.push(SharedZoneRecords {
            owner_record_name: owner.clone(),
            records: zone.records.clone(),
        });
    }
    (tokens, records)
}

/// The warning for a note record clone or pull leaves alone: "Skipped the
/// note <recordName> (shared by <owner>): `why`".
pub(crate) fn skipped_note_notice(record_name: &str, shared_zone_owner: Option<&str>, why: &str) -> SyncNotice {
    let shared_by = shared_zone_owner.map(|o| format!(" shared by {o}")).unwrap_or_default();
    SyncNotice {
        level: NoticeLevel::Warn,
        message: format!("Skipped the note {record_name:?}{shared_by}: {why} - nothing was written for it"),
    }
}

/// The warning for a note clone or pull leaves as it is this run because
/// iCloud answered an error (`failure`: the record, the error's code) when
/// asked for one of its attachment records
/// (`AttachmentRecords::lookup_failure`). `note` is its file, or its title
/// when it has none yet.
pub(crate) fn attachment_lookup_notice(note: &str, failure: &(String, String)) -> SyncNotice {
    let (record_name, code) = failure;
    SyncNotice {
        level: NoticeLevel::Warn,
        message: format!(
            "{note}: iCloud answered {code} when asked for its attachment record {record_name} - the note and its \
             attachments were left as they are this run, and the next pull tries again"
        ),
    }
}

/// [`skipped_note_notice`]'s `why` for a record name that can't name a file.
pub(crate) const UNSAFE_RECORD_NAME: &str = "its record name isn't one plain file name";

/// [`skipped_note_notice`]'s `why` for a record name another zone's note
/// already has here.
pub(crate) const OTHER_ZONES_RECORD_NAME: &str = "a note from another zone is already tracked under its record name";

/// The warning for a shared zone the fetch skipped. What that meant for the
/// run is the caller's to say: `revoked` ends the sentence for a zone the
/// server no longer has, `untouched` the one for a zone whose notes came
/// without their text (`when`: " this run", or nothing).
pub(crate) fn skipped_zone_notice(
    skipped: &SkippedSharedZone,
    revoked: &str,
    when: &str,
    untouched: &str,
) -> SyncNotice {
    let owner = skipped_zone_owner(skipped).unwrap_or("unknown");
    let message = match skipped {
        SkippedSharedZone::ZoneNotFound { server_error_code, .. } => format!(
            "Skipped a shared zone the server no longer has (owner {owner}, {server_error_code}) - its share was \
             likely revoked or deleted; {revoked}"
        ),
        SkippedSharedZone::MissingNoteBodies {
            missing_record_names, ..
        } => format!(
            "Skipped the shared zone owned by {owner}{when}: {} note(s) came through without their text and looking \
             them up didn't fill it in - {untouched}",
            missing_record_names.len()
        ),
    };
    SyncNotice {
        level: NoticeLevel::Warn,
        message,
    }
}

/// A skipped shared zone's owner.
pub(crate) fn skipped_zone_owner(skipped: &SkippedSharedZone) -> Option<&str> {
    match skipped {
        SkippedSharedZone::ZoneNotFound { zone_id, .. } | SkippedSharedZone::MissingNoteBodies { zone_id, .. } => {
            zone_id.owner_record_name.as_deref()
        }
    }
}

/// Once one download fails the rest are not started. The notes that queued
/// downloads (`downloading`) forget their version, so the next pull applies
/// them again, and their attachments forget their checksum, so each file is
/// fetched again, under the name it has.
pub(crate) fn redo_downloads(
    notes: &mut IndexMap<String, crate::vault::state::NoteEntry>,
    attachments: &mut IndexMap<String, crate::vault::state::AttachmentEntry>,
    downloading: &[String],
) {
    for record_name in downloading {
        for attachment in attachments.values_mut().filter(|a| a.note_record_name == *record_name) {
            attachment.media_file_checksum.clear();
        }
        if let Some(entry) = notes.get_mut(record_name) {
            entry.record_change_tag.clear();
        }
    }
}

/// The untracked note files of the whole vault by the id each carries
/// (vault-relative paths), listed the first time a run needs them.
pub(crate) type LeftAnywhere = Option<HashMap<String, Vec<String>>>;

/// The untracked note files of the vault by the id each carries, listed
/// into `left` the first time a run asks.
pub(crate) fn left_files<'a>(
    target_dir: &std::path::Path,
    left: &'a mut LeftAnywhere,
    notes: &IndexMap<String, crate::vault::state::NoteEntry>,
    title_mode: crate::vault::state::TitleMode,
) -> Result<&'a mut HashMap<String, Vec<String>>, Error> {
    if left.is_none() {
        let mut by_id: HashMap<String, Vec<String>> = HashMap::new();
        for file in push::list_untracked_markdown_files(target_dir, notes)? {
            if let Some(id) = crate::vault::pairing::note_id_at(target_dir, &file, title_mode)? {
                by_id.entry(id).or_default().push(file);
            }
        }
        *left = Some(by_id);
    }
    Ok(left.get_or_insert_with(HashMap::new))
}

/// Whether `text`, a file that carries the id of `record_name`, a note
/// nothing tracks, is what a run that never got to record the note wrote
/// for it, untouched since. A clone or pull writes a new note's base copy
/// before its file, and nothing else leaves a base copy for a note no
/// longer tracked (untracking removes it), so a body that reads as the base
/// copy, under no frontmatter but the id and `apple-note-title` such a run
/// writes, is that run's. A file edited since (frontmatter included), or put
/// back from a backup after the note was deleted from here, reads otherwise.
pub(crate) fn left_untouched(
    target_dir: &std::path::Path,
    record_name: &str,
    text: &str,
    title_mode: crate::vault::state::TitleMode,
) -> Result<bool, Error> {
    if !crate::vault::state::is_plain_record_name(record_name) {
        return Ok(false);
    }
    let envelope = crate::md::frontmatter::split_frontmatter(text, crate::vault::local::split_options(title_mode));
    let frontmatter =
        crate::md::frontmatter::clear_note_title(&crate::md::frontmatter::clear_note_id(&envelope.frontmatter));
    if !frontmatter.is_empty() {
        return Ok(false);
    }
    Ok(crate::vault::base::read_base_copy(target_dir, record_name)?.as_deref() == Some(envelope.body.as_str()))
}

/// Removes what a run that never recorded the note `record_name` wrote for
/// it, before this run writes the note's file: every untracked file that
/// carries its id and is untouched since ([`left_untouched`]), or reads
/// exactly as `file_text`, what this run writes for it. Wherever such a file
/// is (the note moved to another folder, or its folder renamed, on another
/// device since) and however the note changed since, its text is in
/// iCloud. Left there, it would carry the id of a note tracked elsewhere: a
/// copy, which the next push creates as a second note. Called before the
/// note's base copy is written, which it reads. Returns the files removed.
pub(crate) fn remove_left_files(
    target_dir: &std::path::Path,
    left: &mut LeftAnywhere,
    notes: &IndexMap<String, crate::vault::state::NoteEntry>,
    record_name: &str,
    file_text: &str,
    title_mode: crate::vault::state::TitleMode,
) -> Result<Vec<String>, Error> {
    let Some(files) = left_files(target_dir, left, notes, title_mode)?.get_mut(record_name) else {
        return Ok(Vec::new());
    };
    let tracked: HashSet<&str> = notes.values().map(|e| e.file.as_str()).collect();
    let mut removed = Vec::new();
    for file in std::mem::take(files) {
        let path = target_dir.join(&file);
        let text = crate::vault::local::read_text(&path)?;
        let untouched = match &text {
            Some(text) if !tracked.contains(file.as_str()) => {
                text == file_text || left_untouched(target_dir, record_name, text, title_mode)?
            }
            _ => false,
        };
        if untouched {
            crate::vault::attachments::safe_unlink(&path)?;
            removed.push(file);
        } else {
            files.push(file);
        }
    }
    Ok(removed)
}

/// state.json's `unshared`: the ids of notes someone stopped sharing whose
/// file had edits made here. Pull keeps such a file (with its id) and lists
/// it here; push leaves a file carrying one of these ids alone instead of
/// creating it as a note of this account. An unknown top-level key to an
/// engine without it, which keeps it as it is.
pub(crate) const UNSHARED_KEY: &str = "unshared";

/// state.json's `pendingDirs`: the directories a pull made for folders new
/// to the vault (or folders with a new name), written before it makes them
/// and taken out by the state it saves once its notes are where the layout
/// puts them. A pull killed in between leaves directories, and new notes in
/// them, that the state names no folder for: a push would make each a
/// folder in iCloud, a second one beside the folder the pull was laying
/// out. So a push leaves everything until a pull has finished. Each
/// directory maps to what it is for: a folder's record name, or a sharer's
/// owner record name for their home. An unknown top-level key to an engine
/// without it, which keeps it.
pub(crate) const PENDING_DIRS_KEY: &str = "pendingDirs";

/// The ids state.json lists under [`UNSHARED_KEY`].
pub(crate) fn unshared_note_ids(extra: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    extra
        .get(UNSHARED_KEY)
        .and_then(|v| v.as_array())
        .map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// Whether the note is in the trash: its Folder reference is the Trash folder.
pub(crate) fn is_in_trash(record: &CloudKitRecord) -> bool {
    record
        .fields
        .get("Folder")
        .and_then(|f| f.value.as_object())
        .and_then(|o| o.get("recordName"))
        .and_then(|v| v.as_str())
        == Some(TRASH_FOLDER_RECORD_NAME)
}

/// Whether the note is purged: Apple's stage-2 `Deleted: 1` mark.
pub(crate) fn is_purged(record: &CloudKitRecord) -> bool {
    record
        .fields
        .get("Deleted")
        .and_then(|f| f.value.as_f64())
        .is_some_and(|v| v != 0.0)
}
