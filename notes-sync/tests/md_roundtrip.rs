//! Format model → Markdown → format model round trips. Originally derived from icloud-md's tests.

use icloud_notes_sync::doc::format::formats_round_trip_equal;
use icloud_notes_sync::doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind};
use icloud_notes_sync::md::parse::parse_note_markdown;
use icloud_notes_sync::md::render::render_note_markdown;

use ParagraphKind::*;

fn span(length: usize) -> InlineSpan {
    InlineSpan {
        style: InlineStyle::PLAIN,
        length,
    }
}

fn styled(length: usize, f: impl FnOnce(&mut InlineStyle)) -> InlineSpan {
    let mut style = InlineStyle::PLAIN;
    f(&mut style);
    InlineSpan { style, length }
}

fn p(kind: ParagraphKind, text: &str) -> FormatParagraph {
    let length = text.encode_utf16().count();
    FormatParagraph {
        kind,
        indent: 0,
        block_quote_level: 0,
        done: None,
        start_number: 0,
        text: text.to_string(),
        spans: if length > 0 { vec![span(length)] } else { vec![] },
        start: 0,
    }
}

fn todo(text: &str, done: bool) -> FormatParagraph {
    FormatParagraph {
        done: Some(done),
        ..p(TodoList, text)
    }
}

fn quoted(kind: ParagraphKind, text: &str, level: u32) -> FormatParagraph {
    FormatParagraph {
        block_quote_level: level,
        ..p(kind, text)
    }
}

fn indented(kind: ParagraphKind, text: &str, indent: i32) -> FormatParagraph {
    FormatParagraph {
        indent,
        ..p(kind, text)
    }
}

fn with_spans(mut paragraph: FormatParagraph, spans: Vec<InlineSpan>) -> FormatParagraph {
    paragraph.spans = spans;
    paragraph
}

/// `parse(render(model))` reproduces the exact text and projection.
fn assert_round_trips(paragraphs: &[FormatParagraph]) -> String {
    let rendered = render_note_markdown(paragraphs);
    let back = parse_note_markdown(&rendered).unwrap_or_else(|e| panic!("parse failed: {e}\nrendered: {rendered:?}"));
    let text = paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(back.text, text, "text drifted through {rendered:?}");
    assert!(
        formats_round_trip_equal(paragraphs, &back.paragraphs),
        "formatting drifted through {rendered:?}"
    );
    rendered
}

#[test]
fn title_heading_subheading_render_as_hashes() {
    let rendered = assert_round_trips(&[
        p(Title, "My Note"),
        p(Heading, "H"),
        p(Subheading, "S"),
        p(Body, "text"),
    ]);
    assert_eq!(rendered, "# My Note\n## H\n### S\ntext");
}

#[test]
fn checklists_render_as_gfm_task_items_including_empty_ones() {
    let rendered = assert_round_trips(&[todo("one", false), todo("two", true), todo("", false), todo("", true)]);
    assert_eq!(rendered, "- [ ] one\n- [x] two\n- [ ]\n- [x]");
}

#[test]
fn a_bullet_whose_text_is_brackets_renders_escaped() {
    let rendered = assert_round_trips(&[p(BulletList, "[ ]")]);
    assert_ne!(rendered, "- [ ]");
}

#[test]
fn blank_lines_are_real_empty_paragraphs_everywhere() {
    assert_round_trips(&[p(Body, "a"), p(Body, ""), p(Body, ""), p(Body, "b")]);
    assert_round_trips(&[p(BulletList, "a"), p(Body, ""), p(BulletList, "b")]);
    assert_round_trips(&[quoted(Body, "q1", 1), quoted(Body, "", 1), quoted(Body, "q2", 1)]);
    assert_round_trips(&[p(Body, "a"), p(Body, "")]);
}

#[test]
fn list_nesting_type_switches_and_numbered_starts() {
    assert_round_trips(&[
        p(BulletList, "top"),
        indented(BulletList, "deep", 1),
        indented(NumberedList, "num", 1),
        p(BulletList, "back"),
    ]);
    let rendered = assert_round_trips(&[
        FormatParagraph {
            start_number: 5,
            ..p(NumberedList, "five")
        },
        p(NumberedList, "six"),
    ]);
    assert_eq!(rendered, "5. five\n6. six");
}

#[test]
fn dash_lists_render_as_dash_items() {
    assert_eq!(render_note_markdown(&[p(DashList, "item")]), "- item");
}

