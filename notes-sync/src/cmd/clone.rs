//! `clone`. Originally derived from icloud-md.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::pull::backfill_new_note_bodies;
use super::remote::{Connector, DefaultConnector, bind_account};
use super::{
    Error, NoticeLevel, OTHER_ZONES_RECORD_NAME, SyncNotice, SyncProgress, UNSAFE_RECORD_NAME,
    shared_zone_tokens_and_records, skipped_note_notice, skipped_zone_notice, used_names_for, zone_for_owner,
};
use crate::cloudkit::client::DownloadQueue;
use crate::cloudkit::CloudKitRecord;
use crate::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
use crate::js::posix;
use crate::md::filename::{note_file_name_for, title_needing_frontmatter, unique_file_name};
use crate::md::frontmatter::compose_note_file;
use crate::vault::attachments::{AttachmentRecords, resolve_note_attachments};
use crate::vault::base::write_base_copy_of;
use crate::vault::layout::{PreviousLayout, build_vault_layout, place_note};
use crate::vault::local::{apply_note_file_times, modification_date_of, write_note_file};
use crate::vault::pairing::{claim_names_on_disk, note_id_at};
use crate::vault::state::{
    AttachmentEntry, CloneState, NoteEntry, TitleMode, is_plain_record_name, read_clone_state, write_clone_state,
};

/// `CloneOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneOptions {
    pub filename_as_title: bool,
    /// Checked against the icloud-session account's dsid (or Apple ID).
    pub account: Option<String>,
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
    options: &CloneOptions,
) -> Result<CloneSummary, Error> {
    run_clone_with(&DefaultConnector, target_dir, progress, options)
}

