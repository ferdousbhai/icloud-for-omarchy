//! `Database<T>`: icloud-md's `databaseClient.ts` functions as methods over a
//! [`Transport`]. Owner: workstream A.
//!
//! Wire constants (copy from databaseClient.ts): `CKJS_BUILD_VERSION =
//! "2310ProjectDev27"`, `CKJS_VERSION = "2.6.4"`, `NOTE_DESIRED_KEYS`,
//! `NOTE_DESIRED_RECORD_TYPES`, `LOOKUP_BATCH_SIZE = 200`. Endpoint path:
//! `/database/1/com.apple.notes/production/{private|shared}/{operation}`.
#![allow(unused_variables)]

use std::path::Path;

use indexmap::IndexMap;
use serde_json::Value;

use super::CkError;
use super::transport::Transport;
use super::types::*;

pub const CKJS_BUILD_VERSION: &str = "2310ProjectDev27";
pub const CKJS_VERSION: &str = "2.6.4";
pub const LOOKUP_BATCH_SIZE: usize = 200;

pub struct Database<T: Transport> {
    pub transport: T,
}

impl<T: Transport> Database<T> {
    pub fn new(transport: T) -> Self {
        Database { transport }
    }

    /// `postDatabase`: POST `body` to `operation` (e.g. `changes/zone`) in
    /// `database`; a non-2xx becomes `RequestFailed("{operation} request
    /// failed ({database} db): HTTP {status}")`.
    pub fn post_database(&self, database: DatabaseScope, operation: &str, body: &Value) -> Result<Value, CkError> {
        todo!()
    }

    /// `fetchZoneNoteRecords`: page `changes/zone` until `moreComing` is false
    /// (`reverse: true` on the private database only). A rejected
    /// `since_sync_token` (zone-level BAD_REQUEST) refetches from scratch and
    /// sets `resynced_from_scratch`.
    pub fn fetch_zone_note_records(
        &self,
        database: DatabaseScope,
        zone_id: &ZoneId,
        since_sync_token: Option<&str>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<ZoneChanges, CkError> {
        todo!()
    }

    /// `fetchAllNoteRecords`: the private `Notes` zone.
    pub fn fetch_all_note_records(
        &self,
        since_sync_token: Option<&str>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<ZoneChanges, CkError> {
        todo!()
    }

    /// `fetchSharedZoneIds`: page shared `changes/database` (empty body, then
    /// the response's `syncToken`); tombstoned zones are dropped; a page with
    /// `moreComing` and no new token is `RequestFailed("shared changes/database
    /// reported moreComing without a new syncToken; refusing to re-request the
    /// same page")`.
    pub fn fetch_shared_zone_ids(&self) -> Result<Vec<ZoneId>, CkError> {
        todo!()
    }

    /// `fetchSharedNoteRecords`: every shared zone's records, bodies backfilled
    /// through `records/lookup`. `since_sync_tokens` is keyed by owner
    /// recordName.
    pub fn fetch_shared_note_records(
        &self,
        since_sync_tokens: &IndexMap<String, String>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<SharedNoteRecords, CkError> {
        todo!()
    }

    /// `lookupRecords`: `records/lookup` in batches of 200; per-record error
    /// entries (no `recordType`/`fields`) are skipped.
    pub fn lookup_records(&self, zone: &NoteZone, record_names: &[String]) -> Result<Vec<CloudKitRecord>, CkError> {
        todo!()
    }

    /// `updateRecords`: one `records/modify` with an `update` op per record;
    /// results in request order.
    pub fn update_records(
        &self,
        zone: &NoteZone,
        updates: &[RecordUpdate],
    ) -> Result<Vec<RecordUpdateResult>, CkError> {
        todo!()
    }

    /// `updateNoteRecord`: `update_records` with one `Note` update.
    pub fn update_note_record(&self, zone: &NoteZone, update: &RecordUpdate) -> Result<RecordUpdateResult, CkError> {
        todo!()
    }

    /// `createZoneRecord`: one `create` op of `record_type`.
    pub fn create_zone_record(
        &self,
        record_type: &str,
        zone: &NoteZone,
        record_name: &str,
        fields: &UpdateFields,
        extras: &CreateExtras,
    ) -> Result<RecordUpdateResult, CkError> {
        todo!()
    }

    /// `createNoteRecord`.
    pub fn create_note_record(
        &self,
        zone: &NoteZone,
        record_name: &str,
        fields: &UpdateFields,
        extras: &CreateExtras,
    ) -> Result<RecordUpdateResult, CkError> {
        todo!()
    }

    /// `createFolderRecord`.
    pub fn create_folder_record(
        &self,
        zone: &NoteZone,
        record_name: &str,
        fields: &UpdateFields,
        extras: &CreateExtras,
    ) -> Result<RecordUpdateResult, CkError> {
        todo!()
    }

    /// `fetchAssetBytes`, streamed to `dest`: a plain signed GET; non-2xx is
    /// `RequestFailed("Attachment download failed: HTTP {status}")`. Returns
    /// the byte count.
    pub fn fetch_asset(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        todo!()
    }
}

/// The `records/modify` request body for `ops` in `zone`:
/// `{"operations":[{"operationType":..,"record":{..}}],"zoneID":{..}}`.
pub fn modify_body(ops: &[RecordOp], zone_id: &ZoneId) -> Value {
    todo!()
}

/// `parseRecord`: a live record entry; `Err(UnexpectedResponse("Unexpected
/// record shape in changes/zone response"))` if it lacks recordName,
/// recordType or fields.
pub fn parse_record(value: &Value) -> Result<CloudKitRecord, CkError> {
    todo!()
}

/// `parseZoneRecord`: a `{recordName, deleted: true}` tombstone becomes a
/// fieldless `Note` record with `deleted: Some(true)`; anything else
/// `parse_record`.
pub fn parse_zone_record(value: &Value) -> Result<CloudKitRecord, CkError> {
    todo!()
}

/// `firstZone`: the first zone of a `changes/zone` response; a zone-level
/// `serverErrorCode` is `ZoneFetchFailed`.
pub fn first_zone(body: &Value) -> Result<ParsedZone, CkError> {
    todo!()
}

/// `parseSharedZoneList`.
pub fn parse_shared_zone_list(body: &Value) -> Result<SharedZoneListPage, CkError> {
    todo!()
}

/// `parseRecordUpdateResponse`.
pub fn parse_record_update_response(body: &Value) -> Result<Vec<RecordUpdateResult>, CkError> {
    todo!()
}

/// `parseNoteUpdateResponse`: the first entry only.
pub fn parse_note_update_response(body: &Value) -> Result<RecordUpdateResult, CkError> {
    todo!()
}

/// `mergeLookedUpRecords`: replace listing records in place with their
/// looked-up versions, keeping the listing's changeTag when the lookup lacks
/// one.
pub fn merge_looked_up_records(records: &mut [CloudKitRecord], looked_up: Vec<CloudKitRecord>) {
    todo!()
}