/// A line of text right below a list item or a quoted line, and quoted
/// less, would be more of that item or quote to Markdown (a lazy
/// continuation line): it is written a blank line below, and that blank line
/// is no line of the note. Empty lines between them, ending at the text's
/// quote level, get one more blank line.
#[test]
fn a_body_line_after_a_list_or_quote_is_written_a_blank_line_below() {
    assert_eq!(
        assert_round_trips(&[p(BulletList, "item"), p(Body, "directly after")]),
        "- item\n\ndirectly after"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "quoted", 1), p(Body, "outside")]),
        "> quoted\n\noutside"
    );
    assert_eq!(
        assert_round_trips(&[quoted(NumberedList, "item", 1), quoted(Body, "after", 1)]),
        "> 1. item\n>\n> after"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "deep", 2), quoted(Body, "less", 1)]),
        "> > deep\n>\n> less"
    );
    assert_eq!(
        assert_round_trips(&[p(BulletList, "item"), p(Body, ""), p(Body, "after")]),
        "- item\n\n\nafter"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "quoted", 1), p(Body, ""), p(Body, "outside")]),
        "> quoted\n\n\noutside"
    );
    // Nothing else changes: an empty quoted line between, quoted lines at
    // one level, a list below text.
    assert_eq!(
        assert_round_trips(&[quoted(Body, "quoted", 1), quoted(Body, "", 1), p(Body, "outside")]),
        "> quoted\n>\noutside"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "a", 1), quoted(Body, "", 1), quoted(Body, "b", 1)]),
        "> a\n>\n> b"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "a"), p(BulletList, "b"), p(Heading, "c")]),
        "a\n- b\n## c"
    );
    // An empty checklist item before text is written as one.
    assert_eq!(
        assert_round_trips(&[todo("", false), p(Body, "after")]),
        "- [ ]\n\nafter"
    );
}

fn kinds(markdown: &str) -> Vec<(ParagraphKind, u32, String)> {
    parse_note_markdown(markdown)
        .unwrap()
        .paragraphs
        .into_iter()
        .map(|p| (p.kind, p.block_quote_level, p.text))
        .collect()
}

/// A hard-wrapped line below a list item (a lazy continuation line, and an
/// indented one alike) is more of the item, as CommonMark reads it: one line
/// of the note, with a line break (U+2028) where the line was wrapped.
#[test]
fn a_wrapped_bullet_item_is_one_item_with_a_line_break() {
    assert_eq!(
        kinds("- first half\nsecond half\n- next"),
        vec![
            (BulletList, 0, "first half\u{2028}second half".into()),
            (BulletList, 0, "next".into())
        ]
    );
    assert_eq!(
        kinds("- first half\n  second half"),
        vec![(BulletList, 0, "first half\u{2028}second half".into())]
    );
    // Written back as a wrapped line, which reads back the same.
    let item = p(BulletList, "first half\u{2028}second half");
    assert_eq!(assert_round_trips(&[item]), "- first half\n  second half");
}

#[test]
fn a_wrapped_numbered_item_keeps_the_list_counting() {
    let parsed = parse_note_markdown("1. one\nwrapped\n2. two\n3. three").unwrap();
    let got: Vec<_> = parsed.paragraphs.iter().map(|p| (p.kind, p.text.as_str())).collect();
    assert_eq!(
        got,
        vec![
            (NumberedList, "one\u{2028}wrapped"),
            (NumberedList, "two"),
            (NumberedList, "three")
        ]
    );
    // One list: the items after the wrapped one go on counting from it.
    assert!(parsed.paragraphs[1..].iter().all(|p| p.start_number == 0));
    assert_eq!(
        assert_round_trips(&parsed.paragraphs),
        "1. one\n   wrapped\n2. two\n3. three"
    );
}

#[test]
fn a_wrapped_checklist_item_is_one_item_with_a_line_break() {
    let parsed = parse_note_markdown("- [x] done and\nwrapped\n- [ ] open").unwrap();
    let got: Vec<_> = parsed
        .paragraphs
        .iter()
        .map(|p| (p.kind, p.done, p.text.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (TodoList, Some(true), "done and\u{2028}wrapped"),
            (TodoList, Some(false), "open")
        ]
    );
    assert_eq!(
        assert_round_trips(&parsed.paragraphs),
        "- [x] done and\n  wrapped\n- [ ] open"
    );
}

