//! `pull`, and its human report. Originally derived from icloud-md.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::remote::{Connector, DefaultConnector, resolve_folder_account};
use super::report::{LISTING_INDENT, labelled_line, remark_line};
use super::{
    Error, LeftBehind, NoticeLevel, OTHER_ZONES_RECORD_NAME, SyncNotice, SyncProgress, UNSAFE_RECORD_NAME,
    UNSHARED_KEY, attachment_lookup_notice, is_purged, left_behind_in, left_file_of, redo_downloads,
    shared_zone_tokens_and_records, skipped_note_notice, skipped_zone_notice, skipped_zone_owner, unshared_note_ids,
    used_names_for, zone_for_owner,
};
use crate::cloudkit::client::{DownloadQueue, merge_looked_up_records, needs_body_lookup};
use crate::cloudkit::{CloudKitRecord, DatabaseScope, NoteZone, SharedZoneChanges, Transport, note_zone};
use crate::cloudkit::{Database, SharedNoteRecords};
use crate::diff3::{has_conflict_markers, merge_note_versions};
use crate::doc::decode::{ClassifyOptions, NoteDecodeResult, classify_note_record};
use crate::js::posix;
use crate::md::filename::{file_name_carries_title, note_file_name_for, title_needing_frontmatter, unique_file_name};
use crate::md::frontmatter::{
    NOTE_TITLE_KEY, clear_note_id, compose_note_file, join_frontmatter, read_note_id, split_frontmatter,
};
use crate::md::title::representability_problem;
use crate::vault::attachments::{
    AttachmentRecords, remove_attachments_for_note, remove_table_attachments_for_note, resolve_note_attachments,
    safe_unlink,
};
use crate::vault::base::{read_base_copy, remove_base_copy, write_base_copy_of};
use crate::vault::epoch::record_epoch;
use crate::vault::folders::{reconcile_note_placements, remove_stale_dirs};
use crate::vault::history::{VersionSnapshotInput, record_version};
use crate::vault::layout::{
    PreviousLayout, VaultLayout, build_vault_layout, expected_note_dir, note_dir_of, place_note, previous_layout_dirs,
};
use crate::vault::local::{
    LocalFileState, apply_note_file_times, file_names_in, local_file_state, modification_date_of, read_text,
    rename_no_replace, split_options, write_note_file,
};
use crate::vault::migrate::require_vault;
use crate::vault::pairing::{claim_names_on_disk, file_exists, pending_rename_target, settle_pending_renames};
use crate::vault::state::{
    AttachmentEntry, CloneState, FolderEntry, NoteEntry, TableAttachmentEntry, TitleMode, TrashedEntry,
    is_plain_record_name, write_clone_state,
};

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
    pub remarks: Option<Vec<PullChangeRemark>>,
}

impl PullChange {
    pub fn new(kind: PullChangeKind, file: impl Into<String>) -> Self {
        PullChange {
            kind,
            file: file.into(),
            previous_file: None,
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
    /// Always 0: a tracked note that can no longer be decoded stays tracked
    /// (read-only). Kept for the shape of the result.
    pub dropped_unsyncable: usize,
    pub unshared_untracked: usize,
    pub changes: Vec<PullChange>,
    pub conflicts: Vec<String>,
    pub notices: Vec<SyncNotice>,
}

/// What a pull says of a file it could not merge into: the markers of an
/// earlier conflict are still in it.
const UNRESOLVED_MARKERS: &str = "still contains diff3 conflict markers - resolve them, then run \"push\" to reconcile";
/// What a pull says of a merge that left conflict markers.
const MERGED_WITH_MARKERS: &str = "merged with conflict markers - resolve manually";
/// Why a tracked note whose text can no longer be decoded is read-only (it
/// follows "this note").
const UNDECODABLE_REASON: &str = "has changed in iCloud into something this version can't read";
/// What a pull with nothing to apply says.
pub const UP_TO_DATE: &str = "Already up to date.";

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
) -> Result<PullSummary, Error> {
    run_pull_with(&DefaultConnector, target_dir, progress, on_status)
}

/// One source of records: the private zone or one shared zone.
struct Source<'a> {
    records: &'a [CloudKitRecord],
    shared_zone_owner: Option<String>,
    resynced: bool,
}

/// The mutable pieces of tracked state a pull rewrites.
struct Tracked {
    notes: IndexMap<String, NoteEntry>,
    attachments: IndexMap<String, AttachmentEntry>,
    table_attachments: IndexMap<String, TableAttachmentEntry>,
    /// The notes no longer shared whose edited file this run kept (state's
    /// `unshared`).
    unshared: Vec<String>,
}

