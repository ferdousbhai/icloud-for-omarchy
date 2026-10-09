//! Lists and reminders as the records hold them, and the record writes
//! that change them.
//!
//! Field names, types and the write shapes are timlaing/pyicloud's
//! (`services/reminders/_mappers.py`, `_writes.py`): a `List` has `Name`,
//! `Color` (a JSON string), `Deleted` and `ReminderIDs` (a JSON array: the
//! list's order); a `Reminder` has `TitleDocument` and `NotesDocument`
//! ([`crate::topotext`]), `List` (a reference), `Completed`,
//! `CompletionDate`, `DueDate`/`AllDay`/`TimeZone` ([`crate::due`]),
//! `Priority`, `Flagged`, `Deleted`, `ParentReminder`, `AlarmIDs`,
//! `LastModifiedDate` and `ResolutionTokenMap`. Deleting is an update to
//! `Deleted = 1`, which Apple shows under Recently Deleted.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::cloudkit::Record;
use crate::due::Due;
use crate::topotext;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct List {
    /// `List/<UUID>`.
    pub id: String,
    pub name: String,
    /// `#rrggbb`: `Color`'s `daHexString`, Apple's display value.
    pub color: Option<String>,
    /// `ReminderIDs`: the list's own order, bare UUIDs. Empty when the list
    /// keeps it in `ReminderIDsAsset` instead (pyicloud: large lists), which
    /// this app does not download; such a list is ordered by due date and
    /// creation only.
    pub order: Vec<String>,
}

