//! `pull`. Ports icloud-md `src/commands/pull.ts` and `src/cli/pullReport.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::remote::{Connector, DefaultConnector, resolve_folder_account};
use super::report::{LISTING_INDENT, labelled_line, remark_line};
use super::{
    Error, NoticeLevel, SyncNotice, SyncProgress, is_purged, skipped_zone_owner, used_names_for, zone_for_owner,
};
use crate::cloudkit::client::{merge_looked_up_records, needs_body_lookup};
use crate::cloudkit::{CloudKitRecord, DatabaseScope, NoteZone, SharedZoneChanges, Transport, note_zone};
use crate::cloudkit::{Database, SkippedSharedZone};
use crate::diff3::{has_conflict_markers, merge_note_versions};
use crate::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
use crate::js::posix;
use crate::md::filename::{file_name_carries_title, note_file_name_for, title_needing_frontmatter, unique_file_name};
use crate::md::frontmatter::{NOTE_TITLE_KEY, clear_note_id, compose_note_file, join_frontmatter, read_note_id, split_frontmatter};
use crate::md::title::representability_problem;
use crate::vault::attachments::{
    remove_attachments_for_note, remove_table_attachments_for_note, resolve_note_attachments, safe_unlink,
};
use crate::vault::base::{read_base_copy, remove_base_copy, write_base_copy};
use crate::vault::epoch::record_epoch;
use crate::vault::folders::{reconcile_note_placements, remove_stale_dirs};
use crate::vault::history::{VersionSnapshotInput, record_version};
use crate::vault::layout::{
    PreviousLayout, SharedZoneRecords, build_vault_layout, note_dir_of, place_note, previous_layout_dirs,
};
use crate::vault::local::{
    LocalFileState, apply_note_file_times, local_file_state, modification_date_of, read_text, split_options,
};
use crate::vault::migrate::require_vault;
use crate::vault::pairing::{claim_names_on_disk, file_exists, pending_rename_target, settle_pending_renames};
use crate::vault::state::{
    AttachmentEntry, CloneState, FolderEntry, NOTE_ADD_ORDER, NoteEntry, PULL_WRITE_ORDER, TableAttachmentEntry,
    TitleMode, write_clone_state,
};

/// `PullOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullOptions {
    pub defer_renames: bool,
}

/// `PullChangeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullChangeKind {
    Add,
    Update,
    Merge,
    Remove,
    Move,
    Untrack,
}

/// `PullChangeRemark`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullChangeRemark {
    /// "conflict" | "unsyncable" | "note".
    pub tone: String,
    pub message: String,
}

impl PullChangeRemark {
    fn new(tone: &str, message: impl Into<String>) -> Self {
        PullChangeRemark {
            tone: tone.into(),
            message: message.into(),
        }
    }
}

/// `PullChange`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullChange {
    pub kind: PullChangeKind,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remarks: Option<Vec<PullChangeRemark>>,
}

impl PullChange {
    pub fn new(kind: PullChangeKind, file: impl Into<String>) -> Self {
        PullChange {
            kind,
            file: file.into(),
            previous_file: None,
            pending_rename: None,
            remarks: None,
        }
    }
}

/// `PullSummary`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullSummary {
    pub added: usize,
    pub updated: usize,
    pub merged: usize,
    pub removed: usize,
    pub attachments_downloaded: usize,
    pub unpublishable: usize,
    pub skipped_new_unsyncable: usize,
    pub dropped_unsyncable: usize,
    pub unshared_untracked: usize,
    pub changes: Vec<PullChange>,
    pub conflicts: Vec<String>,
    pub notices: Vec<SyncNotice>,
}

fn read_only_remark() -> PullChangeRemark {
    PullChangeRemark::new(
        "unsyncable",
        "read-only: contains content this tool couldn't fully parse",
    )
}

/// `remarksFor`: `None` rather than an empty list.
fn remarks_for(leading: &[PullChangeRemark], unpublishable_reason: Option<&str>) -> Option<Vec<PullChangeRemark>> {
    let mut remarks = leading.to_vec();
    if unpublishable_reason.is_some() {
        remarks.push(read_only_remark());
    }
    if remarks.is_empty() { None } else { Some(remarks) }
}

/// `runPull`.
pub fn run_pull(
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &PullOptions,
) -> Result<PullSummary, Error> {
    run_pull_with(&DefaultConnector, target_dir, progress, on_status, options)
}

/// One source of records: the private zone or one shared zone.
struct Source {
    records: Vec<CloudKitRecord>,
    shared_zone_owner: Option<String>,
    resynced: bool,
}

/// The mutable pieces of tracked state a pull rewrites.
struct Tracked {
    notes: IndexMap<String, NoteEntry>,
    attachments: IndexMap<String, AttachmentEntry>,
    table_attachments: IndexMap<String, TableAttachmentEntry>,
}