/// A hard-wrapped line below a quoted line is more of the quote: a line of
/// the note at the quote's level (in body text a line break is a line of
/// the note, as anywhere else).
#[test]
fn a_wrapped_quote_line_stays_in_the_quote() {
    assert_eq!(
        kinds("> quoted\nlazy line"),
        vec![(Body, 1, "quoted".into()), (Body, 1, "lazy line".into())]
    );
    assert_eq!(
        kinds("> - item\n> lazy\nlazier"),
        vec![(BulletList, 1, "item\u{2028}lazy\u{2028}lazier".into())]
    );
}

/// The line break keeps the style around it, and a bold wrapped item is
/// written as one bold run over both lines.
#[test]
fn a_line_break_in_an_item_keeps_its_style() {
    let parsed = parse_note_markdown("- **bold\nwrapped**").unwrap();
    assert_eq!(parsed.paragraphs.len(), 1);
    assert_eq!(parsed.paragraphs[0].spans.len(), 1);
    assert!(parsed.paragraphs[0].spans[0].style.bold);
    assert_eq!(assert_round_trips(&parsed.paragraphs), "- **bold\n  wrapped**");
    // Where a line break can't be written as one (at the end of the item),
    // it is written as the character.
    let trailing = p(BulletList, "item\u{2028}");
    assert_eq!(assert_round_trips(&[trailing]), "- item\u{2028}");
}

#[test]
fn monospaced_paragraphs_group_into_a_fenced_block() {
    let rendered = assert_round_trips(&[
        p(Body, "before"),
        p(Monospaced, "code line"),
        p(Monospaced, ""),
        p(Monospaced, "more"),
        p(Body, "after"),
    ]);
    assert_eq!(rendered, "before\n```\ncode line\n\nmore\n```\nafter");
    assert_round_trips(&[p(Monospaced, "```"), p(Monospaced, "inner")]);
}

#[test]
fn markdown_significant_plain_text_renders_escaped() {
    assert_round_trips(&[
        p(Body, "- [ ] not a todo"),
        p(Body, "# not a heading"),
        p(Body, "1. not a list"),
        p(Body, "> not a quote"),
        p(Body, "a <u>literal</u> b"),
        p(Body, "*stars* and _underscores_"),
    ]);
}

#[test]
fn inline_styles_nest_and_whole_range_styles_wrap_outermost() {
    assert_round_trips(&[with_spans(
        p(Body, "bold italic struck under"),
        vec![
            styled(4, |s| s.bold = true),
            span(1),
            styled(6, |s| s.italic = true),
            span(1),
            styled(6, |s| s.strikethrough = true),
            span(1),
            styled(5, |s| s.underline = true),
        ],
    )]);
    let rendered = assert_round_trips(&[with_spans(
        p(Body, "s15 rest of line"),
        vec![
            styled(3, |s| {
                s.bold = true;
                s.italic = true;
            }),
            styled(13, |s| s.italic = true),
        ],
    )]);
    assert_eq!(rendered, "***s15** rest of line*");
}

#[test]
fn explicit_links_round_trip_and_bare_urls_stay_plain() {
    let linked = assert_round_trips(&[with_spans(
        p(Body, "see docs here"),
        vec![span(4), styled(9, |s| s.link = "https://example.com/".into())],
    )]);
    assert_eq!(linked, "see [docs here](https://example.com/)");
    assert_round_trips(&[with_spans(
        p(Body, "https://example.com/x"),
        vec![styled(21, |s| s.link = "https://example.com/x".into())],
    )]);
}

#[test]
fn checklists_inside_blockquotes_round_trip() {
    let rendered = assert_round_trips(&[FormatParagraph {
        block_quote_level: 1,
        ..todo("quoted todo", false)
    }]);
    assert_eq!(rendered, "> - [ ] quoted todo");
}

#[test]
fn object_replacement_placeholders_pass_through() {
    assert_round_trips(&[p(Body, "\u{FFFC}"), p(Body, "text \u{FFFC} inline")]);
}

#[test]
fn parser_refuses_constructs_apple_notes_cant_express() {
    assert!(
        parse_note_markdown("#### too deep")
            .unwrap_err()
            .reason
            .contains("depth-4 heading")
    );
    assert!(
        parse_note_markdown("a\n\n---\n\nb")
            .unwrap_err()
            .reason
            .contains("thematic break")
    );
    assert!(
        parse_note_markdown("<div>\nblock\n</div>")
            .unwrap_err()
            .reason
            .contains("HTML block")
    );
}

