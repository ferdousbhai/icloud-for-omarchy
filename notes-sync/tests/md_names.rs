//! Note titles and file names: title to file name, sanitizing, and the
//! title paragraph. Originally derived from icloud-md's tests.

use std::collections::HashSet;

use icloud_notes_sync::doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind};
use icloud_notes_sync::md::filename::*;
use icloud_notes_sync::md::parse::parse_note_markdown;
use icloud_notes_sync::md::render::render_note_markdown;
use icloud_notes_sync::md::title::*;
use icloud_notes_sync::vault::state::TitleMode;

const ILLEGAL: [&str; 13] = ["/", "\\", ":", "*", "?", "\"", "<", ">", "|", "#", "^", "[", "]"];

#[test]
fn plain_titles_are_their_own_stem() {
    assert_eq!(encode_title_stem("Grocery list"), "Grocery list");
    assert_eq!(decode_title_stem("Grocery list"), "Grocery list");
}

#[test]
fn illegal_characters_map_to_legal_ones_and_round_trip() {
    for c in ILLEGAL {
        let stem = encode_title_stem(&format!("a{c}b"));
        assert!(!stem.contains(c), "{c} survived into {stem}");
        let title = format!("before{c}after");
        assert_eq!(decode_title_stem(&encode_title_stem(&title)), title);
    }
    let all = ILLEGAL.concat();
    let stem = encode_title_stem(&all);
    assert!(ILLEGAL.iter().all(|c| !stem.contains(c)));
    assert_eq!(decode_title_stem(&stem), all);
}

#[test]
fn homoglyphs_already_in_a_title_round_trip() {
    for title in [
        "Already \u{2044} fraction",
        "Colon \u{A789} here",
        "\u{29F5} leading",
        "mixed \u{2044} and / together",
    ] {
        assert_eq!(decode_title_stem(&encode_title_stem(title)), title);
    }
    let title = "a/b:c*d?e|f#g";
    let once = encode_title_stem(title);
    assert_eq!(decode_title_stem(&once), title);
    assert_eq!(encode_title_stem(&decode_title_stem(&once)), once);
}

#[test]
fn emoji_and_non_latin_scripts_pass_through() {
    for title in ["Café ☕ notes", "日本語のノート", "Ελληνικά", "emoji 👨‍👩‍👧‍👦 family"]
    {
        assert_eq!(encode_title_stem(title), title);
        assert_eq!(decode_title_stem(title), title);
    }
}

#[test]
fn representability() {
    assert!(title_is_representable("Grocery list"));
    assert!(title_is_representable("Recipes: pie/tart"));
    assert!(title_is_representable(&"a".repeat(MAX_TITLE_LENGTH)));
    assert!(!title_is_representable(&"a".repeat(MAX_TITLE_LENGTH + 1)));
    assert!(
        representability_problem(&"a".repeat(MAX_TITLE_LENGTH + 1))
            .unwrap()
            .contains("longer than")
    );
    assert!(representability_problem(".hidden").unwrap().contains("hidden file"));
    assert!(
        representability_problem("Trailing dot.")
            .unwrap()
            .contains("Windows silently strips")
    );
    assert!(title_is_representable("Trailing space "));
    assert_eq!(carried_title_spelling("Trailing space "), "Trailing space");
    assert!(
        representability_problem("CON ")
            .unwrap()
            .contains("reserved device name")
    );
    assert!(!title_is_representable("Trailing dot. "));
    assert!(title_is_representable(&format!("{} ", "x".repeat(60))));
    assert!(!title_is_representable(&format!("{} ", "x".repeat(61))));
    for name in ["CON", "con", "Nul", "COM1", "lpt9", "AUX"] {
        assert!(
            representability_problem(name).unwrap().contains("reserved device name"),
            "{name}"
        );
    }
    assert!(title_is_representable("CONTACTS"));
    assert!(title_is_representable("COM10"));
    assert!(!title_is_representable(""));
    assert!(!title_is_representable("   "));
}

