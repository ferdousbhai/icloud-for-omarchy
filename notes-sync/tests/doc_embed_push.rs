//! Ports icloud-md `src/notes/embedPushEdit.test.ts`. Table markdown below is
//! icloud-md's `renderMarkdownTable` output, spelled out.

use std::collections::HashSet;

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

fn none() -> HashSet<String> {
    HashSet::new()
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

#[test]
fn a_file_attachment_tracked_from_pull_keeps_the_read_only_refusal() {
    let tracked: HashSet<String> = ["FILE-1".to_string()].into();
    let reason = refusal(plan_embed_representations(
        "![photo](attachments/photo.jpeg)",
        &[slot("FILE-1", "public.jpeg")],
        &tracked,
    ));
    assert_eq!(
        reason,
        "this note has a file attachment - it can't be edited through this tool and stays read-only"
    );
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