impl List {
    /// `None` for anything but a live `List` record; an error for a list
    /// whose fields are not what Apple writes.
    pub fn from_record(r: &Record) -> Result<Option<List>, String> {
        if !r.is_type("List") || r.deleted || r.flag("Deleted") {
            return Ok(None);
        }
        let bad = |what: &str| format!("{}: {what}", r.name);
        let order = match r.str("ReminderIDs") {
            None => Vec::new(),
            Some(s) => serde_json::from_str::<Vec<String>>(s)
                .map_err(|e| bad(&format!("ReminderIDs is not a JSON list of IDs ({e})")))?
                .into_iter()
                .map(|id| id.trim_start_matches("Reminder/").to_owned())
                .collect(),
        };
        let color = match r.str("Color") {
            None => None,
            Some(s) => serde_json::from_str::<Value>(s)
                .map_err(|e| bad(&format!("Color is not JSON ({e})")))?
                .get("daHexString")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        Ok(Some(List {
            id: r.name.clone(),
            name: r.str("Name").ok_or_else(|| bad("no Name"))?.to_owned(),
            color,
            order,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    /// `Reminder/<UUID>`.
    pub id: String,
    pub list_id: String,
    pub title: String,
    pub notes: String,
    pub completed: bool,
    pub completed_ms: Option<i64>,
    pub due: Option<Due>,
    /// 0 none, 1 high, 5 medium, 9 low (EventKit's scale).
    pub priority: i64,
    pub flagged: bool,
    pub parent_id: Option<String>,
    /// Alerts set on an Apple device (`Alarm` records). This app does not
    /// read or move them: they keep their time when the due date changes.
    pub alarms: usize,
    /// The record's CloudKit `created` and `modified` times (Unix ms).
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub change_tag: Option<String>,
    /// `ResolutionTokenMap` as last read, for [`bump_tokens`].
    pub tokens: Option<String>,
}

/// What a `Reminder` record says: a live reminder, or that it is gone
/// (deleted, a tombstone).
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Live(Box<Reminder>),
    Gone(String),
    NotAReminder,
}

impl Reminder {
    /// A live reminder, or that it is gone. A `Reminder` record without
    /// the title document or list Apple always writes, or with a document
    /// that does not decode, is an error naming the record: a reminder is
    /// never shown with a made-up title.
    pub fn from_record(r: &Record) -> Result<Parsed, String> {
        if r.deleted {
            return Ok(Parsed::Gone(r.name.clone()));
        }
        if !r.is_type("Reminder") {
            return Ok(Parsed::NotAReminder);
        }
        if r.flag("Deleted") {
            return Ok(Parsed::Gone(r.name.clone()));
        }
        let bad = |what: String| format!("{}: {what}", r.name);
        let doc = |key: &str| {
            r.str(key)
                .map(|b64| topotext::decode(b64).map_err(|e| bad(format!("{key}: {e}"))))
        };
        let title = match doc("TitleDocument") {
            Some(t) => t?,
            None => return Err(bad("no TitleDocument".into())),
        };
        let notes = doc("NotesDocument").transpose()?.unwrap_or_default();
        let due = match r.int("DueDate") {
            None => None,
            Some(wall_ms) => {
                let zone = r.str("TimeZone").map(str::to_owned);
                Some(Due::read(wall_ms, r.flag("AllDay"), zone).map_err(bad)?)
            }
        };
        Ok(Parsed::Live(Box::new(Reminder {
            id: r.name.clone(),
            list_id: r.reference("List").ok_or_else(|| bad("no List".into()))?.to_owned(),
            title,
            notes,
            completed: r.flag("Completed"),
            completed_ms: r.int("CompletionDate"),
            due,
            priority: r.int("Priority").unwrap_or(0),
            flagged: r.flag("Flagged"),
            parent_id: r.reference("ParentReminder").map(str::to_owned),
            alarms: r.strings("AlarmIDs").len(),
            created_ms: r.created_ms,
            modified_ms: r.modified_ms,
            change_tag: r.change_tag.clone(),
            tokens: r.str("ResolutionTokenMap").map(str::to_owned),
        })))
    }

    /// The bare UUID, as list orders and the command line use it.
    pub fn uuid(&self) -> &str {
        self.id.trim_start_matches("Reminder/")
    }
}

/// One change to a reminder.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Title(String),
    Notes(String),
    Completed(bool),
    Due(Option<Due>),
    Deleted,
}

fn field(t: &str, value: Value) -> Value {
    json!({ "type": t, "value": value })
}

fn timestamp(ms: Option<i64>) -> Value {
    field("TIMESTAMP", ms.map_or(Value::Null, Value::from))
}

/// The fields one update writes, and the `ResolutionTokenMap` keys they
/// bump (the field names in lowerCamelCase, as Apple's map names them).
pub fn update_fields(changes: &[Change], now_ms: i64) -> (Map<String, Value>, Vec<&'static str>) {
    let mut fields = Map::new();
    let mut keys = Vec::new();
    for change in changes {
        match change {
            Change::Title(t) => {
                fields.insert("TitleDocument".into(), field("STRING", topotext::encode(t).into()));
                keys.push("titleDocument");
            }
            Change::Notes(n) => {
                fields.insert("NotesDocument".into(), field("STRING", topotext::encode(n).into()));
                keys.push("notesDocument");
            }
            Change::Completed(c) => {
                fields.insert("Completed".into(), field("INT64", i64::from(*c).into()));
                fields.insert("CompletionDate".into(), timestamp(c.then_some(now_ms)));
                keys.extend(["completed", "completionDate"]);
            }
            Change::Due(due) => {
                let zone = due.as_ref().and_then(|d| d.time_zone.clone());
                fields.insert("DueDate".into(), timestamp(due.as_ref().map(|d| d.wall_ms)));
                fields.insert(
                    "AllDay".into(),
                    field("INT64", i64::from(due.as_ref().is_some_and(|d| d.all_day)).into()),
                );
                fields.insert("TimeZone".into(), field("STRING", zone.map_or(Value::Null, Value::from)));
                keys.extend(["dueDate", "allDay", "timeZone"]);
            }
            Change::Deleted => {
                fields.insert("Deleted".into(), field("INT64", 1.into()));
                keys.push("deleted");
            }
        }
    }
    fields.insert("LastModifiedDate".into(), timestamp(Some(now_ms)));
    keys.push("lastModifiedDate");
    (fields, keys)
}

/// Apple's reference date (2001-01-01) in Unix seconds: the map's
/// `modificationTime` counts from it.
const APPLE_EPOCH_SECS: f64 = 978_307_200.0;

/// `ResolutionTokenMap` after this write: Apple's per-field merge tokens
/// (`{"map":{"<field>":{"counter","modificationTime","replicaID"}}}`). The
/// fields written get their counter raised past the last one read, the
/// write's time and this computer's replica ID; every other entry is kept.
///
/// pyicloud writes a fresh map with counter 1 for the fields it writes;
/// raising the existing counter is this app's choice (remindd compares
/// counters with the last synced ones, per mattheworiordan/remi's
/// APPLE_REMINDERS_INTERNALS.md). UNVERIFIED AGAINST APPLE which of the two
/// an iPhone prefers when it holds an edit of its own.
///
/// `existing` is `None` for a new record, or one read without a map
/// (pyicloud's fixtures have none): a fresh map, as pyicloud always
/// writes. A map that is not that shape is an error: writing over it would
/// throw away Apple's merge state.
pub fn bump_tokens(existing: Option<&str>, keys: &[&str], replica: &str, now_ms: i64) -> Result<String, String> {
    let mut root = match existing {
        None => json!({ "map": {} }),
        Some(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(|v| v.get("map").is_some_and(Value::is_object))
            .ok_or("ResolutionTokenMap is not {\"map\":{...}}")?,
    };
    let map = root["map"].as_object_mut().expect("checked above");
    let time = now_ms as f64 / 1000.0 - APPLE_EPOCH_SECS;
    for key in keys {
        let counter = match map.get(*key) {
            None => 0,
            Some(t) => t
                .get("counter")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("ResolutionTokenMap's {key} has no counter"))?,
        };
        map.insert(
            (*key).to_owned(),
            json!({ "counter": counter + 1, "modificationTime": time, "replicaID": replica }),
        );
    }
    Ok(serde_json::to_string(&root).expect("JSON values serialize"))
}

/// The records/modify operation for an update of `r`, with the change tag
/// and tokens last read (CloudKit refuses it if the record has changed).
pub fn update_op(r: &Reminder, changes: &[Change], replica: &str, now_ms: i64) -> Result<Value, String> {
    let bad = |what: &str| format!("{}: {what}", r.id);
    let tag = r.change_tag.as_deref().ok_or_else(|| bad("no recordChangeTag"))?;
    let (mut fields, keys) = update_fields(changes, now_ms);
    let tokens = bump_tokens(r.tokens.as_deref(), &keys, replica, now_ms).map_err(|e| bad(&e))?;
    fields.insert("ResolutionTokenMap".into(), field("STRING", tokens.into()));
    Ok(json!({
        "operationType": "update",
        "record": { "recordName": r.id, "recordType": "Reminder", "recordChangeTag": tag, "fields": fields },
    }))
}

/// The records/modify operation creating a reminder in `list_id`, named
/// `Reminder/<uuid>`: pyicloud's `create` (its fields, its token map keys,
/// the list as the record's parent).
pub fn create_op(uuid: &str, list_id: &str, title: &str, notes: &str, due: Option<&Due>, replica: &str, now_ms: i64) -> Value {
    let mut keys = vec![
        "allDay",
        "titleDocument",
        "notesDocument",
        "parentReminder",
        "priority",
        "icsDisplayOrder",
        "creationDate",
        "list",
        "flagged",
        "completed",
        "completionDate",
        "lastModifiedDate",
        "recurrenceRuleIDs",
    ];
    let mut fields = Map::new();
    let mut put = |k: &str, v: Value| {
        fields.insert(k.to_owned(), v);
    };
    put("AllDay", field("INT64", i64::from(due.is_some_and(|d| d.all_day)).into()));
    put("Completed", field("INT64", 0.into()));
    put("CompletionDate", timestamp(None));
    put("CreationDate", timestamp(Some(now_ms)));
    put("Deleted", field("INT64", 0.into()));
    put("Flagged", field("INT64", 0.into()));
    put("Imported", field("INT64", 0.into()));
    put("LastModifiedDate", timestamp(Some(now_ms)));
    put("List", field("REFERENCE", json!({ "recordName": list_id, "action": "VALIDATE" })));
    put("NotesDocument", field("STRING", topotext::encode(notes).into()));
    put("Priority", field("INT64", 0.into()));
    put("TitleDocument", field("STRING", topotext::encode(title).into()));
    if let Some(d) = due {
        put("DueDate", timestamp(Some(d.wall_ms)));
        keys.push("dueDate");
        if let Some(zone) = &d.time_zone {
            put("TimeZone", field("STRING", zone.clone().into()));
            keys.push("timeZone");
        }
    }
    put(
        "ResolutionTokenMap",
        field("STRING", bump_tokens(None, &keys, replica, now_ms).expect("a fresh map").into()),
    );
    json!({
        "operationType": "create",
        "record": {
            "recordName": format!("Reminder/{uuid}"),
            "recordType": "Reminder",
            "fields": fields,
            "parent": { "recordName": list_id },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(v: Value) -> Record {
        Record::from_value(&v).unwrap()
    }

    /// timlaing/pyicloud tests/fixtures/reminders/reminders_query_reminders_response.json
    fn fixture_reminder() -> Record {
        let v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/pyicloud/reminders_query_reminders_response.json"))
                .unwrap();
        record(v["records"][0].clone())
    }

    fn live(r: &Record) -> Reminder {
        match Reminder::from_record(r).unwrap() {
            Parsed::Live(r) => *r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_pyicloud_s_reminder_fixture() {
        let r = live(&fixture_reminder());
        assert_eq!(r.id, "Reminder/REM-FIXTURE");
        assert_eq!(r.uuid(), "REM-FIXTURE");
        assert_eq!(r.list_id, "List/LIST-A");
        assert_eq!(r.title, "Fixture Reminder");
        assert_eq!(r.notes, "Fixture notes");
        assert!(!r.completed);
        assert_eq!(r.due, Some(Due::read(1_735_862_400_000, false, None).unwrap()));
        assert_eq!(r.created_ms, Some(1_735_689_600_000));
        assert_eq!(r.modified_ms, Some(1_735_776_000_000));
        assert_eq!(r.change_tag.as_deref(), Some("reminder-change-tag-fixture"));
    }

    #[test]
    fn reads_pyicloud_s_list_fixture() {
        let v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/pyicloud/reminders_query_lists_response.json"))
                .unwrap();
        let l = List::from_record(&record(v["records"][0].clone())).unwrap().unwrap();
        assert_eq!(l.id, "List/LIST-A");
        assert_eq!(l.name, "Synthetic List");
        assert_eq!(l.order, vec!["REM-FIXTURE"]);
    }

    #[test]
    fn deleted_and_tombstoned_reminders_are_gone() {
        let mut r = fixture_reminder();
        r.fields.insert("Deleted".into(), json!({"type": "INT64", "value": 1}));
        assert_eq!(Reminder::from_record(&r), Ok(Parsed::Gone("Reminder/REM-FIXTURE".into())));
        let tomb = record(json!({"recordName": "Reminder/X", "deleted": true}));
        assert_eq!(Reminder::from_record(&tomb), Ok(Parsed::Gone("Reminder/X".into())));
    }

    #[test]
    fn a_reminder_that_does_not_read_is_an_error_naming_it() {
        let mut r = fixture_reminder();
        r.fields.insert("TitleDocument".into(), json!({"type": "ENCRYPTED_BYTES", "value": "bm90IHpsaWI="}));
        let err = Reminder::from_record(&r).unwrap_err();
        assert!(err.starts_with("Reminder/REM-FIXTURE: TitleDocument"), "{err}");
        let mut r = fixture_reminder();
        r.fields.insert("TimeZone".into(), json!({"type": "STRING", "value": "Mars/Olympus"}));
        assert!(Reminder::from_record(&r).unwrap_err().contains("Mars/Olympus"));
    }

    #[test]
    fn tokens_are_raised_and_others_kept() {
        let existing = r#"{"map":{"completed":{"counter":4,"modificationTime":1.5,"replicaID":"A"},"titleDocument":{"counter":2,"modificationTime":1.0,"replicaID":"B"}}}"#;
        let out: Value = serde_json::from_str(
            &bump_tokens(Some(existing), &["completed", "lastModifiedDate"], "ME", 1_000_000).unwrap(),
        )
        .unwrap();
        assert_eq!(out["map"]["completed"]["counter"], 5);
        assert_eq!(out["map"]["completed"]["replicaID"], "ME");
        assert_eq!(out["map"]["lastModifiedDate"]["counter"], 1);
        assert_eq!(
            out["map"]["titleDocument"],
            json!({"counter": 2, "modificationTime": 1.0, "replicaID": "B"})
        );
        let t = out["map"]["completed"]["modificationTime"].as_f64().unwrap();
        assert!((t - (1000.0 - APPLE_EPOCH_SECS)).abs() < 1e-6, "{t}");
        assert!(bump_tokens(Some(r#"{"map":3}"#), &["deleted"], "ME", 0).is_err());
    }

    #[test]
    fn completing_writes_only_what_changed() {
        let r = live(&fixture_reminder());
        let op = update_op(&r, &[Change::Completed(true)], "ME", 1_790_000_000_000).unwrap();
        assert_eq!(op["operationType"], "update");
        let rec = &op["record"];
        assert_eq!(rec["recordChangeTag"], "reminder-change-tag-fixture");
        let f = &rec["fields"];
        assert_eq!(f["Completed"], json!({"type": "INT64", "value": 1}));
        assert_eq!(f["CompletionDate"], json!({"type": "TIMESTAMP", "value": 1_790_000_000_000i64}));
        let mut names: Vec<&String> = f.as_object().unwrap().keys().collect();
        names.sort();
        assert_eq!(names, ["Completed", "CompletionDate", "LastModifiedDate", "ResolutionTokenMap"]);

        let op = update_op(&r, &[Change::Completed(false), Change::Due(None)], "ME", 5).unwrap();
        let f = &op["record"]["fields"];
        assert_eq!(f["CompletionDate"]["value"], Value::Null);
        assert_eq!(f["DueDate"]["value"], Value::Null);
        assert_eq!(f["TimeZone"]["value"], Value::Null);
    }

    #[test]
    fn creating_matches_pyicloud_s_shape_and_reads_back() {
        let due = Due::read(1_791_622_800_000, false, Some("Europe/Helsinki".into())).unwrap();
        let op = create_op("U-1", "List/A", "Milk", "", Some(&due), "ME", 7);
        assert_eq!(op["operationType"], "create");
        let rec = &op["record"];
        assert_eq!(rec["parent"]["recordName"], "List/A");
        let f = &rec["fields"];
        assert_eq!(f["List"]["value"], json!({"recordName": "List/A", "action": "VALIDATE"}));
        assert_eq!(f["DueDate"]["value"], 1_791_622_800_000i64);
        assert_eq!(f["TimeZone"]["value"], "Europe/Helsinki");
        let r = live(&record(rec.clone()));
        assert_eq!((r.title.as_str(), r.list_id.as_str()), ("Milk", "List/A"));
        assert_eq!(r.due, Some(due));
    }
}

