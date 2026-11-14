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

/// [`skipped_note_notice`]'s `why` for a record name that can't name a file.
pub(crate) const UNSAFE_RECORD_NAME: &str = "its record name isn't one plain file name";

/// [`skipped_note_notice`]'s `why` for a record name another zone's note
/// already has here.
pub(crate) const OTHER_ZONES_RECORD_NAME: &str = "a note from another zone is already tracked under its record name";

/// The warning for a shared zone the fetch skipped. What that meant for the
/// run is the caller's to say: `revoked` ends the sentence for a zone the
/// server no longer has, `untouched` the one for a zone whose notes came
/// without their text (`when`: " this run", or nothing).
pub(crate) fn skipped_zone_notice(skipped: &SkippedSharedZone, revoked: &str, when: &str, untouched: &str) -> SyncNotice {
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
