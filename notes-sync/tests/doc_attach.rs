//! Adding an attachment from this computer: the file's type and size, the
//! `Media` and `Attachment` records it becomes, the links that ask for it,
//! and its placeholder in the note's document.

mod common;

use std::collections::HashMap;

use icloud_notes_sync::doc::attach::{MAX_NEW_ATTACHMENT_BYTES, image_dimensions, uti_for_file_name};
use icloud_notes_sync::doc::document::{
    ApplyTextEditOptions, apply_text_edit, build_initial_note_document, delete_attachment_placeholder,
    encode_note_document, insert_attachment_placeholder, mark_attachment_placeholder, parse_note_document,
    validate_document_invariants,
};
use icloud_notes_sync::doc::embeds::{
    AttachmentReference, EmbedSlot, attachment_link_spellings, decode_note_embed_slots, find_attachment_links,
    format_attachment_markdown, has_attachment_reference, is_image_uti, plan_embed_representations,
    plan_new_attachment_links,
};
use icloud_notes_sync::doc::encode::{NewAttachmentFields, build_attachment_create_fields, build_media_create_fields};
use icloud_notes_sync::doc::format::decode_note_format;
use icloud_notes_sync::doc::proto::topotext::AttachmentInfo;
use icloud_notes_sync::doc::text::{
    compress_note_document, decode_note_body_text, decode_note_string, decompress_note_document,
};
use icloud_notes_sync::vault::attachments::extension_for_uti;
use serde_json::json;

const O: &str = "\u{fffc}";
const REPLICA: [u8; 16] = [7; 16];

fn options() -> ApplyTextEditOptions {
    ApplyTextEditOptions { replica_id: REPLICA }
}

fn info(id: &str, uti: &str) -> AttachmentInfo {
    AttachmentInfo {
        attachment_identifier: Some(id.into()),
        type_uti: Some(uti.into()),
        ..Default::default()
    }
}

fn slot(id: &str, uti: &str) -> EmbedSlot {
    EmbedSlot::Attachment(AttachmentReference {
        attachment_identifier: id.into(),
        type_uti: uti.into(),
    })
}

// --- the file's type and size -------------------------------------------------------

#[test]
fn the_type_comes_from_the_extension() {
    for (name, uti) in [
        ("a.png", "public.png"),
        ("a.jpg", "public.jpeg"),
        ("a.JPEG", "public.jpeg"),
        ("a.heic", "public.heic"),
        ("a.gif", "com.compuserve.gif"),
        ("a.tiff", "public.tiff"),
        ("a.tif", "public.tiff"),
        ("a.webp", "org.webmproject.webp"),
        ("a.pdf", "com.adobe.pdf"),
        ("a.m4a", "com.apple.m4a-audio"),
        ("a.mp3", "public.mp3"),
        ("a.mp4", "public.mpeg-4"),
        ("a.mov", "com.apple.quicktime-movie"),
        ("a.txt", "public.plain-text"),
        ("a.zip", "public.zip-archive"),
        ("archive.tar.gz", "public.data"),
        ("README", "public.data"),
        (".png", "public.data"),
        ("a.b.Png", "public.png"),
    ] {
        assert_eq!(uti_for_file_name(name), uti, "{name}");
    }
}

