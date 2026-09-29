//! The push plan: entries, their `--json` projection, and every refusal and
//! conflict push can plan, enumerated. Ports icloud-md
//! `src/notes/pushPlan.ts` and the reason strings of `src/commands/push.ts`
//! (plus `folderCreate.ts`'s folder refusals). Owner: workstream D.
//!
//! Every `resolution: "refused" | "conflict"` site in push.ts is one
//! [`Refusal`] variant (the push.ts line numbers at v0.6.2 are in each doc
//! comment). Reason strings are copied verbatim; interpolations become
//! fields. [`Refusal::reason`] is what lands in `PlanEntry.reason` (and so in
//! `status --json`), which is byte-compared against icloud-md.
//!
//! push.ts builds some reasons as `"<file>: <message>"` in a `PushSummary`
//! and strips the prefix again with `stripFilePrefix` for the plan entry;
//! those messages are [`PrepareRefusal`] / [`TextUpdateRefusal`], whose
//! `message()` is the unprefixed text.

use serde::{Deserialize, Serialize};

use crate::md::frontmatter::NOTE_ID_KEY;
use crate::md::frontmatter::NOTE_TITLE_KEY;

/// `PlanEntryKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanEntryKind {
    Create,
    CreateFolder,
    Update,
    Delete,
    Move,
    Rename,
}

/// `PlanResolution`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanResolution {
    Ready,
    Refused,
    Conflict,
    Noop,
}

/// `SerializedPlanEntry`: the `--json` shape of `status` / `push --dry-run`
/// entries, keys in `serializePlanEntry`'s order: kind, file, resolution,
/// reason, previousFile, pendingRename, folderTitle, remark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializedPlanEntry {
    pub kind: PlanEntryKind,
    pub file: String,
    pub resolution: PlanResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
}

/// `sharedNoteWriteRefusal`'s two answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharedWriteRefusal {
    IndividuallyShared,
    ReadOnlyFolder,
}

impl SharedWriteRefusal {
    pub fn message(self) -> &'static str {
        match self {
            SharedWriteRefusal::IndividuallyShared => {
                "individually-shared notes can't be edited yet - only notes inside a shared folder can"
            }
            SharedWriteRefusal::ReadOnlyFolder => {
                "this shared folder is read-only for you - the server would reject the edit"
            }
        }
    }
}

/// `planFolderCreates` refusals (folderCreate.ts): why a directory can't
/// become a Notes folder. Shown in place of the generic "can't become one of
/// the account's folders" when present.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FolderRefusal {
    /// `walked` is the path prefix reached.
    SharerHome {
        walked: String,
    },
    SharedFolder {
        walked: String,
    },
    Hidden {
        segment: String,
    },
    ReservedAttachments {
        segment: String,
    },
    ReservedStateDir {
        segment: String,
    },
}

impl FolderRefusal {
    pub fn message(&self) -> String {
        match self {
            FolderRefusal::SharerHome { walked } => format!(
                "\"{walked}/\" is another user's shared area - folders can only be created in your own Notes, \
                 so create it in their shared folder from Notes instead"
            ),
            FolderRefusal::SharedFolder { walked } => format!(
                "\"{walked}/\" is a shared folder - this tool can't create folders inside someone else's share; \
                 create it in Notes, pull, then move the file into it"
            ),
            FolderRefusal::Hidden { segment } => {
                format!("\"{segment}\" is a hidden directory - this tool won't create a Notes folder for one")
            }
            FolderRefusal::ReservedAttachments { segment } => {
                format!("\"{segment}\" is reserved for a folder's downloaded attachments")
            }
            FolderRefusal::ReservedStateDir { segment } => {
                format!("\"{segment}\" is reserved for this tool's own vault state")
            }
        }
    }
}

/// `path.posix.basename`.
fn posix_basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `restore <file>` advice suffix shared by several refusals.
fn restore_advice(file: &str) -> String {
    format!("Run \"icloud-md restore {file}\" to discard your local edit.")
}

/// `prepareNoteTextUpdate`'s refusals (push.ts 1915-2007). `file` is the
/// entry's file as `prepareNoteTextUpdate` saw it (the move target for a
/// retitle).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextUpdateRefusal {
    /// 1926
    NoTextData,
    /// 1932
    TextAsAsset,
    /// 1939
    NotRoundTrip,
    /// 1955
    TouchesEmbed { file: String },
    /// 1964
    DecoderDisagreement,
    /// 1971: `reconcileNoteFormat`'s reason, passed through.
    Reconcile { reason: String, file: String },
    /// 1981
    RebuiltDecodeFailed,
    /// 1989
    RebuiltEmbedFailed,
    /// 1998
    RebuiltFormatFailed,
    /// 2004: a thrown error's message.
    Exception { message: String },
}