#[test]
fn round_trips_over_generated_character_soup() {
    let alphabet: Vec<&str> = ILLEGAL
        .iter()
        .copied()
        .chain([
            "\u{2044}", "\u{A789}", "\u{FF1F}", "\u{2758}", "\u{FF03}", "\u{FF3B}", "\u{FF3D}", "\u{29F5}", "\u{2217}",
            "\u{201D}", "\u{2039}", "\u{203A}", "\u{FF3E}", "a", "Z", " ", ".", "-", "_", "é", "日", "☕", "\u{2060}",
        ])
        .collect();
    // The TS LCG runs in float64 (the product overflows 2^53), so this does too.
    let mut seed: f64 = 20260730.0;
    let mut next = || {
        seed = (seed * 1103515245.0 + 12345.0) % 2147483648.0;
        seed as u64
    };
    for _ in 0..2000 {
        let length = next() % 12;
        let title: String = (0..length)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        let stem = encode_title_stem(&title);
        assert!(ILLEGAL.iter().all(|c| !stem.contains(c)), "{title:?} → {stem:?}");
        assert_eq!(decode_title_stem(&stem), title);
    }
}

#[test]
fn note_file_names() {
    assert_eq!(note_file_name("Grocery list\nmilk, eggs"), "Grocery list.md");
    assert_eq!(note_file_name(""), "Untitled.md");
    assert_eq!(note_file_name("a/b:c*d?e\"f<g>h|i"), "abcdefghi.md");

    let set = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
    assert_eq!(unique_file_name("New Note.md", &set(&[])), "New Note.md");
    assert_eq!(unique_file_name("New Note.md", &set(&["New Note.md"])), "New Note 2.md");
    assert_eq!(
        unique_file_name("New Note.md", &set(&["New Note.md", "New Note 2.md", "New Note 3.md"])),
        "New Note 4.md"
    );
}

#[test]
fn file_name_carries_title_cases() {
    assert!(file_name_carries_title("Groceries.md", "Groceries"));
    assert!(file_name_carries_title("Pat\u{2044}Alex.md", "Pat/Alex"));
    assert!(file_name_carries_title("Groceries 2.md", "Groceries"));
    assert!(file_name_carries_title("Groceries 17.md", "Groceries"));
    assert_eq!(
        note_file_name_for("Restaurants & Places List ", TitleMode::Filename),
        "Restaurants & Places List.md"
    );
    assert!(file_name_carries_title(
        "Restaurants & Places List.md",
        "Restaurants & Places List "
    ));
    assert!(file_name_carries_title(
        "Restaurants & Places List 2.md",
        "Restaurants & Places List "
    ));
    assert!(!file_name_carries_title("Shopping list.md", "Groceries"));
    assert!(!file_name_carries_title("Groceries 1.md", "Groceries"));
    assert!(!file_name_carries_title("Groceries draft.md", "Groceries"));
    assert!(file_name_carries_title("Groceries 2.md", "Groceries 2"));
    let huge = "x".repeat(200);
    assert!(file_name_carries_title("Untitled.md", &huge));
    assert!(file_name_carries_title("Untitled 3.md", &huge));
}

#[test]
fn title_needing_frontmatter_cases() {
    let f = |t: &str| title_needing_frontmatter(t, TitleMode::Filename);
    assert_eq!(f("Groceries"), None);
    assert_eq!(f("Pat/Alex: notes"), None);
    let huge = "x".repeat(200);
    assert_eq!(f(&huge).as_deref(), Some(huge.as_str()));
    assert_eq!(f(".hidden").as_deref(), Some(".hidden"));
    assert_eq!(f("CON").as_deref(), Some("CON"));
    assert_eq!(f("Trailing space "), None);
    assert_eq!(f("CON ").as_deref(), Some("CON "));
    assert_eq!(f(""), None);
    assert_eq!(f("   "), None);
    assert_eq!(title_needing_frontmatter(".hidden", TitleMode::InBody), None);
    assert_eq!(title_needing_frontmatter(&huge, TitleMode::InBody), None);
    for title in [
        "Groceries",
        "Pat/Alex",
        ".hidden",
        "CON",
        "CON ",
        huge.as_str(),
        "Trailing space ",
    ] {
        assert_eq!(
            note_file_name_for(title, TitleMode::Filename) == "Untitled.md",
            f(title).is_some(),
            "{title}"
        );
    }
    assert_eq!(note_file_name_for("", TitleMode::Filename), "Untitled.md");
}

fn paragraph(kind: ParagraphKind, text: &str, start: usize) -> FormatParagraph {
    let length = text.encode_utf16().count();
    FormatParagraph {
        kind,
        indent: 0,
        block_quote_level: 0,
        done: None,
        start_number: 0,
        text: text.into(),
        spans: if length > 0 {
            vec![InlineSpan {
                style: InlineStyle::PLAIN,
                length,
            }]
        } else {
            vec![]
        },
        start,
    }
}

