//! `Database<T>`: the CloudKit database operations as methods over a
//! [`Transport`]. Originally derived from icloud-md.
//!
//! Request bodies are built as `serde_json` objects in a fixed key order
//! (the crate enables `preserve_order`), so request logs are stable and
//! comparable across runs.
//! Endpoint path: `/database/1/com.apple.notes/production/{private|shared}/
//! {operation}?ckjsBuildVersion=..&ckjsVersion=..`; icloud-session appends
//! the per-session `clientId`/`clientBuildNumber`/`clientMasteringNumber`/
//! `dsid` parameters itself.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::CkError;
use super::transport::Transport;
use super::types::*;
use crate::js::is_record;

const CKJS_BUILD_VERSION: &str = "2310ProjectDev27";
const CKJS_VERSION: &str = "2.6.4";
/// CloudKit web services cap a `records/lookup` at 200 records per request.
const LOOKUP_BATCH_SIZE: usize = 200;
/// How long an incremental shared `changes/database` listing is trusted:
/// past this the shared zones are listed from scratch again, which also
/// catches a share whose removal an incremental listing never reported.
pub const SHARED_FULL_LISTING_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

/// Wider than we strictly need; matches what the real web client requests.
const NOTE_DESIRED_KEYS: &[&str] = &[
    "TitleEncrypted",
    "SnippetEncrypted",
    "FirstAttachmentUTIEncrypted",
    "FirstAttachmentThumbnail",
    "FirstAttachmentThumbnailOrientation",
    "CreationDate",
    "ModificationDate",
    "Deleted",
    "Folders",
    "Folder",
    "Attachments",
    "ParentFolder",
    "Note",
    "LastViewedModificationDate",
    "MinimumSupportedNotesVersion",
    "DisplayTextEncrypted",
    "StandardizedContentEncrypted",
    "TokenContentIdentifierEncrypted",
    "AltTextEncrypted",
    "UTIEncrypted",
    "MergeableDataEncrypted",
    "IsPinned",
    "TextDataEncrypted",
];

/// Where a very large note keeps its text instead of `TextDataEncrypted`;
/// requested after [`NOTE_DESIRED_KEYS`] - see
/// [`Database::inline_asset_bodies`].
pub const TEXT_DATA_ASSET_KEY: &str = "TextDataAsset";

/// The `desiredKeys` of a note `changes/zone` request: [`NOTE_DESIRED_KEYS`],
/// plus `TextDataAsset`.
pub fn note_desired_keys() -> Vec<&'static str> {
    let mut keys = NOTE_DESIRED_KEYS.to_vec();
    keys.push(TEXT_DATA_ASSET_KEY);
    keys
}

pub const NOTE_DESIRED_RECORD_TYPES: &[&str] = &[
    "AccountData",
    "Note",
    "SearchIndexes",
    "Folder",
    "PasswordProtectedNote",
    "User",
    "Users",
    "Note_UserSpecific",
    "PasswordProtectedNote_UserSpecific",
    "Folder_UserSpecific",
    "cloudkit.share",
    "Hashtag",
    "InlineAttachment",
];

const MODIFY_MISSING_RECORDS: &str = "Unexpected response shape from records/modify (missing records array)";
const MODIFY_ENTRY_NOT_OBJECT: &str = "Unexpected response shape from records/modify (record entry is not an object)";

