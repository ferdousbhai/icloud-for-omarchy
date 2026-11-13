//! Pushing edits to notes with embedded objects (tables, attachments).
//! Table markdown below is the table renderer's output, spelled out.
//! Originally derived from icloud-md's tests.

use std::collections::HashMap;

use icloud_notes_sync::doc::embeds::{
    AttachmentReference, EmbedMarkerContent, EmbedRepresentation, EmbedSlot, format_embed_marker,
    plan_embed_representations,
};

fn slot(id: &str, uti: &str) -> EmbedSlot {
    EmbedSlot::Attachment(AttachmentReference {
        attachment_identifier: id.into(),
        type_uti: uti.into(),
    })
}

fn marker(id: Option<&str>, uti: Option<&str>) -> String {
    format_embed_marker(&EmbedMarkerContent {
        type_uti: uti.map(Into::into),
        attachment_identifier: id.map(Into::into),
    })
}

fn gallery() -> (EmbedSlot, String) {
    (
        slot("GALLERY-1", "com.apple.notes.gallery"),
        marker(Some("GALLERY-1"), Some("com.apple.notes.gallery")),
    )
}

fn table_slot() -> EmbedSlot {
    slot("TABLE-1", "com.apple.notes.table")
}

fn ok(result: Result<EmbedRepresentation, String>) -> EmbedRepresentation {
    result.unwrap_or_else(|reason| panic!("expected ok, got {reason}"))
}

fn refusal(result: Result<EmbedRepresentation, String>) -> String {
    result.expect_err("expected a refusal")
}

fn none() -> HashMap<String, String> {
    HashMap::new()
}

const O: &str = "\u{fffc}";

#[test]
fn a_verbatim_marker_maps_back_to_its_placeholder() {
    let (g, m) = gallery();
    let plan = ok(plan_embed_representations(
        &format!("Title\nedited intro\n{m}\nedited outro"),
        &[g],
        &none(),
    ));
    assert_eq!(
        plan.reconstructed_body_text,
        format!("Title\nedited intro\n{O}\nedited outro")
    );
    assert!(plan.tables.is_empty());
}

#[test]
fn an_unknown_slots_marker_round_trips_the_same_way() {
    let plan = ok(plan_embed_representations(
        &format!("Note\n{}\n", marker(None, None)),
        &[EmbedSlot::Unknown { type_uti: None }],
        &none(),
    ));
    assert_eq!(plan.reconstructed_body_text, format!("Note\n{O}\n"));
}

#[test]
fn markers_and_table_blocks_interleave_in_document_order() {
    let (g, m) = gallery();
    let table = "| A | B |\n| - | - |";
    let plan = ok(plan_embed_representations(
        &format!("Title\n{m}\nbetween\n{table}\nafter"),
        &[g, table_slot()],
        &none(),
    ));
    assert_eq!(plan.reconstructed_body_text, format!("Title\n{O}\nbetween\n{O}\nafter"));
    assert_eq!(plan.tables.len(), 1);
    assert_eq!(plan.tables[0].reference.attachment_identifier, "TABLE-1");
    assert_eq!(plan.tables[0].block.grid, vec![vec!["A".to_string(), "B".to_string()]]);
}

#[test]
fn a_table_pull_couldnt_decode_is_marker_represented() {
    let m = marker(Some("TABLE-1"), Some("com.apple.notes.table"));
    let plan = ok(plan_embed_representations(
        &format!("Prose\n{m}\nmore prose"),
        &[table_slot()],
        &none(),
    ));
    assert_eq!(plan.reconstructed_body_text, format!("Prose\n{O}\nmore prose"));
    assert!(plan.tables.is_empty());
}

#[test]
fn a_deleted_marker_refuses_the_push() {
    let (g, _) = gallery();
    let reason = refusal(plan_embed_representations("Just prose, marker gone", &[g], &none()));
    assert_eq!(
        reason,
        "the embed marker for \"com.apple.notes.gallery\" (GALLERY-1) is missing - markers must be left exactly as this tool wrote them"
    );
}

#[test]
fn an_edited_marker_refuses_the_push() {
    let (g, m) = gallery();
    let edited = m.replace(">gallery<", ">my gallery<");
    let reason = refusal(plan_embed_representations(&format!("Text\n{edited}"), &[g], &none()));
    assert!(reason.contains("edited or is out of order"), "{reason}");
}

#[test]
fn a_duplicated_marker_refuses_the_push() {
    let (g, m) = gallery();
    let reason = refusal(plan_embed_representations(&format!("A\n{m}\nB\n{m}"), &[g], &none()));
    assert!(reason.contains("nothing behind it"), "{reason}");
}