#[test]
fn title_paragraph_split_and_restore() {
    use ParagraphKind::*;
    let model = vec![
        paragraph(Title, "My Note", 0),
        paragraph(Body, "", 8),
        paragraph(Body, "Body text", 9),
    ];
    let split = split_title_paragraph(&model);
    assert_eq!(split.title.unwrap().text, "My Note");
    assert_eq!(split.body.len(), 2);
    assert_eq!(split.body[0].text, "");

    let empty = split_title_paragraph(&[]);
    assert_eq!(empty.title, None);
    assert!(empty.body.is_empty());

    let mono = vec![
        paragraph(Monospaced, "code line one", 0),
        paragraph(Monospaced, "code line two", 14),
    ];
    let rendered = render_note_markdown(&split_title_paragraph(&mono).body);
    assert_eq!(rendered.matches("```").count() % 2, 0);
    assert!(parse_note_markdown(&rendered).is_ok());

    let list = vec![
        paragraph(BulletList, "first item", 0),
        paragraph(BulletList, "second item", 11),
    ];
    assert_eq!(
        render_note_markdown(&split_title_paragraph(&list).body),
        "- second item"
    );

    let title = FormatParagraph {
        spans: vec![InlineSpan {
            style: InlineStyle {
                bold: true,
                ..InlineStyle::PLAIN
            },
            length: 12,
        }],
        ..paragraph(Heading, "Styled Title", 0)
    };
    let restored = restore_title_paragraph(&title, &[paragraph(Body, "Body text", 0)]);
    assert_eq!(restored[0].kind, Heading);
    assert!(restored[0].spans[0].style.bold);

    let body = vec![paragraph(Body, "", 999), paragraph(Body, "Body text", 999)];
    let restored = restore_title_paragraph(&paragraph(Title, "My Note", 0), &body);
    assert_eq!(restored.iter().map(|p| p.start).collect::<Vec<_>>(), vec![0, 8, 9]);
    assert_eq!(body[0].start, 999, "the caller's model must not be mutated");

    let t = title_paragraph_from_filename("Renamed Note");
    assert_eq!(t.kind, Title);
    assert_eq!(t.text, "Renamed Note");
    assert_eq!(
        t.spans,
        vec![InlineSpan {
            style: InlineStyle::PLAIN,
            length: 12
        }]
    );
    assert!(title_paragraph_from_filename("").spans.is_empty());

    let model = vec![
        paragraph(Title, "My Note", 0),
        paragraph(Body, "", 8),
        paragraph(BulletList, "one", 9),
        paragraph(BulletList, "two", 13),
    ];
    let split = split_title_paragraph(&model);
    assert_eq!(restore_title_paragraph(&split.title.unwrap(), &split.body), model);

    let model = vec![
        paragraph(Title, "My Note", 0),
        paragraph(Body, "", 8),
        paragraph(Heading, "A section", 9),
        paragraph(Body, "Some prose.", 19),
    ];
    let reparsed = parse_note_markdown(&render_note_markdown(&split_title_paragraph(&model).body)).unwrap();
    assert_eq!(reparsed.text, "\nA section\nSome prose.");

    let parsed = parse_note_markdown("\nBody text\nMore body").unwrap();
    let restored = restore_title_paragraph_text(&title_paragraph_from_filename("My Note"), &parsed.paragraphs);
    assert_eq!(restored.text, "My Note\n\nBody text\nMore body");
    assert_eq!(
        restored.paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(),
        vec![0, 8, 9, 19]
    );
    let reparsed = parse_note_markdown(&render_note_markdown(&restored.paragraphs)).unwrap();
    assert_eq!(reparsed.text, restored.text);
}

#[test]
fn file_names_read_back_as_titles() {
    assert_eq!(title_from_note_file_name("Notes/Shopping list.md"), "Shopping list");
    assert_eq!(
        title_from_note_file_name("Notes/Pat\u{2044}Alex\u{A789} notes.md"),
        "Pat/Alex: notes"
    );
    assert_eq!(title_from_note_file_name("Notes/v1.2 plans.md"), "v1.2 plans");
    assert_eq!(title_from_note_file_name("Notes/Groceries 2.md"), "Groceries 2");
}