/// The ckdatabasews path (with icloud-md's own query parameters) for
/// `operation` in `database`.
fn database_path(database: DatabaseScope, operation: &str) -> String {
    format!(
        "/database/1/com.apple.notes/production/{}/{operation}?ckjsBuildVersion={CKJS_BUILD_VERSION}&ckjsVersion={CKJS_VERSION}",
        database.as_str()
    )
}

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
    fn post_database(&self, database: DatabaseScope, operation: &str, body: &Value) -> Result<Value, CkError> {
        match self.transport.post_json(&database_path(database, operation), body) {
            Err(CkError::Http { status, .. }) => Err(CkError::RequestFailed(format!(
                "{operation} request failed ({} db): HTTP {status}",
                database.as_str()
            ))),
            other => other,
        }
    }

    /// One `changes/zone` walk from `start_token` until `moreComing` is false.
    fn walk_zone(
        &self,
        database: DatabaseScope,
        zone_id: &ZoneId,
        start_token: Option<&str>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<(Vec<CloudKitRecord>, Option<String>), CkError> {
        let mut records = Vec::new();
        let mut sync_token: Option<String> = start_token.map(str::to_owned);
        let mut more_coming = true;
        while more_coming {
            let mut zone_request = Map::new();
            zone_request.insert("zoneID".into(), zone_id_json(zone_id));
            zone_request.insert("desiredKeys".into(), json!(note_desired_keys()));
            zone_request.insert("desiredRecordTypes".into(), json!(NOTE_DESIRED_RECORD_TYPES));
            // The shared database rejects `reverse` outright.
            if database == DatabaseScope::Private {
                zone_request.insert("reverse".into(), Value::Bool(true));
            }
            if let Some(token) = truthy(sync_token.as_deref()) {
                zone_request.insert("syncToken".into(), Value::String(token.to_owned()));
            }
            let body = self.post_database(database, "changes/zone", &json!({ "zones": [zone_request] }))?;
            let zone = first_zone(&body)?;
            let page = zone.records.unwrap_or_default();
            let count = page.len();
            records.extend(page);
            if zone.sync_token.is_some() {
                sync_token = zone.sync_token;
            }
            more_coming = zone.more_coming == Some(true);
            on_page(count);
        }
        let mut records = dedupe_zone_records(records);
        self.inline_asset_bodies(&mut records)?;
        Ok((records, sync_token))
    }

    /// `fetchZoneNoteRecords`: page `changes/zone` until `moreComing` is false
    /// (`reverse: true` on the private database only). A rejected
    /// `since_sync_token` (zone-level BAD_REQUEST) refetches from scratch and
    /// sets `resynced_from_scratch`.
    fn fetch_zone_note_records(
        &self,
        database: DatabaseScope,
        zone_id: &ZoneId,
        since_sync_token: Option<&str>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<ZoneChanges, CkError> {
        let done = |(records, sync_token), resynced_from_scratch| ZoneChanges {
            records,
            sync_token,
            resynced_from_scratch,
        };
        let Some(since) = since_sync_token else {
            return Ok(done(self.walk_zone(database, zone_id, None, on_page)?, false));
        };
        match self.walk_zone(database, zone_id, Some(since), on_page) {
            Ok(walk) => Ok(done(walk, false)),
            Err(CkError::ZoneFetchFailed { server_error_code, .. }) if server_error_code == "BAD_REQUEST" => {
                Ok(done(self.walk_zone(database, zone_id, None, on_page)?, true))
            }
            Err(e) => Err(e),
        }
    }

    /// `fetchAllNoteRecords`: the private `Notes` zone.
    pub fn fetch_all_note_records(
        &self,
        since_sync_token: Option<&str>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<ZoneChanges, CkError> {
        let zone = note_zone(None);
        self.fetch_zone_note_records(zone.database, &zone.zone_id, since_sync_token, on_page)
    }

    /// `fetchSharedZoneIds`: page shared `changes/database` (empty body, then
    /// the response's `syncToken`); tombstoned zones are dropped; a page with
    /// `moreComing` and no new token is `RequestFailed("shared changes/database
    /// reported moreComing without a new syncToken; refusing to re-request the
    /// same page")`.
    pub fn fetch_shared_zone_ids(&self) -> Result<Vec<ZoneId>, CkError> {
        Ok(self.walk_shared_database(None)?.zone_ids)
    }

    /// One shared `changes/database` walk from `start_token` (`None`: every
    /// zone) until `moreComing` is false; see [`Database::fetch_shared_zone_ids`].
    fn walk_shared_database(&self, start_token: Option<&str>) -> Result<SharedZoneListPage, CkError> {
        let mut zone_ids = Vec::new();
        let mut deleted_zone_ids = Vec::new();
        let mut sync_token: Option<String> = start_token.map(str::to_owned);
        let mut more_coming = true;
        while more_coming {
            let mut request = Map::new();
            if let Some(token) = truthy(sync_token.as_deref()) {
                request.insert("syncToken".into(), Value::String(token.to_owned()));
            }
            let body = self.post_database(DatabaseScope::Shared, "changes/database", &Value::Object(request))?;
            let page = parse_shared_zone_list(&body)?;
            zone_ids.extend(page.zone_ids);
            deleted_zone_ids.extend(page.deleted_zone_ids);
            if page.more_coming && (page.sync_token.is_none() || page.sync_token == sync_token) {
                return Err(CkError::RequestFailed(
                    "shared changes/database reported moreComing without a new syncToken; refusing to re-request the same page"
                        .into(),
                ));
            }
            if page.sync_token.is_some() {
                sync_token = page.sync_token;
            }
            more_coming = page.more_coming;
        }
        // docs/DESIGN.md §1:
        // a zone listed on two pages is fetched - and its notes cloned - once.
        let mut seen = std::collections::HashSet::new();
        zone_ids.retain(|z| seen.insert((z.zone_name.clone(), z.owner_record_name.clone())));
        Ok(SharedZoneListPage {
            zone_ids,
            deleted_zone_ids,
            more_coming: false,
            sync_token: truthy(sync_token.as_deref()).map(str::to_owned),
        })
    }

    /// Which shared zones exist and which of them to walk. With a `cursor`
    /// younger than [`SHARED_FULL_LISTING_INTERVAL_MS`], `changes/database`
    /// resumes from its token: the zones it lists are walked, the ones it
    /// marks deleted or purged are dropped, and the cursor's other zones are
    /// unchanged (walked anyway when there's no stored zone sync token to
    /// resume from). A rejected or unreadable incremental answer, an old or
    /// missing cursor, all list every zone from scratch and walk them all.
    fn list_shared_zones(
        &self,
        since_sync_tokens: &IndexMap<String, String>,
        cursor: Option<&SharedDatabaseCursor>,
        now_ms: i64,
    ) -> Result<SharedZoneListing, CkError> {
        let fresh = |c: &&SharedDatabaseCursor| {
            !c.sync_token.is_empty() && (0..SHARED_FULL_LISTING_INTERVAL_MS).contains(&(now_ms - c.listed_at))
        };
        if let Some(cursor) = cursor.filter(fresh) {
            match self.walk_shared_database(Some(&cursor.sync_token)) {
                Ok(delta) => {
                    let mut zones: Vec<ZoneId> = cursor
                        .zones
                        .iter()
                        .filter(|z| !delta.deleted_zone_ids.contains(z))
                        .cloned()
                        .collect();
                    let mut walk: std::collections::HashSet<ZoneId> = std::collections::HashSet::new();
                    for zone in delta.zone_ids {
                        if !zones.contains(&zone) {
                            zones.push(zone.clone());
                        }
                        walk.insert(zone);
                    }
                    for zone in &zones {
                        if stored_zone_token(since_sync_tokens, zone).is_none() {
                            walk.insert(zone.clone());
                        }
                    }
                    return Ok(SharedZoneListing {
                        zones,
                        walk: Some(walk),
                        sync_token: delta.sync_token,
                        listed_at: cursor.listed_at,
                    });
                }
                // An expired or unknown token: list from scratch.
                Err(CkError::RequestFailed(_) | CkError::UnexpectedResponse(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let full = self.walk_shared_database(None)?;
        Ok(SharedZoneListing {
            zones: full.zone_ids,
            walk: None,
            sync_token: full.sync_token,
            listed_at: now_ms,
        })
    }

    /// `fetchSharedNoteRecords`: every shared zone's records, bodies backfilled
    /// through `records/lookup`. `since_sync_tokens` is keyed by owner
    /// recordName. ZONE_NOT_FOUND and still-missing bodies skip the zone;
    /// any other zone-level error is fatal. Lists the shared zones from
    /// scratch; see [`Database::fetch_shared_note_records_since`].
    pub fn fetch_shared_note_records(
        &self,
        since_sync_tokens: &IndexMap<String, String>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<SharedNoteRecords, CkError> {
        self.fetch_shared_note_records_since(since_sync_tokens, None, 0, on_page)
    }

    /// [`Database::fetch_shared_note_records`], walking only the zones the
    /// shared database reports changed since `cursor` (see
    /// `list_shared_zones`). The returned cursor advances only when no zone
    /// was skipped: otherwise it keeps the old token (or none), so the next
    /// listing reports the skipped zone again and it is retried.
    pub fn fetch_shared_note_records_since(
        &self,
        since_sync_tokens: &IndexMap<String, String>,
        cursor: Option<&SharedDatabaseCursor>,
        now_ms: i64,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<SharedNoteRecords, CkError> {
        let listing = self.list_shared_zones(since_sync_tokens, cursor, now_ms)?;
        let mut result = SharedNoteRecords::default();
        for zone_id in listing.zones.iter().cloned() {
            let since = stored_zone_token(since_sync_tokens, &zone_id);
            if listing.walk.as_ref().is_some_and(|walk| !walk.contains(&zone_id)) {
                result.zones.push(SharedZoneChanges {
                    zone_id,
                    records: Vec::new(),
                    sync_token: since.map(str::to_owned),
                    resynced_from_scratch: false,
                });
                continue;
            }
            let fetched = match self.fetch_zone_note_records(DatabaseScope::Shared, &zone_id, since, on_page) {
                Ok(fetched) => fetched,
                Err(CkError::ZoneFetchFailed { server_error_code, .. }) if server_error_code == "ZONE_NOT_FOUND" => {
                    result.skipped_zones.push(SkippedSharedZone::ZoneNotFound {
                        zone_id,
                        server_error_code,
                    });
                    continue;
                }
                Err(e) => return Err(e),
            };
            let ZoneChanges {
                mut records,
                sync_token,
                resynced_from_scratch,
            } = fetched;

            let missing: Vec<String> = records
                .iter()
                .filter(|r| needs_body_lookup(r))
                .map(|r| r.record_name.clone())
                .collect();
            if !missing.is_empty() {
                let zone = NoteZone {
                    database: DatabaseScope::Shared,
                    zone_id: zone_id.clone(),
                };
                let looked_up = self.lookup_records(&zone, &missing)?;
                merge_looked_up_records(&mut records, looked_up);
                self.inline_asset_bodies(&mut records)?;
            }

            let still_missing: Vec<String> = records
                .iter()
                .filter(|r| needs_body_lookup(r))
                .map(|r| r.record_name.clone())
                .collect();
            if !still_missing.is_empty() {
                result.skipped_zones.push(SkippedSharedZone::MissingNoteBodies {
                    zone_id,
                    missing_record_names: still_missing,
                });
                continue;
            }

            result.zones.push(SharedZoneChanges {
                zone_id,
                records,
                sync_token,
                resynced_from_scratch,
            });
        }
        result.cursor = if result.skipped_zones.is_empty() {
            listing.sync_token.map(|sync_token| SharedDatabaseCursor {
                sync_token,
                zones: listing.zones,
                listed_at: listing.listed_at,
            })
        } else {
            cursor.map(|previous| SharedDatabaseCursor {
                sync_token: previous.sync_token.clone(),
                zones: listing.zones,
                listed_at: previous.listed_at,
            })
        };
        Ok(result)
    }

    /// `lookupRecords`: `records/lookup` in batches of 200; per-record error
    /// entries (no `recordType`/`fields`) are skipped.
    pub fn lookup_records(&self, zone: &NoteZone, record_names: &[String]) -> Result<Vec<CloudKitRecord>, CkError> {
        let mut records = Vec::new();
        for batch in record_names.chunks(LOOKUP_BATCH_SIZE) {
            let names: Vec<Value> = batch.iter().map(|name| json!({ "recordName": name })).collect();
            let body = self.post_database(
                zone.database,
                "records/lookup",
                &json!({ "records": names, "zoneID": zone_id_json(&zone.zone_id) }),
            )?;
            let entries = get(&body, "records").and_then(Value::as_array).ok_or_else(|| {
                CkError::UnexpectedResponse(
                    "Unexpected response shape from records/lookup (missing records array)".into(),
                )
            })?;
            for entry in entries {
                if is_record(entry)
                    && get(entry, "recordType").is_some_and(Value::is_string)
                    && get(entry, "fields").is_some_and(is_record)
                {
                    records.push(parse_record(entry)?);
                }
            }
        }
        Ok(records)
    }

    /// `updateRecords`: one `records/modify` with an `update` op per record;
    /// results in request order.
    pub fn update_records(
        &self,
        zone: &NoteZone,
        updates: &[RecordUpdate],
    ) -> Result<Vec<RecordUpdateResult>, CkError> {
        let ops: Vec<RecordOp> = updates.iter().cloned().map(RecordOp::Update).collect();
        let body = self.post_database(zone.database, "records/modify", &modify_body(&ops, &zone.zone_id))?;
        parse_record_update_response(&body)
    }

    /// Many unrelated updates in as few `records/modify` requests as fit
    /// (200 each), non-atomically: each record succeeds or is refused on its
    /// own. Results in request order.
    pub fn update_records_independently(
        &self,
        zone: &NoteZone,
        updates: &[RecordUpdate],
    ) -> Result<Vec<RecordUpdateResult>, CkError> {
        let mut results = Vec::with_capacity(updates.len());
        for batch in updates.chunks(LOOKUP_BATCH_SIZE) {
            let ops: Vec<RecordOp> = batch.iter().cloned().map(RecordOp::Update).collect();
            let mut body = modify_body(&ops, &zone.zone_id);
            body["atomic"] = Value::Bool(false);
            let response = self.post_database(zone.database, "records/modify", &body)?;
            let parsed = parse_record_update_response(&response)?;
            if parsed.len() != batch.len() {
                return Err(CkError::UnexpectedResponse(MODIFY_MISSING_RECORDS.into()));
            }
            results.extend(parsed);
        }
        Ok(results)
    }

    /// `updateNoteRecord`: `update_records` with one `Note` update.
    pub fn update_note_record(&self, zone: &NoteZone, update: &RecordUpdate) -> Result<RecordUpdateResult, CkError> {
        let update = RecordUpdate {
            record_type: "Note".into(),
            ..update.clone()
        };
        self.update_records(zone, std::slice::from_ref(&update))?
            .into_iter()
            .next()
            .ok_or_else(|| CkError::UnexpectedResponse(MODIFY_MISSING_RECORDS.into()))
    }

    /// `createZoneRecord`: one `create` op of `record_type`.
    fn create_zone_record(
        &self,
        record_type: &str,
        zone: &NoteZone,
        record_name: &str,
        fields: &UpdateFields,
        extras: &CreateExtras,
    ) -> Result<RecordUpdateResult, CkError> {
        let op = RecordOp::Create {
            record_name: record_name.into(),
            record_type: record_type.into(),
            fields: fields.clone(),
            extras: extras.clone(),
        };
        let body = self.post_database(zone.database, "records/modify", &modify_body(&[op], &zone.zone_id))?;
        parse_note_update_response(&body)
    }

    /// `createNoteRecord`.
    pub fn create_note_record(
        &self,
        zone: &NoteZone,
        record_name: &str,
        fields: &UpdateFields,
        extras: &CreateExtras,
    ) -> Result<RecordUpdateResult, CkError> {
        self.create_zone_record("Note", zone, record_name, fields, extras)
    }

    /// Deletes a record: `Ok(None)` when gone, `Ok(Some(reason))` when the
    /// server refused (a `CONFLICT` if it changed since `record_change_tag`).
    pub fn delete_record(
        &self,
        zone: &NoteZone,
        record_name: &str,
        record_change_tag: &str,
    ) -> Result<Option<String>, CkError> {
        let op = RecordOp::Delete {
            record_name: record_name.into(),
            record_change_tag: record_change_tag.into(),
        };
        let body = self.post_database(zone.database, "records/modify", &modify_body(&[op], &zone.zone_id))?;
        let entry = first_modify_entry(&body)?;
        Ok(get_str(entry, "serverErrorCode").map(|code| {
            get_str(entry, "reason")
                .filter(|r| !r.is_empty())
                .map_or_else(|| code.to_owned(), |r| format!("{code} ({r})"))
        }))
    }

    /// `createFolderRecord`.
    pub fn create_folder_record(
        &self,
        zone: &NoteZone,
        record_name: &str,
        fields: &UpdateFields,
        extras: &CreateExtras,
    ) -> Result<RecordUpdateResult, CkError> {
        self.create_zone_record("Folder", zone, record_name, fields, extras)
    }

    /// `fetchAssetBytes`, streamed to `dest`: a plain signed GET; non-2xx is
    /// `RequestFailed("Attachment download failed: HTTP {status}")`. Returns
    /// the byte count.
    pub fn fetch_asset(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        self.transport.download(url, dest).map_err(asset_error)
    }

    /// `fetchAssetBytes`: [`Database::fetch_asset`] into memory.
    fn fetch_asset_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        self.transport.download_bytes(url).map_err(asset_error)
    }

    /// `inlineAssetBodies` (upstream PR #29): moves a very large note's text
    /// inline, where every reader expects it. Past some size Apple stores a
    /// note's text as a `TextDataAsset` instead of `TextDataEncrypted`, and
    /// the record then carries no `TextDataEncrypted` at all - so without
    /// this the note reads as body-less and is never cloned or pulled. The
    /// asset holds the same bytes the inline field would (a gzipped
    /// NoteStoreProto document), so its download is inlined as-is and decodes
    /// on the normal path.
    ///
    /// Only the in-memory record changes. Push re-reads the record before any
    /// write and still refuses a note stored as an asset. A failed download
    /// propagates rather than leaving the note body-less: that would read as
    /// a clean sync while the syncToken moved past the note.
    pub fn inline_asset_bodies(&self, records: &mut [CloudKitRecord]) -> Result<(), CkError> {
        for record in records.iter_mut() {
            if !needs_body_lookup(record) {
                continue;
            }
            let Some(url) = record
                .fields
                .get(TEXT_DATA_ASSET_KEY)
                .map(|f| &f.value)
                .filter(|v| is_record(v))
                .and_then(|v| get_str(v, "downloadURL"))
                .map(str::to_owned)
            else {
                continue;
            };
            let bytes = self.fetch_asset_bytes(&url)?;
            record.fields.insert(
                "TextDataEncrypted".into(),
                FieldValue {
                    value: Value::String(crate::js::base64_encode(&bytes)),
                    type_: "ENCRYPTED_BYTES".into(),
                },
            );
        }
        Ok(())
    }
}

/// The shared zones to fetch this run (`list_shared_zones`).
struct SharedZoneListing {
    /// Every live zone, in a stable order.
    zones: Vec<ZoneId>,
    /// The zones to walk; `None` = all of them.
    walk: Option<std::collections::HashSet<ZoneId>>,
    sync_token: Option<String>,
    listed_at: i64,
}

/// The stored `changes/zone` sync token of a shared zone (keyed by owner).
fn stored_zone_token<'a>(since_sync_tokens: &'a IndexMap<String, String>, zone_id: &ZoneId) -> Option<&'a str> {
    truthy(zone_id.owner_record_name.as_deref())
        .and_then(|owner| since_sync_tokens.get(owner))
        .and_then(|t| truthy(Some(t)))
}

/// A failed asset GET: non-2xx is `RequestFailed("Attachment download
/// failed: HTTP {status}")`.
fn asset_error(e: CkError) -> CkError {
    match e {
        CkError::Http { status, .. } => CkError::RequestFailed(format!("Attachment download failed: HTTP {status}")),
        other => other,
    }
}

/// How many attachment downloads a [`DownloadQueue`] keeps in flight when
/// the transport lets them overlap.
pub const DOWNLOAD_WORKERS: usize = 4;

/// Attachment downloads queued while a pull or clone goes through its notes.
/// Over a transport with a [`Transport::shared_downloader`] they run on
/// [`DOWNLOAD_WORKERS`] background threads; otherwise each runs when queued,
/// in order. A destination already queued is not fetched again (it is the
/// same attachment file). [`DownloadQueue::push`] reports a background
/// failure as soon as it is known, so a run stops at the next attachment much
/// as it did when every download ran in line; [`DownloadQueue::finish`]
/// waits for the rest. Dropping the queue waits for the downloads in flight
/// and starts no more.
pub struct DownloadQueue<'a, T: Transport> {
    db: &'a Database<T>,
    queued: std::collections::HashSet<PathBuf>,
    pool: Option<DownloadPool>,
}

struct DownloadPool {
    jobs: Option<std::sync::mpsc::Sender<(usize, String, PathBuf)>>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// (queue position, error), every failure so far.
    errors: std::sync::Arc<std::sync::Mutex<Vec<(usize, CkError)>>>,
    next: usize,
}

impl<'a, T: Transport> DownloadQueue<'a, T> {
    pub fn new(db: &'a Database<T>) -> Self {
        let pool = db.transport.shared_downloader().map(|download| {
            let (jobs, receiver) = std::sync::mpsc::channel::<(usize, String, PathBuf)>();
            let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));
            let errors: std::sync::Arc<std::sync::Mutex<Vec<(usize, CkError)>>> = Default::default();
            let workers = (0..DOWNLOAD_WORKERS)
                .map(|_| {
                    let (receiver, errors, download) = (receiver.clone(), errors.clone(), download.clone());
                    std::thread::spawn(move || {
                        loop {
                            let job = receiver.lock().unwrap_or_else(|e| e.into_inner()).recv();
                            let Ok((i, url, dest)) = job else { break };
                            let mut errors_now = errors.lock().unwrap_or_else(|e| e.into_inner());
                            if !errors_now.is_empty() {
                                continue;
                            }
                            drop(errors_now);
                            if let Err(e) = download(&url, &dest) {
                                errors_now = errors.lock().unwrap_or_else(|e| e.into_inner());
                                errors_now.push((i, asset_error(e)));
                            }
                        }
                    })
                })
                .collect();
            DownloadPool {
                jobs: Some(jobs),
                workers,
                errors,
                next: 0,
            }
        });
        DownloadQueue {
            db,
            queued: std::collections::HashSet::new(),
            pool,
        }
    }

    /// Queues `url` into `dest`. `Err`: this download (in line) or an
    /// earlier one (in the background) failed.
    pub fn push(&mut self, url: String, dest: PathBuf) -> Result<(), CkError> {
        if !self.queued.insert(dest.clone()) {
            return Ok(());
        }
        let Some(pool) = &mut self.pool else {
            return self.db.fetch_asset(&url, &dest).map(drop);
        };
        if let Some(e) = pool.take_error() {
            return Err(e);
        }
        let job = (pool.next, url, dest);
        pool.next += 1;
        if let Some(jobs) = &pool.jobs {
            // The workers only go away after `jobs` is closed.
            let _ = jobs.send(job);
        }
        Ok(())
    }

    /// Waits for every queued download; the earliest failure, if any.
    pub fn finish(mut self) -> Result<(), CkError> {
        match &mut self.pool {
            None => Ok(()),
            Some(pool) => {
                pool.join();
                pool.take_error().map_or(Ok(()), Err)
            }
        }
    }
}

impl DownloadPool {
    fn join(&mut self) {
        self.jobs = None;
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }

    /// The failure earliest in queue order, if any. The workers start no
    /// further download either way once one has failed.
    fn take_error(&mut self) -> Option<CkError> {
        let mut errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
        errors.sort_by_key(|(i, _)| *i);
        let first = (!errors.is_empty()).then(|| errors.remove(0).1);
        if first.is_some() {
            errors.push(stop_marker());
        }
        first
    }
}

/// Recorded as an error so the workers start nothing more; never reported.
fn stop_marker() -> (usize, CkError) {
    (usize::MAX, CkError::Other("download queue stopped".into()))
}

impl Drop for DownloadPool {
    fn drop(&mut self) {
        self.errors.lock().unwrap_or_else(|e| e.into_inner()).push(stop_marker());
        self.join();
    }
}

/// JS truthiness for an optional string: `None` and `""` are both falsy.
fn truthy(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

fn zone_id_json(zone_id: &ZoneId) -> Value {
    serde_json::to_value(zone_id).expect("ZoneId serializes")
}

/// `needsBodyLookup`: a live Note whose `TextDataEncrypted` value isn't a string.
pub fn needs_body_lookup(record: &CloudKitRecord) -> bool {
    if record.record_type != "Note" || record.is_deleted() {
        return false;
    }
    !record
        .fields
        .get("TextDataEncrypted")
        .is_some_and(|f| f.value.is_string())
}

/// The `records/modify` request body for `ops` in `zone`:
/// `{"operations":[{"operationType":..,"record":{..}}],"zoneID":{..}}`.
fn modify_body(ops: &[RecordOp], zone_id: &ZoneId) -> Value {
    let operations: Vec<Value> = ops
        .iter()
        .map(|op| {
            let mut record = Map::new();
            let operation_type = match op {
                RecordOp::Update(update) => {
                    record.insert("recordName".into(), json!(update.record_name));
                    record.insert("recordType".into(), json!(update.record_type));
                    record.insert("recordChangeTag".into(), json!(update.record_change_tag));
                    record.insert("fields".into(), fields_json(&update.fields));
                    if let Some(parent) = &update.parent_record_name {
                        record.insert("parent".into(), json!({ "recordName": parent }));
                    }
                    "update"
                }
                RecordOp::Create {
                    record_name,
                    record_type,
                    fields,
                    extras,
                } => {
                    record.insert("recordName".into(), json!(record_name));
                    record.insert("recordType".into(), json!(record_type));
                    record.insert("fields".into(), fields_json(fields));
                    if let Some(parent) = &extras.parent_record_name {
                        record.insert("parent".into(), json!({ "recordName": parent }));
                    }
                    if extras.create_short_guid {
                        record.insert("createShortGUID".into(), Value::Bool(true));
                    }
                    "create"
                }
                RecordOp::Delete {
                    record_name,
                    record_change_tag,
                } => {
                    record.insert("recordName".into(), json!(record_name));
                    record.insert("recordChangeTag".into(), json!(record_change_tag));
                    "delete"
                }
            };
            json!({ "operationType": operation_type, "record": record })
        })
        .collect();
    json!({ "operations": operations, "zoneID": zone_id_json(zone_id) })
}

fn fields_json(fields: &UpdateFields) -> Value {
    serde_json::to_value(fields).expect("update fields serialize")
}

/// Property access as JS does it: only objects have named properties here.
fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|o| o.get(key))
}

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    get(v, key).and_then(Value::as_str)
}

