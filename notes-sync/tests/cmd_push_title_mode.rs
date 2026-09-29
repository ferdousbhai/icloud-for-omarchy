//! Ports icloud-md `src/commands/pushTitleMode.test.ts`: push's write path for
//! a filename-as-title vault (body-only files, titles in names).

use std::collections::HashMap;

use base64::Engine;
use icloud_notes_sync::cloudkit::{CloudKitRecord, FieldValue};
use icloud_notes_sync::cmd::plan::PrepareRefusal;
use icloud_notes_sync::cmd::push::{Retitle, prepare_retitle, restore_stripped_title, title_expressed_by_file};
use icloud_notes_sync::doc::decode::{ClassifyOptions, DecodedNote, NoteDecodeResult, classify_note_record};
use icloud_notes_sync::doc::document::{build_initial_note_document, encode_note_document};
use icloud_notes_sync::doc::format::{ParagraphKind, decode_note_format};
use icloud_notes_sync::doc::reconcile::reconcile_note_format;
use icloud_notes_sync::doc::text::{compress_note_document, decode_note_string};
use icloud_notes_sync::md::parse::{ParsedNoteMarkdown, parse_note_markdown};
use icloud_notes_sync::vault::state::{NoteEntry, TitleMode};

const REPLICA: [u8; 16] = [7; 16];

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn record_with(text: &str, compressed: &[u8]) -> CloudKitRecord {
    let mut record = CloudKitRecord {
        record_name: "REC1".into(),
        record_type: "Note".into(),
        record_change_tag: Some("1a".into()),
        ..Default::default()
    };
    record.fields.insert(
        "TitleEncrypted".into(),
        FieldValue {
            value: b64(text.split('\n').next().unwrap_or("").as_bytes()).into(),
            type_: "ENCRYPTED_BYTES".into(),
        },
    );
    record.fields.insert(
        "TextDataEncrypted".into(),
        FieldValue {
            value: b64(compressed).into(),
            type_: "ENCRYPTED_BYTES".into(),
        },
    );
    record
}

/// A Note record carrying a real document, built like push's create path.
fn note_record(markdown: &str) -> CloudKitRecord {
    let parsed = parse_note_markdown(markdown).expect("parses");
    let mut doc = build_initial_note_document(&parsed.text, &REPLICA).expect("builds");
    reconcile_note_format(&mut doc, &parsed.paragraphs, &REPLICA)
        .expect("reconciles")
        .expect("reconciles");
    record_with(&parsed.text, &compress_note_document(&encode_note_document(&doc).expect("encodes")))
}

/// From raw text (titles the markdown parser would alter).
fn raw_note_record(text: &str) -> CloudKitRecord {
    let doc = build_initial_note_document(text, &REPLICA).expect("builds");
    record_with(text, &compress_note_document(&encode_note_document(&doc).expect("encodes")))
}

fn classify_ok(record: &CloudKitRecord, title_mode: TitleMode) -> DecodedNote {
    match classify_note_record(record, &ClassifyOptions { title_mode }) {
        NoteDecodeResult::Ok(d) => *d,
        other => panic!("expected ok, got {other:?}"),
    }
}

fn parsed_body(markdown: &str) -> ParsedNoteMarkdown {
    parse_note_markdown(markdown).expect("parses")
}

fn decode_payload(payload_base64: &str) -> (String, Option<ParagraphKind>) {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload_base64)
        .unwrap();
    let decoded = decode_note_string(&bytes).unwrap();
    let kind = decode_note_format(decoded.string(), &decoded.attribute_run)
        .ok()
        .and_then(|f| f.first().map(|p| p.kind));
    (decoded.string().to_owned(), kind)
}

fn entry(file: &str) -> NoteEntry {
    let mut e = NoteEntry::new(file, "1a", 100);
    e.folder_record_name = Some("DefaultFolder-CloudKit".into());
    e
}

fn retitle(record: &CloudKitRecord, from: &str, to_file: &str, new_title: &str) -> Result<Option<Retitle>, String> {
    let e = entry(from);
    prepare_retitle(record, &e, to_file, new_title, &REPLICA, TitleMode::Filename)
        .unwrap()
        .map_err(|r| r.message(&e.file))
}

