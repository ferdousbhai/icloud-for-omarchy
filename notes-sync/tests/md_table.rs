//! Port of icloud-md's `markdownTable.test.ts`.

mod common;

use common::strings;
use icloud_notes_sync::md::table::*;

fn round_trip(rows: &[&[&str]]) {
    let g = strings(rows);
    assert_eq!(parse_markdown_table(&render_markdown_table(&g).unwrap()).unwrap(), g);
}

#[test]
fn render_pins_the_remark_format() {
    assert_eq!(
        render_markdown_table(&strings(&[&["A0", "B0"], &["A1", ""]])).unwrap(),
        "| A0 | B0 |\n| - | - |\n| A1 | |"
    );
    round_trip(&[&["*bold*", "plain"]]);
    assert_eq!(
        render_markdown_table(&strings(&[&["[[Note]]", "![[img.png]]"], &["[[Note|Alias]]", "plain"]])).unwrap(),
        "| [[Note]] | ![[img.png]] |\n| - | - |\n| [[Note\\|Alias]] | plain |"
    );
    round_trip(&[&["[[Note]]"], &["[[Note|Alias]]"], &["[[a]]\nsecond line"]]);
    assert!(render_markdown_table(&[]).unwrap_err().contains("no rows"));
}

#[test]
fn parse_is_the_inverse_of_render() {
    round_trip(&[&["A0", "B0"], &["A1", "B1"]]);
    round_trip(&[&["a|b", "line1\nline2"], &["back\\slash", "plain"]]);
    round_trip(&[
        &["*emphasis*", "`code`", "[link](https://example.com)"],
        &["_under_", "~~strike~~", "www.example.com"],
    ]);
    round_trip(&[&["a", "b", "c"], &["", "", ""]]);
    round_trip(&[&["Only", "Header"]]);
}

#[test]
fn parse_refuses_what_it_cant_trust() {
    assert!(
        parse_markdown_table("| A | B |\n| --- | --- |\n| only-one |")
            .unwrap_err()
            .contains("column count")
    );
    assert!(parse_markdown_table("| A | B |\n| not a separator |").is_err());
    assert!(parse_markdown_table("prose first\n| A |\n| --- |").is_err());
}

#[test]
fn parse_reads_the_previous_renderers_format() {
    let old = "| A0 | pipe\\|cell |\n| --- | --- |\n| back\\\\slash | multi<br>line |\n|  |  |";
    assert_eq!(
        parse_markdown_table(old).unwrap(),
        strings(&[&["A0", "pipe|cell"], &["back\\slash", "multi\nline"], &["", ""]])
    );
    let old = "| *bold* | `code` |\n| --- | --- |\n| [link](x) | plain |";
    assert_eq!(
        parse_markdown_table(old).unwrap(),
        strings(&[&["*bold*", "`code`"], &["[link](x)", "plain"]])
    );
}

#[test]
fn find_blocks() {
    let text = format!(
        "Some intro text.\n\n{}\n\nSome trailing text.",
        render_markdown_table(&strings(&[&["A", "B"]])).unwrap()
    );
    let blocks = find_markdown_table_blocks(&text);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].grid, strings(&[&["A", "B"]]));

    let text = format!(
        "Intro\n{}\nMiddle text\n{}\nOutro",
        render_markdown_table(&strings(&[&["First"]])).unwrap(),
        render_markdown_table(&strings(&[&["Second", "Table"]])).unwrap()
    );
    let blocks = find_markdown_table_blocks(&text);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].grid, strings(&[&["First"]]));
    assert_eq!(blocks[1].grid, strings(&[&["Second", "Table"]]));

    assert!(find_markdown_table_blocks("Just a normal note.\nWith a few lines.\nNo tables here.").is_empty());

    let blocks = find_markdown_table_blocks("line0\n| A |\n| --- |\n| B |\nline4");
    assert_eq!((blocks[0].start_line, blocks[0].end_line), (1, 4));

    let blocks = find_markdown_table_blocks("| A | B |\n| - | - |\n| 1 | 2 |\nprose continues here");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].grid, strings(&[&["A", "B"], &["1", "2"]]));
    assert_eq!(blocks[0].end_line, 3);

    let blocks = find_markdown_table_blocks("| A | B |\n| - | - |\n| 1 | 2 |\n| widowed |");
    assert_eq!(blocks[0].end_line, 3);
    assert_eq!(blocks[0].grid, strings(&[&["A", "B"], &["1", "2"]]));

    let text = format!(
        "```\n| X |\n| --- |\n| fenced |\n```\n\n{}",
        render_markdown_table(&strings(&[&["Real", "Table"]])).unwrap()
    );
    let blocks = find_markdown_table_blocks(&text);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].grid, strings(&[&["Real", "Table"]]));

    assert!(find_markdown_table_blocks("> | q | r |\n> | --- | --- |\n> | 1 | 2 |").is_empty());

    let blocks = find_markdown_table_blocks("Intro prose.\n\n| A0 | B0 |\n| --- | --- |\n|  |  |\n\nOutro.");
    assert_eq!((blocks[0].start_line, blocks[0].end_line), (2, 5));
    assert_eq!(blocks[0].grid, strings(&[&["A0", "B0"], &["", ""]]));
}

#[test]
fn friendly_spellings_in_cells() {
    let rendered = render_markdown_table(&strings(&[&["link", "https://maps.app.goo.gl/abc123"]])).unwrap();
    assert!(rendered.contains("https://maps.app.goo.gl/abc123"));
    assert!(!rendered.contains("https\\:"));
    assert_eq!(
        parse_markdown_table(&rendered).unwrap(),
        strings(&[&["link", "https://maps.app.goo.gl/abc123"]])
    );

    let g = strings(&[
        &["file", "host", "who"],
        &["flow.ts", "Www.VJW.digital.go.jp", "me@example.com"],
    ]);
    let rendered = render_markdown_table(&g).unwrap();
    assert!(!rendered.contains("\\.") && !rendered.contains("\\@"));
    assert_eq!(parse_markdown_table(&rendered).unwrap(), g);

    let mixed = strings(&[&["a"], &["[[N|x]] flow.ts"], &["www.exa_mple.com"]]);
    let rendered = render_markdown_table(&mixed).unwrap();
    assert!(rendered.contains("[[N\\|x]] flow.ts"));
    assert!(rendered.contains("www\\.exa\\_mple.com"));
    assert_eq!(parse_markdown_table(&rendered).unwrap(), mixed);
}