/// `runPull` over an explicit connector.
pub fn run_pull_with(
    connector: &dyn Connector,
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &PullOptions,
) -> Result<PullSummary, Error> {
    let state = require_vault(target_dir, on_status)?;
    let title_mode = state.mode();
    let defer_renames = options.defer_renames;

    let remote = resolve_folder_account(connector, target_dir, state.account.as_ref())?;
    let db = &remote.db;

    progress.on_fetch_start();
    let mut fetched = 0usize;
    let mut changes = {
        let mut on_page = |n: usize| {
            fetched += n;
            progress.on_fetch_page(fetched);
        };
        db.fetch_all_note_records(state.sync_token.as_deref(), &mut on_page)?
    };
    let mut shared = {
        let mut on_page = |n: usize| {
            fetched += n;
            progress.on_fetch_page(fetched);
        };
        db.fetch_shared_note_records(&state.shared_zone_sync_tokens.clone().unwrap_or_default(), &mut on_page)?
    };
    backfill_share_permissions(db, state.folders.as_ref(), &mut shared.zones)?;
    let held_back = backfill_new_note_bodies(db, &state.notes, &mut changes.records)?;

    let mut tracked = Tracked {
        notes: state.notes.clone(),
        attachments: state.attachments.clone().unwrap_or_default(),
        table_attachments: state.table_attachments.clone().unwrap_or_default(),
    };
    let mut trashed = state.trashed.clone().unwrap_or_default();
    let settled = settle_pending_renames(target_dir, &mut tracked.notes, !defer_renames, title_mode)?;

    let mut used_file_names: HashMap<String, HashSet<String>> = HashMap::new();
    for entry in tracked.notes.values() {
        used_names_for(&mut used_file_names, &note_dir_of(&entry.file)).insert(posix::basename(&entry.file).to_owned());
        if let Some(pending) = pending_rename_target(entry) {
            used_names_for(&mut used_file_names, &note_dir_of(&pending)).insert(posix::basename(&pending).to_owned());
        }
    }
    let mut used_attachment_file_names: HashMap<String, HashSet<String>> = HashMap::new();
    for entry in tracked.attachments.values() {
        used_names_for(
            &mut used_attachment_file_names,
            &note_dir_of(&posix::dirname(&entry.file)),
        )
        .insert(posix::basename(&entry.file).to_owned());
    }

    let mut summary = PullSummary::default();
    for done in &settled.performed {
        let mut change = PullChange::new(PullChangeKind::Move, done.to.clone());
        change.previous_file = Some(done.from.clone());
        summary.changes.push(change);
    }
    for stuck in &settled.blocked {
        summary.notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!(
                "Couldn't rename {} to {}: something else is already there. The rename stays pending.",
                stuck.file, stuck.to
            ),
        });
    }

    let mut sources = vec![Source {
        records: changes.records.clone(),
        shared_zone_owner: None,
        resynced: changes.resynced_from_scratch,
    }];
    let mut shared_zone_sync_tokens: IndexMap<String, String> = IndexMap::new();
    let mut shared_zone_records: Vec<SharedZoneRecords> = Vec::new();
    for zone in &shared.zones {
        let owner = zone.zone_id.owner_record_name.clone().filter(|o| !o.is_empty());
        if let (Some(owner), Some(token)) = (&owner, zone.sync_token.as_ref().filter(|t| !t.is_empty())) {
            shared_zone_sync_tokens.insert(owner.clone(), token.clone());
        }
        if let Some(owner) = &owner {
            shared_zone_records.push(SharedZoneRecords {
                owner_record_name: owner.clone(),
                records: zone.records.clone(),
            });
        }
        sources.push(Source {
            records: zone.records.clone(),
            shared_zone_owner: zone.zone_id.owner_record_name.clone(),
            resynced: zone.resynced_from_scratch,
        });
    }
    for skipped in &shared.skipped_zones {
        let owner = skipped_zone_owner(skipped).filter(|o| !o.is_empty());
        if let Some(owner) = owner
            && let Some(carried) = state
                .shared_zone_sync_tokens
                .as_ref()
                .and_then(|t| t.get(owner))
                .filter(|t| !t.is_empty())
        {
            shared_zone_sync_tokens.insert(owner.to_owned(), carried.clone());
        }
        let owner = skipped_zone_owner(skipped).unwrap_or("unknown");
        let message = match skipped {
            SkippedSharedZone::ZoneNotFound { server_error_code, .. } => format!(
                "Skipped a shared zone the server no longer has (owner {owner}, {server_error_code}) - its share was \
                 likely revoked or deleted; its local notes were left in place and stay tracked for now"
            ),
            SkippedSharedZone::MissingNoteBodies {
                missing_record_names, ..
            } => format!(
                "Skipped the shared zone owned by {owner} this run: {} note(s) came through without their text and \
                 looking them up didn't fill it in - nothing from that zone was touched, and the next pull will retry it",
                missing_record_names.len()
            ),
        };
        summary.notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message,
        });
    }

    if !held_back.is_empty() {
        summary.notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!(
                "Skipped {} new note(s) that came through without their text, even when looked up - this vault's \
                 sync token was kept where it was, so the next pull will look for them again",
                held_back.len()
            ),
        });
    }

    let layout = build_vault_layout(&changes.records, &shared_zone_records, PreviousLayout::of(&state));
    for dir in &layout.all_dirs {
        std::fs::create_dir_all(target_dir.join(dir))?;
    }

    let total: usize = sources.iter().map(|s| s.records.len()).sum();
    progress.on_process_start(total);

    for source in &sources {
        for record in &source.records {
            let result = (|| -> Result<(), Error> {
                if record.record_type != "Note" {
                    return Ok(());
                }
                let existing = tracked.notes.get(&record.record_name).cloned();
                // Already at this version: a resync re-listing it, or the
                // push just before this pull writing it. Nothing to apply.
                if let (Some(existing), Some(tag)) = (&existing, &record.record_change_tag)
                    && *tag == existing.record_change_tag
                {
                    return Ok(());
                }

                let decoded = match classify_note_record(record, &ClassifyOptions { title_mode }) {
                    NoteDecodeResult::Deleted => {
                        if trashed.contains_key(&record.record_name) && (record.is_deleted() || is_purged(record)) {
                            trashed.shift_remove(&record.record_name);
                        }
                        if let Some(existing) = &existing {
                            handle_remote_deletion(
                                target_dir,
                                &record.record_name,
                                existing,
                                &mut tracked,
                                &mut summary,
                                title_mode,
                            )?;
                        }
                        return Ok(());
                    }
                    NoteDecodeResult::Unsyncable(reason) => {
                        use crate::doc::decode::UnsyncableReason;
                        if reason == UnsyncableReason::MissingBody {
                            match &existing {
                                Some(existing) => summary.notices.push(SyncNotice {
                                    level: NoticeLevel::Warn,
                                    message: format!(
                                        "{}: the server sent this note without its text this run - left unchanged and still tracked",
                                        existing.file
                                    ),
                                }),
                                None => summary.skipped_new_unsyncable += 1,
                            }
                            return Ok(());
                        }
                        let Some(existing) = &existing else {
                            summary.skipped_new_unsyncable += 1;
                            return Ok(());
                        };
                        drop_unsyncable_note(
                            target_dir,
                            &record.record_name,
                            existing,
                            &mut tracked,
                            &mut summary,
                            reason.as_str(),
                            title_mode,
                        )?;
                        return Ok(());
                    }
                    NoteDecodeResult::Ok(decoded) => decoded,
                };

                let recorded_title = title_needing_frontmatter(&decoded.title_line, title_mode);
                let title_remarks: Vec<PullChangeRemark> = match &recorded_title {
                    None => Vec::new(),
                    Some(title) => vec![PullChangeRemark::new(
                        "note",
                        format!(
                            "title kept in {NOTE_TITLE_KEY}: {}",
                            representability_problem(title).unwrap_or_else(|| "a file name can't carry it".into())
                        ),
                    )],
                };
                let mut recorded_new_snapshot = false;
                if let Some(text) = record.fields.get("TextDataEncrypted").and_then(|f| f.value.as_str()) {
                    recorded_new_snapshot |= record_version(
                        target_dir,
                        &VersionSnapshotInput::note(
                            &record.record_name,
                            record.record_change_tag.as_deref().unwrap_or(""),
                            text,
                        ),
                    )?;
                }

                let placement = place_note(&layout, record, source.shared_zone_owner.as_deref());
                let note_dir = match &existing {
                    Some(e) => note_dir_of(&e.file),
                    None => placement.dir.clone(),
                };

                let mut body_text = decoded.markdown_text.clone();
                let unpublishable_reason = decoded.unpublishable_reason.clone();
                if !decoded.embed_slots.is_empty() {
                    let resolved = resolve_note_attachments(
                        db,
                        &zone_for_owner(source.shared_zone_owner.as_deref()),
                        target_dir,
                        &record.record_name,
                        &decoded.markdown_text,
                        &decoded.embed_slots,
                        &tracked.attachments,
                        &tracked.table_attachments,
                        used_names_for(&mut used_attachment_file_names, &note_dir),
                        &note_dir,
                    )?;
                    body_text = resolved.body_text;
                    for stale in &resolved.stale_attachment_record_names {
                        if let Some(entry) = tracked.attachments.get(stale) {
                            safe_unlink(&target_dir.join(&entry.file))?;
                        }
                        tracked.attachments.shift_remove(stale);
                    }
                    summary.attachments_downloaded += resolved.attachments.len();
                    tracked.attachments.extend(resolved.attachments);
                    for stale in &resolved.stale_table_attachment_record_names {
                        tracked.table_attachments.shift_remove(stale);
                    }
                    tracked.table_attachments.extend(resolved.table_attachments);
                    for snapshot in &resolved.table_attachment_snapshots {
                        recorded_new_snapshot |= record_version(
                            target_dir,
                            &VersionSnapshotInput::table(
                                &snapshot.record_name,
                                &snapshot.record_change_tag,
                                &snapshot.value_base64,
                                &snapshot.note_record_name,
                            ),
                        )?;
                    }
                }
                if unpublishable_reason.as_deref().is_some_and(|r| !r.is_empty()) {
                    summary.unpublishable += 1;
                }

                if recorded_new_snapshot {
                    let mut associated = vec![record.record_name.clone()];
                    associated.extend(
                        tracked
                            .table_attachments
                            .iter()
                            .filter(|(_, e)| e.note_record_name == record.record_name)
                            .map(|(k, _)| k.clone()),
                    );
                    record_epoch(target_dir, &record.record_name, &associated)?;
                }

                let Some(existing) = existing else {
                    let used_in_dir = used_names_for(&mut used_file_names, &note_dir);
                    claim_names_on_disk(target_dir, &note_dir, used_in_dir)?;
                    let file_name = unique_file_name(&note_file_name_for(&decoded.title_line, title_mode), used_in_dir);
                    used_in_dir.insert(file_name.clone());
                    let relative_file = posix::join(&[&note_dir, &file_name]);
                    let file_path = target_dir.join(&relative_file);
                    std::fs::write(
                        &file_path,
                        compose_note_file("", &body_text, &record.record_name, recorded_title.as_deref()),
                    )?;
                    apply_note_file_times(&file_path, record)?;
                    write_base_copy(target_dir, &record.record_name, &body_text)?;
                    tracked.notes.insert(
                        record.record_name.clone(),
                        NoteEntry {
                            file: relative_file.clone(),
                            record_change_tag: record.record_change_tag.clone().unwrap_or_default(),
                            modification_date: modification_date_of(record),
                            shared_zone_owner: source.shared_zone_owner.clone(),
                            unpublishable_reason: unpublishable_reason.clone(),
                            folder_record_name: placement.folder_record_name.clone(),
                            pending_rename: None,
                            frontmatter_title: recorded_title.clone(),
                            key_order: Some(NOTE_ADD_ORDER.to_vec()),
                        },
                    );
                    summary.added += 1;
                    let mut change = PullChange::new(PullChangeKind::Add, relative_file);
                    change.remarks = remarks_for(&title_remarks, unpublishable_reason.as_deref());
                    summary.changes.push(change);
                    return Ok(());
                };

                let local = local_file_state(target_dir, &existing, &record.record_name, title_mode)?;
                if local == LocalFileState::Missing {
                    // Moved or renamed here and not pushed yet: the file that
                    // carries this note's id takes the remote change, merged
                    // against the old base, and the next push pairs the move.
                    let claimants = note_id_claimants(target_dir, &tracked.notes, &record.record_name, title_mode)?;
                    if !claimants.is_empty() {
                        let mut statuses = Vec::new();
                        for claimant in &claimants {
                            statuses.push(merge_remote_change_into_local_file(
                                target_dir,
                                &record.record_name,
                                claimant,
                                &body_text,
                                recorded_title.as_deref(),
                                title_mode,
                            )?);
                        }
                        if statuses.contains(&MergeStatus::UnresolvedMarkers) {
                            summary.conflicts.push(format!(
                                "{}: still contains diff3 conflict markers - resolve them, then run \"push\" to reconcile",
                                claimants.join(", ")
                            ));
                            return Ok(());
                        }
                        let conflicted = statuses.contains(&MergeStatus::Conflict);
                        if !conflicted {
                            write_base_copy(target_dir, &record.record_name, &body_text)?;
                        }
                        let mut entry = existing.clone();
                        if let Some(tag) = &record.record_change_tag {
                            entry.record_change_tag = tag.clone();
                        }
                        entry.modification_date = modification_date_of(record);
                        entry.unpublishable_reason = unpublishable_reason.clone();
                        entry.frontmatter_title = recorded_title.clone();
                        entry.pending_rename = None;
                        tracked.notes.insert(record.record_name.clone(), entry);
                        for claimant in &claimants {
                            if conflicted {
                                summary
                                    .conflicts
                                    .push(format!("{claimant}: merged with conflict markers - resolve manually"));
                            } else {
                                summary.merged += 1;
                            }
                            summary.changes.push(PullChange::new(PullChangeKind::Merge, claimant.clone()));
                        }
                        return Ok(());
                    }
                    // Deleted here: the file stays gone and the next push moves
                    // the note to Recently Deleted. Tracking follows the remote
                    // record, so the base copy is the text being deleted.
                    // (0.6.2 writes the file back.)
                    write_base_copy(target_dir, &record.record_name, &body_text)?;
                    let mut entry = existing.clone();
                    if let Some(tag) = &record.record_change_tag {
                        entry.record_change_tag = tag.clone();
                    }
                    entry.modification_date = modification_date_of(record);
                    entry.unpublishable_reason = unpublishable_reason.clone();
                    entry.folder_record_name = placement.folder_record_name.clone();
                    entry.frontmatter_title = recorded_title.clone();
                    entry.pending_rename = None;
                    tracked.notes.insert(record.record_name.clone(), entry);
                    summary.notices.push(SyncNotice {
                        level: NoticeLevel::Info,
                        message: format!(
                            "{}: deleted here and changed in iCloud since - the next push moves it to Recently Deleted",
                            existing.file
                        ),
                    });
                    return Ok(());
                }
                let (file, pending_rename) = rename_for_remote_title(
                    target_dir,
                    &existing.file,
                    &decoded.title_line,
                    title_mode,
                    &mut used_file_names,
                    local != LocalFileState::Missing,
                    defer_renames,
                )?;
                let previous_file = (file != existing.file).then(|| existing.file.clone());
                let deferred = pending_rename.as_ref().map(|p| posix::join(&[&note_dir_of(&file), p]));
                let deferred_remarks: Vec<PullChangeRemark> = match &pending_rename {
                    None => Vec::new(),
                    Some(p) => vec![PullChangeRemark::new(
                        "note",
                        format!("rename deferred: this file should become \"{p}\""),
                    )],
                };
                let info_remarks: Vec<PullChangeRemark> =
                    title_remarks.iter().chain(&deferred_remarks).cloned().collect();
                let updated_entry = |existing: &NoteEntry| -> NoteEntry {
                    let mut entry = existing.clone();
                    entry.file = file.clone();
                    if let Some(tag) = &record.record_change_tag {
                        entry.record_change_tag = tag.clone();
                    }
                    entry.modification_date = modification_date_of(record);
                    entry.unpublishable_reason = unpublishable_reason.clone();
                    entry.folder_record_name = placement.folder_record_name.clone();
                    entry.frontmatter_title = recorded_title.clone();
                    entry.pending_rename = pending_rename.clone();
                    entry
                };
                let change_with = |kind: PullChangeKind, remarks: Option<Vec<PullChangeRemark>>| PullChange {
                    kind,
                    file: file.clone(),
                    previous_file: previous_file.clone(),
                    pending_rename: deferred.clone(),
                    remarks,
                };

                if local == LocalFileState::Clean {
                    let file_path = target_dir.join(&file);
                    let text = read_text(&file_path)?.ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("{}: not found", file_path.display()),
                        )
                    })?;
                    let frontmatter = split_frontmatter(&text, split_options(title_mode)).frontmatter;
                    std::fs::write(
                        &file_path,
                        compose_note_file(&frontmatter, &body_text, &record.record_name, recorded_title.as_deref()),
                    )?;
                    apply_note_file_times(&file_path, record)?;
                    write_base_copy(target_dir, &record.record_name, &body_text)?;
                    tracked
                        .notes
                        .insert(record.record_name.clone(), updated_entry(&existing));
                    summary.updated += 1;
                    summary.changes.push(change_with(
                        PullChangeKind::Update,
                        remarks_for(&info_remarks, unpublishable_reason.as_deref()),
                    ));
                    return Ok(());
                }

                let merged = merge_remote_change_into_local_file(
                    target_dir,
                    &record.record_name,
                    &file,
                    &body_text,
                    recorded_title.as_deref(),
                    title_mode,
                )?;
                if merged == MergeStatus::UnresolvedMarkers {
                    let mut entry = existing.clone();
                    entry.file = file.clone();
                    entry.pending_rename = pending_rename.clone();
                    tracked.notes.insert(record.record_name.clone(), entry);
                    summary.conflicts.push(format!(
                        "{file}: still contains diff3 conflict markers - resolve them, then run \"push\" to reconcile"
                    ));
                    let mut remarks = vec![PullChangeRemark::new(
                        "conflict",
                        "still contains diff3 conflict markers - resolve them, then run \"push\" to reconcile",
                    )];
                    remarks.extend(deferred_remarks.iter().cloned());
                    summary.changes.push(change_with(PullChangeKind::Update, Some(remarks)));
                    return Ok(());
                }
                tracked
                    .notes
                    .insert(record.record_name.clone(), updated_entry(&existing));
                if merged == MergeStatus::Conflict {
                    summary
                        .conflicts
                        .push(format!("{file}: merged with conflict markers - resolve manually"));
                    let mut remarks = vec![PullChangeRemark::new(
                        "conflict",
                        "merged with conflict markers - resolve manually",
                    )];
                    remarks.extend(info_remarks.iter().cloned());
                    if unpublishable_reason.is_some() {
                        remarks.push(read_only_remark());
                    }
                    summary.changes.push(change_with(PullChangeKind::Merge, Some(remarks)));
                } else {
                    summary.merged += 1;
                    summary.changes.push(change_with(
                        PullChangeKind::Merge,
                        remarks_for(&info_remarks, unpublishable_reason.as_deref()),
                    ));
                }
                Ok(())
            })();
            progress.on_record_processed();
            result?;
        }
    }
    progress.on_process_complete();

    for source in &sources {
        if !source.resynced {
            continue;
        }
        let seen: HashSet<String> = source.records.iter().map(|r| r.record_name.clone()).collect();
        let removed = reconcile_notes_after_resync(
            target_dir,
            source.shared_zone_owner.as_deref(),
            &seen,
            &mut tracked,
            &mut summary,
            title_mode,
        )?;
        let zone_label = match &source.shared_zone_owner {
            None => "this vault's own notes".to_owned(),
            Some(owner) => format!("the notes shared by {owner}"),
        };
        let tail = if removed > 0 {
            format!(" - {removed} note(s) deleted remotely in the meantime were reconciled")
        } else {
            String::new()
        };
        summary.notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!(
                "iCloud no longer accepted the stored sync token for {zone_label}, so they were resynced from scratch{tail}"
            ),
        });
    }

    let live_owners: HashSet<String> = shared
        .zones
        .iter()
        .filter_map(|z| z.zone_id.owner_record_name.clone())
        .chain(
            shared
                .skipped_zones
                .iter()
                .filter_map(|s| skipped_zone_owner(s).map(str::to_owned)),
        )
        .collect();
    handle_vanished_shared_zones(target_dir, &live_owners, &mut tracked, &mut summary)?;

    let relocations = reconcile_note_placements(target_dir, &layout, &mut tracked.notes, &mut tracked.attachments)?;
    for relocation in relocations {
        let mut change = PullChange::new(PullChangeKind::Move, relocation.to);
        change.previous_file = Some(relocation.from);
        summary.changes.push(change);
    }
    let current: HashSet<String> = layout.all_dirs.iter().cloned().collect();
    remove_stale_dirs(target_dir, &previous_layout_dirs(PreviousLayout::of(&state)), &current);

    let new_state = CloneState {
        account: state.account.clone(),
        // Held back: the next pull walks from the old token and sees them again.
        sync_token: if held_back.is_empty() {
            changes.sync_token.clone()
        } else {
            state.sync_token.clone()
        },
        shared_zone_sync_tokens: Some(shared_zone_sync_tokens),
        replica_id: state.replica_id.clone(),
        title_mode: state.title_mode,
        notes: tracked.notes,
        folders: Some(layout.state_folders),
        sharer_homes: Some(layout.state_sharer_homes),
        attachments: Some(tracked.attachments),
        table_attachments: Some(tracked.table_attachments),
        trashed: Some(trashed),
        key_order: Some(PULL_WRITE_ORDER.to_vec()),
        ..Default::default()
    };
    write_clone_state(target_dir, &new_state)?;
    Ok(summary)
}