/// `Object.entries` over an object or array.
fn entries(v: &Value) -> Vec<(String, &Value)> {
    match v {
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => Vec::new(),
    }
}

/// `parseRecord`: a live record entry; `Err(UnexpectedResponse("Unexpected
/// record shape in changes/zone response"))` if it lacks recordName,
/// recordType or fields.
pub fn parse_record(value: &Value) -> Result<CloudKitRecord, CkError> {
    let (Some(record_name), Some(record_type), Some(fields_value)) = (
        get_str(value, "recordName"),
        get_str(value, "recordType"),
        get(value, "fields").filter(|f| is_record(f)),
    ) else {
        return Err(CkError::UnexpectedResponse(
            "Unexpected record shape in changes/zone response".into(),
        ));
    };

    let mut fields = IndexMap::new();
    for (key, field) in entries(fields_value) {
        if is_record(field)
            && let Some(type_) = get_str(field, "type")
        {
            fields.insert(
                key,
                FieldValue {
                    value: get(field, "value").cloned().unwrap_or(Value::Null),
                    type_: type_.to_owned(),
                },
            );
        }
    }

    Ok(CloudKitRecord {
        record_name: record_name.to_owned(),
        record_type: record_type.to_owned(),
        fields,
        record_change_tag: get_str(value, "recordChangeTag").map(str::to_owned),
        deleted: get(value, "deleted").and_then(Value::as_bool),
        parent_record_name: get(value, "parent")
            .and_then(|p| get_str(p, "recordName"))
            .map(str::to_owned),
        created: parse_record_stamp(get(value, "created")),
        modified: parse_record_stamp(get(value, "modified")),
        participants: parse_participants(get(value, "participants")),
        share_record_name: get(value, "share")
            .and_then(|s| get_str(s, "recordName"))
            .map(str::to_owned),
        current_user_permission: get(value, "currentUserParticipant")
            .and_then(|p| get_str(p, "permission"))
            .map(str::to_owned),
    })
}