impl TextUpdateRefusal {
    pub fn message(&self) -> String {
        match self {
            TextUpdateRefusal::NoTextData => "remote note has no readable text data".into(),
            TextUpdateRefusal::TextAsAsset => "remote note stores its text as an asset - refusing to edit".into(),
            TextUpdateRefusal::NotRoundTrip => {
                "the note's document doesn't round-trip byte-for-byte through our model - refusing to edit".into()
            }
            TextUpdateRefusal::TouchesEmbed { file } => format!(
                "this edit would delete or move an embedded object - embeds can only be edited in Notes itself. {}",
                restore_advice(file)
            ),
            TextUpdateRefusal::DecoderDisagreement => {
                "decoder disagreement on the note's current text - refusing to edit".into()
            }
            TextUpdateRefusal::Reconcile { reason, file } => format!("{reason}. {}", restore_advice(file)),
            TextUpdateRefusal::RebuiltDecodeFailed => {
                "rebuilt document failed decode verification - refusing to push".into()
            }
            TextUpdateRefusal::RebuiltEmbedFailed => {
                "rebuilt document failed embed-structure verification - refusing to push".into()
            }
            TextUpdateRefusal::RebuiltFormatFailed => {
                "rebuilt document failed formatting verification - refusing to push".into()
            }
            TextUpdateRefusal::Exception { message } => message.clone(),
        }
    }
}

/// `prepareUpdate` / `prepareEmbedCandidate` / `restoreStrippedTitle`
/// outcomes that stop an update (push.ts 1469-1685, 1851-1883). `file` is
/// `entry.file`. All are `refused` except `TableGoneRemotely` (a conflict).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PrepareRefusal {
    /// 1491: `detail` is "undecodable" | "missing-body" | "deleted".
    NoLongerEditable { detail: String },
    /// 1496
    Unpublishable {
        unpublishable_reason: Option<String>,
        file: String,
    },
    /// 1523: `parseNoteMarkdown`'s reason (plain path).
    Markdown { reason: String, file: String },
    /// 1579: `planEmbedRepresentations`'s reason.
    EmbedPlan { reason: String, file: String },
    /// 1586: `parseNoteMarkdown`'s reason (embed path).
    EmbedMarkdown { reason: String, file: String },
    /// 1638 (conflict)
    TableGoneRemotely,
    /// 1644: `prepareTableAttachmentUpdate`'s reason.
    TableUpdate { reason: String, file: String },
    /// 1865
    TitleHasEmbedCannotRetitle,
    /// 1877
    NoTitleParagraph,
    /// 1926-2004 via `prepareNoteTextUpdate`.
    TextUpdate(TextUpdateRefusal),
}

impl PrepareRefusal {
    pub fn is_conflict(&self) -> bool {
        matches!(self, PrepareRefusal::TableGoneRemotely)
    }

    pub fn message(&self) -> String {
        match self {
            PrepareRefusal::NoLongerEditable { detail } => {
                format!("remote note is no longer safely editable ({detail})")
            }
            PrepareRefusal::Unpublishable {
                unpublishable_reason,
                file,
            } => format!(
                "this note {} - it can't be safely edited. {}",
                unpublishable_reason
                    .as_deref()
                    .unwrap_or("contains content this tool can't parse"),
                restore_advice(file)
            ),
            PrepareRefusal::Markdown { reason, file }
            | PrepareRefusal::EmbedPlan { reason, file }
            | PrepareRefusal::EmbedMarkdown { reason, file }
            | PrepareRefusal::TableUpdate { reason, file } => format!("{reason}. {}", restore_advice(file)),
            PrepareRefusal::TableGoneRemotely => {
                "a table in this note no longer exists remotely - run \"pull\" to reconcile".into()
            }
            PrepareRefusal::TitleHasEmbedCannotRetitle => format!(
                "this note's title contains an embedded object, so \"{NOTE_TITLE_KEY}\" can't retitle it - \
                 retitle it in Notes instead."
            ),
            PrepareRefusal::NoTitleParagraph => {
                "the remote note has no title paragraph to restore - refusing to edit".into()
            }
            PrepareRefusal::TextUpdate(r) => r.message(),
        }
    }
}