/// Deliberate difference from icloud-md 0.6.2 (docs/PORT_PLAN.md §1): a note
/// new to this vault that the private `changes/zone` walk listed without its
/// text is looked up by id (`records/lookup`, then the asset-body inlining)
/// instead of being skipped while the new sync token moves past it. Returns
/// the recordNames that still have no text; the caller (pull, or clone with
/// nothing tracked) then doesn't save the new private sync token, so the
/// next pull sees them again. (Shared zones are covered by the fetch, which
/// already looks such notes up and holds back the zone.)
pub(super) fn backfill_new_note_bodies<T: Transport>(
    db: &Database<T>,
    tracked: &IndexMap<String, NoteEntry>,
    records: &mut [CloudKitRecord],
) -> Result<Vec<String>, Error> {
    let missing = |records: &[CloudKitRecord]| -> Vec<String> {
        records
            .iter()
            .filter(|r| needs_body_lookup(r) && !tracked.contains_key(&r.record_name))
            .map(|r| r.record_name.clone())
            .collect()
    };
    let names = missing(records);
    if names.is_empty() {
        return Ok(names);
    }
    let looked_up = db.lookup_records(&note_zone(None), &names)?;
    merge_looked_up_records(records, looked_up);
    db.inline_asset_bodies(records)?;
    Ok(missing(records))
}