fn parse_participants(value: Option<&Value>) -> Option<Vec<Participant>> {
    let list = value?.as_array()?;
    let empty = Value::Null;
    Some(
        list.iter()
            .filter(|p| is_record(p))
            .map(|participant| {
                let identity = get(participant, "userIdentity").unwrap_or(&empty);
                let names = get(identity, "nameComponents").unwrap_or(&empty);
                let lookup = get(identity, "lookupInfo").unwrap_or(&empty);
                let s = |v: &Value, k: &str| get_str(v, k).map(str::to_owned);
                Participant {
                    type_: s(participant, "type"),
                    user_record_name: s(identity, "userRecordName"),
                    given_name: s(names, "givenName"),
                    family_name: s(names, "familyName"),
                    email_address: s(lookup, "emailAddress"),
                    phone_number: s(lookup, "phoneNumber"),
                }
            })
            .collect(),
    )
}

fn parse_record_stamp(value: Option<&Value>) -> Option<Stamp> {
    let value = value?;
    let timestamp = get(value, "timestamp")?;
    let timestamp = timestamp.as_i64().or_else(|| timestamp.as_f64().map(|f| f as i64))?;
    Some(Stamp {
        timestamp,
        device_id: get_str(value, "deviceID").map(str::to_owned),
    })
}