#[test]
fn parser_degrades_unsupported_inline_constructs_to_source_text() {
    assert_eq!(
        parse_note_markdown("some `inline code` here").unwrap().text,
        "some `inline code` here"
    );
}

#[test]
fn parser_accepts_common_hand_written_variants() {
    assert_eq!(parse_note_markdown("* item").unwrap().paragraphs[0].kind, BulletList);
    let setext = parse_note_markdown("Title\n=====").unwrap();
    let got: Vec<_> = setext.paragraphs.iter().map(|q| (q.kind, q.text.as_str())).collect();
    assert_eq!(got, vec![(Title, "Title")]);
}

#[test]
fn trailing_whitespace_is_trimmed_out_of_the_projection() {
    let heading = render_note_markdown(&[with_spans(p(Title, "Outfits "), vec![styled(8, |s| s.bold = true)])]);
    assert_eq!(heading, "# **Outfits**");
    assert_eq!(render_note_markdown(&[p(Body, "Fried Egg ")]), "Fried Egg");
    assert_eq!(render_note_markdown(&[p(BulletList, "item\t ")]), "- item");
    assert!(formats_round_trip_equal(
        &[p(Body, "Fried Egg ")],
        &[p(Body, "Fried Egg")]
    ));
    let mono = render_note_markdown(&[p(Monospaced, "code  ")]);
    assert_eq!(mono, "```\ncode  \n```");
    assert_eq!(parse_note_markdown(&mono).unwrap().paragraphs[0].text, "code  ");
}

#[test]
fn parser_trims_trailing_whitespace_including_references() {
    let parsed = parse_note_markdown("# **Outfits**&#x20;\nplain trailing \nFried Egg&#x20;").unwrap();
    let got: Vec<_> = parsed.paragraphs.iter().map(|q| (q.kind, q.text.as_str())).collect();
    assert_eq!(
        got,
        vec![(Title, "Outfits"), (Body, "plain trailing"), (Body, "Fried Egg")]
    );
    assert_eq!(parsed.text, "Outfits\nplain trailing\nFried Egg");
}

#[test]
fn bare_urls_render_unescaped() {
    let rendered = assert_round_trips(&[p(
        Body,
        "Possibly Montrose Beach?: https://maps.app.goo.gl/cXebu5pyuyk9sNsG6",
    )]);
    assert_eq!(
        rendered,
        "Possibly Montrose Beach?: https://maps.app.goo.gl/cXebu5pyuyk9sNsG6"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "https://example.com/a?b=1&c=2")]),
        "https://example.com/a?b=1&c=2"
    );
    assert_eq!(
        assert_round_trips(&[p(BulletList, "see https://example.com/x")]),
        "- see https://example.com/x"
    );
    assert_eq!(
        assert_round_trips(&[p(Heading, "See https://example.com")]),
        "## See https://example.com"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "(https://example.com)")]),
        "(https://example.com)"
    );
}

#[test]
fn ordinary_words_keep_autolink_ambiguous_punctuation() {
    for (text, want) in [
        ("Www.VJW.digital.go.jp", "Www.VJW.digital.go.jp"),
        ("Edit the flow.ts file", "Edit the flow.ts file"),
        ("call window.open() now", "call window.open() now"),
        ("a new.txt file", "a new.txt file"),
        ("W.W.", "W.W."),
        ("email me@example.com please", "email me@example.com please"),
        ("See www.example.com for more", "See www.example.com for more"),
        ("[[Note]] and flow.ts", "[[Note]] and flow.ts"),
        ("#new.stuff", "#new.stuff"),
    ] {
        assert_eq!(assert_round_trips(&[p(Body, text)]), want);
    }
    assert_eq!(
        assert_round_trips(&[p(Heading, "See www.example.com")]),
        "## See www.example.com"
    );
    assert_eq!(assert_round_trips(&[p(BulletList, "see flow.ts")]), "- see flow.ts");
    assert_eq!(assert_round_trips(&[todo("read flow.ts", false)]), "- [ ] read flow.ts");
    assert_eq!(assert_round_trips(&[quoted(Body, "see flow.ts", 1)]), "> see flow.ts");
    assert_eq!(
        assert_round_trips(&[with_spans(
            p(Body, "see flow.ts now"),
            vec![span(4), styled(7, |s| s.bold = true), span(4)]
        )]),
        "see **flow.ts** now"
    );
    assert_eq!(
        assert_round_trips(&[with_spans(
            p(Body, "www.example.com"),
            vec![styled(15, |s| s.link = "http://www.example.com".into())]
        )]),
        "[www.example.com](http://www.example.com)"
    );
}