/// `backfillSharePermissions`: look up shared folders with no stored
/// permission, plus their share records, and append them to the zone's
/// records so the layout resolves the permission.
fn backfill_share_permissions<T: Transport>(
    db: &Database<T>,
    folders: Option<&IndexMap<String, FolderEntry>>,
    shared_zones: &mut [SharedZoneChanges],
) -> Result<(), Error> {
    let Some(folders) = folders else {
        return Ok(());
    };
    for zone in shared_zones {
        let Some(owner) = zone.zone_id.owner_record_name.clone() else {
            continue;
        };
        let unknown: Vec<String> = folders
            .iter()
            .filter(|(_, e)| e.shared_zone_owner.as_deref() == Some(owner.as_str()) && e.permission.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        if unknown.is_empty() {
            continue;
        }
        let note_zone = NoteZone {
            database: DatabaseScope::Shared,
            zone_id: zone.zone_id.clone(),
        };
        let folder_records = db.lookup_records(&note_zone, &unknown)?;
        let share_names: Vec<String> = folder_records
            .iter()
            .filter_map(|r| r.share_record_name.clone())
            .collect();
        let share_records = if share_names.is_empty() {
            Vec::new()
        } else {
            db.lookup_records(&note_zone, &share_names)?
        };
        zone.records.extend(folder_records);
        zone.records.extend(share_records);
    }
    Ok(())
}

/// `renameForRemoteTitle`: in a filename-as-title vault, rename a tracked
/// note's file when its title changed remotely (or, deferring, report the
/// name it should get). Returns `(file, pendingRename)`.
pub fn rename_for_remote_title(
    target_dir: &Path,
    current_file: &str,
    title_line: &str,
    title_mode: TitleMode,
    used_file_names: &mut HashMap<String, HashSet<String>>,
    on_disk: bool,
    defer: bool,
) -> Result<(String, Option<String>), Error> {
    let current_name = posix::basename(current_file).to_owned();
    if title_mode != TitleMode::Filename || file_name_carries_title(&current_name, title_line) {
        return Ok((current_file.to_owned(), None));
    }
    let defer = defer && on_disk;
    let dir = note_dir_of(current_file);
    let used_in_dir = used_names_for(used_file_names, &dir);
    used_in_dir.remove(&current_name);

    let mut wanted = unique_file_name(&note_file_name_for(title_line, title_mode), used_in_dir);
    while file_exists(&target_dir.join(&dir).join(&wanted))? {
        used_in_dir.insert(wanted.clone());
        wanted = unique_file_name(&note_file_name_for(title_line, title_mode), used_in_dir);
    }
    used_in_dir.insert(wanted.clone());

    if defer {
        used_in_dir.insert(current_name);
        return Ok((current_file.to_owned(), Some(wanted)));
    }
    let new_file = posix::join(&[&dir, &wanted]);
    if on_disk {
        std::fs::rename(target_dir.join(current_file), target_dir.join(&new_file))?;
    }
    Ok((new_file, None))
}

fn drop_tracking(target_dir: &Path, record_name: &str, tracked: &mut Tracked) -> Result<(), Error> {
    tracked.notes.shift_remove(record_name);
    remove_base_copy(target_dir, record_name)?;
    for removed in remove_attachments_for_note(target_dir, record_name, &tracked.attachments)? {
        tracked.attachments.shift_remove(&removed);
    }
    for removed in remove_table_attachments_for_note(record_name, &tracked.table_attachments) {
        tracked.table_attachments.shift_remove(&removed);
    }
    Ok(())
}

/// `handleVanishedSharedZones`: untrack (but keep) the notes of a sharer no
/// longer sharing with this account.
fn handle_vanished_shared_zones(
    target_dir: &Path,
    live_owners: &HashSet<String>,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
) -> Result<(), Error> {
    let vanished: Vec<(String, String)> = tracked
        .notes
        .iter()
        .filter(|(_, e)| {
            e.shared_zone_owner
                .as_ref()
                .is_some_and(|o| !o.is_empty() && !live_owners.contains(o))
        })
        .map(|(k, e)| (k.clone(), e.file.clone()))
        .collect();
    for (record_name, file) in vanished {
        summary.notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!(
                "{file}: no longer shared with you - leaving local copy in place but no longer tracking it"
            ),
        });
        drop_tracking(target_dir, &record_name, tracked)?;
        summary.unshared_untracked += 1;
    }
    Ok(())
}

