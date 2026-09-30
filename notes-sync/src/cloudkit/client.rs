//! `Database<T>`: icloud-md's `databaseClient.ts` functions as methods over a
//! [`Transport`].
//!
//! Request bodies are built as `serde_json` objects in the exact key order
//! icloud-md builds them (the crate enables `preserve_order`), so a request
//! log from this client is byte-comparable with one from the Node driver.
//! Endpoint path: `/database/1/com.apple.notes/production/{private|shared}/
//! {operation}?ckjsBuildVersion=..&ckjsVersion=..`; icloud-session appends
//! the per-session `clientId`/`clientBuildNumber`/`clientMasteringNumber`/
//! `dsid` parameters itself.

use std::path::Path;

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
/// requested (after [`NOTE_DESIRED_KEYS`]) only with asset bodies on - see
/// [`Database::inline_asset_bodies`].
pub const TEXT_DATA_ASSET_KEY: &str = "TextDataAsset";

/// `ICLOUD_NOTES_SYNC_ASSET_BODIES=0` turns off upstream icloud-md PR #29
/// ("Fetch the text of notes too large to store it inline", unmerged in
/// 0.6.2): no `TextDataAsset` in the desired keys, no asset download, and no
/// read-only marking - stock 0.6.2 behaviour, for the differential scenarios
/// whose expected output comes from 0.6.2. On by default.
pub const ASSET_BODIES_ENV: &str = "ICLOUD_NOTES_SYNC_ASSET_BODIES";

/// Whether PR #29's asset-body behaviour is on (see [`ASSET_BODIES_ENV`]).
pub fn asset_bodies_enabled() -> bool {
    std::env::var_os(ASSET_BODIES_ENV).is_none_or(|v| v != "0")
}

/// The `desiredKeys` of a note `changes/zone` request: [`NOTE_DESIRED_KEYS`],
/// plus `TextDataAsset` with asset bodies on.
pub fn note_desired_keys() -> Vec<&'static str> {
    let mut keys = NOTE_DESIRED_KEYS.to_vec();
    if asset_bodies_enabled() {
        keys.push(TEXT_DATA_ASSET_KEY);
    }
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
        if asset_bodies_enabled() {
            self.inline_asset_bodies(&mut records)?;
        }
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
        let mut zone_ids = Vec::new();
        let mut sync_token: Option<String> = None;
        let mut more_coming = true;
        while more_coming {
            let mut request = Map::new();
            if let Some(token) = truthy(sync_token.as_deref()) {
                request.insert("syncToken".into(), Value::String(token.to_owned()));
            }
            let body = self.post_database(DatabaseScope::Shared, "changes/database", &Value::Object(request))?;
            let page = parse_shared_zone_list(&body)?;
            zone_ids.extend(page.zone_ids);
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
        // Deliberate difference from icloud-md 0.6.2 (docs/PORT_PLAN.md §1):
        // a zone listed on two pages is fetched - and its notes cloned - once.
        let mut seen = std::collections::HashSet::new();
        zone_ids.retain(|z| seen.insert((z.zone_name.clone(), z.owner_record_name.clone())));
        Ok(zone_ids)
    }

    /// `fetchSharedNoteRecords`: every shared zone's records, bodies backfilled
    /// through `records/lookup`. `since_sync_tokens` is keyed by owner
    /// recordName. ZONE_NOT_FOUND and still-missing bodies skip the zone;
    /// any other zone-level error is fatal.
    pub fn fetch_shared_note_records(
        &self,
        since_sync_tokens: &IndexMap<String, String>,
        on_page: &mut dyn FnMut(usize),
    ) -> Result<SharedNoteRecords, CkError> {
        let zone_ids = self.fetch_shared_zone_ids()?;
        let mut result = SharedNoteRecords::default();
        for zone_id in zone_ids {
            let since = truthy(zone_id.owner_record_name.as_deref())
                .and_then(|owner| since_sync_tokens.get(owner))
                .map(String::as_str);
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
                if asset_bodies_enabled() {
                    self.inline_asset_bodies(&mut records)?;
                }
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
        match self.transport.download(url, dest) {
            Err(CkError::Http { status, .. }) => Err(CkError::RequestFailed(format!(
                "Attachment download failed: HTTP {status}"
            ))),
            other => other,
        }
    }

    /// `fetchAssetBytes`: [`Database::fetch_asset`] into memory.
    fn fetch_asset_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        match self.transport.download_bytes(url) {
            Err(CkError::Http { status, .. }) => Err(CkError::RequestFailed(format!(
                "Attachment download failed: HTTP {status}"
            ))),
            other => other,
        }
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
    for zone in zones {
        let zone_name = get(zone, "zoneID").and_then(|id| get_str(id, "zoneName"));
        let Some(zone_name) = zone_name else {
            return Err(CkError::UnexpectedResponse(
                "Unexpected zone shape in shared changes/database response".into(),
            ));
        };
        if get(zone, "deleted") == Some(&Value::Bool(true)) {
            continue;
        }
        zone_ids.push(ZoneId {
            zone_name: zone_name.to_owned(),
            owner_record_name: get(zone, "zoneID")
                .and_then(|id| get_str(id, "ownerRecordName"))
                .map(str::to_owned),
        });
    }
    Ok(SharedZoneListPage {
        zone_ids,
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
/// listing. Deliberate difference from icloud-md 0.6.2 (docs/PORT_PLAN.md
/// §1): CloudKit occasionally returns the same record on two pages of one
/// walk (and could within a page); 0.6.2 keeps every occurrence, so `clone`
/// writes the note twice - the second copy under a uniquified name, one of
/// the two left untracked - and the next `push` would create it as a new
/// note. Callers pass the records of a single zone, so the key is
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