#[test]
fn tokens_that_arent_safe_to_write_raw_keep_their_escapes() {
    for (text, want) in [
        ("www.exa_mple.com", "www\\.exa\\_mple.com"),
        ("www.exa*mple*.com", "www\\.exa\\*mple\\*.com"),
        ("www.example.com/?a&amp;b", "www\\.example.com/?a\\&amp;b"),
        ("say \"www.example.com\" ok", "say \"www\\.example.com\" ok"),
        ("# heading www.example.com", "\\# heading www.example.com"),
    ] {
        assert_eq!(assert_round_trips(&[p(Body, text)]), want);
    }
}

#[test]
fn each_unescaping_rule_stands_on_its_own() {
    assert_eq!(
        assert_round_trips(&[p(BulletList, "[ ]"), p(Body, "flow.ts")]),
        "- \\[ ]\n\nflow.ts"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "[[Note]] and www.exa_mple.com")]),
        "[[Note]] and www\\.exa\\_mple.com"
    );
}

#[test]
fn obsidian_wikilinks_keep_their_bare_brackets() {
    let text = "Between Osaka and Kyoto, do not take a [[Shinkansen]] -- it's close enough for a normal train.";
    assert_eq!(assert_round_trips(&[p(Body, text)]), text);
    for text in [
        "See [[Note|alias]] here",
        "[[Folder/Note#Heading]]",
        "![[embed.png]]",
        "[[a]] and [[b]]",
    ] {
        assert_eq!(assert_round_trips(&[p(Body, text)]), text);
    }
    assert_eq!(assert_round_trips(&[p(BulletList, "see [[Note]]")]), "- see [[Note]]");
    assert_eq!(
        assert_round_trips(&[todo("read [[Note]]", false)]),
        "- [ ] read [[Note]]"
    );
    assert_eq!(assert_round_trips(&[p(Heading, "About [[Note]]")]), "## About [[Note]]");
    assert_eq!(
        assert_round_trips(&[quoted(Body, "quoted [[Note]]", 1)]),
        "> quoted [[Note]]"
    );
    assert_eq!(
        assert_round_trips(&[with_spans(
            p(Body, "go to [[Note]] now"),
            vec![span(6), styled(8, |s| s.bold = true), span(4)]
        )]),
        "go to **[[Note]]** now"
    );
}

#[test]
fn unsafe_bracket_runs_stay_escaped_but_round_trip() {
    for text in [
        "[[a]](b)",
        "[[a_b]]",
        "[[a*b]]",
        "[[a\\b]]",
        "[[a&amp;b]]",
        "[[a<b>c]]",
        "[[a`b]]",
        "[[a]",
        "[a]]",
        "[[[a]]",
        "[[a [[b]] c]]",
    ] {
        assert_round_trips(&[p(Body, text)]);
    }
    assert_round_trips(&[with_spans(
        p(Body, "[[Note]]"),
        vec![styled(4, |s| s.bold = true), span(4)],
    )]);
    assert_eq!(assert_round_trips(&[p(Body, "[a](b)")]), "\\[a]\\(b)");
    assert_eq!(assert_round_trips(&[p(Body, "[a][b]")]), "\\[a][b]");
    assert_eq!(
        assert_round_trips(&[p(Body, "[a]: https://example.com")]),
        "\\[a]: https://example.com"
    );
}

