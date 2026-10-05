//! Every refused/conflict plan entry the engine's push can produce, with the
//! exact `reason` string each one puts in `status --json` / `push --dry-run
//! --json`, and the folder refusals behind moves and creates. Every such
//! entry is built from `cmd::plan::Refusal`, so this table is the checklist
//! of them all: `every_refusal_variant_is_listed` fails to compile when a
//! variant is added without a row here.

use icloud_notes_sync::cmd::plan::{
    FolderRefusal, PlanEntry, PlanEntryKind, PlanResolution, PrepareRefusal, Refusal, RetitleRefusal,
    SharedWriteRefusal, TextUpdateRefusal,
};

const FILE: &str = "Notes/Pie.md";

/// (the refusal, its resolution, its reason verbatim).
fn sites() -> Vec<(Refusal, PlanResolution, String)> {
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
            Refusal::PendingRename,
            Conflict,
            "rename deferred by a previous pull and not yet performed - rename it, or run \"pull\" to have it done for you"
                .into(),
        ),
        (
            Refusal::UpdateSharedNote(SharedWriteRefusal::IndividuallyShared),
            Refused,
            "individually-shared notes can't be edited yet - only notes inside a shared folder can".into(),
        ),
        (
            Refusal::UpdateSharedNote(SharedWriteRefusal::ReadOnlyFolder),
            Refused,
            "this shared folder is read-only for you - the server would reject the edit".into(),
        ),
        (
            Refusal::UpdateConflictMarkers,
            Conflict,
            "still contains diff3 conflict markers - resolve them before pushing".into(),
        ),
        (
            Refusal::UpdateUnknownContent { file: FILE.into() },
            Refused,
            format!(
                "this note contains content this tool can't parse and can never be pushed - run \"icloud-notes restore {FILE}\" to discard your local edit."
            ),
        ),
        (
            Refusal::UpdateNewAttachmentReference { file: FILE.into() },
            Refused,
            format!(
                "contains an \"attachments/...\" reference, but this tool can't upload new attachments - remove it, or run \"icloud-notes restore {FILE}\" to discard the edit."
            ),
        ),
        (
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
            Refusal::DeleteSharedNote { file: FILE.into() },
            Refused,
            format!(
                "deleting notes shared by someone else isn't supported - run \"icloud-notes restore {FILE}\" to bring the file back"
            ),
        ),
        (
            Refusal::MoveSharedNote {
                previous_file: "Pat/Shared/Pie.md".into(),
            },
            Refused,
            "renaming or moving notes shared by someone else isn't supported yet - move the file back to Pat/Shared/Pie.md"
                .into(),
        ),
        (
            Refusal::MoveIntoUnfolderableDir {
                dir: "Somewhere".into(),
                folder_refusal: None,
            },
            Refused,
            "moved into \"Somewhere/\", which can't become one of the account's folders".into(),
        ),
        (
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
            Refusal::MoveIntoSharerArea,
            Refused,
            "moved into a sharer's area - notes can't be moved into someone else's share".into(),
        ),
        (
            Refusal::CreateInUnfolderableDir {
                dir: "Somewhere".into(),
                folder_refusal: None,
            },
            Refused,
            "sits in \"Somewhere/\", which can't become one of the account's folders".into(),
        ),
        (
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
            Refusal::CreateLooseInSharerHome,
            Refused,
            "sits loose at the top of a sharer's area - notes can only be created inside one of their shared folders".into(),
        ),
        (
            Refusal::CreateInReadOnlyShare,
            Refused,
            "sits in a shared folder you only have read access to - the server would reject the create".into(),
        ),
        (
            Refusal::CreateEmptyFile,
            Refused,
            "the file is empty - nothing to create".into(),
        ),
        (
            Refusal::CreateConflictMarkers,
            Refused,
            "still contains diff3 conflict markers - resolve them before pushing".into(),
        ),
        (
            Refusal::CreateUnknownContent,
            Refused,
            "this file contains the unknown-content banner - remove it before pushing".into(),
        ),
        (
            Refusal::CreateEmbedMarker,
            Refused,
            "contains an embed marker, but this tool can't create embeds - remove it before pushing".into(),
        ),
        (
            Refusal::CreateAttachmentReference,
            Refused,
            "contains an \"attachments/...\" reference, but this tool can't upload new attachments - remove it first.".into(),
        ),
        (
            Refusal::MoveGoneRemotely,
            Conflict,
            "no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            Refusal::FolderGoneRemotely,
            Conflict,
            "no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            Refusal::FolderChangedRemotely,
            Conflict,
            "another device put a note or folder in it since the last pull - run \"pull\", then delete it again".into(),
        ),
        (
            Refusal::MoveChangedRemotely,
            Conflict,
            "changed remotely since the last pull - run \"pull\" first".into(),
        ),
        (
            retitle(RetitleRefusal::NoLongerEditable {
                detail: "undecodable".into(),
            }),
            Refused,
            format!(
                "renaming this note would retitle it, but the note is no longer safely editable (undecodable){rename_back}"
            ),
        ),
        (
            retitle(RetitleRefusal::Unpublishable {
                unpublishable_reason: None,
            }),
            Refused,
            format!("renaming this note would retitle it, but this note contains content this tool can't parse{rename_back}"),
        ),
        (
            retitle(RetitleRefusal::TitleHasEmbed),
            Refused,
            format!("this note's title contains an embedded object, so its file name doesn't carry the title{rename_back}"),
        ),
        (
            retitle(RetitleRefusal::TextUpdate(TextUpdateRefusal::NotRoundTrip)),
            Refused,
            format!(
                "renaming this note would retitle it, but the note's document doesn't round-trip byte-for-byte through our model - refusing to edit{rename_back}"
            ),
        ),
        (
            retitle(RetitleRefusal::Unapplied),
            Refused,
            format!("renaming this note would retitle it, but the new title couldn't be applied{rename_back}"),
        ),
        (
            Refusal::CreateMarkdown {
                reason: "a parse refusal".into(),
            },
            Refused,
            "a parse refusal".into(),
        ),
        (
            Refusal::CreateReconcile {
                reason: "a reconcile refusal".into(),
            },
            Refused,
            "a reconcile refusal".into(),
        ),
        (
            Refusal::CreateVerificationFailed,
            Refused,
            "built document failed decode verification - refusing to create".into(),
        ),
        (
            Refusal::CreateBuildError {
                message: "a thrown error".into(),
            },
            Refused,
            "a thrown error".into(),
        ),
        (
            Refusal::UpdateGoneRemotely,
            Conflict,
            "no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            Refusal::UpdateChangedRemotelyUnmergeable,
            Conflict,
            "changed remotely since the last pull - run \"pull\" (which merges) first".into(),
        ),
        (
            Refusal::TitleOnlySharedNote(SharedWriteRefusal::IndividuallyShared),
            Refused,
            "individually-shared notes can't be edited yet - only notes inside a shared folder can".into(),
        ),
        (
            Refusal::MergedWithConflicts,
            Conflict,
            "changed remotely since the last pull - merged with conflict markers, resolve manually".into(),
        ),
        (
            Refusal::UpdateRefusedUnspecified,
            Refused,
            "refused".into(),
        ),
        // prepareUpdate & co. (reported with the
        // "<file>: " prefix stripped)
        (
            Refusal::UpdatePrepare(PrepareRefusal::NoLongerEditable {
                detail: "missing-body".into(),
            }),
            Refused,
            "remote note is no longer safely editable (missing-body)".into(),
        ),
        (
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
            Refusal::UpdatePrepare(PrepareRefusal::Markdown {
                reason: "unsupported markdown".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("unsupported markdown. {restore}"),
        ),
        (
            Refusal::UpdatePrepare(PrepareRefusal::EmbedPlan {
                reason: "an embed moved".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("an embed moved. {restore}"),
        ),
        (
            Refusal::UpdatePrepare(PrepareRefusal::EmbedMarkdown {
                reason: "unsupported markdown".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("unsupported markdown. {restore}"),
        ),
        (
            Refusal::UpdatePrepare(PrepareRefusal::TableGoneRemotely),
            Conflict,
            "a table in this note no longer exists remotely - run \"pull\" to reconcile".into(),
        ),
        (
            Refusal::UpdatePrepare(PrepareRefusal::TableUpdate {
                reason: "the table changed shape".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("the table changed shape. {restore}"),
        ),
        (
            Refusal::UpdatePrepare(PrepareRefusal::TitleHasEmbedCannotRetitle),
            Refused,
            "this note's title contains an embedded object, so \"apple-note-title\" can't retitle it - retitle it in Notes instead."
                .into(),
        ),
        (
            Refusal::UpdatePrepare(PrepareRefusal::NoTitleParagraph),
            Refused,
            "the remote note has no title paragraph to restore - refusing to edit".into(),
        ),
        (
            text(TextUpdateRefusal::NoTextData),
            Refused,
            "remote note has no readable text data".into(),
        ),
        (
            text(TextUpdateRefusal::TextAsAsset),
            Refused,
            "remote note stores its text as an asset - refusing to edit".into(),
        ),
        (
            text(TextUpdateRefusal::NotRoundTrip),
            Refused,
            "the note's document doesn't round-trip byte-for-byte through our model - refusing to edit".into(),
        ),
        (
            text(TextUpdateRefusal::TouchesEmbed { file: FILE.into() }),
            Refused,
            format!(
                "this edit would delete or move an embedded object - embeds can only be edited in Notes itself. {restore}"
            ),
        ),
        (
            text(TextUpdateRefusal::DecoderDisagreement),
            Refused,
            "decoder disagreement on the note's current text - refusing to edit".into(),
        ),
        (
            text(TextUpdateRefusal::Reconcile {
                reason: "a checklist changed".into(),
                file: FILE.into(),
            }),
            Refused,
            format!("a checklist changed. {restore}"),
        ),
        (
            text(TextUpdateRefusal::RebuiltDecodeFailed),
            Refused,
            "rebuilt document failed decode verification - refusing to push".into(),
        ),
        (
            text(TextUpdateRefusal::RebuiltEmbedFailed),
            Refused,
            "rebuilt document failed embed-structure verification - refusing to push".into(),
        ),
        (
            text(TextUpdateRefusal::RebuiltFormatFailed),
            Refused,
            "rebuilt document failed formatting verification - refusing to push".into(),
        ),
        (
            text(TextUpdateRefusal::Exception {
                message: "invariant violated".into(),
            }),
            Refused,
            "invariant violated".into(),
        ),
    ]
}

/// Folder refusals (what a move/create into that directory shows).
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
    for (refusal, resolution, reason) in sites() {
        assert_eq!(refusal.reason(), reason, "{refusal:?}");
        assert_eq!(refusal.resolution(), resolution, "{refusal:?}");
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
    let listed: Vec<Refusal> = sites().into_iter().map(|(r, _, _)| r).collect();
    let covers = |pred: &dyn Fn(&Refusal) -> bool| listed.iter().any(pred);
    let all: [fn(&Refusal) -> bool; 1] = [|r| {
        use Refusal::*;
        match r {
            PendingRename
            | UpdateSharedNote(_)
            | UpdateConflictMarkers
            | UpdateUnknownContent { .. }
            | UpdateNewAttachmentReference { .. }
            | AmbiguousNoteId { .. }
            | DeleteSharedNote { .. }
            | MoveSharedNote { .. }
            | MoveIntoUnfolderableDir { .. }
            | MoveIntoSharerArea
            | CreateInUnfolderableDir { .. }
            | CreateLooseInSharerHome
            | CreateInReadOnlyShare
            | CreateEmptyFile
            | CreateConflictMarkers
            | CreateUnknownContent
            | CreateEmbedMarker
            | CreateAttachmentReference
            | MoveGoneRemotely
            | MoveChangedRemotely
            | FolderGoneRemotely
            | FolderChangedRemotely
            | MoveRetitle { .. }
            | CreateMarkdown { .. }
            | CreateReconcile { .. }
            | CreateVerificationFailed
            | CreateBuildError { .. }
            | UpdateGoneRemotely
            | UpdateChangedRemotelyUnmergeable
            | MergedWithConflicts
            | TitleOnlySharedNote(_)
            | UpdatePrepare(_)
            | UpdateRefusedUnspecified => true,
        }
    }];
    assert!(covers(&all[0]));
    let discriminants: std::collections::HashSet<std::mem::Discriminant<Refusal>> =
        listed.iter().map(std::mem::discriminant).collect();
    assert_eq!(discriminants.len(), 33, "one row per Refusal variant at least");

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