/// `runPull` over an explicit connector.
pub fn run_pull_with(
    connector: &dyn Connector,
    target_dir: &Path,
    progress: &mut dyn SyncProgress,
    on_status: &mut dyn FnMut(&str),
) -> Result<PullSummary, Error> {
    let state = require_vault(target_dir, on_status)?;
    let title_mode = state.mode();

    let remote = resolve_folder_account(connector, target_dir, state.account.as_ref())?;
    let db = &remote.db;

    let mut fetched = 0usize;
    let mut on_page = |n: usize| {
        fetched += n;
        progress.on_fetch_page(fetched);
    };
    let mut changes = db.fetch_all_note_records(state.sync_token.as_deref(), &mut on_page)?;
    let mut shared = db.fetch_shared_note_records_since(
        &state.shared_zone_sync_tokens.clone().unwrap_or_default(),
        state.shared_database.as_ref(),
        crate::vault::rt::now_ms(),
        &mut on_page,
    )?;
    backfill_share_permissions(db, state.folders.as_ref(), &mut shared.zones)?;
    let held_back = backfill_new_note_bodies(db, &state.notes, &mut changes.records)?;

    let mut tracked = Tracked {
        notes: state.notes.clone(),
        attachments: state.attachments.clone().unwrap_or_default(),
        table_attachments: state.table_attachments.clone().unwrap_or_default(),
        unshared: Vec::new(),
    };
    let mut trashed = state.trashed.clone().unwrap_or_default();
    // A rename an older engine's pull left pending (`pull --defer-renames`,
    // which is gone) is carried out now.
    let settled = settle_pending_renames(target_dir, &mut tracked.notes, true, title_mode)?;

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
        records: &changes.records,
        shared_zone_owner: None,
        resynced: changes.resynced_from_scratch,
    }];
    let (mut shared_zone_sync_tokens, shared_zone_records) = shared_zone_tokens_and_records(&shared.zones);
    sources.extend(shared.zones.iter().map(|zone| Source {
        records: &zone.records,
        shared_zone_owner: zone.zone_id.owner_record_name.clone(),
        resynced: zone.resynced_from_scratch,
    }));
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
        summary.notices.push(skipped_zone_notice(
            skipped,
            "its local notes were left in place and stay tracked for now",
            " this run",
            "nothing from that zone was touched, and the next pull will retry it",
        ));
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

    // A zone listed from scratch holds no tombstones: its folders are the
    // listing's alone.
    let mut complete_zones: HashSet<Option<String>> = HashSet::new();
    if changes.resynced_from_scratch {
        complete_zones.insert(None);
    }
    for zone in shared.zones.iter().filter(|z| z.resynced_from_scratch) {
        if let Some(owner) = zone.zone_id.owner_record_name.as_ref().filter(|o| !o.is_empty()) {
            complete_zones.insert(Some(owner.clone()));
        }
    }
    let previous = PreviousLayout {
        complete_zones: Some(&complete_zones),
        ..PreviousLayout::of(&state)
    };
    let layout = build_vault_layout(&changes.records, &shared_zone_records, previous);
    for dir in &layout.all_dirs {
        std::fs::create_dir_all(target_dir.join(dir))?;
    }

    // One records/lookup walk per zone for the attachments of every note the
    // loop below applies, rather than two per note.
    let mut attachment_records = AttachmentRecords::default();
    for source in &sources {
        let to_apply: Vec<&CloudKitRecord> = source
            .records
            .iter()
            .filter(|r| match (tracked.notes.get(&r.record_name), &r.record_change_tag) {
                (Some(existing), Some(tag)) => existing.record_change_tag != *tag,
                _ => true,
            })
            .collect();
        attachment_records.prefetch(db, &zone_for_owner(source.shared_zone_owner.as_deref()), &to_apply)?;
    }
    let mut downloads = DownloadQueue::new(db);

    let total: usize = sources.iter().map(|s| s.records.len()).sum();
    progress.on_process_start(total);

    // A failure part way still records what was applied before it (with
    // the old sync tokens, so the rest is listed again): a new note's file
    // already carries its id, and untracked it would be pushed as a new
    // note. `downloading`: the notes this run queued attachment downloads
    // for, which a failure may have left undone.
    //
    // A run that is killed never gets to record anything, so what was
    // applied is also saved every `CHECKPOINT_EVERY` notes along the way
    // ([`checkpoint_state`]), while the layout leaves every note where it is.
    let mut failure: Option<Error> = None;
    let mut downloading: Vec<String> = Vec::new();
    // Zones (by shared-zone owner; `None` is the private one) with a note
    // left alone because an attachment record could not be looked up: their
    // sync tokens stay where they were, so the next pull lists it again.
    let mut retry_zones: HashSet<Option<String>> = HashSet::new();
    let mut left_behind: HashMap<String, LeftBehind> = HashMap::new();
    let mut claimed_attachment_dirs: HashSet<String> = HashSet::new();
    let notes_at_start = tracked.notes.clone();
    let checkpoints = layout_moves_nothing(&state, &layout, &tracked.notes);
    let mut notes_seen = 0usize;
    'records: for source in &sources {
        for record in source.records {
            let queued_before = downloads.len();
            let result = (|| -> Result<(), Error> {
                if record.record_type != "Note" {
                    return Ok(());
                }
                if !is_plain_record_name(&record.record_name) {
                    summary.notices.push(skipped_note_notice(
                        &record.record_name,
                        source.shared_zone_owner.as_deref(),
                        UNSAFE_RECORD_NAME,
                    ));
                    return Ok(());
                }
                let existing = tracked.notes.get(&record.record_name).cloned();
                // A record name is unique within its zone only: a note from
                // another zone (a sharer's, or this account's own) under a
                // tracked name is never that note's update or deletion.
                if existing
                    .as_ref()
                    .is_some_and(|e| e.shared_zone_owner != source.shared_zone_owner)
                {
                    summary.notices.push(skipped_note_notice(
                        &record.record_name,
                        source.shared_zone_owner.as_deref(),
                        OTHER_ZONES_RECORD_NAME,
                    ));
                    return Ok(());
                }
                // Already at this version: a resync re-listing it, or the
                // push just before this pull writing it. Nothing to apply.
                if let (Some(existing), Some(tag)) = (&existing, &record.record_change_tag)
                    && *tag == existing.record_change_tag
                {
                    return Ok(());
                }

                let decoded = match classify_note_record(record, &ClassifyOptions { title_mode }) {
                    NoteDecodeResult::Deleted => {
                        // Trashed notes are this account's own.
                        if source.shared_zone_owner.is_none()
                            && trashed.contains_key(&record.record_name)
                            && (record.is_deleted() || is_purged(record))
                        {
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
                                Gone::Deleted,
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
                        // The note is still in iCloud: it stays tracked, its
                        // file as it is. Untracked, the file (which carries
                        // the note's id) would go up as a new note with the
                        // next push, a second copy of this one.
                        let mut entry = existing.clone();
                        if let Some(tag) = &record.record_change_tag {
                            entry.record_change_tag = tag.clone();
                        }
                        entry.unpublishable_reason = Some(UNDECODABLE_REASON.to_owned());
                        tracked.notes.insert(record.record_name.clone(), entry);
                        summary.unpublishable += 1;
                        summary.notices.push(SyncNotice {
                            level: NoticeLevel::Warn,
                            message: format!(
                                "{}: its text in iCloud can't be read by this version ({}) - left as it is here, \
                                 read-only, and still tracked",
                                existing.file,
                                reason.as_str()
                            ),
                        });
                        return Ok(());
                    }
                    NoteDecodeResult::Ok(decoded) => decoded,
                };
                let zone = zone_for_owner(source.shared_zone_owner.as_deref());
                if let Some(failure) = attachment_records.lookup_failure(db, &zone, &decoded.embed_slots)? {
                    let note = existing
                        .as_ref()
                        .map_or_else(|| format!("The new note {:?}", decoded.title_line), |e| e.file.clone());
                    summary.notices.push(attachment_lookup_notice(&note, &failure));
                    retry_zones.insert(source.shared_zone_owner.clone());
                    return Ok(());
                }

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
                // A note that lost its last embed (an attachment removed on
                // another device) still has what it tracked to let go of.
                let tracks_embeds = tracked
                    .attachments
                    .values()
                    .any(|a| a.note_record_name == record.record_name)
                    || tracked
                        .table_attachments
                        .values()
                        .any(|t| t.note_record_name == record.record_name);
                if !decoded.embed_slots.is_empty() || tracks_embeds {
                    if claimed_attachment_dirs.insert(note_dir.clone()) {
                        claim_untracked_attachment_names(
                            target_dir,
                            &note_dir,
                            &tracked,
                            used_names_for(&mut used_attachment_file_names, &note_dir),
                            title_mode,
                        )?;
                    }
                    let resolved = resolve_note_attachments(
                        db,
                        &mut attachment_records,
                        &mut downloads,
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
                    // What an earlier pull wrote here and never recorded
                    // (killed, or failed on that note): the files no note
                    // tracks, found once per directory.
                    if !left_behind.contains_key(&note_dir) {
                        let on_disk = file_names_in(&target_dir.join(&note_dir))?;
                        let untracked = on_disk.iter().filter(|name| !used_in_dir.contains(*name));
                        let left = left_behind_in(target_dir, &note_dir, untracked, title_mode)?;
                        left_behind.insert(note_dir.clone(), left);
                    }
                    claim_names_on_disk(target_dir, &note_dir, used_in_dir)?;
                    let file_text = compose_note_file("", &body_text, &record.record_name, recorded_title.as_deref());
                    let adopted = left_file_of(
                        target_dir,
                        &note_dir,
                        &left_behind[&note_dir],
                        &record.record_name,
                        &file_text,
                    );
                    let file_name = adopted.clone().unwrap_or_else(|| {
                        unique_file_name(&note_file_name_for(&decoded.title_line, title_mode), used_in_dir)
                    });
                    used_in_dir.insert(file_name.clone());
                    let relative_file = posix::join(&[&note_dir, &file_name]);
                    let file_path = target_dir.join(&relative_file);
                    if adopted.is_none() {
                        write_note_file(&file_path, &file_text)?;
                    }
                    apply_note_file_times(&file_path, record)?;
                    write_base_copy_of(target_dir, &record.record_name, &file_text, title_mode)?;
                    tracked.notes.insert(
                        record.record_name.clone(),
                        NoteEntry {
                            file: relative_file.clone(),
                            record_change_tag: record.record_change_tag.clone().unwrap_or_default(),
                            modification_date: modification_date_of(record),
                            shared_zone_owner: source.shared_zone_owner.clone(),
                            unpublishable_reason: unpublishable_reason.clone(),
                            folder_record_name: placement.folder_record_name,
                            pending_rename: None,
                            frontmatter_title: recorded_title,
                        },
                    );
                    summary.added += 1;
                    let mut change = PullChange::new(PullChangeKind::Add, relative_file);
                    change.remarks = remarks_for(&title_remarks, unpublishable_reason.as_deref());
                    summary.changes.push(change);
                    return Ok(());
                };

                // The tracked entry brought up to this record (where its file
                // and its folder are is each case's own).
                let refreshed = |entry: &NoteEntry| -> NoteEntry {
                    let mut entry = entry.clone();
                    if let Some(tag) = &record.record_change_tag {
                        entry.record_change_tag = tag.clone();
                    }
                    entry.modification_date = modification_date_of(record);
                    entry.unpublishable_reason = unpublishable_reason.clone();
                    entry.frontmatter_title = recorded_title.clone();
                    entry.pending_rename = None;
                    entry
                };
                let local = local_file_state(target_dir, &existing, &record.record_name, title_mode)?;
                // A note someone shared with you can't be deleted from here
                // (push refuses), so its file comes back below.
                if local == LocalFileState::Missing && existing.shared_zone_owner.is_none() {
                    // The base copy is the remote text as a file freshly written for it reads back.
                    let fresh_file_text =
                        compose_note_file("", &body_text, &record.record_name, recorded_title.as_deref());
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
                            summary
                                .conflicts
                                .push(format!("{}: {UNRESOLVED_MARKERS}", claimants.join(", ")));
                            return Ok(());
                        }
                        let conflicted = statuses.contains(&MergeStatus::Conflict);
                        if !conflicted {
                            write_base_copy_of(target_dir, &record.record_name, &fresh_file_text, title_mode)?;
                        }
                        tracked.notes.insert(record.record_name.clone(), refreshed(&existing));
                        for claimant in &claimants {
                            if conflicted {
                                summary.conflicts.push(format!("{claimant}: {MERGED_WITH_MARKERS}"));
                            } else {
                                summary.merged += 1;
                            }
                            summary
                                .changes
                                .push(PullChange::new(PullChangeKind::Merge, claimant.clone()));
                        }
                        return Ok(());
                    }
                    // Deleted here: the file stays gone and the next push moves
                    // the note to Recently Deleted. Tracking follows the remote
                    // record, so the base copy is the text being deleted.
                    // (icloud-md writes the file back.)
                    write_base_copy_of(target_dir, &record.record_name, &fresh_file_text, title_mode)?;
                    let mut entry = refreshed(&existing);
                    entry.folder_record_name = placement.folder_record_name.clone();
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
                let file = rename_for_remote_title(
                    target_dir,
                    &existing.file,
                    &decoded.title_line,
                    title_mode,
                    &mut used_file_names,
                    local != LocalFileState::Missing,
                )?;
                let previous_file = (file != existing.file).then(|| existing.file.clone());
                let updated_entry = |existing: &NoteEntry| -> NoteEntry {
                    let mut entry = refreshed(existing);
                    entry.file = file.clone();
                    entry.folder_record_name = placement.folder_record_name.clone();
                    entry
                };
                let change_with = |kind: PullChangeKind, remarks: Option<Vec<PullChangeRemark>>| PullChange {
                    kind,
                    file: file.clone(),
                    previous_file: previous_file.clone(),
                    remarks,
                };

                if local == LocalFileState::Clean || local == LocalFileState::Missing {
                    let file_path = target_dir.join(&file);
                    let frontmatter = read_text(&file_path)?
                        .map(|text| split_frontmatter(&text, split_options(title_mode)).frontmatter)
                        .unwrap_or_default();
                    if local == LocalFileState::Missing {
                        summary.notices.push(SyncNotice {
                            level: NoticeLevel::Info,
                            message: format!(
                                "Restored {file}: notes shared by someone else can't be deleted from here"
                            ),
                        });
                    }
                    let file_text =
                        compose_note_file(&frontmatter, &body_text, &record.record_name, recorded_title.as_deref());
                    write_note_file(&file_path, &file_text)?;
                    apply_note_file_times(&file_path, record)?;
                    write_base_copy_of(target_dir, &record.record_name, &file_text, title_mode)?;
                    tracked
                        .notes
                        .insert(record.record_name.clone(), updated_entry(&existing));
                    summary.updated += 1;
                    summary.changes.push(change_with(
                        PullChangeKind::Update,
                        remarks_for(&title_remarks, unpublishable_reason.as_deref()),
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
                    let mut entry = existing;
                    entry.file = file.clone();
                    entry.pending_rename = None;
                    tracked.notes.insert(record.record_name.clone(), entry);
                    summary.conflicts.push(format!("{file}: {UNRESOLVED_MARKERS}"));
                    let remarks = vec![PullChangeRemark::new("conflict", UNRESOLVED_MARKERS)];
                    summary.changes.push(change_with(PullChangeKind::Update, Some(remarks)));
                    return Ok(());
                }
                tracked
                    .notes
                    .insert(record.record_name.clone(), updated_entry(&existing));
                if merged == MergeStatus::Conflict {
                    summary.conflicts.push(format!("{file}: {MERGED_WITH_MARKERS}"));
                    let mut remarks = vec![PullChangeRemark::new("conflict", MERGED_WITH_MARKERS)];
                    remarks.extend(title_remarks.iter().cloned());
                    if unpublishable_reason.is_some() {
                        remarks.push(read_only_remark());
                    }
                    summary.changes.push(change_with(PullChangeKind::Merge, Some(remarks)));
                } else {
                    summary.merged += 1;
                    summary.changes.push(change_with(
                        PullChangeKind::Merge,
                        remarks_for(&title_remarks, unpublishable_reason.as_deref()),
                    ));
                }
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
            if record.record_type == "Note" {
                notes_seen += 1;
                if checkpoints && notes_seen.is_multiple_of(CHECKPOINT_EVERY) {
                    let checkpoint =
                        checkpoint_state(&state, &layout, &tracked, &notes_at_start, &trashed, &downloading);
                    if let Err(e) = write_clone_state(target_dir, &checkpoint) {
                        failure = Some(e);
                        break 'records;
                    }
                }
            }
        }
    }
    progress.on_process_complete();
    // Before anything below moves or removes attachment files.
    let finished = downloads.finish();
    if let Err(e) = finished {
        failure.get_or_insert(e.into());
    }
    if failure.is_some() {
        redo_downloads(&mut tracked.notes, &mut tracked.attachments, &downloading);
    }

    if failure.is_none()
        && let Err(e) = reconcile_listings(target_dir, &sources, &shared, &mut tracked, &mut summary, title_mode)
    {
        failure = Some(e);
    }

    // A failure here still gets to the state write below, like any other:
    // what was applied above stays tracked.
    let relocations = reconcile_note_placements(target_dir, &layout, &mut tracked.notes, &mut tracked.attachments)
        .unwrap_or_else(|e| {
            failure.get_or_insert(e);
            Vec::new()
        });
    for relocation in relocations {
        let mut change = PullChange::new(PullChangeKind::Move, relocation.to);
        change.previous_file = Some(relocation.from);
        summary.changes.push(change);
    }
    let current: HashSet<String> = layout.all_dirs.iter().cloned().collect();
    remove_stale_dirs(target_dir, &previous_layout_dirs(PreviousLayout::of(&state)), &current);

    let failed = failure.is_some();
    // A shared zone is listed again only from the old listing cursor.
    let shared_held = failed || retry_zones.iter().any(Option::is_some);
    // A note tracked (again) is no kept copy of one no longer shared.
    let mut extra = state.extra;
    let mut unshared = unshared_note_ids(&extra);
    unshared.extend(std::mem::take(&mut tracked.unshared));
    unshared.retain(|id| !tracked.notes.contains_key(id));
    if unshared.is_empty() {
        extra.remove(UNSHARED_KEY);
    } else {
        unshared.dedup();
        extra.insert(UNSHARED_KEY.to_owned(), unshared.into());
    }
    let new_state = CloneState {
        account: state.account.clone(),
        // Held back, or not all applied: the next pull walks from the old
        // token and sees them again.
        sync_token: if held_back.is_empty() && !failed && !retry_zones.contains(&None) {
            changes.sync_token.clone()
        } else {
            state.sync_token.clone()
        },
        shared_zone_sync_tokens: if shared_held {
            state.shared_zone_sync_tokens.clone()
        } else {
            Some(shared_zone_sync_tokens)
        },
        shared_database: if shared_held {
            state.shared_database.clone()
        } else {
            shared.cursor.clone()
        },
        replica_id: state.replica_id.clone(),
        title_mode: state.title_mode,
        notes: tracked.notes,
        folders: Some(layout.state_folders),
        sharer_homes: Some(layout.state_sharer_homes),
        attachments: Some(tracked.attachments),
        table_attachments: Some(tracked.table_attachments),
        trashed: Some(trashed),
        extra,
        ..Default::default()
    };
    if let Some(e) = failure {
        let _ = write_clone_state(target_dir, &new_state);
        return Err(e);
    }
    write_clone_state(target_dir, &new_state)?;
    Ok(summary)
}

/// After every listed record is applied: reconcile the zones listed from
/// scratch, and the shared zones that are gone.
fn reconcile_listings(
    target_dir: &Path,
    sources: &[Source<'_>],
    shared: &SharedNoteRecords,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
    title_mode: TitleMode,
) -> Result<(), Error> {
    for source in sources {
        if !source.resynced {
            continue;
        }
        let seen: HashSet<String> = source.records.iter().map(|r| r.record_name.clone()).collect();
        let removed = reconcile_notes_after_resync(
            target_dir,
            source.shared_zone_owner.as_deref(),
            &seen,
            tracked,
            summary,
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
    handle_vanished_shared_zones(target_dir, &live_owners, tracked, summary, title_mode)?;
    Ok(())
}

/// (docs/DESIGN.md §1) A note
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
/// note's file when its title changed remotely. Returns the file's path
/// from then on.
pub fn rename_for_remote_title(
    target_dir: &Path,
    current_file: &str,
    title_line: &str,
    title_mode: TitleMode,
    used_file_names: &mut HashMap<String, HashSet<String>>,
    on_disk: bool,
) -> Result<String, Error> {
    let current_name = posix::basename(current_file).to_owned();
    if title_mode != TitleMode::Filename || file_name_carries_title(&current_name, title_line) {
        return Ok(current_file.to_owned());
    }
    let dir = note_dir_of(current_file);
    let used_in_dir = used_names_for(used_file_names, &dir);
    used_in_dir.remove(&current_name);

    let mut wanted = unique_file_name(&note_file_name_for(title_line, title_mode), used_in_dir);
    while file_exists(&target_dir.join(&dir).join(&wanted))? {
        used_in_dir.insert(wanted.clone());
        wanted = unique_file_name(&note_file_name_for(title_line, title_mode), used_in_dir);
    }
    used_in_dir.insert(wanted.clone());

    if on_disk {
        let from = target_dir.join(current_file);
        // A file that turned up under the name since the check above is
        // never replaced: the next name.
        while let Err(e) = rename_no_replace(&from, &target_dir.join(&dir).join(&wanted)) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
            wanted = unique_file_name(&note_file_name_for(title_line, title_mode), used_in_dir);
            used_in_dir.insert(wanted.clone());
        }
    }
    Ok(posix::join(&[&dir, &wanted]))
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

/// `handleVanishedSharedZones`: the notes of a sharer no longer sharing with
/// this account are gone for it, as deleted ones are
/// ([`handle_remote_deletion`], [`Gone::Unshared`]).
fn handle_vanished_shared_zones(
    target_dir: &Path,
    live_owners: &HashSet<String>,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
    title_mode: TitleMode,
) -> Result<(), Error> {
    let vanished: Vec<(String, NoteEntry)> = tracked
        .notes
        .iter()
        .filter(|(_, e)| {
            e.shared_zone_owner
                .as_ref()
                .is_some_and(|o| !o.is_empty() && !live_owners.contains(o))
        })
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    for (record_name, entry) in vanished {
        handle_remote_deletion(
            target_dir,
            &record_name,
            &entry,
            tracked,
            summary,
            title_mode,
            Gone::Unshared,
        )?;
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
        handle_remote_deletion(
            target_dir,
            &record_name,
            &entry,
            tracked,
            summary,
            title_mode,
            Gone::Deleted,
        )?;
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
        unshared: Vec::new(),
    };
    let result = reconcile_notes_after_resync(target_dir, shared_zone_owner, seen, &mut tracked, summary, title_mode);
    *notes = tracked.notes;
    *attachments = tracked.attachments;
    *table_attachments = tracked.table_attachments;
    result
}

/// How many notes a pull applies between two saves of what it has applied
/// so far. A first pull of thousands of notes rewrites the state a few
/// hundred times, not thousands.
const CHECKPOINT_EVERY: usize = 25;

/// Whether `entry`'s file is where `layout` puts the note.
fn in_place(layout: &VaultLayout, entry: &NoteEntry) -> bool {
    expected_note_dir(layout, entry).is_none_or(|dir| dir == note_dir_of(&entry.file))
}

/// Whether `layout` leaves every tracked note's file and every folder
/// directory of `state` where it is. Only then is the state saved part way
/// through a pull ([`checkpoint_state`]): the notes are moved to a changed
/// layout after they have all been applied, and a state saved before that
/// would name folders whose notes are still somewhere else, which the next
/// push would read as notes moved here.
fn layout_moves_nothing(state: &CloneState, layout: &VaultLayout, notes: &IndexMap<String, NoteEntry>) -> bool {
    let dirs: HashSet<&String> = layout.all_dirs.iter().collect();
    notes.values().all(|entry| in_place(layout, entry))
        && previous_layout_dirs(PreviousLayout::of(state))
            .iter()
            .all(|dir| dir.is_empty() || dirs.contains(dir))
}

/// What a pull has applied so far, as the state to save part way through:
/// a new note's file already carries its id, and a run killed before it
/// records the note would leave the file untracked, for the next push to
/// send to iCloud as a new note. It is what a pull that failed here would
/// save - the old sync tokens, so everything is listed again; the notes
/// with downloads queued forgetting their version ([`redo_downloads`]) -
/// with one difference: a note this pull gave another folder is still in
/// its old directory (notes move once all are applied), so it is saved as it
/// was when the pull started, and the next pull applies it again.
fn checkpoint_state(
    state: &CloneState,
    layout: &VaultLayout,
    tracked: &Tracked,
    notes_at_start: &IndexMap<String, NoteEntry>,
    trashed: &IndexMap<String, TrashedEntry>,
    downloading: &[String],
) -> CloneState {
    let mut notes: IndexMap<String, NoteEntry> = tracked
        .notes
        .iter()
        .filter_map(|(record_name, entry)| {
            let entry = if in_place(layout, entry) {
                entry
            } else {
                notes_at_start.get(record_name)?
            };
            Some((record_name.clone(), entry.clone()))
        })
        .collect();
    let mut attachments = tracked.attachments.clone();
    redo_downloads(&mut notes, &mut attachments, downloading);
    CloneState {
        account: state.account.clone(),
        sync_token: state.sync_token.clone(),
        shared_zone_sync_tokens: state.shared_zone_sync_tokens.clone(),
        shared_database: state.shared_database.clone(),
        replica_id: state.replica_id.clone(),
        title_mode: state.title_mode,
        notes,
        folders: Some(layout.state_folders.clone()),
        sharer_homes: Some(layout.state_sharer_homes.clone()),
        attachments: Some(attachments),
        table_attachments: Some(tracked.table_attachments.clone()),
        trashed: Some(trashed.clone()),
        extra: state.extra.clone(),
        ..Default::default()
    }
}

/// Adds to `used` the names of the files in `note_dir`'s `attachments/` that
/// no attachment tracks, so a download never replaces one: a file put there
/// to be attached, linked from a note not pushed yet or from none so far.
///
/// Not the files a pull that was killed left: those are linked from a note
/// file it wrote and never recorded (untracked, with an id nothing tracks).
/// The pull after it fetches them again under the same names, so that the
/// note file it writes reads as the one left, and is taken for the note
/// instead of being written a second time beside it.
fn claim_untracked_attachment_names(
    target_dir: &Path,
    note_dir: &str,
    tracked: &Tracked,
    used: &mut HashSet<String>,
    title_mode: TitleMode,
) -> Result<(), Error> {
    const ATTACHMENTS_DIR: &str = "attachments";
    let attachments_dir = posix::join(&[note_dir, ATTACHMENTS_DIR]);
    let tracked_files: HashSet<&str> = tracked.attachments.values().map(|a| a.file.as_str()).collect();
    let mut untracked: Vec<String> = file_names_in(&target_dir.join(&attachments_dir))?
        .into_iter()
        .filter(|name| !tracked_files.contains(posix::join(&[&attachments_dir, name]).as_str()))
        .collect();
    if untracked.is_empty() {
        return Ok(());
    }
    let tracked_notes: HashSet<&str> = tracked.notes.values().map(|e| e.file.as_str()).collect();
    for name in file_names_in(&target_dir.join(note_dir))? {
        let file = posix::join(&[note_dir, &name]);
        if !name.ends_with(".md") || super::push::is_sync_conflict_name(&name) || tracked_notes.contains(file.as_str())
        {
            continue;
        }
        let Some(text) = read_text(&target_dir.join(&file))? else {
            continue;
        };
        let frontmatter = split_frontmatter(&text, split_options(title_mode)).frontmatter;
        if read_note_id(&frontmatter).is_some_and(|id| !tracked.notes.contains_key(&id)) {
            untracked.retain(|attachment| {
                !crate::doc::embeds::has_attachment_link_to(&text, &posix::join(&[ATTACHMENTS_DIR, attachment]))
            });
        }
    }
    used.extend(untracked);
    Ok(())
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

/// Why a tracked note is gone from what iCloud lists for this account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gone {
    /// Deleted (or moved to Recently Deleted) on another device.
    Deleted,
    /// Its owner stopped sharing it with this account.
    Unshared,
}

/// `handleRemoteDeletion`: the note's file goes with the note when it still
/// reads as it did at the last sync. The file is the tracked one, or, when
/// that is gone, the one the note was renamed or moved to here and not
/// pushed yet (it carries the note's id): untouched since, a rename is no
/// reason to bring a deleted note back.
///
/// A file with local edits is kept and stops being tracked. Of a deleted
/// note it loses its id and so becomes a new note, which the next push
/// creates, edits and all. (icloud-md writes delete/modify conflict markers
/// and keeps tracking a note that no longer exists, which no push can ever
/// settle.) Of a note no longer shared it keeps its id, which state's
/// `unshared` lists: push leaves such a file alone, since someone else's
/// note is not uploaded as a note of this account just because access to it
/// ended.
fn handle_remote_deletion(
    target_dir: &Path,
    record_name: &str,
    existing: &NoteEntry,
    tracked: &mut Tracked,
    summary: &mut PullSummary,
    title_mode: TitleMode,
    gone: Gone,
) -> Result<(), Error> {
    let files = match local_file_state(target_dir, existing, record_name, title_mode)? {
        LocalFileState::Missing => note_id_claimants(target_dir, &tracked.notes, record_name, title_mode)?,
        _ => vec![existing.file.clone()],
    };
    let base = read_base_copy(target_dir, record_name)?;
    let mut kept: Vec<String> = Vec::new();
    for file in files {
        let path = target_dir.join(&file);
        let text = read_text(&path)?.unwrap_or_default();
        let envelope = split_frontmatter(&text, split_options(title_mode));
        if base.as_deref() == Some(envelope.body.as_str()) {
            safe_unlink(&path)?;
            continue;
        }
        if gone == Gone::Deleted {
            write_note_file(
                &path,
                &join_frontmatter(&clear_note_id(&envelope.frontmatter), &envelope.body),
            )?;
        }
        kept.push(file);
    }
    let what = match gone {
        Gone::Deleted => "deleted on another device",
        Gone::Unshared => "no longer shared with you",
    };
    if kept.is_empty() {
        drop_tracking(target_dir, record_name, tracked)?;
        summary.removed += 1;
        summary
            .changes
            .push(PullChange::new(PullChangeKind::Remove, existing.file.clone()));
        if gone == Gone::Unshared {
            summary.notices.push(SyncNotice {
                level: NoticeLevel::Warn,
                message: format!("{}: {what} - removed here", existing.file),
            });
        }
        return Ok(());
    }

    // Its attachment files stay with the kept text; only tracking goes.
    tracked.notes.shift_remove(record_name);
    remove_base_copy(target_dir, record_name)?;
    tracked.attachments.retain(|_, a| a.note_record_name != record_name);
    tracked
        .table_attachments
        .retain(|_, a| a.note_record_name != record_name);
    let remark = match gone {
        Gone::Deleted => format!("{what}, but has local edits - kept as a new note, which the next push creates"),
        Gone::Unshared => {
            tracked.unshared.push(record_name.to_owned());
            format!("{what}, but has local edits - kept here as a file that is not synced")
        }
    };
    for file in kept {
        summary.notices.push(SyncNotice {
            level: NoticeLevel::Warn,
            message: format!("{file}: {remark}"),
        });
        let mut change = PullChange::new(PullChangeKind::Untrack, file);
        change.remarks = Some(vec![PullChangeRemark::new("note", remark.clone())]);
        summary.changes.push(change);
    }
    Ok(())
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
    write_note_file(
        &path,
        &compose_note_file(&envelope.frontmatter, &outcome.text, record_name, unrepresentable_title),
    )?;
    if !outcome.has_conflict {
        let remote_file_text = compose_note_file(
            &envelope.frontmatter,
            remote_body_text,
            record_name,
            unrepresentable_title,
        );
        write_base_copy_of(target_dir, record_name, &remote_file_text, title_mode)?;
    }
    Ok(if outcome.has_conflict {
        MergeStatus::Conflict
    } else {
        MergeStatus::Merged
    })
}

// --- the human report ----------------------------------------------------------

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
        return vec![UP_TO_DATE.into()];
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
