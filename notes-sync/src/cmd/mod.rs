//! Commands. Owner: workstream D. Ports icloud-md `src/commands/{clone,pull,
//! push,status,history,diff,restore}.ts`, `src/cli/{output,pullReport,
//! reportStyle}.ts`, `src/progress.ts` and `src/errors.ts`.

pub mod clone;
pub mod diff;
pub mod errors;
pub mod history;
pub mod output;
pub mod plan;
pub mod pull;
pub mod push;
pub mod restore;
pub mod status;

use serde::{Deserialize, Serialize};

pub use errors::Error;

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