#[test]
#[ignore = "needs A/B/C"]
fn edited_body_only_file_is_made_whole_with_the_notes_own_title() {
    let classified = classify_ok(&note_record("# Shopping list\n\nMilk\nEggs"), TitleMode::Filename);
    assert!(classified.title_stripped);
    let restored = restore_stripped_title(&classified, parsed_body("\nMilk\nEggs\nBread"), None).unwrap();
    assert_eq!(restored.text, "Shopping list\n\nMilk\nEggs\nBread");
    assert_eq!(restored.paragraphs[0].kind, ParagraphKind::Title);
    assert_eq!(
        restored.paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(),
        vec![0, 14, 15, 20, 25]
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn unrenamed_note_keeps_the_records_title() {
    let classified = classify_ok(&note_record("Shopping list\n\nMilk"), TitleMode::Filename);
    let restored = restore_stripped_title(&classified, parsed_body("\nMilk\nEggs"), None).unwrap();
    assert_eq!(restored.text, "Shopping list\n\nMilk\nEggs");
}

#[test]
#[ignore = "needs A/B/C"]
fn in_body_vault_prepends_nothing() {
    let classified = classify_ok(&note_record("Shopping list\n\nMilk"), TitleMode::InBody);
    let parsed = parsed_body("Shopping list\n\nMilk\nEggs");
    assert_eq!(
        restore_stripped_title(&classified, parsed.clone(), None).unwrap(),
        parsed
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn renamed_file_pushes_new_title_and_leaves_body() {
    let record = note_record("# Shopping list\n\nMilk\nEggs");
    let r = retitle(&record, "Notes/Shopping list.md", "Notes/Groceries.md", "Groceries")
        .unwrap()
        .expect("a retitle");
    assert_eq!(r.plain_text, "Groceries\n\nMilk\nEggs");
    let (text, kind) = decode_payload(&r.payload_base64);
    assert_eq!(text, "Groceries\n\nMilk\nEggs");
    assert_eq!(kind, Some(ParagraphKind::Title));
}

#[test]
#[ignore = "needs A/B/C"]
fn homoglyph_title_comes_back_as_the_character() {
    let record = note_record("Shopping list\n\nMilk");
    let r = retitle(
        &record,
        "Notes/Shopping list.md",
        "Notes/Pat\u{2044}Alex.md",
        "Pat/Alex",
    )
    .unwrap();
    assert_eq!(r.unwrap().plain_text, "Pat/Alex\n\nMilk");
}

#[test]
#[ignore = "needs A/B/C"]
fn renaming_to_the_title_the_note_already_has_sends_nothing() {
    let record = note_record("Shopping list\n\nMilk");
    let r = retitle(
        &record,
        "Notes/Shopping list 2.md",
        "Notes/Shopping list.md",
        "Shopping list",
    )
    .unwrap();
    assert!(r.is_none());
}

#[test]
#[ignore = "needs A/B/C"]
fn trimmed_spelling_of_trailing_whitespace_title_sends_nothing() {
    let record = raw_note_record("Shopping list \n\nMilk");
    assert_eq!(
        classify_ok(&record, TitleMode::Filename).format.unwrap()[0].text,
        "Shopping list "
    );
    let r = retitle(
        &record,
        "Notes/Shopping list 2.md",
        "Notes/Shopping list.md",
        "Shopping list",
    )
    .unwrap();
    assert!(r.is_none());
}

#[test]
#[ignore = "needs A/B/C"]
fn genuinely_different_name_retitles_trailing_whitespace_note() {
    let record = raw_note_record("Shopping list \n\nMilk");
    let r = retitle(&record, "Notes/Shopping list.md", "Notes/Groceries.md", "Groceries").unwrap();
    assert_eq!(r.unwrap().plain_text, "Groceries\n\nMilk");
}

#[test]
#[ignore = "needs A/B/C"]
fn rename_is_refused_when_the_note_cant_be_safely_edited() {
    let mut record = note_record("Shopping list\n\nMilk");
    record.fields.insert(
        "TextDataEncrypted".into(),
        FieldValue {
            value: "bm90IGEgbm90ZQ==".into(),
            type_: "ENCRYPTED_BYTES".into(),
        },
    );
    let reason = retitle(&record, "Notes/Shopping list.md", "Notes/Groceries.md", "Groceries").unwrap_err();
    assert!(reason.contains("no longer safely editable"));
    assert!(reason.contains("rename the file back to Shopping list.md"));
}

#[test]
#[ignore = "needs A/B/C"]
fn genuine_retitle_normalizes_title_paragraph_to_title_style() {
    let record = note_record("Shopping list\n\nMilk");
    assert_eq!(
        classify_ok(&record, TitleMode::Filename).format.unwrap()[0].kind,
        ParagraphKind::Body
    );
    let r = retitle(&record, "Notes/Shopping list.md", "Notes/Groceries.md", "Groceries")
        .unwrap()
        .unwrap();
    assert_eq!(decode_payload(&r.payload_base64).1, Some(ParagraphKind::Title));
}

#[test]
#[ignore = "needs A/B/C"]
fn recorded_title_outranks_the_file_name() {
    let long = "A title far too long for any file name to hold, ".repeat(3);
    let recorded: HashMap<String, String> = [("Notes/Untitled.md".to_owned(), long.clone())].into_iter().collect();
    assert_eq!(title_expressed_by_file("Notes/Untitled.md", &recorded), long);
    assert_eq!(title_expressed_by_file("Notes/Untitled 2.md", &recorded), "Untitled 2");
    assert_eq!(
        title_expressed_by_file("Notes/Groceries.md", &HashMap::new()),
        "Groceries"
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn title_only_a_name_can_hold_is_taken_from_the_name_homoglyphs_decoded() {
    assert_eq!(
        title_expressed_by_file("Notes/Pat\u{2044}Alex.md", &HashMap::new()),
        "Pat/Alex"
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn frontmatter_title_the_note_doesnt_have_is_pushed_as_new_title() {
    let classified = classify_ok(&note_record("# Shopping list\n\nMilk\nEggs"), TitleMode::Filename);
    let long = "A title far too long for any file name to hold, "
        .repeat(3)
        .trim_end()
        .to_owned();
    let restored = restore_stripped_title(&classified, parsed_body("\nMilk\nEggs"), Some(&long)).unwrap();
    assert_eq!(restored.text, format!("{long}\n\nMilk\nEggs"));
    assert_eq!(restored.paragraphs[0].kind, ParagraphKind::Title);
    let n = long.encode_utf16().count();
    assert_eq!(
        restored.paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(),
        vec![0, n + 1, n + 2, n + 7]
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn frontmatter_title_equal_to_the_notes_own_is_left_alone() {
    let classified = classify_ok(&note_record("Shopping list\n\nMilk"), TitleMode::Filename);
    assert_eq!(classified.format.as_ref().unwrap()[0].kind, ParagraphKind::Body);
    let restored = restore_stripped_title(&classified, parsed_body("\nMilk"), Some("Shopping list")).unwrap();
    assert_eq!(restored.text, "Shopping list\n\nMilk");
    assert_eq!(restored.paragraphs[0].kind, ParagraphKind::Body);
}

#[test]
#[ignore = "needs A/B/C"]
fn in_body_vault_ignores_the_key() {
    let classified = classify_ok(&note_record("Shopping list\n\nMilk"), TitleMode::InBody);
    let parsed = parsed_body("Shopping list\n\nMilk");
    assert_eq!(
        restore_stripped_title(&classified, parsed.clone(), None).unwrap(),
        parsed
    );
}

#[test]
#[ignore = "needs A/B/C"]
fn title_holding_an_embed_refuses_the_retitle() {
    let classified = classify_ok(&note_record("Shopping list\n\nMilk"), TitleMode::InBody);
    assert!(!classified.title_stripped);
    let refusal =
        restore_stripped_title(&classified, parsed_body("Shopping list\n\nMilk"), Some("Groceries")).unwrap_err();
    assert_eq!(refusal, PrepareRefusal::TitleHasEmbedCannotRetitle);
    let message = refusal.message();
    assert!(message.contains("apple-note-title"));
    assert!(message.contains("can't retitle it"));
}