#[test]
fn reordered_markers_refuse_the_push() {
    let (g, m) = gallery();
    let other = slot("GALLERY-2", "com.apple.notes.gallery");
    let other_marker = marker(Some("GALLERY-2"), Some("com.apple.notes.gallery"));
    let reason = refusal(plan_embed_representations(
        &format!("A\n{other_marker}\nB\n{m}"),
        &[g, other],
        &none(),
    ));
    assert!(reason.contains("edited or is out of order"), "{reason}");
}

#[test]
fn a_hand_added_marker_with_nothing_behind_it_refuses_the_push() {
    let reason = refusal(plan_embed_representations(
        &format!("Only prose\n{}", marker(None, None)),
        &[],
        &none(),
    ));
    assert!(reason.contains("nothing behind it"), "{reason}");
}

/// The link pull wrote for a file attachment stands for it: text around it
/// can be edited, and the attachment goes back where it was.
#[test]
fn a_file_attachment_link_maps_back_to_its_placeholder() {
    let tracked: HashMap<String, String> = [("FILE-1".to_string(), "attachments/photo.jpeg".to_string())].into();
    let plan = ok(plan_embed_representations(
        "Edited intro\n![photo.jpeg](attachments/photo.jpeg)\nmore",
        &[slot("FILE-1", "public.jpeg")],
        &tracked,
    ));
    assert_eq!(plan.reconstructed_body_text, format!("Edited intro\n{O}\nmore"));
}

/// A link text with brackets is escaped now; one an earlier release wrote
/// unescaped still stands for its attachment.
#[test]
fn an_attachment_link_with_brackets_maps_back_escaped_or_as_written_before() {
    let tracked: HashMap<String, String> = [("FILE-1".to_string(), "attachments/a]b.m4a".to_string())].into();
    for link in [r"[a\]b.m4a](attachments/a%5Db.m4a)", "[a]b.m4a](attachments/a%5Db.m4a)"] {
        let plan = ok(plan_embed_representations(
            &format!("Intro\n{link}\nmore"),
            &[slot("FILE-1", "com.apple.m4a-audio")],
            &tracked,
        ));
        assert_eq!(plan.reconstructed_body_text, format!("Intro\n{O}\nmore"), "{link}");
    }
}

/// A link to the attachment's file that is still there, but not as pull
/// wrote it or not where it was, is refused: only a link gone entirely
/// removes the attachment.
#[test]
fn a_changed_or_moved_attachment_link_refuses_the_push() {
    let tracked: HashMap<String, String> = [
        ("FILE-1".to_string(), "attachments/photo.jpeg".to_string()),
        ("FILE-2".to_string(), "attachments/b c.gif".to_string()),
    ]
    .into();
    let slots = [slot("FILE-1", "public.jpeg"), slot("FILE-2", "com.compuserve.gif")];
    let second = "![b c.gif](attachments/b%20c.gif)";
    for text in [
        // Another link text, and the two out of order.
        format!("![my photo](attachments/photo.jpeg)\n{second}"),
        format!("{second}\n![photo.jpeg](attachments/photo.jpeg)"),
        // The second one's link text changed.
        "![photo.jpeg](attachments/photo.jpeg)\n![a gif](attachments/b%20c.gif)".to_string(),
    ] {
        let reason = refusal(plan_embed_representations(&text, &slots, &tracked));
        assert!(reason.contains("was changed or moved"), "{text}: {reason}");
        assert!(reason.contains("put the link back as it was"), "{reason}");
    }
}

