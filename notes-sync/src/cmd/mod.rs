//! Commands. Ports icloud-md `src/commands/{clone,pull,
//! push,status,history,diff,restore}.ts`, `src/cli/{output,pullReport,
//! reportStyle}.ts`, `src/progress.ts` and `src/errors.ts`.

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

use serde::{Deserialize, Serialize};

pub use errors::Error;

use crate::cloudkit::{CloudKitRecord, NoteZone, SkippedSharedZone, note_zone};
use crate::doc::encode::TRASH_FOLDER_RECORD_NAME;

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

/// `SyncProgress`: progress callbacks for clone/pull (`--json` renders them
/// as `icloud-md:progress:...` lines on stderr).
pub trait SyncProgress {
    fn on_fetch_start(&mut self) {}
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

/// A skipped shared zone's owner.
pub(crate) fn skipped_zone_owner(skipped: &SkippedSharedZone) -> Option<&str> {
    match skipped {
        SkippedSharedZone::ZoneNotFound { zone_id, .. } | SkippedSharedZone::MissingNoteBodies { zone_id, .. } => {
            zone_id.owner_record_name.as_deref()
        }
    }
}

/// `isInTrash` (delete.ts): the note's Folder reference is the Trash folder.
pub(crate) fn is_in_trash(record: &CloudKitRecord) -> bool {
    record
        .fields
        .get("Folder")
        .and_then(|f| f.value.as_object())
        .and_then(|o| o.get("recordName"))
        .and_then(|v| v.as_str())
        == Some(TRASH_FOLDER_RECORD_NAME)
}

/// `isPurged` (delete.ts): Apple's stage-2 `Deleted: 1` mark.
pub(crate) fn is_purged(record: &CloudKitRecord) -> bool {
    record
        .fields
        .get("Deleted")
        .and_then(|f| f.value.as_f64())
        .is_some_and(|v| v != 0.0)
}
