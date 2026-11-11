//! `carry-over`: gives one tracked note what Notes shows of another besides
//! its text. `icloud-notes renew` runs it between pushing the fresh copy of a
//! note and trashing the old one, so the copy stands where the old note
//! stood: pinned if it was, with its creation date, paper style and
//! attachment view.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Error;
use super::remote::{Connector, DefaultConnector, resolve_folder_account};
use crate::cloudkit::{CloudKitRecord, RecordUpdate, RecordUpdateResult, UpdateFieldValue, UpdateFields, note_zone};
use crate::vault::history::resolve_tracked_note;
use crate::vault::migrate::require_vault;
use crate::vault::state::write_clone_state;

/// The Note record fields that are not its text, title or place, yet change
/// how Notes shows it. A field the source lacks is left as it is.
pub const CARRIED_FIELDS: [&str; 4] = ["IsPinned", "CreationDate", "PaperStyleType", "AttachmentViewType"];

/// `carry-over`'s result: the two notes and the fields that were copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarryOverResult {
    pub from: String,
    pub to: String,
    pub carried: Vec<String>,
}

/// The fields of `from` to write on `to`: each of [`CARRIED_FIELDS`] that
/// `from` has and `to` doesn't have with the same value.
pub fn carried_fields(from: &CloudKitRecord, to: &CloudKitRecord) -> UpdateFields {
    let mut fields = UpdateFields::new();
    for name in CARRIED_FIELDS {
        let Some(value) = from.fields.get(name).map(|f| &f.value).filter(|v| !v.is_null()) else {
            continue;
        };
        if to.fields.get(name).map(|f| &f.value) != Some(value) {
            fields.insert(name.into(), UpdateFieldValue::new(Value::clone(value)));
        }
    }
    fields
}

/// `carry-over FROM TO`.
pub fn run_carry_over(target_dir: &Path, from: &str, to: &str) -> Result<CarryOverResult, Error> {
    run_carry_over_with(&DefaultConnector, target_dir, from, to)
}

/// `carry-over` over an explicit connector.
pub fn run_carry_over_with(
    connector: &dyn Connector,
    target_dir: &Path,
    from: &str,
    to: &str,
) -> Result<CarryOverResult, Error> {
    let mut state = require_vault(target_dir, &mut |_| {})?;
    let source = resolve_tracked_note(&state, from, target_dir)?;
    let target = resolve_tracked_note(&state, to, target_dir)?;
    for note in [&source, &target] {
        if note.entry.shared_zone_owner.is_some() {
            return Err(Error::Internal(format!(
                "\"{}\" is shared by someone else; only your own notes can be carried over.",
                note.entry.file
            )));
        }
    }
    let remote = resolve_folder_account(connector, target_dir, state.account.as_ref())?;
    let zone = note_zone(None);
    let records = remote
        .db
        .lookup_records(&zone, &[source.record_name.clone(), target.record_name.clone()])?;
    let find = |name: &str, file: &str| {
        records
            .iter()
            .find(|r| r.record_name == name)
            .ok_or_else(|| Error::Internal(format!("\"{file}\" is no longer in iCloud.")))
    };
    let from_record = find(&source.record_name, &source.entry.file)?;
    let to_record = find(&target.record_name, &target.entry.file)?;
    let fields = carried_fields(from_record, to_record);
    let carried: Vec<String> = fields.keys().cloned().collect();
    if !fields.is_empty() {
        let update = RecordUpdate {
            record_name: target.record_name.clone(),
            record_type: "Note".into(),
            record_change_tag: to_record.record_change_tag.clone().unwrap_or_default(),
            fields,
            parent_record_name: to_record.parent_record_name.clone(),
        };
        match remote.db.update_note_record(&zone, &update)? {
            RecordUpdateResult::Ok(updated) => {
                if let (Some(entry), Some(tag)) = (state.notes.get_mut(&target.record_name), &updated.record_change_tag)
                {
                    entry.record_change_tag = tag.clone();
                    write_clone_state(target_dir, &state)?;
                }
            }
            RecordUpdateResult::Rejected {
                server_error_code,
                reason,
            } => {
                return Err(Error::Internal(format!(
                    "iCloud refused to carry \"{}\" over to \"{}\": {server_error_code}{}",
                    source.entry.file,
                    target.entry.file,
                    reason
                        .filter(|r| !r.is_empty())
                        .map(|r| format!(" ({r})"))
                        .unwrap_or_default()
                )));
            }
        }
    }
    Ok(CarryOverResult {
        from: source.entry.file,
        to: target.entry.file,
        carried,
    })
}