/// `runClone` over an explicit connector.
pub fn run_clone_with(
    connector: &dyn Connector,
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
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

    let mut fetched = 0usize;
    let mut on_page = |n: usize| {
        fetched += n;
        progress.on_fetch_page(fetched);
    };
    let mut changes = db.fetch_all_note_records(None, &mut on_page)?;
    let shared =
        db.fetch_shared_note_records_since(&IndexMap::new(), None, crate::vault::rt::now_ms(), &mut on_page)?;
    let held_back = backfill_new_note_bodies(db, &IndexMap::new(), &mut changes.records)?;

    let mut summary = CloneSummary::default();
    for skipped in &shared.skipped_zones {
        summary.notices.push(skipped_zone_notice(
            skipped,
            "no notes from it were cloned",
            "",
            "no notes from it were cloned; the first pull will retry it",
        ));
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
    // Directories whose existing files are claimed: clone writes beside
    // what is already there, never over it (anything there afterwards is
    // in the set already, so once per directory is enough).
    let mut claimed_dirs: HashSet<String> = HashSet::new();
    // The note files a directory already held, by the `apple-note-id` each
    // carries: what a clone that never finished (killed before its state
    // was written) left there.
    let mut left_behind: HashMap<String, HashMap<String, String>> = HashMap::new();
    let (shared_zone_sync_tokens, shared_zone_records) = shared_zone_tokens_and_records(&shared.zones);
    let mut sources: Vec<(&[CloudKitRecord], Option<String>)> = vec![(&changes.records, None)];
    sources.extend(
        shared
            .zones
            .iter()
            .map(|zone| (&zone.records[..], zone.zone_id.owner_record_name.clone())),
    );

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

    // A failure part way still records the notes written before it, as a
    // pull's does: their files carry their ids, and with no state a second
    // clone would write each again beside them, for push to create as new
    // notes. `downloading`: the notes that queued attachment downloads,
    // which a failure may have left undone.
    let mut failure: Option<Error> = None;
    let mut downloading: Vec<String> = Vec::new();
    'records: for (records, shared_zone_owner) in &sources {
        let owner = shared_zone_owner.as_deref().filter(|o| !o.is_empty());
        for record in records.iter() {
            let queued_before = downloads.len();
            let result = (|| -> Result<(), Error> {
                if record.record_type != "Note" {
                    return Ok(());
                }
                if !is_plain_record_name(&record.record_name) {
                    summary
                        .notices
                        .push(skipped_note_notice(&record.record_name, owner, UNSAFE_RECORD_NAME));
                    return Ok(());
                }
                // Unique within a zone only: the private zone, cloned first,
                // keeps its note.
                if notes
                    .get(&record.record_name)
                    .is_some_and(|e| e.shared_zone_owner != *shared_zone_owner)
                {
                    summary
                        .notices
                        .push(skipped_note_notice(&record.record_name, owner, OTHER_ZONES_RECORD_NAME));
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
                if claimed_dirs.insert(placement.dir.clone()) {
                    claim_names_on_disk(target_dir, &placement.dir, used_in_dir)?;
                    let ids = left_behind.entry(placement.dir.clone()).or_default();
                    for name in used_in_dir.iter().filter(|name| name.ends_with(".md")) {
                        let file = posix::join(&[&placement.dir, name]);
                        if let Some(id) = note_id_at(target_dir, &file, title_mode)? {
                            ids.insert(id, name.clone());
                        }
                    }
                }
                let recorded_title = title_needing_frontmatter(&decoded.title_line, title_mode);
                let file_text = compose_note_file("", &body_text, &record.record_name, recorded_title.as_deref());
                // The file an unfinished clone wrote for this very note is
                // the note's file, not a name to write beside. Only when it
                // reads exactly as this clone would write it: one that
                // differs is kept as it is, beside the note.
                let adopted = left_behind
                    .get(&placement.dir)
                    .and_then(|ids| ids.get(&record.record_name))
                    .filter(|name| {
                        let path = target_dir.join(posix::join(&[&placement.dir, name]));
                        std::fs::read_to_string(path).is_ok_and(|text| text == file_text)
                    })
                    .cloned();
                let file_name = adopted.clone().unwrap_or_else(|| {
                    unique_file_name(&note_file_name_for(&decoded.title_line, title_mode), used_in_dir)
                });
                used_in_dir.insert(file_name.clone());
                let relative_file = posix::join(&[&placement.dir, &file_name]);

                let file_path = target_dir.join(&relative_file);
                if adopted.is_none() {
                    write_note_file(&file_path, &file_text)?;
                }
                apply_note_file_times(&file_path, record)?;
                write_base_copy_of(target_dir, &record.record_name, &file_text, title_mode)?;
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
                    },
                );
                Ok(())
            })();
            progress.on_record_processed();
            if downloads.len() > queued_before {
                downloading.push(record.record_name.clone());
            }
            if let Err(e) = result {
                failure = Some(e);
                break 'records;
            }
        }
    }
    progress.on_process_complete();
    if let Err(e) = downloads.finish() {
        failure.get_or_insert(e.into());
    }
    if failure.is_some() {
        // Once one download fails the rest are not started: these notes
        // forget their attachments and their version, so the first pull
        // applies them again and fetches what is missing.
        for record_name in &downloading {
            attachments.retain(|_, a: &mut AttachmentEntry| a.note_record_name != *record_name);
            if let Some(entry) = notes.get_mut(record_name) {
                entry.record_change_tag.clear();
            }
        }
    }
    let failed = failure.is_some();
    // Nothing written, nothing to record: the directory is not a clone yet.
    if failed && notes.is_empty() {
        return Err(failure.expect("failed"));
    }

    let state = CloneState {
        account: Some(remote.account.clone()),
        title_mode: Some(title_mode),
        // Held back, or not all written: no token, so the first pull walks
        // from scratch and sees them.
        sync_token: if held_back.is_empty() && !failed {
            changes.sync_token.clone()
        } else {
            None
        },
        shared_zone_sync_tokens: Some(if failed { IndexMap::new() } else { shared_zone_sync_tokens }),
        shared_database: if failed { None } else { shared.cursor.clone() },
        notes,
        folders: Some(layout.state_folders),
        sharer_homes: Some(layout.state_sharer_homes),
        attachments: Some(attachments),
        table_attachments: Some(table_attachments),
        ..Default::default()
    };
    if let Some(e) = failure {
        let _ = write_clone_state(target_dir, &state);
        return Err(e);
    }
    write_clone_state(target_dir, &state)?;
    Ok(summary)
}