/// `prepareRetitle`'s refusals (push.ts 1703-1776), for a move pair whose
/// rename retitles the note. Every one is wrapped as
/// `"{inner} - rename the file back to {basename(previous_file)}, or retitle
/// the note in Notes instead"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RetitleRefusal {
    /// 1720
    NoLongerEditable { detail: String },
    /// 1724
    Unpublishable { unpublishable_reason: Option<String> },
    /// 1731
    TitleHasEmbed,
    /// 1770: `detail` is a `TextUpdateRefusal` message with the
    /// `"{toFile}: "` prefix removed (`.replace`, first occurrence).
    TextUpdate(TextUpdateRefusal),
    /// 1769 fallback when no refusal was recorded.
    Unapplied,
}

impl RetitleRefusal {
    fn inner(&self) -> String {
        match self {
            RetitleRefusal::NoLongerEditable { detail } => {
                format!("renaming this note would retitle it, but the note is no longer safely editable ({detail})")
            }
            RetitleRefusal::Unpublishable { unpublishable_reason } => format!(
                "renaming this note would retitle it, but this note {}",
                unpublishable_reason
                    .as_deref()
                    .unwrap_or("contains content this tool can't parse")
            ),
            RetitleRefusal::TitleHasEmbed => {
                "this note's title contains an embedded object, so its file name doesn't carry the title".into()
            }
            RetitleRefusal::TextUpdate(r) => format!("renaming this note would retitle it, but {}", r.message()),
            RetitleRefusal::Unapplied => {
                "renaming this note would retitle it, but the new title couldn't be applied".into()
            }
        }
    }

    pub fn message(&self, previous_file: &str) -> String {
        format!(
            "{} - rename the file back to {}, or retitle the note in Notes instead",
            self.inner(),
            posix_basename(previous_file)
        )
    }
}

/// Every refused/conflict plan entry push can produce. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Refusal {
    // --- plan-time, no network (buildPushPlan) ---
    /// 262 (rename, conflict)
    PendingRename,
    /// 327 (update, refused)
    UpdateSharedNote(SharedWriteRefusal),
    /// 335 (update, conflict)
    UpdateConflictMarkers,
    /// 348 (update, refused)
    UpdateEmptied,
    /// 358 (update, refused); `file` = entry.file
    UpdateUnknownContent { file: String },
    /// 379 (update, refused)
    UpdateNewAttachmentReference { file: String },
    /// 436 (create, refused): one of several files sharing an `apple-note-id`
    /// whose note's own file is gone. `others` = the other claiming files in
    /// claim order; `other_count` = `claim.files.length - 1`.
    AmbiguousNoteId {
        other_count: usize,
        others: Vec<String>,
        tracked_file: Option<String>,
    },
    /// 504 (delete, refused)
    DeleteSharedNote { file: String },
    /// 561 (move, refused); `previous_file` = the tracked path
    MoveSharedNote { previous_file: String },
    /// 569 (move, refused)
    MoveToTopLevel,
    /// 577 (move, refused)
    MoveIntoUnfolderableDir {
        dir: String,
        folder_refusal: Option<FolderRefusal>,
    },
    /// 583 (move, refused)
    MoveIntoSharerArea,
    /// 597 (move, refused)
    MoveWithAttachments,
    /// 640 (create, refused)
    CreateAtTopLevel,
    /// 650 (create, refused)
    CreateInUnfolderableDir {
        dir: String,
        folder_refusal: Option<FolderRefusal>,
    },
    /// 663 (create, refused)
    CreateLooseInSharerHome,
    /// 672 (create, refused)
    CreateInReadOnlyShare,
    /// 680 (create, refused)
    CreateEmptyFile,
    /// 688 (create, refused - unlike the update case)
    CreateConflictMarkers,
    /// 697 (create, refused)
    CreateUnknownContent,
    /// 706 (create, refused)
    CreateEmbedMarker,
    /// 715 (create, refused)
    CreateAttachmentReference,

    // --- after the live lookup ---
    /// 845 (move, conflict)
    MoveGoneRemotely,
    /// 849 (move, conflict)
    MoveChangedRemotely,
    /// 860 (move): `prepareRetitle` refused; `previous_file` = tracked path.
    MoveRetitle {
        refusal: RetitleRefusal,
        previous_file: String,
    },
    /// 967 (delete, conflict)
    DeleteChangedRemotely,
    /// 1009 (create, refused): `parseNoteMarkdown`'s reason.
    CreateMarkdown { reason: String },
    /// 1024 (create, refused): `reconcileNoteFormat`'s reason.
    CreateReconcile { reason: String },
    /// 1039 (create, refused)
    CreateVerificationFailed,
    /// 1047 (create, refused): a thrown error's message.
    CreateBuildError { message: String },
    /// 1127 (update, conflict)
    UpdateGoneRemotely,
    /// 1164 (update, conflict): changed remotely and not eagerly mergeable.
    UpdateChangedRemotelyUnmergeable,
    /// 1356 (update, conflict): `planRemoteChangedMerge` left markers.
    MergedWithConflicts,
    /// 1366 (update, conflict): merged cleanly, push again.
    MergedCleanly,
    /// 1194 (update, refused): title-only candidate, shared note.
    TitleOnlySharedNote(SharedWriteRefusal),
    /// 1231/1237 (update): `prepareUpdate` stopped.
    UpdatePrepare(PrepareRefusal),
    /// 1237 fallback: `prepareUpdate` stopped without recording why.
    UpdateRefusedUnspecified,
}

