//! CloudKit wire types. Originally derived from icloud-md.
//!
//! Field maps are `IndexMap`, not `BTreeMap`: request field objects are
//! built in a deliberate order (`doc::encode` matches captured web-client
//! requests key for key), and JSON object order survives
//! serialization, so request bodies are only byte-equal if insertion order is
//! kept end to end.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `CloudKitFieldValue`: one field of a record as read from the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldValue {
    pub value: Value,
    #[serde(rename = "type")]
    pub type_: String,
}

/// `CloudKitRecordStamp`: CloudKit's record-level bookkeeping (distinct from
/// the Notes-level CreationDate/ModificationDate fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stamp {
    /// ms epoch.
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

/// `ShareParticipant`: one participant of a `cloudkit.share` record,
/// flattened from CloudKit's nested `userIdentity` shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_record_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub given_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
}

/// `CloudKitRecord`, as `parseRecord`/`parseZoneRecord` produce it.
///
/// Differences from the plan's sketch, following the TypeScript: the share
/// link is `share_record_name` (TS `shareRecordName`, from `share.recordName`),
/// and `current_user_permission` (from `currentUserParticipant.permission`,
/// `cloudkit.share` records only) is carried too - `folderLayout` needs it for
/// a shared folder's READ_ONLY/READ_WRITE. `deleted` is `Option<bool>` because
/// TS keeps `undefined` distinct from `false` (every reader tests `=== true`).
/// `participants` is `None` when the record had no `participants` array.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudKitRecord {
    pub record_name: String,
    pub record_type: String,
    pub fields: IndexMap<String, FieldValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_change_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_record_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<Stamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<Stamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<Participant>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_record_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_user_permission: Option<String>,
}

impl CloudKitRecord {
    /// `record.deleted === true`.
    pub fn is_deleted(&self) -> bool {
        self.deleted == Some(true)
    }
}

/// `CloudKitDatabase`: which of the container's databases to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseScope {
    Private,
    Shared,
}

impl DatabaseScope {
    pub fn as_str(self) -> &'static str {
        match self {
            DatabaseScope::Private => "private",
            DatabaseScope::Shared => "shared",
        }
    }
}

/// `CloudKitZoneID`. Serialized exactly as it goes on the wire
/// (`{"zoneName":"Notes"}` or `{"zoneName":"Notes","ownerRecordName":"_x"}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneId {
    pub zone_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_record_name: Option<String>,
}

/// `NoteZone`: the database + zone a note's reads and writes go to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NoteZone {
    pub database: DatabaseScope,
    pub zone_id: ZoneId,
}

/// `noteZone(sharedZoneOwner)`: own notes live in the private `Notes` zone; a
/// shared note in its sharer's `Notes` zone of the shared database.
pub fn note_zone(shared_zone_owner: Option<&str>) -> NoteZone {
    match shared_zone_owner {
        None => NoteZone {
            database: DatabaseScope::Private,
            zone_id: ZoneId {
                zone_name: "Notes".into(),
                owner_record_name: None,
            },
        },
        Some(owner) => NoteZone {
            database: DatabaseScope::Shared,
            zone_id: ZoneId {
                zone_name: "Notes".into(),
                owner_record_name: Some(owner.into()),
            },
        },
    }
}

/// `ZoneChangesResult`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ZoneChanges {
    pub records: Vec<CloudKitRecord>,
    pub sync_token: Option<String>,
    /// The caller's sync token was rejected (zone-level BAD_REQUEST) and the
    /// zone was refetched from scratch: `records` is a complete listing, not
    /// a delta, and carries no tombstones.
    pub resynced_from_scratch: bool,
}

/// `SharedZoneChanges`: one shared zone's note records, tagged with its owner.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedZoneChanges {
    pub zone_id: ZoneId,
    pub records: Vec<CloudKitRecord>,
    pub sync_token: Option<String>,
    pub resynced_from_scratch: bool,
}

/// `SkippedSharedZone`: a shared zone whose fetch didn't complete this run.
#[derive(Debug, Clone, PartialEq)]
pub enum SkippedSharedZone {
    /// `changes/zone` answered ZONE_NOT_FOUND for an advertised zone.
    ZoneNotFound { zone_id: ZoneId, server_error_code: String },
    /// Live Note records still had no `TextDataEncrypted` after the
    /// `records/lookup` backfill; the zone's sync token must not advance.
    MissingNoteBodies {
        zone_id: ZoneId,
        missing_record_names: Vec<String>,
    },
}