/// `reconcileNotesAfterResync`: a tracked note of this zone absent from the
/// complete listing was deleted remotely. Returns how many.
fn reconcile_notes_after_resync(
    target_dir: &Path,
    shared_zone_owner: Option<&str>,
    seen: &HashSet<String>,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
    title_mode: TitleMode,
) -> Result<usize, Error> {
    let gone: Vec<(String, NoteEntry)> = tracked
        .notes
        .iter()
        .filter(|(rn, e)| e.shared_zone_owner.as_deref() == shared_zone_owner && !seen.contains(*rn))
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    let removed = gone.len();
    for (record_name, entry) in gone {
        handle_remote_deletion(target_dir, &record_name, &entry, tracked, summary, title_mode)?;
    }
    Ok(removed)
}

/// Public wrapper of `reconcileNotesAfterResync` over bare maps (tests).
#[allow(clippy::too_many_arguments)]
pub fn reconcile_notes_after_resync_in(
    target_dir: &Path,
    shared_zone_owner: Option<&str>,
    seen: &HashSet<String>,
    notes: &mut IndexMap<String, NoteEntry>,
    attachments: &mut IndexMap<String, AttachmentEntry>,
    table_attachments: &mut IndexMap<String, TableAttachmentEntry>,
    summary: &mut PullSummary,
    title_mode: TitleMode,
) -> Result<usize, Error> {
    let mut tracked = Tracked {
        notes: std::mem::take(notes),
        attachments: std::mem::take(attachments),
        table_attachments: std::mem::take(table_attachments),
    };
    let result = reconcile_notes_after_resync(target_dir, shared_zone_owner, seen, &mut tracked, summary, title_mode);
    *notes = tracked.notes;
    *attachments = tracked.attachments;
    *table_attachments = tracked.table_attachments;
    result
}