impl Refusal {
    pub fn resolution(&self) -> PlanResolution {
        use Refusal::*;
        match self {
            PendingRename
            | UpdateConflictMarkers
            | MoveGoneRemotely
            | MoveChangedRemotely
            | DeleteChangedRemotely
            | UpdateGoneRemotely
            | UpdateChangedRemotelyUnmergeable
            | MergedWithConflicts
            | MergedCleanly => PlanResolution::Conflict,
            UpdatePrepare(p) if p.is_conflict() => PlanResolution::Conflict,
            _ => PlanResolution::Refused,
        }
    }

    /// The plan entry's `reason`, verbatim.
    pub fn reason(&self) -> String {
        use Refusal::*;
        match self {
            PendingRename => {
                "rename deferred by a previous pull and not yet performed - rename it, or run \"pull\" to have icloud-md do it"
                    .into()
            }
            UpdateSharedNote(r) | TitleOnlySharedNote(r) => r.message().into(),
            UpdateConflictMarkers | CreateConflictMarkers => {
                "still contains diff3 conflict markers - resolve them before pushing".into()
            }
            UpdateEmptied => "pushing a fully emptied note isn't supported yet - edit it in Notes instead".into(),
            UpdateUnknownContent { file } => format!(
                "this note contains content this tool can't parse and can never be pushed - \
                 run \"icloud-md restore {file}\" to discard your local edit."
            ),
            UpdateNewAttachmentReference { file } => format!(
                "contains an \"attachments/...\" reference, but this tool can't upload new attachments - \
                 remove it, or run \"icloud-md restore {file}\" to discard the edit."
            ),
            AmbiguousNoteId { other_count, others, tracked_file } => format!(
                "shares its \"{NOTE_ID_KEY}\" with {other_count} other file(s) ({}), and the note's own file ({}) is gone, \
                 so which one is the original can't be told apart - remove the \"{NOTE_ID_KEY}\" line from every copy but \
                 one, then push again. The note itself is left untouched in the meantime.",
                others.join(", "),
                tracked_file.as_deref().unwrap_or("unknown")
            ),
            DeleteSharedNote { file } => format!(
                "deleting notes shared by someone else isn't supported - run \"icloud-md restore {file}\" to bring the file back"
            ),
            MoveSharedNote { previous_file } => format!(
                "renaming or moving notes shared by someone else isn't supported yet - move the file back to {previous_file}"
            ),
            MoveToTopLevel => {
                "moved to the top level of the clone, but every note lives in a folder - move it into a folder directory".into()
            }
            MoveIntoUnfolderableDir { dir, folder_refusal } => match folder_refusal {
                Some(r) => r.message(),
                None => format!("moved into \"{dir}/\", which can't become one of the account's folders"),
            },
            MoveIntoSharerArea => "moved into a sharer's area - notes can't be moved into someone else's share".into(),
            MoveWithAttachments => "this note has attachments, whose files can't be relocated safely yet - move it back \
                                    (or move the note in Notes and pull instead)"
                .into(),
            CreateAtTopLevel => "sits at the top level of the clone, outside any folder - every note lives in a folder, \
                                 so move it into one of the folder directories first"
                .into(),
            CreateInUnfolderableDir { dir, folder_refusal } => match folder_refusal {
                Some(r) => r.message(),
                None => format!("sits in \"{dir}/\", which can't become one of the account's folders"),
            },
            CreateLooseInSharerHome => {
                "sits loose at the top of a sharer's area - notes can only be created inside one of their shared folders".into()
            }
            CreateInReadOnlyShare => {
                "sits in a shared folder you only have read access to - the server would reject the create".into()
            }
            CreateEmptyFile => "the file is empty - nothing to create".into(),
            CreateUnknownContent => "this file contains the unknown-content banner - remove it before pushing".into(),
            CreateEmbedMarker => "contains an embed marker, but this tool can't create embeds - remove it before pushing".into(),
            CreateAttachmentReference => {
                "contains an \"attachments/...\" reference, but this tool can't upload new attachments - remove it first."
                    .into()
            }
            MoveGoneRemotely | UpdateGoneRemotely => "no longer exists remotely - run \"pull\" to reconcile".into(),
            MoveChangedRemotely | DeleteChangedRemotely => {
                "changed remotely since the last pull - run \"pull\" first".into()
            }
            MoveRetitle { refusal, previous_file } => refusal.message(previous_file),
            CreateMarkdown { reason } | CreateReconcile { reason } => reason.clone(),
            CreateVerificationFailed => "built document failed decode verification - refusing to create".into(),
            CreateBuildError { message } => message.clone(),
            UpdateChangedRemotelyUnmergeable => {
                "changed remotely since the last pull - run \"pull\" (which merges) first".into()
            }
            MergedWithConflicts => {
                "changed remotely since the last pull - merged with conflict markers, resolve manually".into()
            }
            MergedCleanly => "merged remote changes into your local edit - re-run push to upload".into(),
            UpdatePrepare(p) => p.message(),
            UpdateRefusedUnspecified => "refused".into(),
        }
    }
}