#[test]
fn obsidian_callouts_tags_highlights_and_footnotes_keep_their_notation() {
    assert_eq!(
        assert_round_trips(&[quoted(Body, "[!NOTE] Worth knowing", 1)]),
        "> [!NOTE] Worth knowing"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "[!TIP]- Folded", 1)]),
        "> [!TIP]- Folded"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "[!WARNING]", 1), quoted(Body, "body line", 1)]),
        "> [!WARNING]\n> body line"
    );
    assert_eq!(assert_round_trips(&[p(Body, "#project")]), "#project");
    assert_eq!(
        assert_round_trips(&[p(Body, "#work/urgent and #done")]),
        "#work/urgent and #done"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "intro"), p(Body, "#project")]),
        "intro\n#project"
    );
    assert_eq!(assert_round_trips(&[p(BulletList, "#project")]), "- #project");
    assert_eq!(
        assert_round_trips(&[p(Body, "==highlight== and more")]),
        "==highlight== and more"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "intro"), p(Body, "==highlight==")]),
        "intro\n==highlight=="
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "See [^1] for the details")]),
        "See [^1] for the details"
    );
    assert_eq!(
        assert_round_trips(&[p(Body, "See ^[an inline note] there")]),
        "See ^[an inline note] there"
    );
    assert_eq!(assert_round_trips(&[p(Body, "[Shinkansen]")]), "[Shinkansen]");
}

#[test]
fn markup_that_really_is_markdown_stays_escaped() {
    for (text, want) in [
        ("# Real heading", "\\# Real heading"),
        ("#", "\\#"),
        ("#\tTabbed", "\\#\tTabbed"),
        ("---", "\\---"),
        ("&amp;", "\\&amp;"),
        ("<div>x</div>", "\\<div>x\\</div>"),
    ] {
        assert_eq!(assert_round_trips(&[p(Body, text)]), want);
    }
    assert_eq!(assert_round_trips(&[p(Body, "intro"), p(Body, "===")]), "intro\n\\===");
}

#[test]
fn the_obsidian_spelling_is_dropped_whole_when_it_would_change_the_document() {
    assert_ne!(assert_round_trips(&[p(BulletList, "[ ]")]), "- [ ]");
    assert_eq!(
        assert_round_trips(&[p(BulletList, "[ ]"), p(Body, "[[Note]]")]),
        "- \\[ ]\n\n\\[\\[Note]]"
    );
}

#[test]
fn urls_that_could_open_inline_constructs_fall_back_to_escaped_text() {
    for text in [
        "https://en.wikipedia.org/wiki/A_B",
        "https://example.com/?a&amp;b",
        "foohttps://example.com",
        "https://example.com/*star*",
    ] {
        assert_round_trips(&[p(Body, text)]);
    }
}

/// Issue #21: markdown-rs ends lines at `\r\n`, `\r` and `\n`, so a lone
/// `\r` must not leave a node pointing past the parser's line table.
/// markdown-rs reads a web address in a link's text as a link of its own
/// (micromark doesn't), and ends it on the backslash that escapes the
/// character after the address: that backslash is no text of the note's.
#[test]
fn a_link_text_holding_an_address_before_an_escaped_character_round_trips() {
    for text in [
        "http://x.co/a_",
        "see http://x.co/a* now",
        "www.x.co~",
        "http://x.co/a<b",
        "http://x.co/a\\",
        "http://x.co/a\\_",
    ] {
        let length = text.encode_utf16().count();
        let link = styled(length, |style| style.link = "https://example.com/".into());
        let rendered = assert_round_trips(&[with_spans(p(Body, text), vec![link])]);
        assert!(
            rendered.contains('\\'),
            "{text:?} is written with an escape: {rendered:?}"
        );
    }
    let parsed = parse_note_markdown("[http://x.co/a\\_](https://example.com/)").unwrap();
    assert_eq!(parsed.text, "http://x.co/a_");
}

#[test]
fn carriage_returns_never_panic_the_parser() {
    for input in ["```\n\r", "```\na\rb\n```", "```\r```", "# h\rx", "- a\r- b", "a\r\rb"] {
        let parsed = std::panic::catch_unwind(|| parse_note_markdown(input));
        assert!(parsed.is_ok(), "{input:?} panicked");
    }
}

#[test]
fn carriage_returns_keep_their_round_trips() {
    assert_eq!(parse_note_markdown("a\rb").unwrap().text, "a\rb");
    assert_eq!(parse_note_markdown("> a\r> b").unwrap().text, "a\rb");
    assert_eq!(parse_note_markdown("a\r\nb").unwrap().text, "a\r\nb");
    assert_eq!(parse_note_markdown("```\na\rb\n```").unwrap().text, "a\rb");
    assert_eq!(parse_note_markdown("# h\rx").unwrap().text, "h\nx");
    assert_round_trips(&[p(Body, "a\rb")]);
    assert_round_trips(&[p(Monospaced, "a\rb")]);
}

