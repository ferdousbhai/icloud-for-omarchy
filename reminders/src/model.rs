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
    /// `Color`, as Apple stores it (JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `ReminderIDs`: the list's own order, bare UUIDs.
    #[serde(default)]
    pub order: Vec<String>,
}

impl List {
    /// `None` for anything but a live `List` record.
    pub fn from_record(r: &Record) -> Option<List> {
        if !r.is_type("List") || r.deleted || r.flag("Deleted") {
            return None;
        }
        let order = r
            .str("ReminderIDs")
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|id| id.trim_start_matches("Reminder/").to_owned())
            .collect();
        Some(List {
            id: r.name.clone(),
            name: r.str("Name").filter(|n| !n.is_empty()).unwrap_or("Untitled").to_owned(),
            color: r.str("Color").map(str::to_owned),
            order,
        })
    }

    /// `#rrggbb` from `Color` (`daHexString`, Apple's display value), if any.
    pub fn hex_color(&self) -> Option<String> {
        let v: Value = serde_json::from_str(self.color.as_deref()?).ok()?;
        let hex = v.get("daHexString")?.as_str()?;
        (hex.len() == 7 && hex.starts_with('#')).then(|| hex.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    /// `Reminder/<UUID>`.
    pub id: String,
    pub list_id: String,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Due>,
    /// 0 none, 1 high, 5 medium, 9 low (EventKit's scale).
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Alerts set on an Apple device (`Alarm` records). This app does not
    /// read or move them: they keep their time when the due date changes.
    #[serde(default)]
    pub alarms: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_tag: Option<String>,
    /// `ResolutionTokenMap` as last read, for [`bump_tokens`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    pub fn from_record(r: &Record) -> Parsed {
        if r.deleted {
            return Parsed::Gone(r.name.clone());
        }
        if !r.is_type("Reminder") {
            return Parsed::NotAReminder;
        }
        if r.flag("Deleted") {
            return Parsed::Gone(r.name.clone());
        }
        let doc = |key: &str, fallback: &str| match r.str(key) {
            Some(b64) => topotext::decode(b64).unwrap_or_else(|_| fallback.to_owned()),
            None => String::new(),
        };
        let mut title = doc("TitleDocument", "(unreadable title)");
        if title.is_empty() {
            title = "Untitled".into();
        }
        let due = r.int("DueDate").map(|wall_ms| Due {
            wall_ms,
            all_day: r.flag("AllDay"),
            time_zone: r.str("TimeZone").filter(|z| !z.is_empty()).map(str::to_owned),
        });
        Parsed::Live(Box::new(Reminder {
            id: r.name.clone(),
            list_id: r.reference("List").unwrap_or_default().to_owned(),
            title,
            notes: doc("NotesDocument", ""),
            completed: r.flag("Completed"),
            completed_ms: r.int("CompletionDate"),
            due,
            priority: r.int("Priority").unwrap_or(0),
            flagged: r.flag("Flagged"),
            parent_id: r.reference("ParentReminder").map(str::to_owned),
            alarms: r.strings("AlarmIDs").len(),
            created_ms: r.int("CreationDate").or(r.created_ms),
            modified_ms: r.int("LastModifiedDate").or(r.modified_ms),
            change_tag: r.change_tag.clone(),
            tokens: r.str("ResolutionTokenMap").map(str::to_owned),
        }))
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

impl Change {
    /// Applies the change to a cached copy (what the server now holds).
    pub fn apply(&self, r: &mut Reminder, now_ms: i64) {
        match self {
            Change::Title(t) => r.title = t.clone(),
            Change::Notes(n) => r.notes = n.clone(),
            Change::Completed(c) => {
                r.completed = *c;
                r.completed_ms = c.then_some(now_ms);
            }
            Change::Due(d) => r.due = d.clone(),
            Change::Deleted => {}
        }
        r.modified_ms = Some(now_ms);
    }
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
pub fn bump_tokens(existing: Option<&str>, keys: &[&str], replica: &str, now_ms: i64) -> String {
    let mut root = existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v.get("map").is_some_and(Value::is_object))
        .unwrap_or_else(|| json!({ "map": {} }));
    let map = root["map"].as_object_mut().expect("checked above");
    let time = now_ms as f64 / 1000.0 - APPLE_EPOCH_SECS;
    for key in keys {
        let counter = map
            .get(*key)
            .and_then(|t| t.get("counter"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        map.insert(
            (*key).to_owned(),
            json!({ "counter": counter + 1, "modificationTime": time, "replicaID": replica }),
        );
    }
    serde_json::to_string(&root).expect("JSON values serialize")
}

/// The records/modify operation for an update of `r` (whose change tag
/// and tokens are the ones last read).
pub fn update_op(r: &Reminder, changes: &[Change], replica: &str, now_ms: i64) -> Value {
    let (mut fields, keys) = update_fields(changes, now_ms);
    let tokens = bump_tokens(r.tokens.as_deref(), &keys, replica, now_ms);
    fields.insert("ResolutionTokenMap".into(), field("STRING", tokens.into()));
    let mut record = json!({ "recordName": r.id, "recordType": "Reminder", "fields": fields });
    if let Some(tag) = &r.change_tag {
        record["recordChangeTag"] = json!(tag);
    }
    json!({ "operationType": "update", "record": record })
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
        field("STRING", bump_tokens(None, &keys, replica, now_ms).into()),
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

    #[test]
    fn reads_pyicloud_s_reminder_fixture() {
        let Parsed::Live(r) = Reminder::from_record(&fixture_reminder()) else {
            panic!("live reminder")
        };
        assert_eq!(r.id, "Reminder/REM-FIXTURE");
        assert_eq!(r.uuid(), "REM-FIXTURE");
        assert_eq!(r.list_id, "List/LIST-A");
        assert_eq!(r.title, "Fixture Reminder");
        assert_eq!(r.notes, "Fixture notes");
        assert!(!r.completed);
        assert_eq!(
            r.due,
            Some(Due {
                wall_ms: 1_735_862_400_000,
                all_day: false,
                time_zone: None
            })
        );
        assert_eq!(r.created_ms, Some(1_735_689_600_000));
        assert_eq!(r.modified_ms, Some(1_735_776_000_000));
        assert_eq!(r.change_tag.as_deref(), Some("reminder-change-tag-fixture"));
        assert_eq!(r.alarms, 0);
    }

    #[test]
    fn reads_pyicloud_s_list_fixture() {
        let v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/pyicloud/reminders_query_lists_response.json"))
                .unwrap();
        let l = List::from_record(&record(v["records"][0].clone())).unwrap();
        assert_eq!(l.id, "List/LIST-A");
        assert_eq!(l.name, "Synthetic List");
        assert_eq!(l.order, vec!["REM-FIXTURE"]);
    }

    #[test]
    fn deleted_and_tombstoned_reminders_are_gone() {
        let mut r = fixture_reminder();
        r.fields.insert("Deleted".into(), json!({"type": "INT64", "value": 1}));
        assert_eq!(Reminder::from_record(&r), Parsed::Gone("Reminder/REM-FIXTURE".into()));
        let tomb = record(json!({"recordName": "Reminder/X", "deleted": true}));
        assert_eq!(Reminder::from_record(&tomb), Parsed::Gone("Reminder/X".into()));
        let list = record(json!({"recordName": "List/X", "recordType": "List", "fields": {}}));
        assert_eq!(Reminder::from_record(&list), Parsed::NotAReminder);
    }

    #[test]
    fn list_colors() {
        let mut l = List {
            id: "List/A".into(),
            name: "A".into(),
            color: Some(r##"{"daHexString":"#FF9500","ckSymbolicColorName":"orange"}"##.into()),
            order: vec![],
        };
        assert_eq!(l.hex_color().as_deref(), Some("#FF9500"));
        l.color = Some("garbage".into());
        assert_eq!(l.hex_color(), None);
    }

    #[test]
    fn tokens_are_raised_and_others_kept() {
        let existing = r#"{"map":{"completed":{"counter":4,"modificationTime":1.5,"replicaID":"A"},"titleDocument":{"counter":2,"modificationTime":1.0,"replicaID":"B"}}}"#;
        let out: Value =
            serde_json::from_str(&bump_tokens(Some(existing), &["completed", "lastModifiedDate"], "ME", 1_000_000))
                .unwrap();
        assert_eq!(out["map"]["completed"]["counter"], 5);
        assert_eq!(out["map"]["completed"]["replicaID"], "ME");
        assert_eq!(out["map"]["lastModifiedDate"]["counter"], 1);
        assert_eq!(out["map"]["titleDocument"]["counter"], 2);
        assert_eq!(out["map"]["titleDocument"]["replicaID"], "B");
        let t = out["map"]["completed"]["modificationTime"].as_f64().unwrap();
        assert!((t - (1000.0 - APPLE_EPOCH_SECS)).abs() < 1e-6, "{t}");
        // Unreadable or absent: a fresh map.
        for bad in [None, Some("nope"), Some(r#"{"map":3}"#)] {
            let out: Value = serde_json::from_str(&bump_tokens(bad, &["deleted"], "ME", 0)).unwrap();
            assert_eq!(out["map"].as_object().unwrap().len(), 1);
        }
    }

    #[test]
    fn completing_writes_completed_completion_date_and_tokens() {
        let Parsed::Live(r) = Reminder::from_record(&fixture_reminder()) else {
            panic!()
        };
        let op = update_op(&r, &[Change::Completed(true)], "ME", 1_790_000_000_000);
        assert_eq!(op["operationType"], "update");
        let rec = &op["record"];
        assert_eq!(rec["recordName"], "Reminder/REM-FIXTURE");
        assert_eq!(rec["recordChangeTag"], "reminder-change-tag-fixture");
        let f = &rec["fields"];
        assert_eq!(f["Completed"], json!({"type": "INT64", "value": 1}));
        assert_eq!(f["CompletionDate"], json!({"type": "TIMESTAMP", "value": 1_790_000_000_000i64}));
        assert_eq!(f["LastModifiedDate"]["value"], 1_790_000_000_000i64);
        assert!(f.get("TitleDocument").is_none(), "only what changed is written");
        let tokens: Value = serde_json::from_str(f["ResolutionTokenMap"]["value"].as_str().unwrap()).unwrap();
        let mut keys: Vec<&String> = tokens["map"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["completed", "completionDate", "lastModifiedDate"]);

        let op = update_op(&r, &[Change::Completed(false), Change::Due(None)], "ME", 5);
        let f = &op["record"]["fields"];
        assert_eq!(f["CompletionDate"]["value"], Value::Null);
        assert_eq!(f["DueDate"]["value"], Value::Null);
        assert_eq!(f["AllDay"]["value"], 0);
        assert_eq!(f["TimeZone"]["value"], Value::Null);
    }

    #[test]
    fn creating_matches_pyicloud_s_shape() {
        let due = Due {
            wall_ms: 1_791_622_800_000,
            all_day: false,
            time_zone: Some("Europe/Helsinki".into()),
        };
        let op = create_op("U-1", "List/A", "Milk", "", Some(&due), "ME", 7);
        assert_eq!(op["operationType"], "create");
        let rec = &op["record"];
        assert_eq!(rec["recordName"], "Reminder/U-1");
        assert_eq!(rec["parent"]["recordName"], "List/A");
        let f = &rec["fields"];
        assert_eq!(f["List"]["value"], json!({"recordName": "List/A", "action": "VALIDATE"}));
        assert_eq!(topotext::decode(f["TitleDocument"]["value"].as_str().unwrap()).unwrap(), "Milk");
        assert_eq!(f["DueDate"]["value"], 1_791_622_800_000i64);
        assert_eq!(f["TimeZone"]["value"], "Europe/Helsinki");
        assert_eq!(f["CreationDate"]["value"], 7);
        // It reads back as the reminder it creates.
        let mut back = rec.clone();
        back["recordType"] = json!("Reminder");
        let Parsed::Live(r) = Reminder::from_record(&record(back)) else {
            panic!()
        };
        assert_eq!((r.title.as_str(), r.list_id.as_str()), ("Milk", "List/A"));
        assert_eq!(r.due, Some(due));
    }
}