/// `parseZoneRecord`: a `{recordName, deleted: true}` tombstone becomes a
/// fieldless `Note` record with `deleted: Some(true)`; anything else
/// `parse_record`.
fn parse_zone_record(value: &Value) -> Result<CloudKitRecord, CkError> {
    if let Some(record_name) = get_str(value, "recordName")
        && get(value, "deleted") == Some(&Value::Bool(true))
        && !get(value, "fields").is_some_and(is_record)
    {
        return Ok(CloudKitRecord {
            record_name: record_name.to_owned(),
            record_type: "Note".into(),
            deleted: Some(true),
            ..CloudKitRecord::default()
        });
    }
    parse_record(value)
}

/// `firstZone`: the first zone of a `changes/zone` response; a zone-level
/// `serverErrorCode` is `ZoneFetchFailed`.
pub fn first_zone(body: &Value) -> Result<ParsedZone, CkError> {
    let zone = get(body, "zones")
        .and_then(Value::as_array)
        .and_then(|z| z.first())
        .ok_or_else(|| {
            CkError::UnexpectedResponse("Unexpected response shape from changes/zone (missing zones array)".into())
        })?;
    if !is_record(zone) {
        return Err(CkError::UnexpectedResponse(
            "Unexpected response shape from changes/zone (zone entry is not an object)".into(),
        ));
    }
    if let Some(code) = get_str(zone, "serverErrorCode") {
        return Err(CkError::ZoneFetchFailed {
            server_error_code: code.to_owned(),
            reason: get_str(zone, "reason").unwrap_or("no reason given").to_owned(),
        });
    }
    let records = match get(zone, "records").and_then(Value::as_array) {
        Some(list) => Some(list.iter().map(parse_zone_record).collect::<Result<Vec<_>, _>>()?),
        None => None,
    };
    Ok(ParsedZone {
        more_coming: get(zone, "moreComing").and_then(Value::as_bool),
        sync_token: get_str(zone, "syncToken").map(str::to_owned),
        records,
    })
}

