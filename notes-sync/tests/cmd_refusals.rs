//! Every refused/conflict plan entry icloud-md 0.6.2's push can produce,
//! site by site (`src/commands/push.ts`, `src/notes/folderCreate.ts`), with
//! the exact `reason` string each one puts in `status --json` / `push
//! --dry-run --json`. The port builds every such entry from
//! `cmd::plan::Refusal`, so this table is the checklist that none was lost:
//! `every_refusal_variant_is_listed` fails to compile when a variant is
//! added without a row here. Rows with line 0 are port-only refusals (deliberate
//! differences from 0.6.2, docs/PORT_PLAN.md §1) with no push.ts site.

use icloud_notes_sync::cmd::plan::{
    FolderRefusal, PlanEntry, PlanEntryKind, PlanResolution, PrepareRefusal, Refusal, RetitleRefusal,
    SharedWriteRefusal, TextUpdateRefusal,
};

const FILE: &str = "Notes/Pie.md";

/// (push.ts line, a fragment of source at that line, the refusal, its
/// resolution, its reason verbatim).
fn sites() -> Vec<(u32, &'static str, Refusal, PlanResolution, String)> {
    use PlanResolution::{Conflict, Refused};
    let restore = format!("Run \"icloud-notes restore {FILE}\" to discard your local edit.");
    let text = |r: TextUpdateRefusal| Refusal::UpdatePrepare(PrepareRefusal::TextUpdate(r));
    let retitle = |r: RetitleRefusal| Refusal::MoveRetitle {
        refusal: r,
        previous_file: "Notes/Old Title.md".into(),
    };
    let rename_back = " - rename the file back to Old Title.md, or retitle the note in Notes instead";
    vec![
        (
            262,
            "rename deferred by a previous pull",
            Refusal::PendingRename,
            Conflict,
            "rename deferred by a previous pull and not yet performed - rename it, or run \"pull\" to have it done for you"
                .into(),
        ),
        (
            327,
            "reason: sharedRefusal",
            Refusal::UpdateSharedNote(SharedWriteRefusal::IndividuallyShared),
            Refused,
            "individually-shared notes can't be edited yet - only notes inside a shared folder can".into(),
        ),
        (
            327,
            "reason: sharedRefusal",
            Refusal::UpdateSharedNote(SharedWriteRefusal::ReadOnlyFolder),
            Refused,
            "this shared folder is read-only for you - the server would reject the edit".into(),
        ),
        (
            335,
            "still contains diff3 conflict markers",
            Refusal::UpdateConflictMarkers,
            Conflict,
            "still contains diff3 conflict markers - resolve them before pushing".into(),
        ),
        (
            348,
            "pushing a fully emptied note",
            Refusal::UpdateEmptied,
            Refused,
            "pushing a fully emptied note isn't supported yet - edit it in Notes instead".into(),
        ),
        (
            358,
            "this note contains content this tool can't parse and can never be pushed",
            Refusal::UpdateUnknownContent { file: FILE.into() },
            Refused,
            format!(
                "this note contains content this tool can't parse and can never be pushed - run \"icloud-notes restore {FILE}\" to discard your local edit."
            ),
        ),
        (
            379,
            "reference, but this tool can",
            Refusal::UpdateNewAttachmentReference { file: FILE.into() },
            Refused,
            format!(
                "contains an \"attachments/...\" reference, but this tool can't upload new attachments - remove it, or run \"icloud-notes restore {FILE}\" to discard the edit."
            ),
        ),
        (
            436,
            "shares its",
            Refusal::AmbiguousNoteId {
                other_count: 2,
                others: vec!["Notes/B.md".into(), "Notes/C.md".into()],
                tracked_file: Some("Notes/Gone.md".into()),
            },
            Refused,
            "shares its \"apple-note-id\" with 2 other file(s) (Notes/B.md, Notes/C.md), and the note's own file \
             (Notes/Gone.md) is gone, so which one is the original can't be told apart - remove the \"apple-note-id\" line \
             from every copy but one, then push again. The note itself is left untouched in the meantime."
                .into(),
        ),
        (
            436,
            "shares its",
            Refusal::AmbiguousNoteId {
                other_count: 1,
                others: vec!["Notes/B.md".into()],
                tracked_file: None,
            },
            Refused,
            "shares its \"apple-note-id\" with 1 other file(s) (Notes/B.md), and the note's own file (unknown) is gone, so \
             which one is the original can't be told apart - remove the \"apple-note-id\" line from every copy but one, \
             then push again. The note itself is left untouched in the meantime."
                .into(),
        ),
        (
            504,
            "deleting notes shared by someone else",
            Refusal::DeleteSharedNote { file: FILE.into() },
            Refused,
            format!(
                "deleting notes shared by someone else isn't supported - run \"icloud-notes restore {FILE}\" to bring the file back"
            ),
        ),
        (
            561,
            "renaming or moving notes shared by someone else",
            Refusal::MoveSharedNote {
                previous_file: "Pat/Shared/Pie.md".into(),
            },
            Refused,
            "renaming or moving notes shared by someone else isn't supported yet - move the file back to Pat/Shared/Pie.md"
                .into(),
        ),
        (
            569,
            "moved to the top level of the clone",
            Refusal::MoveToTopLevel,
            Refused,
            "moved to the top level of the clone, but every note lives in a folder - move it into a folder directory".into(),
        ),
        (
            578,
            "which can't become one of the account's folders",
            Refusal::MoveIntoUnfolderableDir {
                dir: "Somewhere".into(),
                folder_refusal: None,
            },
            Refused,
            "moved into \"Somewhere/\", which can't become one of the account's folders".into(),
        ),
        (
            577,
            "folderRefusals.get(toDir)",
            Refusal::MoveIntoUnfolderableDir {
                dir: ".obsidian".into(),
                folder_refusal: Some(FolderRefusal::Hidden {
                    segment: ".obsidian".into(),
                }),
            },
            Refused,
            "\".obsidian\" is a hidden directory - this tool won't create a Notes folder for one".into(),
        ),
        (
            583,
            "moved into a sharer's area",
            Refusal::MoveIntoSharerArea,
            Refused,
            "moved into a sharer's area - notes can't be moved into someone else's share".into(),
        ),
        (
            597,
            "this note has attachments, whose files can't be relocated safely yet",
            Refusal::MoveWithAttachments,
            Refused,
            "this note has attachments, whose files can't be relocated safely yet - move it back (or move the note in Notes and pull instead)"
                .into(),
        ),
        (
            640,
            "sits at the top level of the clone",
            Refusal::CreateAtTopLevel,
            Refused,
            "sits at the top level of the clone, outside any folder - every note lives in a folder, so move it into one of the folder directories first"
                .into(),
        ),
        (
            650,
            "which can't become one of the account's folders",
            Refusal::CreateInUnfolderableDir {
                dir: "Somewhere".into(),
                folder_refusal: None,
            },
            Refused,
            "sits in \"Somewhere/\", which can't become one of the account's folders".into(),
        ),
        (
            650,
            "folderRefusals.get(dir)",
            Refusal::CreateInUnfolderableDir {
                dir: "Notes/attachments".into(),
                folder_refusal: Some(FolderRefusal::ReservedAttachments {
                    segment: "attachments".into(),
                }),
            },
            Refused,
            "\"attachments\" is reserved for a folder's downloaded attachments".into(),
        ),
        (
            663,
            "sits loose at the top of a sharer's area",
            Refusal::CreateLooseInSharerHome,
            Refused,
            "sits loose at the top of a sharer's area - notes can only be created inside one of their shared folders".into(),
        ),
        (
            672,
            "sits in a shared folder you only have read access to",
            Refusal::CreateInReadOnlyShare,
            Refused,
            "sits in a shared folder you only have read access to - the server would reject the create".into(),
        ),
        (
            680,
            "the file is empty - nothing to create",
            Refusal::CreateEmptyFile,
            Refused,
            "the file is empty - nothing to create".into(),
        ),
        (
            688,
            "still contains diff3 conflict markers",
            Refusal::CreateConflictMarkers,
            Refused,
            "still contains diff3 conflict markers - resolve them before pushing".into(),
        ),
        (
            697,
            "unknown-content banner",
            Refusal::CreateUnknownContent,
            Refused,
            "this file contains the unknown-content banner - remove it before pushing".into(),
        ),
        (
            706,
            "contains an embed marker",
            Refusal::CreateEmbedMarker,
            Refused,
            "contains an embed marker, but this tool can't create embeds - remove it before pushing".into(),
        ),
        (
            715,
            "remove it first.",
            Refusal::CreateAttachmentReference,
            Refused,
            "contains an \"attachments/...\" reference, but this tool can't upload new attachments - remove it first.".into(),
        ),
        (
            // Port only (docs/PORT_PLAN.md §1): no push.ts site.
            0,
            "",
            Refusal::CreateDuplicatesTrackedNote {
                tracked_file: "Notes/Pie.md".into(),
            },
            Refused,
            "carries the \"apple-note-id\" of Notes/Pie.md, a note this clone already tracks, so pushing it would \
             create a duplicate of that note - delete this file if it is a leftover copy, or remove its \
             \"apple-note-id\" line to push it as a new note"
                .into(),
        ),
        (
            845,
            "no longer exists remotely",
            Refusal::MoveGoneRemotely,
            Conflict,
            "no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            849,
            "changed remotely since the last pull - run \"pull\" first",
            Refusal::MoveChangedRemotely,
            Conflict,
            "changed remotely since the last pull - run \"pull\" first".into(),
        ),
        (
            1720,
            "the note is no longer safely editable",
            retitle(RetitleRefusal::NoLongerEditable {
                detail: "undecodable".into(),
            }),
            Refused,
            format!(
                "renaming this note would retitle it, but the note is no longer safely editable (undecodable){rename_back}"
            ),
        ),
        (
            1724,
            "contains content this tool can't parse",
            retitle(RetitleRefusal::Unpublishable {
                unpublishable_reason: None,
            }),
            Refused,
            format!("renaming this note would retitle it, but this note contains content this tool can't parse{rename_back}"),
        ),
        (
            1731,
            "its file name doesn't carry the title",
            retitle(RetitleRefusal::TitleHasEmbed),
            Refused,
            format!("this note's title contains an embedded object, so its file name doesn't carry the title{rename_back}"),
        ),
        (
            1770,
            "renaming this note would retitle it, but ${detail}",
            retitle(RetitleRefusal::TextUpdate(TextUpdateRefusal::NotRoundTrip)),
            Refused,
            format!(
                "renaming this note would retitle it, but the note's document doesn't round-trip byte-for-byte through our model - refusing to edit{rename_back}"
            ),
        ),
        (
            1769,
            "the new title couldn't be applied",
            retitle(RetitleRefusal::Unapplied),
            Refused,
            format!("renaming this note would retitle it, but the new title couldn't be applied{rename_back}"),
        ),
        (
            1009,
            "reason: parsed.reason",
            Refusal::CreateMarkdown {
                reason: "a parse refusal".into(),
            },
            Refused,
            "a parse refusal".into(),
        ),
        (
            1024,
            "reason: reconciled.reason",
            Refusal::CreateReconcile {
                reason: "a reconcile refusal".into(),
            },
            Refused,
            "a reconcile refusal".into(),
        ),
        (
            1039,
            "built document failed decode verification - refusing to create",
            Refusal::CreateVerificationFailed,
            Refused,
            "built document failed decode verification - refusing to create".into(),
        ),
        (
            1047,
            "reason: message",
            Refusal::CreateBuildError {
                message: "a thrown error".into(),
            },
            Refused,
            "a thrown error".into(),
        ),
        (
            1127,
            "no longer exists remotely",
            Refusal::UpdateGoneRemotely,
            Conflict,
            "no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            1164,
            "(which merges) first",
            Refusal::UpdateChangedRemotelyUnmergeable,
            Conflict,
            "changed remotely since the last pull - run \"pull\" (which merges) first".into(),
        ),
        (
            1194,
            "reason: sharedRefusal",
            Refusal::TitleOnlySharedNote(SharedWriteRefusal::IndividuallyShared),
            Refused,
            "individually-shared notes can't be edited yet - only notes inside a shared folder can".into(),
        ),
        (
            1356,
            "merged with conflict markers, resolve manually",
            Refusal::MergedWithConflicts,
            Conflict,
            "changed remotely since the last pull - merged with conflict markers, resolve manually".into(),
        ),
        (
            1366,
            "merged remote changes into your local edit",
            Refusal::MergedCleanly,
            Conflict,
            "merged remote changes into your local edit - re-run push to upload".into(),
        ),
        (
            1237,
            "newRefusal ?? \"refused\"",
            Refusal::UpdateRefusedUnspecified,
            Refused,
            "refused".into(),
        ),
        // prepareUpdate & co. (reported through 1231 / 1237 with the
        // "<file>: " prefix stripped)
        (
            1491,
            "remote note is no longer safely editable",
            Refusal::UpdatePrepare(PrepareRefusal::NoLongerEditable {
                detail: "missing-body".into(),
            }),
            Refused,
            "remote note is no longer safely editable (missing-body)".into(),
        ),
        (
            1496,
            "it can't be safely edited",
            Refusal::UpdatePrepare(PrepareRefusal::Unpublishable {
                unpublishable_reason: Some("contains embedded content this tool can't parse (com.example)".into()),
                file: FILE.into(),
            }),
            Refused,
            format!(
                "this note contains embedded content this tool can't parse (com.example) - it can't be safely edited. {restore}"
            ),
        ),
        (
            1523,
            "${parsed.reason}. Run",
            Refusal::UpdatePrepare(PrepareRefusal::Markdown {
                reason: "unsupported markdown".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("unsupported markdown. {restore}"),
        ),
        (
            1579,
            "${plan.reason}. Run",
            Refusal::UpdatePrepare(PrepareRefusal::EmbedPlan {
                reason: "an embed moved".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("an embed moved. {restore}"),
        ),
        (
            1586,
            "${parsed.reason}. Run",
            Refusal::UpdatePrepare(PrepareRefusal::EmbedMarkdown {
                reason: "unsupported markdown".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("unsupported markdown. {restore}"),
        ),
        (
            1638,
            "a table in this note no longer exists remotely",
            Refusal::UpdatePrepare(PrepareRefusal::TableGoneRemotely),
            Conflict,
            "a table in this note no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            1644,
            "${result.reason}. Run",
            Refusal::UpdatePrepare(PrepareRefusal::TableUpdate {
                reason: "the table changed shape".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("the table changed shape. {restore}"),
        ),
        (
            1865,
            "can't retitle it",
            Refusal::UpdatePrepare(PrepareRefusal::TitleHasEmbedCannotRetitle),
            Refused,
            "this note's title contains an embedded object, so \"apple-note-title\" can't retitle it - retitle it in Notes instead."
                .into(),
        ),
        (
            1877,
            "the remote note has no title paragraph to restore",
            Refusal::UpdatePrepare(PrepareRefusal::NoTitleParagraph),
            Refused,
            "the remote note has no title paragraph to restore - refusing to edit".into(),
        ),
        (
            1926,
            "remote note has no readable text data",
            text(TextUpdateRefusal::NoTextData),
            Refused,
            "remote note has no readable text data".into(),
        ),
        (
            1932,
            "remote note stores its text as an asset",
            text(TextUpdateRefusal::TextAsAsset),
            Refused,
            "remote note stores its text as an asset - refusing to edit".into(),
        ),
        (
            1939,
            "doesn't round-trip byte-for-byte",
            text(TextUpdateRefusal::NotRoundTrip),
            Refused,
            "the note's document doesn't round-trip byte-for-byte through our model - refusing to edit".into(),
        ),
        (
            1955,
            "this edit would delete or move an embedded object",
            text(TextUpdateRefusal::TouchesEmbed { file: FILE.into() }),
            Refused,
            format!(
                "this edit would delete or move an embedded object - embeds can only be edited in Notes itself. {restore}"
            ),
        ),
        (
            1964,
            "decoder disagreement on the note's current text",
            text(TextUpdateRefusal::DecoderDisagreement),
            Refused,
            "decoder disagreement on the note's current text - refusing to edit".into(),
        ),
        (
            1971,
            "${reconciled.reason}. Run",
            text(TextUpdateRefusal::Reconcile {
                reason: "a checklist changed".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("a checklist changed. {restore}"),
        ),
        (
            1981,
            "rebuilt document failed decode verification",
            text(TextUpdateRefusal::RebuiltDecodeFailed),
            Refused,
            "rebuilt document failed decode verification - refusing to push".into(),
        ),
        (
            1989,
            "rebuilt document failed embed-structure verification",
            text(TextUpdateRefusal::RebuiltEmbedFailed),
            Refused,
            "rebuilt document failed embed-structure verification - refusing to push".into(),
        ),
        (
            1998,
            "rebuilt document failed formatting verification",
            text(TextUpdateRefusal::RebuiltFormatFailed),
            Refused,
            "rebuilt document failed formatting verification - refusing to push".into(),
        ),
        (
            2004,
            "summary.refused.push(`${entry.file}: ${message}`)",
            text(TextUpdateRefusal::Exception {
                message: "invariant violated".into(),
            }),
            Refused,
            "invariant violated".into(),
        ),
    ]
}

/// folderCreate.ts's refusals (what a move/create into that directory shows).
fn folder_sites() -> Vec<(&'static str, FolderRefusal, String)> {
    vec![
        (
            "is another user's shared area",
            FolderRefusal::SharerHome { walked: "Pat".into() },
            "\"Pat/\" is another user's shared area - folders can only be created in your own Notes, so create it in their shared folder from Notes instead".into(),
        ),
        (
            "is a shared folder - this tool can't create folders inside someone else's share",
            FolderRefusal::SharedFolder {
                walked: "Pat/Shared".into(),
            },
            "\"Pat/Shared/\" is a shared folder - this tool can't create folders inside someone else's share; create it in Notes, pull, then move the file into it".into(),
        ),
        (
            "is a hidden directory",
            FolderRefusal::Hidden { segment: ".git".into() },
            "\".git\" is a hidden directory - this tool won't create a Notes folder for one".into(),
        ),
        (
            "is reserved for a folder's downloaded attachments",
            FolderRefusal::ReservedAttachments {
                segment: "attachments".into(),
            },
            "\"attachments\" is reserved for a folder's downloaded attachments".into(),
        ),
        (
            "is reserved for this tool's own vault state",
            FolderRefusal::ReservedStateDir {
                segment: ".icloud-md".into(),
            },
            "\".icloud-md\" is reserved for this tool's own vault state".into(),
        ),
    ]
}

#[test]
fn every_site_has_its_verbatim_reason_and_resolution() {
    for (line, _, refusal, resolution, reason) in sites() {
        assert_eq!(refusal.reason(), reason, "push.ts:{line}");
        assert_eq!(refusal.resolution(), resolution, "push.ts:{line}");
        let entry = PlanEntry::refused(PlanEntryKind::Update, FILE, refusal.clone());
        assert_eq!(entry.resolution, resolution);
        assert_eq!(entry.reason.as_deref(), Some(reason.as_str()));
        assert_eq!(entry.serialize().reason.as_deref(), Some(reason.as_str()));
    }
    for (_, refusal, message) in folder_sites() {
        assert_eq!(refusal.message(), message);
    }
}

/// Fails to compile when a `Refusal` variant has no row in `sites()`.
#[test]
fn every_refusal_variant_is_listed() {
    let listed: Vec<Refusal> = sites().into_iter().map(|(_, _, r, _, _)| r).collect();
    let covers = |pred: &dyn Fn(&Refusal) -> bool| listed.iter().any(pred);
    let all: [fn(&Refusal) -> bool; 1] = [|r| {
        use Refusal::*;
        match r {
            PendingRename
            | UpdateSharedNote(_)
            | UpdateConflictMarkers
            | UpdateEmptied
            | UpdateUnknownContent { .. }
            | UpdateNewAttachmentReference { .. }
            | AmbiguousNoteId { .. }
            | DeleteSharedNote { .. }
            | MoveSharedNote { .. }
            | MoveToTopLevel
            | MoveIntoUnfolderableDir { .. }
            | MoveIntoSharerArea
            | MoveWithAttachments
            | CreateAtTopLevel
            | CreateInUnfolderableDir { .. }
            | CreateLooseInSharerHome
            | CreateInReadOnlyShare
            | CreateEmptyFile
            | CreateConflictMarkers
            | CreateUnknownContent
            | CreateEmbedMarker
            | CreateAttachmentReference
            | CreateDuplicatesTrackedNote { .. }
            | MoveGoneRemotely
            | MoveChangedRemotely
            | MoveRetitle { .. }
            | CreateMarkdown { .. }
            | CreateReconcile { .. }
            | CreateVerificationFailed
            | CreateBuildError { .. }
            | UpdateGoneRemotely
            | UpdateChangedRemotelyUnmergeable
            | MergedWithConflicts
            | MergedCleanly
            | TitleOnlySharedNote(_)
            | UpdatePrepare(_)
            | UpdateRefusedUnspecified => true,
        }
    }];
    assert!(covers(&all[0]));
    let discriminants: std::collections::HashSet<std::mem::Discriminant<Refusal>> =
        listed.iter().map(std::mem::discriminant).collect();
    assert_eq!(discriminants.len(), 37, "one row per Refusal variant at least");

    let prepare: std::collections::HashSet<std::mem::Discriminant<PrepareRefusal>> = listed
        .iter()
        .filter_map(|r| match r {
            Refusal::UpdatePrepare(p) => Some(std::mem::discriminant(p)),
            _ => None,
        })
        .collect();
    assert_eq!(prepare.len(), 10, "one row per PrepareRefusal variant");
    let text: std::collections::HashSet<std::mem::Discriminant<TextUpdateRefusal>> = listed
        .iter()
        .filter_map(|r| match r {
            Refusal::UpdatePrepare(PrepareRefusal::TextUpdate(t)) => Some(std::mem::discriminant(t)),
            _ => None,
        })
        .collect();
    assert_eq!(text.len(), 10, "one row per TextUpdateRefusal variant");
    let retitle: std::collections::HashSet<std::mem::Discriminant<RetitleRefusal>> = listed
        .iter()
        .filter_map(|r| match r {
            Refusal::MoveRetitle { refusal, .. } => Some(std::mem::discriminant(refusal)),
            _ => None,
        })
        .collect();
    assert_eq!(retitle.len(), 5, "one row per RetitleRefusal variant");
}