/// Untracked files whose `apple-note-id` names `record_name`: where a note
/// whose tracked file is gone was moved or renamed to.
fn note_id_claimants(
    target_dir: &Path,
    notes: &IndexMap<String, NoteEntry>,
    record_name: &str,
    title_mode: TitleMode,
) -> Result<Vec<String>, Error> {
    let mut claimants = Vec::new();
    for file in super::push::list_untracked_markdown_files(target_dir, notes)? {
        let text = read_text(&target_dir.join(&file))?.unwrap_or_default();
        let frontmatter = split_frontmatter(&text, split_options(title_mode)).frontmatter;
        if read_note_id(&frontmatter).as_deref() == Some(record_name) {
            claimants.push(file);
        }
    }
    Ok(claimants)
}

/// `handleRemoteDeletion`: a clean or missing file goes with the note. One
/// with local edits is kept as a new note: it stops being tracked and loses
/// its id, so the next push creates it, edits and all. (0.6.2 writes
/// delete/modify conflict markers and keeps tracking a note that no longer
/// exists, which no push can ever settle.)
fn handle_remote_deletion(
    target_dir: &Path,
    record_name: &str,
    existing: &NoteEntry,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
    title_mode: TitleMode,
) -> Result<(), Error> {
    let local = local_file_state(target_dir, existing, record_name, title_mode)?;
    if local != LocalFileState::Modified {
        if local == LocalFileState::Clean {
            safe_unlink(&target_dir.join(&existing.file))?;
        }
        drop_tracking(target_dir, record_name, tracked)?;
        summary.removed += 1;
        summary
            .changes
            .push(PullChange::new(PullChangeKind::Remove, existing.file.clone()));
        return Ok(());
    }

    let path = target_dir.join(&existing.file);
    let text = read_text(&path)?.unwrap_or_default();
    let envelope = split_frontmatter(&text, split_options(title_mode));
    std::fs::write(&path, join_frontmatter(&clear_note_id(&envelope.frontmatter), &envelope.body))?;
    // Its attachment files stay with the kept text; only tracking goes.
    tracked.notes.shift_remove(record_name);
    remove_base_copy(target_dir, record_name)?;
    tracked.attachments.retain(|_, a| a.note_record_name != record_name);
    tracked.table_attachments.retain(|_, a| a.note_record_name != record_name);
    let remark = "deleted on another device, but has local edits - kept as a new note, which the next push creates";
    summary.notices.push(SyncNotice {
        level: NoticeLevel::Warn,
        message: format!("{}: {remark}", existing.file),
    });
    let mut change = PullChange::new(PullChangeKind::Untrack, existing.file.clone());
    change.remarks = Some(vec![PullChangeRemark::new("note", remark)]);
    summary.changes.push(change);
    Ok(())
}