/// `parseSharedZoneList`.
pub fn parse_shared_zone_list(body: &Value) -> Result<SharedZoneListPage, CkError> {
    let zones = get(body, "zones").and_then(Value::as_array).ok_or_else(|| {
        CkError::UnexpectedResponse(
            "Unexpected response shape from shared changes/database (missing zones array)".into(),
        )
    })?;
    let mut zone_ids = Vec::new();
    let mut deleted_zone_ids = Vec::new();
    for zone in zones {
        let zone_name = get(zone, "zoneID").and_then(|id| get_str(id, "zoneName"));
        let Some(zone_name) = zone_name else {
            return Err(CkError::UnexpectedResponse(
                "Unexpected zone shape in shared changes/database response".into(),
            ));
        };
        let zone_id = ZoneId {
            zone_name: zone_name.to_owned(),
            owner_record_name: get(zone, "zoneID")
                .and_then(|id| get_str(id, "ownerRecordName"))
                .map(str::to_owned),
        };
        if get(zone, "deleted") == Some(&Value::Bool(true)) || get(zone, "purged") == Some(&Value::Bool(true)) {
            deleted_zone_ids.push(zone_id);
        } else {
            zone_ids.push(zone_id);
        }
    }
    Ok(SharedZoneListPage {
        zone_ids,
        deleted_zone_ids,
        more_coming: get(body, "moreComing") == Some(&Value::Bool(true)),
        sync_token: get_str(body, "syncToken").map(str::to_owned),
    })
}