/// `SharedNoteRecordsResult`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedNoteRecords {
    /// Every shared zone this account has, in the cursor's order. A zone the
    /// shared database reported unchanged since the cursor is here with no
    /// records and its stored sync token, as an incremental `changes/zone`
    /// walk of it would have come back.
    pub zones: Vec<SharedZoneChanges>,
    pub skipped_zones: Vec<SkippedSharedZone>,
    /// Where the next fetch's `changes/database` listing resumes; `None`
    /// lists every zone from scratch next time.
    pub cursor: Option<SharedDatabaseCursor>,
}

/// Where the shared `changes/database` listing left off (state.json
/// `sharedDatabase`): its sync token and the shared zones known as of that
/// token, so a pull walks only the zones changed since and still knows the
/// rest are there (and which have gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDatabaseCursor {
    pub sync_token: String,
    pub zones: Vec<ZoneId>,
    /// ms epoch of the last listing from scratch.
    pub listed_at: i64,
}

/// `SharedZoneListPage`: one page of a shared `changes/database` listing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedZoneListPage {
    pub zone_ids: Vec<ZoneId>,
    /// Zones the listing marks `deleted` or `purged` (an incremental
    /// listing's way of saying a share went away).
    pub deleted_zone_ids: Vec<ZoneId>,
    pub more_coming: bool,
    pub sync_token: Option<String>,
}

/// `ParsedZone`: what `firstZone` extracts from a `changes/zone` page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedZone {
    pub more_coming: Option<bool>,
    pub sync_token: Option<String>,
    pub records: Option<Vec<CloudKitRecord>>,
}

/// A modify-request field (as `doc::encode` builds them,
/// `{ value: unknown }`): bare `{value}` with no `type`.
///
/// TS distinguishes three shapes that must stay distinct on the wire:
/// `{value: undefined}` serializes to `{}` ([`UpdateFieldValue::EMPTY`], the
/// deletion/create "placeholder trio"), `{value: null}` to `{"value":null}`
/// (the update path's NULL_FIELDS), and anything else to `{"value":...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateFieldValue {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "some_value")]
    pub value: Option<Value>,
}

fn some_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    // A present `"value": null` is `Some(Value::Null)`, not `None`.
    Value::deserialize(d).map(Some)
}

impl UpdateFieldValue {
    /// `{value: undefined}` - serializes as `{}`.
    pub const EMPTY: UpdateFieldValue = UpdateFieldValue { value: None };

    pub fn new(value: Value) -> Self {
        UpdateFieldValue { value: Some(value) }
    }

    /// `{value: null}` - serializes as `{"value":null}`.
    pub fn null() -> Self {
        UpdateFieldValue {
            value: Some(Value::Null),
        }
    }
}

/// Request field set, in insertion order.
pub type UpdateFields = IndexMap<String, UpdateFieldValue>;

/// `RecordUpdate` (and `NoteUpdate`, which is this with `record_type: "Note"`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordUpdate {
    pub record_name: String,
    pub record_type: String,
    /// Optimistic-concurrency token.
    pub record_change_tag: String,
    pub fields: UpdateFields,
    /// Sent as `parent: {recordName}` when present.
    pub parent_record_name: Option<String>,
}

/// `CreateNoteExtras`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateExtras {
    /// Sent as `record.parent = {recordName}`.
    pub parent_record_name: Option<String>,
    /// Sent as `record.createShortGUID = true`.
    pub create_short_guid: bool,
}

/// One `records/modify` operation, as `updateRecords` / `createZoneRecord`
/// build them. Serialization order within `record` is
/// `recordName, recordType, [recordChangeTag,] fields, [parent,] [createShortGUID]`.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordOp {
    /// `operationType: "update"`.
    Update(RecordUpdate),
    /// `operationType: "create"`: client-generated recordName, no change tag.
    Create {
        record_name: String,
        record_type: String,
        fields: UpdateFields,
        extras: CreateExtras,
    },
    /// `operationType: "delete"`: refused with `CONFLICT` if the record
    /// changed since `record_change_tag`.
    Delete {
        record_name: String,
        record_change_tag: String,
    },
}

/// `NoteUpdateResult` / `RecordUpdateResult`: per-record outcome of a
/// `records/modify`. A changeTag conflict is `Rejected { server_error_code:
/// "CONFLICT", .. }` inside an HTTP 200 - an expected outcome, not an error.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordUpdateResult {
    Ok(Box<CloudKitRecord>),
    Rejected {
        server_error_code: String,
        reason: Option<String>,
    },
}

impl RecordUpdateResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, RecordUpdateResult::Ok(_))
    }
}
