//! `clone`. Ports icloud-md `src/commands/clone.ts`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::pull::backfill_new_note_bodies;
use super::remote::{Connector, DefaultConnector, bind_account};
use super::{Error, NoticeLevel, SyncNotice, SyncProgress, skipped_zone_owner, used_names_for, zone_for_owner};
use crate::cloudkit::client::DownloadQueue;
use crate::cloudkit::{CloudKitRecord, SkippedSharedZone};
use crate::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
use crate::js::posix;
use crate::md::filename::{note_file_name_for, title_needing_frontmatter, unique_file_name};
use crate::md::frontmatter::compose_note_file;
use crate::vault::attachments::{AttachmentRecords, resolve_note_attachments};
use crate::vault::base::write_base_copy;
use crate::vault::layout::{PreviousLayout, SharedZoneRecords, build_vault_layout, place_note};
use crate::vault::local::{apply_note_file_times, modification_date_of};
use crate::vault::state::{
    Account, CLONE_WRITE_ORDER, CloneState, NOTE_ADD_ORDER, NoteEntry, TitleMode, read_clone_state, write_clone_state,
};

/// `CloneOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneOptions {
    pub filename_as_title: bool,
    /// Checked against the icloud-session account's dsid (or Apple ID).
    pub account: Option<String>,
    /// Accepted for compatibility; a no-op (icloud-session never opens a
    /// window from here).
    pub non_interactive: bool,
}

/// `CloneSummary`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneSummary {
    pub written: usize,
    pub written_shared: usize,
    pub written_unpublishable: usize,
    pub attachments_downloaded: usize,
    pub skipped_deleted: usize,
    pub skipped_undecodable: usize,
    pub notices: Vec<SyncNotice>,
}

/// `runClone`.
pub fn run_clone(
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
    options: &CloneOptions,
) -> Result<CloneSummary, Error> {
    run_clone_with(&DefaultConnector, target_dir, progress, on_status, options)
}