#[test]
fn deeply_nested_markdown_is_refused_before_it_is_parsed() {
    let indented_items: String = (0..5_000).map(|i| format!("{}- x\n", "  ".repeat(i))).collect();
    for input in [
        ">".repeat(100_000) + " x",
        "- ".repeat(10_000) + "x",
        "1. ".repeat(10_000) + "x",
        indented_items,
    ] {
        let reason = parse_note_markdown(&input).unwrap_err().reason;
        assert_eq!(
            reason,
            "the note nests quotes or lists more than 16 levels deep, deeper than this tool can represent"
        );
    }
    let quoted_list = format!("{}- x", "> ".repeat(16));
    assert_eq!(
        parse_note_markdown(&quoted_list).unwrap().paragraphs[0].block_quote_level,
        16
    );
    // The deepest nesting a note decodes with stays parseable.
    let ladder: Vec<FormatParagraph> = (0..=16)
        .map(|indent| FormatParagraph {
            block_quote_level: 16,
            ..indented(NumberedList, "deep", indent)
        })
        .collect();
    assert_round_trips(&ladder);
}

/// A note decodes with at most 16 quote levels and 16 list indents, so a
/// local note nesting deeper is refused when it is parsed, not when the
/// push re-decodes what it uploaded.
#[test]
fn markdown_nesting_past_what_a_note_decodes_with_is_refused() {
    let ladder = |levels: usize| -> String { (0..levels).map(|i| format!("{}- x\n", "  ".repeat(i))).collect() };
    for input in [
        "> ".repeat(17) + "x",
        format!("{}- x", "> ".repeat(17)),
        ladder(18),
        format!(
            "{}{}",
            "> ".repeat(16),
            ladder(18).replace('\n', &format!("\n{}", "> ".repeat(16)))
        ),
    ] {
        let reason = parse_note_markdown(&input).unwrap_err().reason;
        assert_eq!(
            reason, "the note nests quotes or lists more than 16 levels deep, deeper than this tool can represent",
            "{input:?}"
        );
    }
    let deepest = parse_note_markdown(&ladder(17)).unwrap();
    assert_eq!(deepest.paragraphs.iter().map(|p| p.indent).max(), Some(16));
    assert_eq!(
        parse_note_markdown(&("> ".repeat(16) + "x")).unwrap().paragraphs[0].block_quote_level,
        16
    );
}

/// Nesting no line-start scan sees: emphasis nests a node per level, and
/// 1000 levels of `**` used to overflow the stack and abort the whole push
/// or pull.
#[test]
fn deeply_nested_inline_markdown_is_refused_without_overflowing_the_stack() {
    let too_deep = "the note nests its Markdown (quotes, lists, emphasis or links) too deeply to read";
    for input in [
        "**".repeat(5_000) + "x" + &"**".repeat(5_000),
        format!("{}{}x{}", "> - ".repeat(16), "**".repeat(100), "**".repeat(100)),
    ] {
        let reason = parse_note_markdown(&input).err().map(|refusal| refusal.reason);
        assert_eq!(reason.as_deref(), Some(too_deep), "{}...", &input[..40]);
    }
    // Delimiters that don't pair up stay text, however many there are.
    assert_eq!(parse_note_markdown(&"*a ".repeat(5_000)).unwrap().paragraphs.len(), 1);
    let styled = parse_note_markdown("> - ***~~[a <u>b</u>](https://example.com)~~***").unwrap();
    assert_eq!(styled.text, "a b");
}

/// The line-start scan reads fenced code as code: a code block whose lines
/// start with `>` or list markers is not nesting.
#[test]
fn markers_inside_a_fenced_code_block_are_not_nesting() {
    for fence in ["```", "~~~", "  ````"] {
        let close = fence.trim_start();
        let input = format!("{fence}\n{}x\n{}x\n{close}\nafter", "> ".repeat(100), "- ".repeat(100));
        let parsed = parse_note_markdown(&input).unwrap_or_else(|r| panic!("{fence}: {}", r.reason));
        assert!(parsed.text.ends_with("after"), "{:?}", parsed.text);
    }
    // A closing fence ends it: nesting after it is still seen.
    let after_close = format!("```\ncode\n```\n{}x", ">".repeat(100_000));
    assert!(parse_note_markdown(&after_close).is_err());
    // A fence inside a quote ends with the quote, so it hides nothing.
    let quoted = format!("> ```\n{}x", ">".repeat(100_000));
    assert!(parse_note_markdown(&quoted).is_err());
}