fn parse_update_entry(entry: &Value) -> Result<RecordUpdateResult, CkError> {
    if !is_record(entry) {
        return Err(CkError::UnexpectedResponse(MODIFY_ENTRY_NOT_OBJECT.into()));
    }
    if let Some(code) = get_str(entry, "serverErrorCode") {
        return Ok(RecordUpdateResult::Rejected {
            server_error_code: code.to_owned(),
            reason: get_str(entry, "reason").map(str::to_owned),
        });
    }
    Ok(RecordUpdateResult::Ok(Box::new(parse_record(entry)?)))
}

/// `parseRecordUpdateResponse`.
pub fn parse_record_update_response(body: &Value) -> Result<Vec<RecordUpdateResult>, CkError> {
    get(body, "records")
        .and_then(Value::as_array)
        .ok_or_else(|| CkError::UnexpectedResponse(MODIFY_MISSING_RECORDS.into()))?
        .iter()
        .map(parse_update_entry)
        .collect()
}

fn first_modify_entry(body: &Value) -> Result<&Value, CkError> {
    get(body, "records")
        .and_then(Value::as_array)
        .and_then(|r| r.first())
        .ok_or_else(|| CkError::UnexpectedResponse(MODIFY_MISSING_RECORDS.into()))
}

/// `parseNoteUpdateResponse`: the first entry only.
pub fn parse_note_update_response(body: &Value) -> Result<RecordUpdateResult, CkError> {
    parse_update_entry(first_modify_entry(body)?)
}

