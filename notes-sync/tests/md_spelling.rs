//! Markdown spelling 1 files rewritten in spelling 2 (`md::spelling`): what
//! spelling 1 read a file as is what spelling 2 reads the rewritten file as.

mod common;

use std::io::Read;

use icloud_notes_sync::doc::format::{FormatParagraph, ParagraphKind, formats_round_trip_equal};
use icloud_notes_sync::md::parse::{ParsedNoteMarkdown, parse_note_markdown};
use icloud_notes_sync::md::render::render_note_markdown;
use icloud_notes_sync::md::spelling::{read_as_spelling_1, upgrade_spelling};
use serde::Deserialize;
use serde_json::Value;

use ParagraphKind::*;

fn gz_json(path: &str) -> Value {
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn same(a: &ParsedNoteMarkdown, b: &ParsedNoteMarkdown) -> bool {
    a.text == b.text && formats_round_trip_equal(&a.paragraphs, &b.paragraphs)
}

/// What spelling 2 reads `markdown` as once upgraded.
fn upgraded(markdown: &str) -> String {
    upgrade_spelling(markdown).unwrap_or_else(|| markdown.to_owned())
}

/// Every note of the render corpus as spelling 1 wrote it
/// (`tests/fixtures/md_render_spelling_1.json.gz`, the `markdown` of
/// md_render.json at 0.11.1, in its order): upgraded, it reads as spelling 1
/// read it, and it is mostly just what spelling 2 writes for the note.
#[test]
fn every_spelling_1_render_upgrades_to_what_it_meant() {
    let here = env!("CARGO_MANIFEST_DIR");
    let old = gz_json(&format!("{here}/tests/fixtures/md_render_spelling_1.json.gz"));
    let cases = gz_json(&format!("{here}/tests/golden/md_render.json.gz"));
    let (old, cases) = (old.as_array().unwrap(), cases.as_array().unwrap());
    assert_eq!(old.len(), cases.len());
    let (mut rewritten, mut as_written_now, mut differs) = (0, 0, Vec::new());
    for (index, (old, case)) in old.iter().zip(cases).enumerate() {
        let old = old.as_str().unwrap();
        let Ok(meant) = read_as_spelling_1(old) else {
            continue;
        };
        let upgrade = upgrade_spelling(old);
        rewritten += usize::from(upgrade.is_some());
        let file = upgrade.unwrap_or_else(|| old.to_owned());
        let read = parse_note_markdown(&file).unwrap_or_else(|e| panic!("case {index}: {e}\n{file:?}"));
        let paragraphs: Vec<FormatParagraph> = Vec::deserialize(&case["paragraphs"]).unwrap();
        let now = render_note_markdown(&paragraphs);
        as_written_now += usize::from(file == now);
        // Or, where spelling 2 has no exact spelling for what spelling 1
        // read (a numbered list counting from 2 right below text), what a
        // pull now writes for the note reads as it does.
        if !same(&meant, &read) && !parse_note_markdown(&now).is_ok_and(|now| same(&now, &read)) {
            differs.push(format!(
                "case {index}: {old:?} upgraded to {file:?}, now written {now:?}"
            ));
        }
    }
    eprintln!(
        "spelling 1 renders: {} cases, {rewritten} rewritten, {as_written_now} now exactly as spelling 2 writes them",
        old.len()
    );
    assert!(
        differs.is_empty(),
        "{} read otherwise:\n{}",
        differs.len(),
        differs.join("\n")
    );
    assert!(rewritten > 500, "{rewritten}");
}

fn read(markdown: &str) -> Vec<(ParagraphKind, u32, String)> {
    parse_note_markdown(markdown)
        .unwrap()
        .paragraphs
        .into_iter()
        .map(|p| (p.kind, p.block_quote_level, p.text))
        .collect()
}

#[test]
fn a_line_below_a_list_item_stays_a_line_of_its_own() {
    assert_eq!(upgraded("- item\ntext"), "- item\n\ntext");
    assert_eq!(upgraded("- item\n\ntext"), "- item\n\n\ntext");
    // The item after them counted from 1 again (it followed text), which a
    // numbered item there says.
    assert_eq!(upgraded("1. one\ntext\nmore\n2. two"), "1. one\n\ntext\nmore\n1. two");
    assert_eq!(read(&upgraded("1. one\ntext\nmore\n2. two"))[3].0, NumberedList);
    // Its text is text, whatever it starts with.
    assert_eq!(upgraded("- [x] done\n  5. five"), "- [x] done\n\n5\\. five");
    // An indented line below the item: its indentation goes.
    assert_eq!(upgraded("- item\n  text"), "- item\n\ntext");
    assert_eq!(upgraded("> - item\n> text"), "> - item\n>\n> text");
}

#[test]
fn a_lazy_line_below_a_quote_stays_less_quoted() {
    assert_eq!(upgraded("> quoted\ntext"), "> quoted\n\ntext");
    assert_eq!(upgraded("> > deep\n> less\nnone"), "> > deep\n>\n> less\n\nnone");
    assert_eq!(
        read(&upgraded("> > deep\n> less\nnone")),
        vec![
            (Body, 2, "deep".into()),
            (Body, 1, "less".into()),
            (Body, 0, "none".into())
        ]
    );
}

#[test]
fn a_hard_line_break_stays_a_line_ending() {
    assert_eq!(upgraded("a\\\nb"), "a\nb");
    assert_eq!(upgraded("a  \nb"), "a\nb");
    assert_eq!(upgraded("- a\\\nb"), "- a\n\nb");
}

#[test]
fn what_both_spellings_read_alike_is_left_as_it_is() {
    for markdown in [
        "# Title\ntext\n\nmore",
        "- a\n- b\n\n## heading",
        "> a\n>\n> b",
        "text\u{2028}more",
        "- a\u{2028}b",
        "a\n\n\nb",
        "```\n- a\nb\n```",
    ] {
        assert_eq!(upgrade_spelling(markdown), None, "{markdown:?}");
    }
}

#[test]
fn a_table_below_a_list_item_is_upgraded_around_it() {
    let old = "- item\n| a | b |\n| --- | --- |\n| 1 | 2 |\nafter";
    assert_eq!(upgraded(old), "- item\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\nafter");
}