/// No link to the attachment's file anywhere: the attachment is removed.
/// Its slot gets no placeholder; the others keep theirs.
#[test]
fn an_attachment_link_gone_entirely_is_a_removal() {
    let tracked: HashMap<String, String> = [
        ("FILE-1".to_string(), "attachments/photo.jpeg".to_string()),
        ("FILE-2".to_string(), "attachments/b c.gif".to_string()),
        ("FILE-3".to_string(), "attachments/plan.pdf".to_string()),
    ]
    .into();
    let slots = [
        slot("FILE-1", "public.jpeg"),
        slot("FILE-2", "com.compuserve.gif"),
        slot("FILE-3", "com.adobe.pdf"),
    ];
    let (first, second, third) = (
        "![photo.jpeg](attachments/photo.jpeg)",
        "![b c.gif](attachments/b%20c.gif)",
        "[plan.pdf](attachments/plan.pdf)",
    );

    let plan = ok(plan_embed_representations(
        &format!("A\n{first}\n{second}\n{third}"),
        &slots,
        &tracked,
    ));
    assert!(plan.removed_slots.is_empty());

    // The middle one, the first and last, and all of them.
    let plan = ok(plan_embed_representations(
        &format!("A\n{first}\n\n{third}"),
        &slots,
        &tracked,
    ));
    assert_eq!(plan.removed_slots, [1]);
    assert_eq!(plan.reconstructed_body_text, format!("A\n{O}\n\n{O}"));
    let plan = ok(plan_embed_representations(&format!("A\n{second} b"), &slots, &tracked));
    assert_eq!(plan.removed_slots, [0, 2]);
    assert_eq!(plan.reconstructed_body_text, format!("A\n{O} b"));
    let plan = ok(plan_embed_representations("prose only", &slots, &tracked));
    assert_eq!(plan.removed_slots, [0, 1, 2]);
    assert_eq!(plan.reconstructed_body_text, "prose only");

    // A link to another file is that file's (a new attachment), and takes
    // nothing from the removal.
    let plan = ok(plan_embed_representations(
        &format!("{first}\n{second}\n[new.pdf](attachments/new.pdf)"),
        &slots,
        &tracked,
    ));
    assert_eq!(plan.removed_slots, [2]);
    assert_eq!(plan.new_links.len(), 1);
    assert_eq!(plan.new_links[0].slot_index, 2);

    // The GIF's link as it was written before GIFs counted as images still
    // stands for it; a link to it left anywhere, in any spelling, means it
    // was not removed.
    let plan = ok(plan_embed_representations(
        &format!("{first}\n[b c.gif](attachments/b%20c.gif)\n{third}"),
        &slots,
        &tracked,
    ));
    assert!(plan.removed_slots.is_empty());
    for kept in ["[see this](attachments/b%20c.gif)", "![b c.gif](attachments/b%20c.gif)"] {
        let text = format!("{kept}\n{first}\n{third}");
        let reason = refusal(plan_embed_representations(&text, &slots, &tracked));
        assert!(reason.contains("\"b c.gif\" was changed or moved"), "{reason}");
    }
}

/// Only a tracked file attachment can be removed: any other embed whose
/// marker is gone is refused as before.
#[test]
fn only_a_file_attachment_is_removed_by_deleting_its_link() {
    let (g, _) = gallery();
    let tracked: HashMap<String, String> = [("FILE-1".to_string(), "attachments/photo.jpeg".to_string())].into();
    let reason = refusal(plan_embed_representations(
        "prose only",
        &[slot("FILE-1", "public.jpeg"), g],
        &tracked,
    ));
    assert!(reason.contains("(GALLERY-1) is missing"), "{reason}");
    let reason = refusal(plan_embed_representations(
        "prose only",
        &[slot("FILE-1", "public.jpeg"), table_slot()],
        &tracked,
    ));
    assert!(reason.contains("can't tell which table(s) changed"), "{reason}");
}

#[test]
fn an_identified_non_table_slot_with_no_marker_refuses_with_the_missing_marker_reason() {
    let reason = refusal(plan_embed_representations(
        "prose only",
        &[slot("FILE-1", "public.jpeg")],
        &none(),
    ));
    assert!(reason.contains("is missing"), "{reason}");
}

#[test]
fn an_extra_hand_typed_table_block_refuses_the_push() {
    let reason = refusal(plan_embed_representations(
        "| A |\n| - |\nprose\n| B |\n| - |",
        &[table_slot()],
        &none(),
    ));
    assert_eq!(
        reason,
        "can't tell which table(s) changed (found 2 table-shaped block(s) locally, expected 1)"
    );
}

#[test]
fn a_missing_table_block_refuses_the_push() {
    let reason = refusal(plan_embed_representations("prose only", &[table_slot()], &none()));
    assert_eq!(
        reason,
        "can't tell which table(s) changed (found 0 table-shaped block(s) locally, expected more)"
    );
}

#[test]
fn a_table_only_note_reconstructs_exactly_like_the_old_table_path_did() {
    let plan = ok(plan_embed_representations(
        "| Only | Content |\n| - | - |",
        &[table_slot()],
        &none(),
    ));
    assert_eq!(plan.reconstructed_body_text, O);
}

#[test]
fn two_tables_reconstruct_in_document_order() {
    let second = slot("TABLE-2", "com.apple.notes.table");
    let text = "Intro\n| First |\n| - |\nMiddle\n| Second | Table |\n| - | - |\nOutro";
    let plan = ok(plan_embed_representations(text, &[table_slot(), second], &none()));
    assert_eq!(plan.reconstructed_body_text, format!("Intro\n{O}\nMiddle\n{O}\nOutro"));
    assert_eq!(
        plan.tables
            .iter()
            .map(|t| t.reference.attachment_identifier.as_str())
            .collect::<Vec<_>>(),
        vec!["TABLE-1", "TABLE-2"]
    );
}