/// A type pull names a file after (`extension_for_uti`) is the type a file
/// with that extension is given here, for every type Notes itself uses, and
/// such a file is linked as pull would link it.
#[test]
fn the_type_agrees_with_the_extension_pull_gives_it() {
    for uti in [
        "public.jpeg",
        "public.png",
        "public.heic",
        "public.tiff",
        "com.apple.m4a-audio",
        "com.adobe.pdf",
    ] {
        let name = format!("file{}", extension_for_uti(uti));
        assert_eq!(uti_for_file_name(&name), uti);
    }
    for (name, link) in [
        ("p.png", "![p.png](attachments/p.png)"),
        ("p.jpg", "![p.jpg](attachments/p.jpg)"),
        ("p.heic", "![p.heic](attachments/p.heic)"),
        ("p.tif", "![p.tif](attachments/p.tif)"),
        // Images in Apple Notes too, under the names Apple gives them.
        ("p.gif", "![p.gif](attachments/p.gif)"),
        ("p.webp", "![p.webp](attachments/p.webp)"),
        ("p.pdf", "[p.pdf](attachments/p.pdf)"),
        ("a b [1].txt", "[a b \\[1\\].txt](attachments/a%20b%20%5B1%5D.txt)"),
        // Parentheses that pair up stay; ones that don't would end the link.
        ("shot (1).png", "![shot (1).png](attachments/shot%20(1).png)"),
        ("odd).pdf", "[odd).pdf](attachments/odd%29.pdf)"),
        ("odd)(.pdf", "[odd)(.pdf](attachments/odd%29%28.pdf)"),
        // The links `icloud-notes attach` writes (notes/tests/cli_test.cpp).
        ("photo 2.PNG", "![photo 2.PNG](attachments/photo%202.PNG)"),
        (
            "plan [v1] (a).pdf",
            "[plan \\[v1\\] (a).pdf](attachments/plan%20%5Bv1%5D%20(a).pdf)",
        ),
    ] {
        let reference = AttachmentReference {
            attachment_identifier: "A".into(),
            type_uti: uti_for_file_name(name).into(),
        };
        assert_eq!(
            format_attachment_markdown(&reference, &format!("attachments/{name}")),
            link
        );
        assert_eq!(link.starts_with('!'), is_image_uti(uti_for_file_name(name)));
    }
    assert_eq!(MAX_NEW_ATTACHMENT_BYTES, 100 * 1024 * 1024);
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0]);
    bytes
}