/// `dropUnsyncableNote`.
fn drop_unsyncable_note(
    target_dir: &Path,
    record_name: &str,
    existing: &NoteEntry,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
    reason: &str,
    title_mode: TitleMode,
) -> Result<(), Error> {
    let local = local_file_state(target_dir, existing, record_name, title_mode)?;
    if local == LocalFileState::Modified {
        summary.conflicts.push(format!(
            "{}: became unsyncable remotely ({reason}), and has local edits - left in place, untracked",
            existing.file
        ));
        let mut change = PullChange::new(PullChangeKind::Untrack, existing.file.clone());
        change.remarks = Some(vec![PullChangeRemark::new(
            "conflict",
            format!("became unsyncable remotely ({reason}), and has local edits - left in place"),
        )]);
        summary.changes.push(change);
    } else {
        summary.dropped_unsyncable += 1;
        if local == LocalFileState::Clean {
            let mut change = PullChange::new(PullChangeKind::Untrack, existing.file.clone());
            change.remarks = Some(vec![PullChangeRemark::new(
                "unsyncable",
                format!("no longer syncable remotely ({reason}) - local copy left in place"),
            )]);
            summary.changes.push(change);
        }
    }
    drop_tracking(target_dir, record_name, tracked)
}

/// `mergeRemoteChangeIntoLocalFile`'s status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStatus {
    Merged,
    Conflict,
    UnresolvedMarkers,
}