/// `renderPlan` options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderPlanOptions {
    pub preview: bool,
    pub unchanged: Option<usize>,
}

/// `PlanEntry`: what `buildPushPlan` produces (minus push's `execute`,
/// which lives in `push.rs`). `refusal` is kept alongside the `reason` it
/// produced so tests can name the site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub kind: PlanEntryKind,
    pub file: String,
    pub resolution: PlanResolution,
    pub reason: Option<String>,
    pub folder_title: Option<String>,
    pub previous_file: Option<String>,
    pub pending_rename: Option<String>,
    pub remark: Option<String>,
    pub refusal: Option<Refusal>,
}

impl PlanEntry {
    /// An entry with no reason.
    pub fn new(kind: PlanEntryKind, file: impl Into<String>, resolution: PlanResolution) -> Self {
        PlanEntry {
            kind,
            file: file.into(),
            resolution,
            reason: None,
            folder_title: None,
            previous_file: None,
            pending_rename: None,
            remark: None,
            refusal: None,
        }
    }

    /// A refused or conflicting entry: resolution and reason from `refusal`.
    pub fn refused(kind: PlanEntryKind, file: impl Into<String>, refusal: Refusal) -> Self {
        let mut entry = PlanEntry::new(kind, file, refusal.resolution());
        entry.reason = Some(refusal.reason());
        entry.refusal = Some(refusal);
        entry
    }

    /// `serializePlanEntry`.
    pub fn serialize(&self) -> SerializedPlanEntry {
        SerializedPlanEntry {
            kind: self.kind,
            file: self.file.clone(),
            resolution: self.resolution,
            reason: self.reason.clone(),
            previous_file: self.previous_file.clone(),
            pending_rename: self.pending_rename.clone(),
            folder_title: self.folder_title.clone(),
            remark: self.remark.clone(),
        }
    }
}

fn label_of(kind: PlanEntryKind) -> &'static str {
    match kind {
        PlanEntryKind::Create => "new file:",
        PlanEntryKind::CreateFolder => "new dir:",
        PlanEntryKind::Update => "modified:",
        PlanEntryKind::Delete => "deleted:",
        PlanEntryKind::Move => "moved:",
        PlanEntryKind::Rename => "rename:",
    }
}

/// `LABEL_WIDTH`: the longest label.
const LABEL_WIDTH: usize = 9;