/// Collapses repeated occurrences of a record in one zone's `changes/zone`
/// listing (docs/DESIGN.md §1): CloudKit occasionally returns the same
/// record on two pages of one walk (and could within a page); kept, every
/// occurrence would make `clone` write the note twice - the second copy
/// under a uniquified name, one of the two left untracked - and the next
/// `push` create it as a new note. Callers pass the records of a single zone, so the key is
/// `recordName`; the same recordName in another zone (private vs a shared
/// zone) is a different record and is never collapsed here.
///
/// Which occurrence wins:
/// 1. the one with the greater `modified.timestamp`, when both carry one and
///    they differ;
/// 2. otherwise the later one in listing order (a later page is a later read
///    of the server, so it is never staler). This covers a tombstone (no
///    `modified`) that follows a live copy, and two copies whose
///    `recordChangeTag`s differ - change tags are opaque, not ordered, so they
///    are never compared.
///
/// The winner takes the position of the record's first occurrence, so file
/// naming order is the listing order of first appearance.
pub fn dedupe_zone_records(records: Vec<CloudKitRecord>) -> Vec<CloudKitRecord> {
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out: Vec<CloudKitRecord> = Vec::with_capacity(records.len());
    for record in records {
        match index.get(&record.record_name) {
            Some(&at) => {
                if supersedes(&record, &out[at]) {
                    out[at] = record;
                }
            }
            None => {
                index.insert(record.record_name.clone(), out.len());
                out.push(record);
            }
        }
    }
    out
}

/// Whether `later` (seen after `kept` in the listing) replaces it; see
/// [`dedupe_zone_records`].
fn supersedes(later: &CloudKitRecord, kept: &CloudKitRecord) -> bool {
    match (&later.modified, &kept.modified) {
        (Some(a), Some(b)) if a.timestamp != b.timestamp => a.timestamp > b.timestamp,
        _ => true,
    }
}

/// `mergeLookedUpRecords`: replace listing records in place with their
/// looked-up versions, keeping the listing's changeTag when the lookup lacks
/// one.
pub fn merge_looked_up_records(records: &mut [CloudKitRecord], looked_up: Vec<CloudKitRecord>) {
    // `new Map(...)`: a later duplicate recordName wins.
    let by_name: std::collections::HashMap<String, CloudKitRecord> =
        looked_up.into_iter().map(|r| (r.record_name.clone(), r)).collect();
    for record in records.iter_mut() {
        if let Some(full) = by_name.get(&record.record_name) {
            let mut full = full.clone();
            if full.record_change_tag.is_none() {
                full.record_change_tag = record.record_change_tag.take();
            }
            *record = full;
        }
    }
}
