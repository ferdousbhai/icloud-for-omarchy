//! Port of icloud-md's `src/notes/noteMarkdownRoundTrip.test.ts`.

use icloud_notes_sync::doc::format::{FormatParagraph, InlineSpan, InlineStyle, ParagraphKind};
use icloud_notes_sync::md::parse::parse_note_markdown;
use icloud_notes_sync::md::projection::formats_round_trip_equal;
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

#[test]
fn a_body_line_after_a_list_or_quote_is_not_lazily_absorbed() {
    assert_eq!(
        assert_round_trips(&[p(BulletList, "item"), p(Body, "directly after")]),
        "- item\ndirectly after"
    );
    assert_eq!(
        assert_round_trips(&[quoted(Body, "quoted", 1), p(Body, "outside")]),
        "> quoted\noutside"
    );
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
fn parser_splits_lazy_continuations_into_body_paragraphs() {
    let result = parse_note_markdown("> quoted\nlazy line").unwrap();
    let got: Vec<_> = result
        .paragraphs
        .iter()
        .map(|q| (q.kind, q.block_quote_level, q.text.as_str()))
        .collect();
    assert_eq!(got, vec![(Body, 1, "quoted"), (Body, 0, "lazy line")]);
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
        "- \\[ ]\nflow.ts"
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
        "- \\[ ]\n\\[\\[Note]]"
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