/// `renderPlan`: the git-status-style listing (human text; not
/// byte-compared).
pub fn render_plan(
    entries: &[SerializedPlanEntry],
    format_path: &dyn Fn(&str) -> String,
    options: RenderPlanOptions,
) -> Vec<String> {
    use super::report::{LISTING_INDENT, labelled_line, remark_line};
    let visible: Vec<&SerializedPlanEntry> = entries
        .iter()
        .filter(|e| e.resolution != PlanResolution::Noop)
        .collect();
    if visible.is_empty() {
        return vec![match options.unchanged {
            Some(n) if n > 0 => format!(
                "Nothing to push; all {n} {} the last pull.",
                if n == 1 { "note matches" } else { "notes match" }
            ),
            _ => "Nothing to push.".into(),
        }];
    }

    let mut lines: Vec<String> = Vec::new();
    if options.preview {
        lines.push("Changes not yet pushed to iCloud:".into());
        lines.push("  (use \"icloud-notes-sync push\" to send them)".into());
        lines.push("  (use \"icloud-notes-sync restore <file>\" to discard a local edit)".into());
        lines.push(String::new());
    } else {
        lines.push(String::new());
    }
    let (mut to_create, mut to_create_folder, mut to_update, mut to_delete, mut to_move) = (0, 0, 0, 0, 0);
    let (mut refused, mut conflicts) = (0, 0);
    for entry in visible {
        let subject = match entry.kind {
            PlanEntryKind::CreateFolder => format!("{}/", format_path(&entry.file)),
            PlanEntryKind::Move => format!(
                "{} -> {}",
                format_path(entry.previous_file.as_deref().unwrap_or(&entry.file)),
                format_path(&entry.file)
            ),
            PlanEntryKind::Rename if entry.pending_rename.is_some() => format!(
                "{} -> {}",
                format_path(&entry.file),
                format_path(entry.pending_rename.as_deref().unwrap_or_default())
            ),
            _ => format_path(&entry.file),
        };
        lines.push(format!(
            "{LISTING_INDENT}{}",
            labelled_line(label_of(entry.kind), LABEL_WIDTH, &subject)
        ));
        if matches!(entry.resolution, PlanResolution::Refused | PlanResolution::Conflict) {
            let reason = entry
                .reason
                .as_deref()
                .unwrap_or("refused")
                .split(entry.file.as_str())
                .collect::<Vec<_>>()
                .join(&format_path(&entry.file));
            lines.push(format!(
                "{LISTING_INDENT}{}",
                remark_line(LABEL_WIDTH, &format!("! {reason}"))
            ));
            if entry.resolution == PlanResolution::Refused {
                refused += 1;
            } else {
                conflicts += 1;
            }
            continue;
        }
        if let Some(remark) = &entry.remark {
            lines.push(format!("{LISTING_INDENT}{}", remark_line(LABEL_WIDTH, remark)));
        }
        match entry.kind {
            PlanEntryKind::Create => to_create += 1,
            PlanEntryKind::CreateFolder => to_create_folder += 1,
            PlanEntryKind::Update => to_update += 1,
            PlanEntryKind::Move => to_move += 1,
            PlanEntryKind::Delete => to_delete += 1,
            PlanEntryKind::Rename => {}
        }
    }

    let mut summary = format!("{to_create} to create, {to_update} changed, {to_delete} to delete");
    if to_move > 0 {
        summary.push_str(&format!(", {to_move} to move"));
    }
    if to_create_folder > 0 {
        summary.push_str(&format!(
            ", {to_create_folder} new folder{}",
            if to_create_folder == 1 { "" } else { "s" }
        ));
    }
    summary.push('.');
    if conflicts > 0 || refused > 0 {
        let mut parts = Vec::new();
        if conflicts > 0 {
            parts.push(format!("{conflicts} conflict(s)"));
        }
        if refused > 0 {
            parts.push(format!("{refused} refused"));
        }
        summary.push_str(&format!(" ({})", parts.join(", ")));
    }
    lines.push(String::new());
    lines.push(summary);
    if let Some(n) = options.unchanged.filter(|n| *n > 0) {
        lines.push(if n == 1 {
            "1 other note matches the last pull.".into()
        } else {
            format!("{n} other notes match the last pull.")
        });
    }
    lines
}

/// `countUnchangedNotes`: tracked notes minus the non-create, non-rename,
/// non-noop entries.
pub fn count_unchanged_notes(entries: &[SerializedPlanEntry], tracked_notes: usize) -> usize {
    let touched = entries
        .iter()
        .filter(|e| {
            e.kind != PlanEntryKind::Create && e.kind != PlanEntryKind::Rename && e.resolution != PlanResolution::Noop
        })
        .count();
    tracked_notes.saturating_sub(touched)
}

/// `stripFilePrefix`.
pub fn strip_file_prefix<'a>(message: &'a str, file: &str) -> &'a str {
    message.strip_prefix(&format!("{file}: ")).unwrap_or(message)
}