/// `runClone` over an explicit connector.
pub fn run_clone_with(
    connector: &dyn Connector,
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    _on_status: &mut dyn FnMut(&str),
    options: &CloneOptions,
) -> Result<CloneSummary, Error> {
    if read_clone_state(target_dir)?.is_some() {
        return Err(Error::AlreadyClonedDirectory {
            target_dir: target_dir.display().to_string(),
        });
    }
    let title_mode = if options.filename_as_title {
        TitleMode::Filename
    } else {
        TitleMode::InBody
    };

    let remote = bind_account(connector, options.account.as_deref())?;
    let db = &remote.db;
    std::fs::create_dir_all(target_dir)?;

    progress.on_fetch_start();
    let mut fetched = 0usize;
    let mut changes = {
        let mut on_page = |n: usize| {
            fetched += n;
            progress.on_fetch_page(fetched);
        };
        db.fetch_all_note_records(None, &mut on_page)?
    };
    let shared = {
        let mut on_page = |n: usize| {
            fetched += n;
            progress.on_fetch_page(fetched);
        };
        db.fetch_shared_note_records_since(&IndexMap::new(), None, crate::vault::rt::now_ms(), &mut on_page)?
    };
    let held_back = backfill_new_note_bodies(db, &IndexMap::new(), &mut changes.records)?;

    let mut summary = CloneSummary::default();
    for skipped in &shared.skipped_zones {
        let owner = skipped_zone_owner(skipped).unwrap_or("unknown");
        let message = match skipped {
            SkippedSharedZone::ZoneNotFound { server_error_code, .. } => format!(
                "Skipped a shared zone the server no longer has (owner {owner}, {server_error_code}) - its share was \
                 likely revoked or deleted; no notes from it were cloned"
            ),
            SkippedSharedZone::MissingNoteBodies {
                missing_record_names, ..
            } => format!(
                "Skipped the shared zone owned by {owner}: {} note(s) came through without their text and looking \
                 them up didn't fill it in - no notes from it were cloned; the first pull will retry it",
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
                "Skipped {} note(s) that came through without their text, even when looked up - no sync token was \
                 saved for your own notes, so the first pull will look for them again (and re-read the rest)",
                held_back.len()
            ),
        });
    }

    let mut notes: IndexMap<String, NoteEntry> = IndexMap::new();
    let mut attachments = IndexMap::new();
    let mut table_attachments = IndexMap::new();
    let mut used_file_names: HashMap<String, HashSet<String>> = HashMap::new();
    let mut used_attachment_file_names: HashMap<String, HashSet<String>> = HashMap::new();
    let mut shared_zone_sync_tokens: IndexMap<String, String> = IndexMap::new();

    let mut sources: Vec<(&[CloudKitRecord], Option<String>)> = vec![(&changes.records, None)];
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
        sources.push((&zone.records, zone.zone_id.owner_record_name.clone()));
    }

    let layout = build_vault_layout(&changes.records, &shared_zone_records, PreviousLayout::default());
    for dir in &layout.all_dirs {
        std::fs::create_dir_all(target_dir.join(dir))?;
    }

    // One records/lookup walk per zone for every note's attachments.
    let mut attachment_records = AttachmentRecords::default();
    for (records, shared_zone_owner) in &sources {
        let notes: Vec<&CloudKitRecord> = records.iter().collect();
        attachment_records.prefetch(db, &zone_for_owner(shared_zone_owner.as_deref()), &notes)?;
    }
    let mut downloads = DownloadQueue::new(db);

    let total: usize = sources.iter().map(|(records, _)| records.len()).sum();
    progress.on_process_start(total);

    for (records, shared_zone_owner) in &sources {
        let owner = shared_zone_owner.as_deref().filter(|o| !o.is_empty());
        for record in records.iter() {
            let result = (|| -> Result<(), Error> {
                if record.record_type != "Note" {
                    return Ok(());
                }
                let decoded = match classify_note_record(record, &ClassifyOptions { title_mode }) {
                    NoteDecodeResult::Deleted => {
                        summary.skipped_deleted += 1;
                        return Ok(());
                    }
                    NoteDecodeResult::Unsyncable(_) => {
                        summary.skipped_undecodable += 1;
                        return Ok(());
                    }
                    NoteDecodeResult::Ok(decoded) => decoded,
                };
                let placement = place_note(&layout, record, shared_zone_owner.as_deref());

                let mut body_text = decoded.markdown_text.clone();
                let unpublishable_reason = decoded.unpublishable_reason.clone();
                if !decoded.embed_slots.is_empty() {
                    let resolved = resolve_note_attachments(
                        db,
                        &mut attachment_records,
                        &mut downloads,
                        &zone_for_owner(owner),
                        target_dir,
                        &record.record_name,
                        &decoded.markdown_text,
                        &decoded.embed_slots,
                        &attachments,
                        &table_attachments,
                        used_names_for(&mut used_attachment_file_names, &placement.dir),
                        &placement.dir,
                    )?;
                    body_text = resolved.body_text;
                    summary.attachments_downloaded += resolved.attachments.len();
                    attachments.extend(resolved.attachments);
                    table_attachments.extend(resolved.table_attachments);
                }

                let used_in_dir = used_names_for(&mut used_file_names, &placement.dir);
                let file_name = unique_file_name(&note_file_name_for(&decoded.title_line, title_mode), used_in_dir);
                used_in_dir.insert(file_name.clone());
                let relative_file = posix::join(&[&placement.dir, &file_name]);

                let file_path = target_dir.join(&relative_file);
                let recorded_title = title_needing_frontmatter(&decoded.title_line, title_mode);
                std::fs::write(
                    &file_path,
                    compose_note_file("", &body_text, &record.record_name, recorded_title.as_deref()),
                )?;
                apply_note_file_times(&file_path, record)?;
                write_base_copy(target_dir, &record.record_name, &body_text)?;
                if owner.is_some() {
                    summary.written_shared += 1;
                } else {
                    summary.written += 1;
                }
                if unpublishable_reason.as_deref().is_some_and(|r| !r.is_empty()) {
                    summary.written_unpublishable += 1;
                }

                notes.insert(
                    record.record_name.clone(),
                    NoteEntry {
                        file: relative_file,
                        record_change_tag: record.record_change_tag.clone().unwrap_or_default(),
                        modification_date: modification_date_of(record),
                        shared_zone_owner: shared_zone_owner.clone(),
                        unpublishable_reason,
                        folder_record_name: placement.folder_record_name,
                        pending_rename: None,
                        frontmatter_title: recorded_title,
                        key_order: Some(NOTE_ADD_ORDER.to_vec()),
                    },
                );
                Ok(())
            })();
            progress.on_record_processed();
            result?;
        }
    }
    progress.on_process_complete();
    downloads.finish()?;

    let state = CloneState {
        account: Some(Account {
            apple_id: remote.account.apple_id.clone(),
            dsid: remote.account.dsid.clone(),
        }),
        title_mode: Some(title_mode),
        // Held back: no token, so the first pull walks from scratch and sees them.
        sync_token: if held_back.is_empty() {
            changes.sync_token.clone()
        } else {
            None
        },
        shared_zone_sync_tokens: Some(shared_zone_sync_tokens),
        shared_database: shared.cursor.clone(),
        notes,
        folders: Some(layout.state_folders),
        sharer_homes: Some(layout.state_sharer_homes),
        attachments: Some(attachments),
        table_attachments: Some(table_attachments),
        key_order: Some(CLONE_WRITE_ORDER.to_vec()),
        ..Default::default()
    };
    write_clone_state(target_dir, &state)?;
    Ok(summary)
}