/// `mergeRemoteChangeIntoLocalFile`: diff3 the remote body into the local
/// edit; a clean merge advances the base copy to the remote text.
pub fn merge_remote_change_into_local_file(
    target_dir: &Path,
    record_name: &str,
    file: &str,
    remote_body_text: &str,
    unrepresentable_title: Option<&str>,
    title_mode: TitleMode,
) -> Result<MergeStatus, Error> {
    let base = read_base_copy(target_dir, record_name)?.unwrap_or_default();
    let path = target_dir.join(file);
    let text = read_text(&path)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{}: not found", path.display())))?;
    let envelope = split_frontmatter(&text, split_options(title_mode));
    if has_conflict_markers(&envelope.body) {
        return Ok(MergeStatus::UnresolvedMarkers);
    }
    let outcome = merge_note_versions(&base, &envelope.body, remote_body_text);
    std::fs::write(
        &path,
        compose_note_file(&envelope.frontmatter, &outcome.text, record_name, unrepresentable_title),
    )?;
    if !outcome.has_conflict {
        write_base_copy(target_dir, record_name, remote_body_text)?;
    }
    Ok(if outcome.has_conflict {
        MergeStatus::Conflict
    } else {
        MergeStatus::Merged
    })
}

// --- pullReport.ts -------------------------------------------------------------

fn label_of(kind: PullChangeKind) -> &'static str {
    match kind {
        PullChangeKind::Add => "new file:",
        PullChangeKind::Update => "modified:",
        PullChangeKind::Merge => "merged:",
        PullChangeKind::Remove => "deleted:",
        PullChangeKind::Move => "moved:",
        PullChangeKind::Untrack => "untracked:",
    }
}

const PULL_LABEL_WIDTH: usize = 10;

/// `renderPullReport`: the human changelist.
pub fn render_pull_report(summary: &PullSummary, format_path: &dyn Fn(&str) -> String) -> Vec<String> {
    if summary.changes.is_empty() {
        return vec!["Already up to date.".into()];
    }
    let mut lines = vec!["Changes pulled from iCloud:".to_owned(), String::new()];
    for change in &summary.changes {
        let subject = match &change.previous_file {
            Some(prev) if *prev != change.file => format!("{} -> {}", format_path(prev), format_path(&change.file)),
            _ => format_path(&change.file),
        };
        lines.push(format!(
            "{LISTING_INDENT}{}",
            labelled_line(label_of(change.kind), PULL_LABEL_WIDTH, &subject)
        ));
        for remark in change.remarks.iter().flatten() {
            let mark = if remark.tone == "note" { " " } else { "!" };
            lines.push(format!(
                "{LISTING_INDENT}{}",
                remark_line(PULL_LABEL_WIDTH, &format!("{mark} {}", remark.message))
            ));
        }
    }
    lines.push(String::new());
    lines.push(tally_line(summary));
    lines
}

fn tally_line(summary: &PullSummary) -> String {
    let moved = summary
        .changes
        .iter()
        .filter(|c| c.kind == PullChangeKind::Move)
        .count();
    let untracked = summary
        .changes
        .iter()
        .filter(|c| c.kind == PullChangeKind::Untrack)
        .count();
    let mut tally = format!(
        "{} added, {} updated, {} auto-merged, {} deleted",
        summary.added, summary.updated, summary.merged, summary.removed
    );
    if moved > 0 {
        tally.push_str(&format!(", {moved} moved"));
    }
    if untracked > 0 {
        tally.push_str(&format!(", {untracked} untracked"));
    }
    if summary.attachments_downloaded > 0 {
        tally.push_str(&format!(
            ", {} attachment(s) downloaded",
            summary.attachments_downloaded
        ));
    }
    tally.push('.');
    let mut attention = Vec::new();
    if !summary.conflicts.is_empty() {
        attention.push(format!("{} conflict(s)", summary.conflicts.len()));
    }
    if summary.unpublishable > 0 {
        attention.push(format!("{} read-only", summary.unpublishable));
    }
    if !attention.is_empty() {
        tally.push_str(&format!(" ({})", attention.join(", ")));
    }
    tally
}
