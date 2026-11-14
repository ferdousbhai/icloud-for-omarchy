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

/// The note files a directory holds that nothing tracks, by the
/// `apple-note-id` each carries: what a clone or pull that never got to
/// record them (killed, or failed on that note) left there. Several files
/// may carry one id (a copy made since); a Syncthing conflict copy is never
/// a note file.
pub(crate) type LeftBehind = HashMap<String, Vec<String>>;

/// The [`LeftBehind`] files among `names` (file names in `dir`).
pub(crate) fn left_behind_in<'a>(
    target_dir: &std::path::Path,
    dir: &str,
    names: impl IntoIterator<Item = &'a String>,
    title_mode: crate::vault::state::TitleMode,
) -> Result<LeftBehind, Error> {
    let mut names: Vec<&String> = names
        .into_iter()
        .filter(|name| name.ends_with(".md") && !push::is_sync_conflict_name(name))
        .collect();
    names.sort();
    let mut ids = LeftBehind::new();
    for name in names {
        let file = crate::js::posix::join(&[dir, name]);
        if let Some(id) = crate::vault::pairing::note_id_at(target_dir, &file, title_mode)? {
            ids.entry(id).or_default().push(name.clone());
        }
    }
    Ok(ids)
}

/// The file a run that never recorded it wrote for the note `record_name`:
/// the note's file, not a name to write beside. Only one that reads exactly
/// as `file_text`, what this run would write for the note: a file with the
/// id that differs (edited since, or the note changed in iCloud) is kept as
/// it is, and the note written beside it.
pub(crate) fn left_file_of(
    target_dir: &std::path::Path,
    dir: &str,
    left_behind: &LeftBehind,
    record_name: &str,
    file_text: &str,
) -> Option<String> {
    left_behind
        .get(record_name)?
        .iter()
        .find(|name| {
            let path = target_dir.join(crate::js::posix::join(&[dir, name]));
            std::fs::read_to_string(path).is_ok_and(|text| text == file_text)
        })
        .cloned()
}

/// state.json's `unshared`: the ids of notes someone stopped sharing whose
/// file had edits made here. Pull keeps such a file (with its id) and lists
/// it here; push leaves a file carrying one of these ids alone instead of
/// creating it as a note of this account. An unknown top-level key to an
/// engine without it, which keeps it as it is.
pub(crate) const UNSHARED_KEY: &str = "unshared";

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