#[test]
fn image_headers_give_their_pixel_size() {
    assert_eq!(image_dimensions(&png(640, 480)), Some((640, 480)));
    assert_eq!(image_dimensions(&png(0, 480)), None);
    assert_eq!(image_dimensions(&png(640, 480)[..20]), None);

    assert_eq!(
        image_dimensions(b"GIF89a\x40\x01\xf0\x00\x00\x00\x00"),
        Some((320, 240))
    );
    assert_eq!(image_dimensions(b"GIF87a\x02\x00\x03\x00"), Some((2, 3)));
    assert_eq!(image_dimensions(b"GIF89a\x40"), None);
    // WebP: the extended canvas, a lossy frame, a lossless frame.
    assert_eq!(
        image_dimensions(b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00\x3f\x01\x00\xef\x00\x00"),
        Some((320, 240))
    );
    assert_eq!(
        image_dimensions(b"RIFF\x00\x00\x00\x00WEBPVP8 \x00\x00\x00\x00\x00\x00\x00\x9d\x01\x2a\x40\x01\xf0\x00"),
        Some((320, 240))
    );
    assert_eq!(
        image_dimensions(b"RIFF\x00\x00\x00\x00WEBPVP8L\x00\x00\x00\x00\x2f\x3f\xc1\x3b\x00"),
        Some((320, 240))
    );
    assert_eq!(image_dimensions(b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a"), None);

    // SOI, an APP0 segment to skip, a fill byte, then SOF0: 16x32.
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x4a, 0x46];
    jpeg.extend([0xff, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x20, 0x00, 0x10, 0x03]);
    assert_eq!(image_dimensions(&jpeg), Some((16, 32)));
    // A progressive frame (SOF2) counts; a Huffman table (c4) is skipped.
    let jpeg = [
        0xff, 0xd8, 0xff, 0xc4, 0x00, 0x03, 0x00, 0xff, 0xc2, 0x00, 0x11, 0x08, 0x01, 0x00, 0x02, 0x00,
    ];
    assert_eq!(image_dimensions(&jpeg), Some((512, 256)));
    // The image data starts before any frame header, or the file ends.
    assert_eq!(image_dimensions(&[0xff, 0xd8, 0xff, 0xda, 0x00, 0x02]), None);
    assert_eq!(image_dimensions(&[0xff, 0xd8, 0xff, 0xe0, 0x00]), None);
    assert_eq!(image_dimensions(&[0xff, 0xd8, 0xff, 0xe0, 0xff, 0xff]), None);

    assert_eq!(image_dimensions(b"%PDF-1.7"), None);
    assert_eq!(image_dimensions(b""), None);
}

// --- the records ------------------------------------------------------------------

#[test]
fn the_media_record_holds_the_file_and_its_name() {
    let asset =
        json!({"fileChecksum": "CK", "size": 5, "wrappingKey": "WK", "referenceChecksum": "RC", "receipt": "R"});
    let fields = build_media_create_fields(asset.clone(), "Plan é.pdf");
    assert_eq!(
        serde_json::to_string(&fields).unwrap(),
        json!({
            "Asset": {"value": asset},
            "FilenameEncrypted": {"value": "UGxhbiDDqS5wZGY="},
            "MinimumSupportedNotesVersion": {"value": 0},
            "Deleted": {"value": 0},
        })
        .to_string()
    );
    assert_eq!(
        fields.keys().collect::<Vec<_>>(),
        ["Asset", "FilenameEncrypted", "MinimumSupportedNotesVersion", "Deleted"]
    );
}

#[test]
fn the_attachment_record_is_what_notes_makes_without_the_previews() {
    let fields = build_attachment_create_fields(&NewAttachmentFields {
        type_uti: "public.png",
        file_name: "pic.png",
        media_record_name: "MEDIA-1",
        note_record_name: "note-1",
        file_size: 1234,
        now_ms: 1790000000000,
        dimensions: Some((640, 480)),
    });
    let reference = |name: &str| json!({"recordName": name, "action": "VALIDATE", "zoneID": {"zoneName": "Notes"}});
    let expected = format!(
        concat!(
            r#"{{"UTI":{{"value":"public.png"}},"UTIEncrypted":{{"value":"cHVibGljLnBuZw=="}},"#,
            r#""TitleEncrypted":{{"value":"cGljLnBuZw=="}},"Media":{{"value":{media}}},"Note":{{"value":{note}}},"#,
            r#""FileSize":{{"value":1234}},"CreationDate":{{"value":1790000000000}},"#,
            r#""LastModificationDate":{{"value":1790000000000}},"MinimumSupportedNotesVersion":{{"value":0}},"#,
            r#""Deleted":{{"value":0}},"Orientation":{{"value":0}},"ImageFilterType":{{"value":0}},"#,
            r#""CroppingQuadTopLeftX":{{"value":0.0}},"CroppingQuadTopLeftY":{{"value":1.0}},"#,
            r#""CroppingQuadTopRightX":{{"value":1.0}},"CroppingQuadTopRightY":{{"value":1.0}},"#,
            r#""CroppingQuadBottomRightX":{{"value":1.0}},"CroppingQuadBottomRightY":{{"value":0.0}},"#,
            r#""CroppingQuadBottomLeftX":{{"value":0.0}},"CroppingQuadBottomLeftY":{{"value":0.0}},"#,
            r#""Width":{{"value":640.0}},"Height":{{"value":480.0}}}}"#,
        ),
        media = reference("MEDIA-1"),
        note = reference("note-1"),
    );
    assert_eq!(serde_json::to_string(&fields).unwrap(), expected);

    // No size for a file whose header gives none, and nothing Apple's
    // devices add later.
    let fields = build_attachment_create_fields(&NewAttachmentFields {
        type_uti: "com.adobe.pdf",
        file_name: "plan.pdf",
        media_record_name: "MEDIA-2",
        note_record_name: "note-1",
        file_size: 9,
        now_ms: 1,
        dimensions: None,
    });
    assert!(!fields.contains_key("Width") && !fields.contains_key("Height"));
    for absent in [
        "PreviewImages",
        "PreviewWidths",
        "OcrSummaryEncrypted",
        "ReplicaIDToNotesVersionDataEncrypted",
    ] {
        assert!(!fields.contains_key(absent), "{absent}");
    }
    assert_eq!(
        serde_json::to_value(&fields).unwrap()["TitleEncrypted"],
        json!({"value": "cGxhbi5wZGY="})
    );
}

// --- the links ----------------------------------------------------------------------

#[test]
fn attachment_links_are_found_where_they_are() {
    let text = "A ![p.png](attachments/p.png) and [a \\[1\\].txt](attachments/a%20%5B1%5D.txt).";
    let links = find_attachment_links(text);
    assert_eq!(links.len(), 2);
    assert_eq!(
        (links[0].start, links[0].bracket, links[0].end),
        (2, 3, 29),
        "{links:?}"
    );
    assert_eq!(links[0].text, "![p.png](attachments/p.png)");
    assert_eq!(links[0].href, "attachments/p.png");
    assert_eq!(links[1].text, "[a \\[1\\].txt](attachments/a%20%5B1%5D.txt)");
    assert_eq!(links[1].href, "attachments/a%20%5B1%5D.txt");

    // Parentheses in the target pair up, as Markdown reads them.
    let links =
        find_attachment_links("(see ![shot (1).png](attachments/shot%20(1).png)) [odd).pdf](attachments/odd%29.pdf)");
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(links[0].href, "attachments/shot%20(1).png");
    assert_eq!(links[0].text, "![shot (1).png](attachments/shot%20(1).png)");
    assert_eq!(links[1].href, "attachments/odd%29.pdf");

    // A `!` before a file that isn't an image is the note's own text.
    let links = find_attachment_links("Wow![plan.pdf](attachments/plan.pdf)");
    assert_eq!((links[0].start, links[0].bracket), (4, 4));
    // Offsets count UTF-16 units.
    let links = find_attachment_links("😀 [a.txt](attachments/a.txt)");
    assert_eq!(links[0].start, 3);

    // Not links: an escaped bracket, another target, no file named.
    for text in [
        "\\[a.txt\\](attachments/a.txt)",
        "[a](https://example.com/attachments/a.txt)",
        "[a](attachments/)",
        "plain attachments/a.txt",
    ] {
        assert!(find_attachment_links(text).is_empty(), "{text}");
        assert!(!has_attachment_reference(text), "{text}");
    }
    // Written like one but not readable as one: a reference all the same.
    for text in [
        "[a\nb](attachments/a.txt)",
        "[a](attachments/a b.txt)",
        "[a](attachments/a.txt",
    ] {
        assert!(find_attachment_links(text).is_empty(), "{text}");
        assert!(has_attachment_reference(text), "{text}");
        assert!(
            plan_new_attachment_links(text)
                .unwrap_err()
                .contains("isn't written the way")
        );
    }
}

#[test]
fn a_link_to_no_embed_of_the_note_is_a_file_to_add() {
    let tracked: HashMap<String, String> = [("ATT-1".to_owned(), "attachments/old.png".to_owned())].into();
    let existing = slot("ATT-1", "public.png");

    // After the note's own attachment.
    let text = "Title\n![old.png](attachments/old.png)\n[new.pdf](attachments/new.pdf)";
    let plan = plan_embed_representations(text, std::slice::from_ref(&existing), &tracked).unwrap();
    assert_eq!(plan.reconstructed_body_text, format!("Title\n{O}\n{O}"));
    assert_eq!(plan.new_links.len(), 1);
    assert_eq!(plan.new_links[0].slot_index, 1);
    assert_eq!(plan.new_links[0].text, "[new.pdf](attachments/new.pdf)");
    assert_eq!(plan.new_links[0].href, "attachments/new.pdf");

    // Before it, and two at once.
    let text = "[a.txt](attachments/a.txt)\n![old.png](attachments/old.png) x ![b.png](attachments/b.png)";
    let plan = plan_embed_representations(text, std::slice::from_ref(&existing), &tracked).unwrap();
    assert_eq!(plan.reconstructed_body_text, format!("{O}\n{O} x {O}"));
    assert_eq!(plan.new_links.iter().map(|l| l.slot_index).collect::<Vec<_>>(), [0, 2]);

    // The note's own link alone is no new one.
    let plan = plan_embed_representations(
        "![old.png](attachments/old.png)",
        std::slice::from_ref(&existing),
        &tracked,
    )
    .unwrap();
    assert!(plan.new_links.is_empty());

    // A second link to the note's own attachment is one more link (which
    // push then refuses: the file is attached already).
    let text = "![old.png](attachments/old.png)\n![old.png](attachments/old.png)";
    let plan = plan_embed_representations(text, std::slice::from_ref(&existing), &tracked).unwrap();
    assert_eq!(plan.new_links.len(), 1);
    assert_eq!(plan.new_links[0].slot_index, 1);

    // The note's own link gone, another file's in its place: one removed,
    // one added (docs/DESIGN.md, "Removing an attachment").
    let plan = plan_embed_representations("[new.pdf](attachments/new.pdf)", &[existing], &tracked).unwrap();
    assert_eq!(plan.removed_slots, [0]);
    assert_eq!(plan.new_links.len(), 1);
    assert_eq!(plan.new_links[0].slot_index, 0);
    assert_eq!(plan.reconstructed_body_text, O);

    // A new note: every link is a file to add.
    let plan = plan_new_attachment_links("Hi\n\n[a.txt](attachments/a.txt)\n").unwrap();
    assert_eq!(plan.reconstructed_body_text, format!("Hi\n\n{O}\n"));
    assert_eq!(plan.new_links[0].slot_index, 0);
    assert!(plan_new_attachment_links("Hi").unwrap().new_links.is_empty());
}

#[test]
fn a_link_is_spelled_as_pull_spells_it() {
    let reference = AttachmentReference {
        attachment_identifier: "A".into(),
        type_uti: "public.plain-text".into(),
    };
    assert_eq!(
        attachment_link_spellings(&reference, "attachments/a [1].txt"),
        [
            "[a \\[1\\].txt](attachments/a%20%5B1%5D.txt)".to_owned(),
            "[a [1].txt](attachments/a%20%5B1%5D.txt)".to_owned(),
        ]
    );
}

// --- the placeholder in the document ------------------------------------------------

/// The document's text and embeds as every reader decodes them.
fn decoded(doc: &icloud_notes_sync::doc::document::NoteDocument) -> (String, Vec<EmbedSlot>) {
    validate_document_invariants(doc).unwrap();
    let compressed = compress_note_document(&encode_note_document(doc).unwrap());
    let text = decode_note_body_text(&compressed).unwrap();
    let slots = decode_note_embed_slots(&compressed).unwrap().expect("embeds decode");
    // What was sent parses back to the same document.
    let reparsed = parse_note_document(&decompress_note_document(&compressed).unwrap()).unwrap();
    assert_eq!(&reparsed, doc);
    let string = decode_note_string(&compressed).unwrap();
    decode_note_format(string.string(), &string.attribute_run).unwrap();
    (text, slots)
}

#[test]
fn a_placeholder_inserted_in_a_real_note_decodes_as_its_attachment() {
    let raw = decompress_note_document(&common::payload("real_plain_note.json")).unwrap();
    let mut doc = parse_note_document(&raw).unwrap();
    let before = doc.text.clone();
    let runs_before = doc.attribute_runs.len();
    let end = before.encode_utf16().count();

    apply_text_edit(&mut doc, &format!("{before}\n"), &options()).unwrap();
    insert_attachment_placeholder(&mut doc, end + 1, info("ATT-NEW", "public.png"), &options()).unwrap();

    let (text, slots) = decoded(&doc);
    assert_eq!(text, format!("{before}\n{O}"));
    assert_eq!(slots, [slot("ATT-NEW", "public.png")]);
    // The placeholder has a run of its own, one unit long.
    let last = doc.attribute_runs.last().unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last.attachment_info, Some(info("ATT-NEW", "public.png")));
    assert!(doc.attribute_runs.len() > runs_before);
    assert!(
        doc.attribute_runs
            .iter()
            .rev()
            .skip(1)
            .all(|r| r.attachment_info.is_none())
    );
}

#[test]
fn a_placeholder_goes_in_beside_one_that_is_there_without_taking_its_place() {
    let mut doc = build_initial_note_document(&format!("Title\n{O}"), &[1; 16]).unwrap();
    mark_attachment_placeholder(&mut doc, 6, info("OLD", "public.jpeg")).unwrap();
    assert_eq!(decoded(&doc).1, [slot("OLD", "public.jpeg")]);

    // After it: what `attach` does to a note that ends in an attachment.
    let mut after = doc.clone();
    apply_text_edit(&mut after, &format!("Title\n{O}\n"), &options()).unwrap();
    insert_attachment_placeholder(&mut after, 8, info("NEW", "com.adobe.pdf"), &options()).unwrap();
    let (text, slots) = decoded(&after);
    assert_eq!(text, format!("Title\n{O}\n{O}"));
    assert_eq!(slots, [slot("OLD", "public.jpeg"), slot("NEW", "com.adobe.pdf")]);

    // Right before it, and in the middle of a line.
    let mut before = doc.clone();
    insert_attachment_placeholder(&mut before, 6, info("NEW", "com.adobe.pdf"), &options()).unwrap();
    assert_eq!(
        decoded(&before).1,
        [slot("NEW", "com.adobe.pdf"), slot("OLD", "public.jpeg")]
    );
    let mut middle = doc.clone();
    insert_attachment_placeholder(&mut middle, 2, info("NEW", "com.adobe.pdf"), &options()).unwrap();
    let (text, slots) = decoded(&middle);
    assert_eq!(text, format!("Ti{O}tle\n{O}"));
    assert_eq!(slots, [slot("NEW", "com.adobe.pdf"), slot("OLD", "public.jpeg")]);
    // The line's style continues on both sides of it.
    assert_eq!(
        middle.attribute_runs.iter().map(|r| r.len()).collect::<Vec<_>>(),
        [2, 1, 4, 1]
    );

    // The old one's character is the one it was: only ours is new.
    let old_run = |d: &icloud_notes_sync::doc::document::NoteDocument| d.runs[1].coord;
    assert_eq!(old_run(&doc), old_run(&after));
}

#[test]
fn a_placeholder_needs_its_place() {
    let mut doc = build_initial_note_document("Title\nBody", &[1; 16]).unwrap();
    let past = insert_attachment_placeholder(&mut doc.clone(), 99, info("A", "public.png"), &options());
    assert!(past.unwrap_err().to_string().contains("past the end"));
    let none = mark_attachment_placeholder(&mut doc, 2, info("A", "public.png"));
    assert!(none.unwrap_err().to_string().contains("no attachment placeholder"));

    let mut doc = build_initial_note_document(&format!("A{O}"), &[1; 16]).unwrap();
    mark_attachment_placeholder(&mut doc, 1, info("A", "public.png")).unwrap();
    let twice = mark_attachment_placeholder(&mut doc, 1, info("B", "public.png"));
    assert!(twice.unwrap_err().to_string().contains("already holds"));
}

// --- removing a placeholder -----------------------------------------------------------

/// The removed attachment's character is deleted as any character is (a
/// tombstone of the run it was, by this replica's edit); the other
/// placeholders keep their characters and their attachments.
#[test]
fn a_deleted_placeholder_takes_only_its_own_attachment() {
    let mut doc = build_initial_note_document(&format!("Title\n{O}\n{O} x {O}"), &[1; 16]).unwrap();
    mark_attachment_placeholder(&mut doc, 6, info("ONE", "public.jpeg")).unwrap();
    mark_attachment_placeholder(&mut doc, 8, info("TWO", "com.adobe.pdf")).unwrap();
    mark_attachment_placeholder(&mut doc, 12, info("THREE", "public.png")).unwrap();
    assert_eq!(decoded(&doc).1.len(), 3);

    let mut middle = doc.clone();
    delete_attachment_placeholder(&mut middle, 8, "TWO", &options()).unwrap();
    let (text, slots) = decoded(&middle);
    assert_eq!(text, format!("Title\n{O}\n x {O}"));
    assert_eq!(slots, [slot("ONE", "public.jpeg"), slot("THREE", "public.png")]);
    // Its run is gone with it; no other run carries its attachment.
    assert!(
        middle
            .attribute_runs
            .iter()
            .all(|run| run.attachment_info != Some(info("TWO", "com.adobe.pdf")))
    );
    assert_eq!(
        middle.attribute_runs.iter().map(|r| r.len()).sum::<u32>() as usize,
        text.encode_utf16().count()
    );
    // The same document as deleting that one character as a text edit.
    let mut edited = doc.clone();
    apply_text_edit(&mut edited, &format!("Title\n{O}\n x {O}"), &options()).unwrap();
    assert_eq!(middle, edited);

    // First and last, last first as push does it.
    let mut ends = doc.clone();
    delete_attachment_placeholder(&mut ends, 12, "THREE", &options()).unwrap();
    delete_attachment_placeholder(&mut ends, 6, "ONE", &options()).unwrap();
    let (text, slots) = decoded(&ends);
    assert_eq!(text, format!("Title\n\n{O} x "));
    assert_eq!(slots, [slot("TWO", "com.adobe.pdf")]);

    // Text edits and a new attachment after a removal still decode.
    apply_text_edit(&mut ends, &format!("Title\n\n{O} x\n"), &options()).unwrap();
    insert_attachment_placeholder(&mut ends, 11, info("NEW", "public.png"), &options()).unwrap();
    let (text, slots) = decoded(&ends);
    assert_eq!(text, format!("Title\n\n{O} x\n{O}"));
    assert_eq!(slots, [slot("TWO", "com.adobe.pdf"), slot("NEW", "public.png")]);
}

/// A placeholder put in a real note and taken out again leaves the text it
/// had.
#[test]
fn a_placeholder_deleted_from_a_real_note_leaves_its_text() {
    let raw = decompress_note_document(&common::payload("real_plain_note.json")).unwrap();
    let mut doc = parse_note_document(&raw).unwrap();
    let before = doc.text.clone();
    let end = before.encode_utf16().count();
    apply_text_edit(&mut doc, &format!("{before}\n"), &options()).unwrap();
    insert_attachment_placeholder(&mut doc, end + 1, info("ATT-NEW", "public.png"), &options()).unwrap();

    delete_attachment_placeholder(
        &mut doc,
        end + 1,
        "ATT-NEW",
        &ApplyTextEditOptions { replica_id: [9; 16] },
    )
    .unwrap();
    let (text, slots) = decoded(&doc);
    assert_eq!(text, format!("{before}\n"));
    assert_eq!(slots, []);
    assert!(doc.attribute_runs.iter().all(|r| r.attachment_info.is_none()));
}

#[test]
fn only_the_named_attachments_placeholder_is_deleted() {
    let mut doc = build_initial_note_document(&format!("A{O}{O}"), &[1; 16]).unwrap();
    mark_attachment_placeholder(&mut doc, 1, info("ONE", "public.png")).unwrap();
    let untouched = doc.clone();

    let no_placeholder = delete_attachment_placeholder(&mut doc, 0, "ONE", &options());
    assert!(
        no_placeholder
            .unwrap_err()
            .to_string()
            .contains("no attachment placeholder")
    );
    let past = delete_attachment_placeholder(&mut doc, 99, "ONE", &options());
    assert!(past.unwrap_err().to_string().contains("no attachment placeholder"));
    let another = delete_attachment_placeholder(&mut doc, 1, "TWO", &options());
    assert!(another.unwrap_err().to_string().contains("holds another attachment"));
    // A placeholder that carries no attachment is nobody's to remove.
    let bare = delete_attachment_placeholder(&mut doc, 2, "ONE", &options());
    assert!(bare.unwrap_err().to_string().contains("holds another attachment"));
    assert_eq!(doc, untouched);
}
