//! Seeded property tests of the note ↔ Markdown round trip: generated notes
//! (every paragraph kind, nesting, inline styles, links, embeds of every
//! kind and in every position, frontmatter this tool does not own, titles of
//! every shape) and generated edits to their Markdown.
//!
//! The property is safety, not perfection. For every case either
//!
//! - (a) the round trip is exact: the note pulled, the Markdown read back
//!   is the note (document → Markdown → document), and a Markdown file
//!   pushed then pulled reads back as itself (Markdown → document →
//!   Markdown); an edit to the Markdown changes exactly that in the note;
//! - or (b) the engine says so itself: the note is read-only (with a
//!   reason), or the push is refused (with a reason) and sends nothing;
//!
//! never (c): a push that sends a document other than what the Markdown
//! says, an edit that changes the note elsewhere too, or a pull that writes
//! Markdown reading back as something else without a word. A pulled file
//! that doesn't read back as its note counts as (b) when every edit to it
//! is refused: that is how the held-back cases show up (a table right below
//! a list item or quote; strikethrough or an emoji next to a letter, which
//! makes the note read-only).
//!
//! Four suites: `codec_*` (the codec and the Markdown in memory, many
//! cases), `markdown_*` (generated Markdown, as a new note's file),
//! `vault_*` (clone, push, pull and a fresh clone against an in-memory
//! iCloud, with attachments, tables and frontmatter), and `splices_*` (a
//! text edit made in a document is exactly that edit, keeping what the
//! characters it leaves alone carry). Every case is drawn
//! from a seeded generator: a failure prints the seed and the case, shrunk
//! to a smaller one that still fails. `ICLOUD_NOTES_SYNC_PROPERTY_SEED` runs
//! one seed; `ICLOUD_NOTES_SYNC_PROPERTY_CASES` runs more (or fewer) cases;
//! `ICLOUD_NOTES_SYNC_PROPERTY_SURVEY=1` lists every failing seed instead.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use icloud_notes_sync::cloudkit::{CkError, CloudKitRecord, Database, Transport};
use icloud_notes_sync::cmd::clone::{CloneOptions, run_clone_with};
use icloud_notes_sync::cmd::plan::PlanResolution;
use icloud_notes_sync::cmd::pull::run_pull_with;
use icloud_notes_sync::cmd::push::{PushOptions, PushResult, restore_stripped_title, run_push_with};
use icloud_notes_sync::cmd::remote::{Connector, FnConnector, Remote};
use icloud_notes_sync::cmd::{Error, NoProgress};
use icloud_notes_sync::doc::decode::{ClassifyOptions, DecodedNote, NoteDecodeResult, classify_note_record};
use icloud_notes_sync::doc::document::{
    ApplyTextEditOptions, NoteDocument, Splice, apply_text_edit, build_initial_note_document, compute_splice,
    compute_splices, encode_note_document, parse_note_document,
};
use icloud_notes_sync::doc::embeds::{EmbedSlot, OBJECT_REPLACEMENT_CHARACTER, plan_embed_representations};
use icloud_notes_sync::doc::encode::derive_note_title;
use icloud_notes_sync::doc::format::{
    FormatParagraph, InlineStyle, ParagraphKind, decode_note_format, formats_round_trip_equal, note_holds_format,
    paragraph_projections_equal, trim_trailing_whitespace,
};
use icloud_notes_sync::doc::proto::topotext::{AttachmentInfo, AttributeRun, Color, Font, ParagraphStyle, Todo};
use icloud_notes_sync::doc::reconcile::reconcile_note_format_with;
use icloud_notes_sync::doc::table_edit::apply_table_edit_with;
use icloud_notes_sync::doc::tables::{encode_table_document, grid_from_table_document, parse_table_document};
use icloud_notes_sync::doc::text::{compress_note_document, decode_note_string, decompress_note_document};
use icloud_notes_sync::js::{base64_decode, base64_encode, len16, posix, utf16};
use icloud_notes_sync::md::frontmatter::{SplitOptions, split_frontmatter};
use icloud_notes_sync::md::parse::{ParsedNoteMarkdown, parse_note_markdown};
use icloud_notes_sync::vault::state::{Account, CloneState, TitleMode, read_clone_state};
use serde_json::{Value, json};

use ParagraphKind::*;

// --- a seeded generator ----------------------------------------------------------

/// splitmix64: small, fast and the same everywhere.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn range(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn bytes16(&mut self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.next().to_le_bytes());
        out[8..].copy_from_slice(&self.next().to_le_bytes());
        out
    }
}

/// How many cases a suite runs, and from which seeds.
fn seeds(suite: &str, default_cases: u64) -> Vec<u64> {
    if let Some(seed) = std::env::var("ICLOUD_NOTES_SYNC_PROPERTY_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        return vec![seed];
    }
    let cases = std::env::var("ICLOUD_NOTES_SYNC_PROPERTY_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_cases);
    let base = suite.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    (0..cases)
        .map(|i| base.wrapping_add(i.wrapping_mul(0x9e37_79b9)))
        .collect()
}

/// Text pieces that stress the renderer's escaping and the parser: Markdown
/// syntax, entities, autolink shapes, HTML, astral characters and combining
/// marks (which sit on style boundaries when a piece is styled).
const WORDS: &[&str] = &[
    "word",
    "Hello",
    "a",
    "x1",
    "foo_bar",
    "snake_case_name",
    "*star*",
    "**",
    "_u_",
    "~~del~~",
    "~",
    "`code`",
    "``",
    "[x]",
    "[ ]",
    "[link](http://e.x)",
    "[[Wiki]]",
    "#tag",
    "# not",
    "1.",
    "2)",
    "- ",
    "+ ",
    "> ",
    ">",
    "|",
    "a|b",
    "\\",
    "\\*",
    "&amp;",
    "&#35;",
    "&",
    "<u>",
    "</u>",
    "<b>",
    "<br>",
    "https://example.com/a_b?c=d&e=(f)",
    "http://x.y",
    "www.example.com",
    "user@example.com",
    "e\u{301}",
    "\u{301}",
    "😀",
    "👩‍👩‍👧",
    "𝔸𝔹",
    "中文",
    "العربية",
    " ",
    "  ",
    "\t",
    "!",
    "![",
    "](",
    ")",
    "(",
    "=",
    "==",
    "---",
    "***",
    ":",
    "\u{a0}",
    "'",
    "\"",
    "$",
    "%",
    "^",
    "{",
    "}",
    "1",
    "10.",
    "\u{200b}",
    "\u{2060}",
    "<apple-embed>",
    "[^1]",
    "<!-- c -->",
    "<https://a.b>",
    "*",
    "_",
    "a_",
    "_a",
    "**b**",
    "x*y*z",
    "~x~",
    "\\[",
    "&lt;",
    "&nbsp;",
    "\u{fe0f}",
    "👍🏽",
    "Ω",
    "ﬁ",
    "-",
    "+",
    "#",
    "1)",
    "0.",
    "99999.",
    "*)",
    "[",
    "]",
    "]:",
    "[a]: b",
    "`",
    "```",
    "~~~",
    "<",
    "</",
    "<a href=x>",
];

const URLS: &[&str] = &[
    "https://example.com",
    "https://example.com/a b",
    "http://x.y/(p)",
    "http://x.y/p)",
    "mailto:a@b.c",
    "notes://x",
    "https://ex.com/<x>",
    "https://例え.jp/パス",
    "a",
    "#frag",
    "https://e.x/a_b*c*",
    "https://e.x/\\",
    "https://e.x/\"q\"",
    "javascript:alert(1)",
    "https://e.x/%20",
    "www.example.com",
];

fn words(rng: &mut Rng, low: usize, high: usize) -> String {
    let count = rng.range(low, high);
    let mut text = String::new();
    for i in 0..count {
        if i > 0 && rng.chance(70) {
            text.push(' ');
        }
        let mut word = *rng.pick(WORDS);
        while TAME.get() && word.chars().any(|c| c as u32 > 0xFFFF) {
            word = rng.pick(WORDS);
        }
        text.push_str(word);
    }
    text
}

thread_local! {
    /// Most notes are drawn tame: no astral characters and no strikethrough,
    /// whose spelling next to a letter makes nearly every note that has them
    /// read-only (the held-back attention-encoding change), so that most
    /// cases reach the push.
    static TAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What a U+FFFC stands for.
#[derive(Debug, Clone, PartialEq)]
enum Embed {
    /// A file attachment, its name as iCloud has it.
    File { name: String, uti: &'static str },
    /// A table, as rows of cells.
    Table(Vec<Vec<String>>),
    /// An attachment this tool has no file or table for (a drawing).
    Drawing,
    /// A U+FFFC with no attachment, or one with a type and no identifier.
    Bare(Option<&'static str>),
}

#[derive(Debug, Clone, PartialEq)]
struct Piece {
    text: String,
    style: InlineStyle,
    embed: Option<Embed>,
    /// What this tool's Markdown never shows of it, which an edit elsewhere
    /// must keep ([`hidden_attributes`]): 0 nothing, else which.
    hidden: u8,
}

#[derive(Debug, Clone, PartialEq)]
struct GenParagraph {
    kind: ParagraphKind,
    indent: i32,
    quote: u32,
    done: bool,
    start_number: Option<u32>,
    /// The style left unset (`None`) where Body is meant.
    implicit_body: bool,
    pieces: Vec<Piece>,
}

#[derive(Debug, Clone, PartialEq)]
struct GenNote {
    paragraphs: Vec<GenParagraph>,
    /// Attribute runs split where nothing changes (Notes writes such).
    split_runs: bool,
    filename_mode: bool,
}

const FILE_NAMES: &[(&str, &str)] = &[
    ("photo.png", "public.png"),
    ("Scan 1.jpeg", "public.jpeg"),
    ("a(b).pdf", "com.adobe.pdf"),
    ("x)y.pdf", "com.adobe.pdf"),
    ("[draft].pdf", "com.adobe.pdf"),
    ("100%.pdf", "com.adobe.pdf"),
    ("naïve résumé.pdf", "com.adobe.pdf"),
    ("report #2.pdf", "com.adobe.pdf"),
    ("a b  c.txt", "public.plain-text"),
    ("😀.png", "public.png"),
    ("anim.gif", "com.compuserve.gif"),
    ("photo.png", "public.png"),
    ("my_file*.pdf", "com.adobe.pdf"),
    ("back\\slash.pdf", "com.adobe.pdf"),
    ("Call.m4a", "com.apple.m4a-audio"),
];

const CELLS: &[&str] = &[
    "",
    "a",
    "Cell",
    "|",
    "a|b",
    "\\|",
    "**b**",
    "[l](https://u)",
    "`c`",
    " lead",
    "trail ",
    "two\nlines",
    "<br>",
    "&amp;",
    "😀",
    "e\u{301}",
    "*",
    "_x_",
    "~~s~~",
    "https://e.x/a_b",
    "www.e.x",
    "#",
    "- item",
    "> q",
    "1.",
    "\\",
];

fn gen_grid(rng: &mut Rng) -> Vec<Vec<String>> {
    let rows = rng.range(1, 4);
    let columns = rng.range(1, 3);
    (0..rows)
        .map(|_| (0..columns).map(|_| rng.pick(CELLS).to_string()).collect())
        .collect()
}

fn gen_embed(rng: &mut Rng) -> Embed {
    match rng.below(10) {
        0..=4 => {
            let (name, uti) = *rng.pick(FILE_NAMES);
            Embed::File {
                name: name.to_string(),
                uti,
            }
        }
        5..=7 => Embed::Table(gen_grid(rng)),
        8 => Embed::Drawing,
        _ => Embed::Bare(if rng.chance(50) { None } else { Some("com.apple.paper") }),
    }
}

fn gen_style(rng: &mut Rng, text: &str) -> InlineStyle {
    let mut style = InlineStyle::PLAIN;
    if rng.chance(55) {
        return style;
    }
    style.bold = rng.chance(35);
    style.italic = rng.chance(30);
    style.strikethrough = !TAME.get() && rng.chance(15);
    style.underline = rng.chance(15);
    if rng.chance(20) {
        style.link = if rng.chance(25) && !text.is_empty() {
            text.to_string()
        } else {
            rng.pick(URLS).to_string()
        };
    }
    style
}

fn gen_paragraph(rng: &mut Rng, allow_embeds: bool) -> GenParagraph {
    let kind = *rng.pick(&[
        Title,
        Heading,
        Subheading,
        Body,
        Body,
        Body,
        Monospaced,
        BulletList,
        DashList,
        NumberedList,
        TodoList,
        TodoList,
    ]);
    let indent = if kind.is_list() {
        *rng.pick(&[0, 0, 0, 1, 1, 2, 3])
    } else if rng.chance(5) {
        1
    } else {
        0
    };
    let quote = *rng.pick(&[0, 0, 0, 0, 1, 1, 2]);
    let mut pieces = Vec::new();
    let count = if rng.chance(8) { 0 } else { rng.range(1, 6) };
    for _ in 0..count {
        if allow_embeds && rng.chance(12) {
            pieces.push(Piece {
                text: OBJECT_REPLACEMENT_CHARACTER.to_string(),
                style: if rng.chance(20) {
                    gen_style(rng, "")
                } else {
                    InlineStyle::PLAIN
                },
                embed: Some(gen_embed(rng)),
                hidden: 0,
            });
            continue;
        }
        let mut text = words(rng, 1, 4);
        if rng.chance(3) {
            text = text.repeat(rng.range(10, 40));
        }
        let style = if kind == Monospaced && rng.chance(70) {
            InlineStyle::PLAIN
        } else {
            gen_style(rng, &text)
        };
        pieces.push(Piece {
            text,
            style,
            embed: None,
            hidden: if rng.chance(15) { rng.range(1, 4) as u8 } else { 0 },
        });
    }
    GenParagraph {
        kind,
        indent,
        quote,
        done: rng.chance(40),
        start_number: match rng.below(10) {
            0 => Some(0),
            1 => Some(rng.range(2, 30) as u32),
            2 if kind == NumberedList => Some(1),
            3 if kind == NumberedList => Some(rng.range(2, 12) as u32),
            _ => None,
        },
        implicit_body: kind == Body && rng.chance(50),
        pieces,
    }
}

fn gen_note(rng: &mut Rng, allow_embeds: bool) -> GenNote {
    TAME.set(rng.chance(65));
    let count = rng.range(1, 9);
    let mut paragraphs: Vec<GenParagraph> = (0..count).map(|_| gen_paragraph(rng, allow_embeds)).collect();
    // Mostly as Notes nests: a list item at most one level below the one
    // before it (an item nested under nothing is the pending orphan-indent
    // decision), a table on a plain line of its own, a list item with text.
    for i in 0..paragraphs.len() {
        let below = match i.checked_sub(1).map(|p| &paragraphs[p]) {
            Some(previous) if previous.kind.is_list() && previous.quote == paragraphs[i].quote => previous.indent + 1,
            _ => 0,
        };
        let p = &mut paragraphs[i];
        if p.kind.is_list() && p.indent > below && rng.chance(90) {
            p.indent = below;
        }
        if p.pieces
            .iter()
            .any(|piece| matches!(piece.embed, Some(Embed::Table(_))))
            && rng.chance(80)
        {
            p.pieces.retain(|piece| matches!(piece.embed, Some(Embed::Table(_))));
            p.pieces.truncate(1);
            p.pieces[0].style = InlineStyle::PLAIN;
            p.kind = Body;
            p.quote = 0;
        }
        if p.kind.is_list() && p.pieces.is_empty() && rng.chance(70) {
            p.pieces.push(Piece {
                text: "item".into(),
                style: InlineStyle::PLAIN,
                embed: None,
                hidden: 0,
            });
        }
    }
    // A title of some shape first, most of the time: Apple's notes start with one.
    if rng.chance(75) {
        let title = &mut paragraphs[0];
        if rng.chance(70) {
            title.kind = Title;
        }
        if rng.chance(30) {
            let shape = *rng.pick(&[
                "Plain title",
                "a/b: c*d?",
                ".hidden",
                "ends with dot.",
                "CON",
                "  spaced  ",
                "x\u{2044}y",
                "😀 emoji",
                "e\u{301}cole",
                "#tag",
                "> quoted",
                "- dash",
                "1. one",
                "---",
                "[x] done",
                "a\tb",
                "",
                "very long title that goes on and on well past the sixty characters a file name may carry",
                // Long titles stay in the frontmatter, folded over lines:
                // quoted, with no space to fold at, or a no-break space at one.
                "\"q\": 😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀",
                "a: b # c 'd' 👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧👩\u{200d}👧",
                "words and words and words and words and words and more words and \u{a0}words \u{a0} at the fold",
            ]);
            title.pieces = vec![Piece {
                text: shape.to_string(),
                style: InlineStyle::PLAIN,
                embed: None,
                hidden: 0,
            }];
        }
    }
    GenNote {
        paragraphs,
        split_runs: rng.chance(30),
        filename_mode: rng.chance(40),
    }
}

// --- the note as iCloud holds it ----------------------------------------------------

fn paragraph_text(p: &GenParagraph) -> String {
    p.pieces.iter().map(|piece| piece.text.as_str()).collect()
}

fn note_text(note: &GenNote) -> String {
    note.paragraphs
        .iter()
        .map(paragraph_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn embeds_of(note: &GenNote) -> Vec<Embed> {
    note.paragraphs
        .iter()
        .flat_map(|p| p.pieces.iter().filter_map(|piece| piece.embed.clone()))
        .collect()
}

fn embed_id(index: usize) -> String {
    format!("A0000000-0000-4000-8000-{index:012X}")
}

fn media_id(index: usize) -> String {
    format!("B0000000-0000-4000-8000-{index:012X}")
}

fn embed_uti(embed: &Embed) -> Option<&'static str> {
    match embed {
        Embed::File { uti, .. } => Some(uti),
        Embed::Table(_) => Some("com.apple.notes.table"),
        Embed::Drawing => Some("com.apple.drawing.2"),
        Embed::Bare(uti) => *uti,
    }
}

/// The note's attribute runs, written the way Notes writes them: each
/// paragraph's style on all of its runs, a line break in its line's last
/// run, an embed in a run of its own.
fn attribute_runs(note: &GenNote, rng: &mut Rng) -> Vec<AttributeRun> {
    let mut runs: Vec<AttributeRun> = Vec::new();
    let mut embed_index = 0;
    let last = note.paragraphs.len() - 1;
    for (index, p) in note.paragraphs.iter().enumerate() {
        let style = ParagraphStyle {
            style: (!p.implicit_body).then(|| p.kind.style()),
            indent: (p.indent != 0 || rng.chance(30)).then_some(p.indent),
            block_quote_level: (p.quote != 0 || rng.chance(30)).then_some(p.quote),
            todo: (p.kind == TodoList).then(|| Todo {
                todo_uuid: Some(rng.bytes16().to_vec()),
                done: Some(u32::from(p.done)),
                ..Default::default()
            }),
            starting_list_item_number: p.start_number,
            ..Default::default()
        };
        let first_run = runs.len();
        for piece in &p.pieces {
            let mut run = AttributeRun {
                length: Some(len16(&piece.text) as u32),
                paragraph_style: Some(style.clone()),
                ..Default::default()
            };
            let hints = u32::from(piece.style.bold) | (u32::from(piece.style.italic) << 1);
            run.font_hints = (hints != 0).then_some(hints);
            run.underline = piece.style.underline.then_some(1);
            run.strikethrough = piece.style.strikethrough.then_some(1);
            run.link = (!piece.style.link.is_empty()).then(|| piece.style.link.clone());
            match piece.hidden {
                1 => run.emphasis = Some(rng.range(1, 5) as u32),
                2 => run.superscript = Some(*rng.pick(&[1, -1])),
                3 => {
                    run.color = Some(Color {
                        red: Some(1.0f32.to_bits()),
                        green: Some(0.25f32.to_bits()),
                        blue: Some(0.0f32.to_bits()),
                        alpha: Some(1.0f32.to_bits()),
                        ..Default::default()
                    })
                }
                4 => {
                    run.font = Some(Font {
                        point_size: Some(24.0f32.to_bits()),
                        ..Default::default()
                    })
                }
                _ => {}
            }
            if let Some(embed) = &piece.embed {
                run.attachment_info = Some(AttachmentInfo {
                    attachment_identifier: (!matches!(embed, Embed::Bare(_))).then(|| embed_id(embed_index)),
                    type_uti: embed_uti(embed).map(str::to_string),
                    ..Default::default()
                });
                embed_index += 1;
                runs.push(run);
                continue;
            }
            if note.split_runs && run.len() > 1 && rng.chance(50) {
                let head = rng.range(1, run.len() as usize - 1);
                // Never inside a surrogate pair: a run boundary is a character's.
                let units: Vec<u16> = piece.text.encode_utf16().collect();
                if !(0xDC00..0xE000).contains(&units[head]) {
                    let mut tail = run.clone();
                    tail.length = Some(run.len() - head as u32);
                    run.length = Some(head as u32);
                    runs.push(run);
                    runs.push(tail);
                    continue;
                }
            }
            runs.push(run);
        }
        if index == last {
            continue;
        }
        let newline_joins = runs.len() > first_run && runs.last().is_some_and(|r| r.attachment_info.is_none());
        if newline_joins {
            let run = runs.last_mut().unwrap();
            run.length = Some(run.len() + 1);
        } else {
            runs.push(AttributeRun {
                length: Some(1),
                paragraph_style: Some(style),
                ..Default::default()
            });
        }
    }
    runs
}

fn replica() -> [u8; 16] {
    *b"property-replica"
}

/// The note's document, or `None` for a note with no text (Notes has no such).
fn build_document(note: &GenNote, rng: &mut Rng) -> Option<NoteDocument> {
    let text = note_text(note);
    if text.is_empty() {
        return None;
    }
    let mut doc = build_initial_note_document(&text, &rng.bytes16()).ok()?;
    doc.attribute_runs = attribute_runs(note, rng);
    Some(doc)
}

fn document_base64(doc: &NoteDocument) -> String {
    base64_encode(&compress_note_document(&encode_note_document(doc).unwrap()))
}

fn note_record(record_name: &str, doc: &NoteDocument) -> CloudKitRecord {
    record_of(&record_json(
        record_name,
        "Note",
        json!({
            "TitleEncrypted": { "value": base64_encode(derive_note_title(&doc.text).as_bytes()), "type": "ENCRYPTED_BYTES" },
            "TextDataEncrypted": { "value": document_base64(doc), "type": "ENCRYPTED_BYTES" },
        }),
    ))
}

fn title_mode(filename_mode: bool) -> TitleMode {
    if filename_mode {
        TitleMode::Filename
    } else {
        TitleMode::default()
    }
}

fn format_of(doc: &NoteDocument) -> Vec<FormatParagraph> {
    decode_note_format(&doc.text, &doc.attribute_runs).expect("a generated note decodes")
}

fn decode_sent(base64: &str) -> (String, Vec<FormatParagraph>) {
    let s = decode_note_string(&base64_decode(base64)).unwrap();
    let format = decode_note_format(s.string(), &s.attribute_run).unwrap();
    (s.string().to_string(), format)
}

// --- checks shared by the suites ----------------------------------------------------

/// What a case came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Outcome {
    /// (a) Everything round-tripped exactly.
    Exact,
    /// (b) The engine marked the note read-only, with a reason.
    ReadOnly,
    /// (b) The engine refused the push (or would, as push checks what it
    /// built before sending it), with a reason.
    Refused,
    /// Not a case: what was generated is no note at all.
    Skipped,
}

type Checked = Result<Outcome, String>;

/// Each UTF-16 unit's style, but for bold, italic and strikethrough on
/// whitespace, which nothing shows (Markdown can't even write it).
fn unit_styles(p: &FormatParagraph) -> Vec<InlineStyle> {
    let units: Vec<u16> = p.text.encode_utf16().collect();
    let mut styles = Vec::new();
    for span in &p.spans {
        for _ in 0..span.length {
            let mut style = span.style.clone();
            if units
                .get(styles.len())
                .is_some_and(|&u| icloud_notes_sync::js::is_whitespace16(u))
            {
                style.bold = false;
                style.italic = false;
                style.strikethrough = false;
            }
            styles.push(style);
        }
    }
    styles
}

fn same_paragraph(a: &FormatParagraph, b: &FormatParagraph) -> bool {
    let (a, b) = (trim_trailing_whitespace(a), trim_trailing_whitespace(b));
    a.kind == b.kind
        && a.indent == b.indent
        && a.block_quote_level == b.block_quote_level
        && a.done == b.done
        && a.start_number == b.start_number
        && a.text == b.text
        && unit_styles(&a) == unit_styles(&b)
}

/// An edit changes the note only where it changed the Markdown: every
/// paragraph the edit left alone (same text, same meaning, where it was) is
/// exactly as it was, down to what the Markdown doesn't show (a dash list
/// stays a dash list, a link the same as its text stays a link, a code
/// block's styling stays). Paragraphs are matched by a longest common
/// subsequence of their texts. Trailing whitespace, which no Markdown line
/// keeps, is the one thing allowed to go.
fn untouched_paragraphs_kept(before: &[FormatParagraph], after: &[FormatParagraph]) -> Result<(), String> {
    let (n, m) = (before.len(), after.len());
    let text = |p: &FormatParagraph| trim_trailing_whitespace(p).text;
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if text(&before[i]) == text(&after[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if text(&before[i]) == text(&after[j]) {
            let previous_before = i.checked_sub(1).map(|p| &before[p]);
            let previous_after = j.checked_sub(1).map(|p| &after[p]);
            // Lines with the same text (a line duplicated, one of several
            // empty lines removed): which copy is the one kept is a guess.
            let kept_somewhere = || {
                let copies = |side: &[FormatParagraph]| side.iter().filter(|p| text(p) == text(&before[i])).count();
                after.iter().any(|a| same_paragraph(&before[i], a)) || copies(before) > 1 || copies(after) > 1
            };
            // A note's last line, empty but for whitespace, is an empty line
            // (`note_holds_format`).
            let empty_last = (i + 1 == n || j + 1 == m) && text(&before[i]).is_empty();
            if paragraph_projections_equal(&before[i], &after[j], previous_before, previous_after)
                && !same_paragraph(&before[i], &after[j])
                && !kept_somewhere()
                && !empty_last
            {
                return Err(format!(
                    "paragraph {i} was left alone by the edit but changed in the note:\n  before {:?}\n  after  {:?}",
                    before[i], after[j]
                ));
            }
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Ok(())
}

/// What `doc` carries on each UTF-16 unit from `start` on that this tool's
/// Markdown never shows (a highlight, superscript, a colour, a font size).
fn hidden_attributes(doc: &NoteDocument, start: usize, length: usize) -> Vec<String> {
    let mut units = Vec::new();
    for run in &doc.attribute_runs {
        let hidden = format!(
            "{:?} {:?} {:?} {:?}",
            run.emphasis,
            run.superscript,
            run.color,
            run.font.as_ref().and_then(|f| f.point_size)
        );
        units.extend(std::iter::repeat_n(hidden, run.len() as usize));
    }
    units.into_iter().skip(start).take(length).collect()
}

/// Every paragraph an edit left alone (its text, but for trailing
/// whitespace, matched as in [`untouched_paragraphs_kept`], and found once on
/// each side) keeps what the Markdown doesn't show of its text.
fn hidden_attributes_kept(
    before: (&NoteDocument, &[FormatParagraph]),
    after: (&NoteDocument, &[FormatParagraph]),
) -> Result<(), String> {
    let text = |p: &FormatParagraph| trim_trailing_whitespace(p).text;
    let (n, m) = (before.1.len(), after.1.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if text(&before.1[i]) == text(&after.1[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let once = |side: &[FormatParagraph], wanted: &str| side.iter().filter(|p| text(p) == wanted).count() == 1;
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        let (b, a) = (&before.1[i], &after.1[j]);
        if text(b) == text(a) {
            let length = len16(&text(b));
            if once(before.1, &text(b)) && once(after.1, &text(a)) {
                let (was, now) = (
                    hidden_attributes(before.0, b.start, length),
                    hidden_attributes(after.0, a.start, length),
                );
                if was != now {
                    return Err(format!(
                        "paragraph {i} was left alone by the edit but lost what Markdown doesn't show of it:\n  \
                         text {:?}\n  before {was:?}\n  after  {now:?}",
                        b.text
                    ));
                }
            }
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Ok(())
}

/// Whether Markdown that reads back as `back` holds the note `format`: the
/// same text, but for trailing whitespace, and the same formatting.
fn reads_back_as(format: &[FormatParagraph], back: &ParsedNoteMarkdown) -> bool {
    let trim = |paragraphs: &[FormatParagraph]| -> Vec<FormatParagraph> {
        paragraphs.iter().map(trim_trailing_whitespace).collect()
    };
    let text = |paragraphs: &[FormatParagraph]| {
        paragraphs
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (note, back) = (trim(format), trim(&back.paragraphs));
    text(&note) == text(&back) && formats_round_trip_equal(&note, &back)
}

/// `parsed` with the title paragraph a filename-as-title note keeps out of
/// its file put back, as push does.
fn with_title(classified: &DecodedNote, parsed: ParsedNoteMarkdown) -> Option<ParsedNoteMarkdown> {
    restore_stripped_title(classified, parsed, None).ok()
}

/// What `markdown` (embeds as U+FFFC) says the note is.
fn wanted(classified: &DecodedNote, markdown: &str) -> Option<ParsedNoteMarkdown> {
    with_title(classified, parse_note_markdown(markdown).ok()?)
}

fn reconcile(doc: &mut NoteDocument, desired: &[FormatParagraph]) -> Result<bool, String> {
    let mut n = 0u8;
    reconcile_note_format_with(doc, desired, &replica(), &mut || {
        n = n.wrapping_add(1);
        [n; 16]
    })
    .map_err(|e| e.to_string())?
}

/// What push does to a note's document for an edit, in memory: the text
/// edit (refused when it touches an embed), then the formatting. `Err` is a
/// refusal; the document sent is checked by the caller.
fn edit_document(doc: &NoteDocument, desired: &ParsedNoteMarkdown) -> Result<NoteDocument, String> {
    let touches_embed = compute_splices(&doc.text, &desired.text).iter().any(|splice| {
        icloud_notes_sync::js::slice16(&doc.text, splice.start, splice.start + splice.delete_length)
            .contains(OBJECT_REPLACEMENT_CHARACTER)
            || splice.insert_text.contains(OBJECT_REPLACEMENT_CHARACTER)
    });
    if touches_embed {
        return Err("the edit touches an embed".into());
    }
    let mut edited = parse_note_document(&encode_note_document(doc).unwrap()).unwrap();
    apply_text_edit(
        &mut edited,
        &desired.text,
        &ApplyTextEditOptions { replica_id: replica() },
    )
    .map_err(|e| e.to_string())?;
    reconcile(&mut edited, &desired.paragraphs)?;
    Ok(edited)
}

// --- Markdown edits -----------------------------------------------------------------

const LINES: &[&str] = &[
    "- {t}",
    "* {t}",
    "+ {t}",
    "1. {t}",
    "3. {t}",
    "1) {t}",
    "- [ ] {t}",
    "- [x] {t}",
    "- [ ]",
    "  - {t}",
    "    - {t}",
    "   1. {t}",
    "> {t}",
    "> > {t}",
    "> - {t}",
    ">",
    "## {t}",
    "# {t}",
    "### {t}",
    "#### {t}",
    "```",
    "    {t}",
    "{t}",
    "",
    "**{t}**",
    "_{t}_",
    "~~{t}~~",
    "<u>{t}</u>",
    "[{t}](https://e.x)",
    "| a | b |",
    "| --- | --- |",
    "---",
    "{t}  ",
    "\t{t}",
    "{t}\\",
    "=====",
    "<div>{t}</div>",
];

fn markdown_line(rng: &mut Rng) -> String {
    {
        let template = *rng.pick(LINES);
        template.replace("{t}", &words(rng, 1, 3))
    }
}

fn char_boundary(text: &str, rng: &mut Rng) -> usize {
    let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).chain([text.len()]).collect();
    *rng.pick(&boundaries)
}

/// One edit, as someone might make it in an editor: text typed or deleted
/// somewhere in a line, a line added, removed, duplicated or moved, a box
/// ticked, a word styled, a table cell changed. What it did, for the report.
fn edit_markdown(body: &str, rng: &mut Rng) -> (String, String) {
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    let at = rng.below(lines.len());
    let what = match rng.below(13) {
        0 | 1 => {
            let insert = words(rng, 1, 2);
            let cut = char_boundary(&lines[at], rng);
            lines[at].insert_str(cut, &insert);
            format!("typed {insert:?} into line {at}")
        }
        2 | 3 => {
            let line = markdown_line(rng);
            lines.insert(at + usize::from(rng.chance(50)), line.clone());
            format!("added the line {line:?} near line {at}")
        }
        4 => {
            let removed = lines.remove(at);
            if lines.is_empty() {
                lines.push(String::new());
            }
            format!("removed line {at} ({removed:?})")
        }
        5 => {
            let line = &mut lines[at];
            let (a, b) = (char_boundary(line, rng), char_boundary(line, rng));
            let (a, b) = (a.min(b), a.max(b));
            let gone = line[a..b].to_string();
            line.replace_range(a..b, "");
            format!("deleted {gone:?} from line {at}")
        }
        6 => {
            let line = &mut lines[at];
            let toggled = if line.contains("[ ]") {
                line.replacen("[ ]", "[x]", 1)
            } else {
                line.replacen("[x]", "[ ]", 1)
            };
            *line = toggled;
            format!("ticked or unticked line {at}")
        }
        7 => {
            let line = &mut lines[at];
            let (a, b) = (char_boundary(line, rng), char_boundary(line, rng));
            let (a, b) = (a.min(b), a.max(b));
            let (open, close) = *rng.pick(&[
                ("**", "**"),
                ("*", "*"),
                ("_", "_"),
                ("~~", "~~"),
                ("<u>", "</u>"),
                ("`", "`"),
            ]);
            line.insert_str(b, close);
            line.insert_str(a, open);
            format!("wrapped part of line {at} in {open}{close}")
        }
        8 => {
            let copy = lines[at].clone();
            lines.insert(at, copy);
            format!("duplicated line {at}")
        }
        9 if at + 1 < lines.len() => {
            lines.swap(at, at + 1);
            format!("swapped lines {at} and {}", at + 1)
        }
        10 => {
            let cut = char_boundary(&lines[at], rng);
            let tail = lines[at].split_off(cut);
            lines.insert(at + 1, tail);
            format!("broke line {at} in two at {cut}")
        }
        11 if at + 1 < lines.len() => {
            let next = lines.remove(at + 1);
            lines[at].push_str(&next);
            format!("joined lines {at} and {}", at + 1)
        }
        _ => {
            // A table cell, when there is a table: its text replaced.
            let rows: Vec<usize> = (0..lines.len())
                .filter(|&i| lines[i].starts_with('|') && !lines[i].contains("---"))
                .collect();
            if rows.is_empty() {
                lines[at].push_str(" x");
                format!("appended \" x\" to line {at}")
            } else {
                let row = *rng.pick(&rows);
                let cells: Vec<&str> = lines[row].split('|').collect();
                let cell = rng.range(1, (cells.len() - 1).max(1));
                let mut rebuilt: Vec<String> = cells.iter().map(|c| c.to_string()).collect();
                if cell < rebuilt.len() {
                    rebuilt[cell] = format!(" {} ", rng.pick(CELLS).replace('\n', " "));
                }
                lines[row] = rebuilt.join("|");
                format!("changed cell {cell} of table line {row}")
            }
        }
    };
    (lines.join("\n"), what)
}

/// Two lines swapped: which of them moved (its text deleted and typed again,
/// as in Notes) and which stayed is anyone's guess.
fn moves_lines(what: &str) -> bool {
    what.starts_with("swapped")
}

// --- the codec suite ----------------------------------------------------------------

/// A note through the codec and the Markdown, in memory: classified (read
/// only, or its Markdown reads back as it), its Markdown parsed and
/// reconciled into its document changing nothing, then an edit to that
/// Markdown made in its document: refused, or exactly that edit.
fn check_codec(note: &GenNote, seed: u64) -> Checked {
    let mut rng = Rng(seed ^ 0x5eed);
    let Some(doc) = build_document(note, &mut rng) else {
        return Ok(Outcome::Skipped);
    };
    let format = format_of(&doc);
    let options = ClassifyOptions {
        title_mode: title_mode(note.filename_mode),
    };
    let classified = match classify_note_record(&note_record("N", &doc), &options) {
        NoteDecodeResult::Ok(classified) => classified,
        other => return Err(format!("a note Notes could write isn't read at all: {other:?}")),
    };
    if !classified.publishable {
        if classified.unpublishable_reason.as_deref().is_none_or(str::is_empty) {
            return Err("read-only without a reason".into());
        }
        return Ok(Outcome::ReadOnly);
    }
    if classified.embed_slots.len() != embeds_of(note).len() {
        return Err(format!(
            "embed slots {:?} for {:?}",
            classified.embed_slots,
            embeds_of(note)
        ));
    }
    let markdown = &classified.markdown_text;
    let Some(back) = wanted(&classified, markdown) else {
        return Err(format!("the Markdown pull writes doesn't read back: {markdown:?}"));
    };
    if !reads_back_as(&format, &back) {
        return Err(format!(
            "the Markdown pull writes reads back as another note:\n  markdown {markdown:?}\n  note {format:?}\n  read back {:?}",
            back.paragraphs
        ));
    }
    // Pushed back unedited (as when the file was saved again): nothing changes
    // but the trailing whitespace Markdown can't keep.
    let unedited = edit_document(&doc, &back).map_err(|e| format!("the unedited Markdown is refused: {e}"))?;
    let rewritten = format_of(&unedited);
    if rewritten.len() != format.len()
        || untouched_paragraphs_kept(&format, &rewritten).is_err()
        || hidden_attributes_kept((&doc, &format), (&unedited, &rewritten)).is_err()
    {
        return Err(format!(
            "pushing the unedited Markdown {markdown:?} rewrites the note's formatting:\n  before {:?}\n  after  {:?}",
            format, rewritten
        ));
    }

    // An edit.
    let (edited, what) = edit_markdown(markdown, &mut rng);
    let Some(desired) = wanted(&classified, &edited) else {
        return Ok(Outcome::Refused);
    };
    let Ok(sent) = edit_document(&doc, &desired) else {
        return Ok(Outcome::Refused);
    };
    let sent_format = format_of(&sent);
    if sent.text != desired.text || !note_holds_format(&sent_format, &desired.paragraphs) {
        // Push checks this itself before it sends anything (a refusal).
        return Ok(Outcome::Refused);
    }
    if !moves_lines(&what) {
        untouched_paragraphs_kept(&format, &sent_format).map_err(|e| format!("{what}: {e}\n  edited {edited:?}"))?;
        hidden_attributes_kept((&doc, &format), (&sent, &sent_format))
            .map_err(|e| format!("{what}: {e}\n  edited {edited:?}"))?;
    }
    // Pulled again: read-only, or it reads back as the edit.
    match classify_note_record(&note_record("N", &sent), &options) {
        NoteDecodeResult::Ok(again) if again.publishable => {
            let Some(reread) = wanted(&again, &again.markdown_text) else {
                return Err(format!(
                    "{what}: the pulled Markdown {:?} doesn't read back",
                    again.markdown_text
                ));
            };
            if reread.text != desired.text || !note_holds_format(&reread.paragraphs, &desired.paragraphs) {
                return Err(format!(
                    "{what}: the edit {edited:?} pulled back reads as another note: {:?}",
                    again.markdown_text
                ));
            }
            Ok(Outcome::Exact)
        }
        NoteDecodeResult::Ok(_) => Ok(Outcome::ReadOnly),
        other => Err(format!("{what}: the edited note isn't read at all: {other:?}")),
    }
}

// --- the Markdown suite ---------------------------------------------------------------

/// Generated Markdown, as a new note's file: push builds the note it reads
/// (or refuses), and pulling that note writes Markdown that reads back the
/// same (or marks it read-only).
fn check_markdown(lines: &[String], seed: u64) -> Checked {
    let markdown = lines.join("\n");
    let Ok(parsed) = parse_note_markdown(&markdown) else {
        return Ok(Outcome::Refused);
    };
    if parsed.text.is_empty() {
        return Ok(Outcome::Refused);
    }
    let mut rng = Rng(seed);
    let mut doc = build_initial_note_document(&parsed.text, &rng.bytes16()).map_err(|e| e.to_string())?;
    if reconcile(&mut doc, &parsed.paragraphs).is_err() {
        return Ok(Outcome::Refused);
    }
    let built = format_of(&doc);
    if doc.text != parsed.text || !note_holds_format(&built, &parsed.paragraphs) {
        // The create path checks this and refuses.
        return Ok(Outcome::Refused);
    }
    let classified = match classify_note_record(&note_record("N", &doc), &ClassifyOptions::default()) {
        NoteDecodeResult::Ok(classified) => classified,
        other => return Err(format!("the note made from {markdown:?} isn't read: {other:?}")),
    };
    if !classified.publishable {
        return Ok(Outcome::ReadOnly);
    }
    let Ok(back) = parse_note_markdown(&classified.markdown_text) else {
        return Err(format!(
            "the pulled Markdown {:?} doesn't read back",
            classified.markdown_text
        ));
    };
    if back.text != parsed.text || !note_holds_format(&back.paragraphs, &parsed.paragraphs) {
        return Err(format!(
            "{markdown:?} pushed and pulled back reads as another note: {:?}",
            classified.markdown_text
        ));
    }
    Ok(Outcome::Exact)
}

// --- shrinking and running ------------------------------------------------------------

/// Smaller versions of `note`: a paragraph, then a piece, left out; a style
/// or a nesting dropped.
fn smaller_notes(note: &GenNote) -> Vec<GenNote> {
    let mut out = Vec::new();
    for i in 0..note.paragraphs.len() {
        if note.paragraphs.len() > 1 {
            let mut smaller = note.clone();
            smaller.paragraphs.remove(i);
            out.push(smaller);
        }
        for j in 0..note.paragraphs[i].pieces.len() {
            let mut smaller = note.clone();
            smaller.paragraphs[i].pieces.remove(j);
            out.push(smaller);
            let piece = &note.paragraphs[i].pieces[j];
            if piece.style != InlineStyle::PLAIN {
                let mut plain = note.clone();
                plain.paragraphs[i].pieces[j].style = InlineStyle::PLAIN;
                out.push(plain);
            }
            if piece.embed.is_none() && piece.text.chars().count() > 1 {
                let half: String = piece.text.chars().take(piece.text.chars().count() / 2).collect();
                let mut shorter = note.clone();
                shorter.paragraphs[i].pieces[j].text = half;
                out.push(shorter);
            }
        }
        let p = &note.paragraphs[i];
        if p.quote > 0 || p.indent > 0 || p.start_number.is_some() || p.kind != Body {
            let mut flat = note.clone();
            let q = &mut flat.paragraphs[i];
            if q.quote > 0 {
                q.quote = 0;
            } else if q.indent > 0 {
                q.indent = 0;
            } else if q.start_number.is_some() {
                q.start_number = None;
            } else {
                q.kind = Body;
            }
            out.push(flat);
        }
    }
    if note.split_runs {
        out.push(GenNote {
            split_runs: false,
            ..note.clone()
        });
    }
    out
}

fn shrink<T: Clone>(case: T, smaller: impl Fn(&T) -> Vec<T>, fails: impl Fn(&T) -> Option<String>) -> (T, String) {
    let mut case = case;
    let mut why = fails(&case).expect("the case failed");
    'outer: for _ in 0..400 {
        for candidate in smaller(&case) {
            if let Some(reason) = fails(&candidate) {
                case = candidate;
                why = reason;
                continue 'outer;
            }
        }
        break;
    }
    (case, why)
}

/// Runs `check` on every seed's case; on a failure, prints the seed and the
/// case shrunk, and fails. Prints what the cases came to.
fn run_suite<T: Clone + std::fmt::Debug>(
    suite: &str,
    default_cases: u64,
    generate: impl Fn(&mut Rng) -> T,
    smaller: impl Fn(&T) -> Vec<T>,
    check: impl Fn(&T, u64) -> Checked,
) {
    let mut outcomes: BTreeMap<Outcome, usize> = BTreeMap::new();
    // Every failing seed listed, unshrunk, rather than the first in full.
    let survey = std::env::var_os("ICLOUD_NOTES_SYNC_PROPERTY_SURVEY").is_some();
    let mut failed = 0;
    for seed in seeds(suite, default_cases) {
        let case = generate(&mut Rng(seed));
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&case, seed)))
            .unwrap_or_else(|panic| Err(format!("panicked: {}", panic_message(&panic))));
        match checked {
            Ok(outcome) => *outcomes.entry(outcome).or_default() += 1,
            Err(reason) if survey => {
                eprintln!("{suite}: seed {seed}: {}", reason.lines().next().unwrap_or_default());
                failed += 1;
            }
            Err(reason) => {
                let fails = |c: &T| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(c, seed)))
                        .unwrap_or_else(|panic| Err(format!("panicked: {}", panic_message(&panic))))
                        .err()
                };
                let (small, why) = shrink(case.clone(), &smaller, fails);
                panic!(
                    "{suite}: seed {seed} fails (rerun with ICLOUD_NOTES_SYNC_PROPERTY_SEED={seed}):\n{reason}\n\
                     shrunk to:\n{small:#?}\nwhich fails with:\n{why}"
                );
            }
        }
    }
    eprintln!("{suite}: {outcomes:?}");
    assert_eq!(failed, 0, "{suite}: {failed} seeds fail");
    assert!(
        outcomes.get(&Outcome::Exact).copied().unwrap_or(0) > 0 || outcomes.values().sum::<usize>() < 5,
        "{suite}: no case round-tripped: the generator is broken"
    );
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

#[test]
fn codec_round_trips_are_exact_or_flagged() {
    run_suite("codec", 1500, |rng| gen_note(rng, true), smaller_notes, check_codec);
}

#[test]
fn markdown_round_trips_are_exact_or_flagged() {
    run_suite(
        "markdown",
        1500,
        |rng| {
            (0..rng.range(1, 8))
                .map(|_| markdown_line(rng))
                .collect::<Vec<String>>()
        },
        |lines: &Vec<String>| {
            (0..lines.len())
                .map(|i| {
                    let mut smaller = lines.clone();
                    smaller.remove(i);
                    smaller
                })
                .filter(|l| !l.is_empty())
                .collect()
        },
        |lines, seed| check_markdown(lines, seed),
    );
}

// --- the splice suite -----------------------------------------------------------------

/// Text pieces for the splice suite: repeated characters (where a diff can
/// match more than one way), line breaks, trailing whitespace, astral
/// characters, combining marks, joiners and embeds.
const SPLICE_TOKENS: &[&str] = &[
    "a",
    "a",
    "b",
    "aa",
    "ab",
    "ba",
    "word",
    "the cat",
    " ",
    "  ",
    "\t",
    "\n",
    "\n",
    "\n\n",
    " \n",
    "😀",
    "😀😀",
    "e\u{301}",
    "\u{301}",
    "👩\u{200d}👧",
    "\u{fffc}",
    "\u{fffc}\n",
    "中",
    "-",
    "x",
];

fn splice_text(rng: &mut Rng, low: usize, high: usize) -> String {
    let mut text = String::new();
    for _ in 0..rng.range(low, high) {
        let token = *rng.pick(SPLICE_TOKENS);
        if rng.chance(3) {
            // A line long enough that a replacement in it isn't diffed.
            text.push_str(&token.replace('\n', "").repeat(rng.range(150, 450)));
        } else {
            text.push_str(token);
        }
    }
    text
}

/// One edit to `text`, as typed in an editor: text typed, deleted or
/// replaced (across lines too), a line broken in two or two joined, the
/// trailing whitespace of every line trimmed (as push does), a line
/// duplicated or moved. `Some((start, end, inserted))` in `text`'s bytes
/// when the edit is one contiguous replacement.
fn splice_edit(text: &str, rng: &mut Rng) -> (String, String, Option<(usize, usize, String)>) {
    let (a, b) = (char_boundary(text, rng), char_boundary(text, rng));
    let (a, b) = (a.min(b), a.max(b));
    let one = |start: usize, end: usize, inserted: &str, what: String| {
        let edited = format!("{}{inserted}{}", &text[..start], &text[end..]);
        (edited, what, Some((start, end, inserted.to_string())))
    };
    match rng.below(9) {
        0 | 1 => {
            let inserted = splice_text(rng, 1, 3);
            one(a, a, &inserted, format!("typed {inserted:?} at {a}"))
        }
        2 => one(a, b, "", format!("deleted {a}..{b}")),
        3 => {
            let inserted = splice_text(rng, 1, 3);
            one(a, b, &inserted, format!("replaced {a}..{b} with {inserted:?}"))
        }
        4 => one(a, a, "\n", format!("broke the line at {a}")),
        5 => match text.match_indices('\n').map(|(i, _)| i).collect::<Vec<_>>() {
            breaks if !breaks.is_empty() => {
                let at = *rng.pick(&breaks);
                let glue = if rng.chance(50) { "" } else { " " };
                one(at, at + 1, glue, format!("joined the lines at {at} with {glue:?}"))
            }
            _ => one(a, a, "\n", format!("broke the line at {a}")),
        },
        6 => {
            let trimmed: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches([' ', '\t'])).collect();
            (trimmed.join("\n"), "trimmed every line".into(), None)
        }
        7 => {
            let mut lines: Vec<&str> = text.split('\n').collect();
            let at = rng.below(lines.len());
            lines.insert(at, lines[at]);
            (lines.join("\n"), format!("duplicated line {at}"), None)
        }
        _ => {
            let mut lines: Vec<&str> = text.split('\n').collect();
            let line = lines.remove(rng.below(lines.len()));
            lines.insert(rng.below(lines.len() + 1), line);
            (lines.join("\n"), "moved a line".into(), None)
        }
    }
}

/// A splice case: a text, the edited text, and what the edit was.
#[derive(Debug, Clone)]
struct SpliceCase {
    old: String,
    new: String,
    what: Vec<String>,
    /// The edit as one contiguous replacement, when it is one.
    single: Option<(usize, usize, String)>,
}

fn gen_splice_case(rng: &mut Rng) -> SpliceCase {
    let old = if rng.chance(30) {
        note_text(&gen_note(rng, true))
    } else {
        splice_text(rng, 0, 30)
    };
    let mut case = SpliceCase {
        new: old.clone(),
        old,
        what: Vec::new(),
        single: None,
    };
    let edits = *rng.pick(&[1, 1, 1, 2, 3]);
    for _ in 0..edits {
        let (edited, what, single) = splice_edit(&case.new, rng);
        case.single = if edits == 1 { single } else { None };
        case.new = edited;
        case.what.push(what);
    }
    case
}

/// The UTF-16 units of `text`, and each unit's emphasis in a document whose
/// every character carries an emphasis of its own (one this tool's Markdown
/// never shows, so only the characters an edit keeps can keep it).
fn emphasized_document(text: &str, rng: &mut Rng) -> Option<NoteDocument> {
    let mut doc = build_initial_note_document(text, &rng.bytes16()).ok()?;
    doc.attribute_runs = text
        .chars()
        .enumerate()
        .map(|(index, c)| AttributeRun {
            length: Some(c.len_utf16() as u32),
            emphasis: Some(index as u32 + 1),
            ..Default::default()
        })
        .collect();
    Some(doc)
}

fn unit_emphasis(doc: &NoteDocument) -> Vec<Option<u32>> {
    doc.attribute_runs
        .iter()
        .flat_map(|run| std::iter::repeat_n(run.emphasis, run.len() as usize))
        .collect()
}

/// `compute_splices` is exactly the edit: its splices are in order, never
/// split a character, and make the edited text from the old one; they change
/// no more than the edit did (for an edit of one stretch of text, no more
/// than that stretch, so what was around it is kept: a line broken in two
/// isn't half typed again); and the document edited with them keeps what
/// every character they keep carries.
fn check_splices(case: &SpliceCase, seed: u64) -> Checked {
    let (old, new) = (utf16(&case.old), utf16(&case.new));
    let splices = compute_splices(&case.old, &case.new);
    let mut rebuilt: Vec<u16> = Vec::new();
    let mut kept: Vec<usize> = Vec::new();
    let mut at = 0usize;
    let inside_pair = |i: usize| i > 0 && i < old.len() && (0xDC00..0xE000).contains(&old[i]);
    for splice in &splices {
        let insert = utf16(&splice.insert_text);
        if splice.start < at || splice.start + splice.delete_length > old.len() {
            return Err(format!("{:?}: splices out of order: {splices:?}", case.what));
        }
        if splice.delete_length == 0 && insert.is_empty() {
            return Err(format!("{:?}: an empty splice: {splices:?}", case.what));
        }
        if inside_pair(splice.start) || inside_pair(splice.start + splice.delete_length) {
            return Err(format!("{:?}: a splice splits a character: {splices:?}", case.what));
        }
        rebuilt.extend_from_slice(&old[at..splice.start]);
        kept.extend(at..splice.start);
        rebuilt.extend_from_slice(&insert);
        kept.extend(std::iter::repeat_n(usize::MAX, insert.len()));
        at = splice.start + splice.delete_length;
    }
    rebuilt.extend_from_slice(&old[at..]);
    kept.extend(at..old.len());
    if rebuilt != new {
        return Err(format!(
            "{:?}: the splices make {:?}, not {:?}: {splices:?}",
            case.what,
            String::from_utf16_lossy(&rebuilt),
            case.new
        ));
    }
    let cost = |s: &Splice| s.delete_length + len16(&s.insert_text);
    let total: usize = splices.iter().map(cost).sum();
    let spanning = cost(&compute_splice(&case.old, &case.new));
    if total > spanning {
        return Err(format!(
            "{:?}: the splices change {total} units, one splice over all of it {spanning}: {splices:?}\n  old {:?}\n  new {:?}",
            case.what, case.old, case.new
        ));
    }
    if let Some((start, end, inserted)) = &case.single {
        let edit = len16(&case.old[*start..*end]) + len16(inserted);
        if total > edit {
            return Err(format!(
                "{:?}: the splices change {total} units, the edit {edit}: {splices:?}",
                case.what
            ));
        }
    }
    let mut rng = Rng(seed ^ 0x5911ce);
    let Some(mut doc) = emphasized_document(&case.old, &mut rng) else {
        return Ok(Outcome::Skipped);
    };
    let before = unit_emphasis(&doc);
    apply_text_edit(&mut doc, &case.new, &ApplyTextEditOptions { replica_id: replica() })
        .map_err(|e| format!("{:?}: the edit fails: {e}", case.what))?;
    if doc.text != case.new {
        return Err(format!("{:?}: the document holds {:?}", case.what, doc.text));
    }
    let after = unit_emphasis(&doc);
    for (unit, &from) in kept.iter().enumerate() {
        if from != usize::MAX && after[unit] != before[from] {
            return Err(format!(
                "{:?}: unit {unit}, kept from {from}, lost its emphasis: {splices:?}",
                case.what
            ));
        }
    }
    Ok(Outcome::Exact)
}

#[test]
fn splices_are_exactly_the_edit() {
    run_suite("splices", 600, gen_splice_case, |_| Vec::new(), check_splices);
}

// --- the vault suite: an in-memory iCloud ---------------------------------------------

const PRIVATE: &str = "/database/1/com.apple.notes/production/private";
const SHARED: &str = "/database/1/com.apple.notes/production/shared";
const NOTE: &str = "C0000000-0000-4000-8000-000000000001";

/// A private Notes zone holding `records`: it lists them all, answers
/// lookups, applies every `records/modify` and serves asset downloads.
#[derive(Default)]
struct Server {
    records: Mutex<Vec<Value>>,
    assets: Mutex<HashMap<String, Vec<u8>>>,
    /// Every `records/modify` operation, as (type, record name).
    operations: Mutex<Vec<(String, String)>>,
    changes: Mutex<u32>,
}

fn typed(value: &Value) -> Value {
    let type_ = match value {
        Value::String(_) => "ENCRYPTED_BYTES",
        Value::Number(_) => "INT64",
        Value::Array(_) => "REFERENCE_LIST",
        _ => "REFERENCE",
    };
    json!({ "value": value, "type": type_ })
}

impl Server {
    fn record(&self, name: &str) -> Option<Value> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r["recordName"] == name)
            .cloned()
    }

    fn set_field(&self, name: &str, key: &str, value: Value) {
        let mut records = self.records.lock().unwrap();
        let record = records.iter_mut().find(|r| r["recordName"] == name).unwrap();
        record["fields"][key] = typed(&value);
        let mut changes = self.changes.lock().unwrap();
        *changes += 1;
        record["recordChangeTag"] = json!(format!("r{changes}"));
    }

    fn modify(&self, body: &Value) -> Value {
        let mut records = self.records.lock().unwrap();
        let mut changes = self.changes.lock().unwrap();
        let mut answers = Vec::new();
        for op in body["operations"].as_array().unwrap() {
            *changes += 1;
            let sent = &op["record"];
            let operation = op["operationType"].as_str().unwrap().to_owned();
            self.operations
                .lock()
                .unwrap()
                .push((operation.clone(), sent["recordName"].as_str().unwrap().to_owned()));
            let tag = json!(format!("t{changes}"));
            let record = match operation.as_str() {
                "update" => {
                    let record = records
                        .iter_mut()
                        .find(|r| r["recordName"] == sent["recordName"])
                        .expect("an update names a record the zone has");
                    assert_eq!(record["recordChangeTag"], sent["recordChangeTag"], "a stale change tag");
                    for (name, field) in sent["fields"].as_object().unwrap() {
                        if field["value"].is_null() {
                            record["fields"].as_object_mut().unwrap().remove(name);
                        } else {
                            record["fields"][name] = typed(&field["value"]);
                        }
                    }
                    record["recordChangeTag"] = tag;
                    record.clone()
                }
                "create" => {
                    let mut record = sent.clone();
                    let fields = record["fields"].as_object_mut().unwrap();
                    fields.retain(|_, field| !field["value"].is_null());
                    for field in fields.values_mut() {
                        *field = typed(&field["value"]);
                    }
                    record["recordChangeTag"] = tag;
                    records.push(record.clone());
                    record
                }
                other => panic!("unexpected operation {other}"),
            };
            answers.push(record);
        }
        json!({ "records": answers })
    }
}

impl Transport for Server {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        let path = path.split('?').next().unwrap_or(path);
        let operation = path
            .strip_prefix(PRIVATE)
            .or_else(|| path.strip_prefix(SHARED))
            .unwrap_or_else(|| panic!("unexpected request to {path}"));
        Ok(match operation {
            "/records/lookup" => {
                let records = self.records.lock().unwrap();
                let found: Vec<Value> = body["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|wanted| records.iter().find(|r| r["recordName"] == wanted["recordName"]))
                    .cloned()
                    .collect();
                json!({ "records": found })
            }
            "/records/modify" => self.modify(body),
            "/changes/zone" => {
                let token = *self.changes.lock().unwrap();
                json!({ "zones": [{
                    "zoneID": { "zoneName": "Notes", "ownerRecordName": "_defaultOwner", "zoneType": "REGULAR_CUSTOM_ZONE" },
                    "moreComing": false,
                    "syncToken": format!("token-{token}"),
                    "records": self.records.lock().unwrap().clone(),
                }] })
            }
            "/changes/database" => json!({ "zones": [], "moreComing": false, "syncToken": "shared" }),
            _ => panic!("unexpected request to {path}"),
        })
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        let bytes = self
            .assets
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .expect("a download of a known asset");
        std::fs::write(dest, &bytes).unwrap();
        Ok(bytes.len() as u64)
    }
}

struct Shared(Arc<Server>);

impl Transport for Shared {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        self.0.post_json(path, body)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<u64, CkError> {
        self.0.download(url, dest)
    }
}

fn serve(server: &Arc<Server>) -> impl Connector + use<> {
    let server = server.clone();
    FnConnector(move || -> Result<Remote, Error> {
        Ok(Remote {
            db: Database::new(Box::new(Shared(server.clone()))),
            account: Account {
                apple_id: "property@example.com".into(),
                dsid: "10000000001".into(),
            },
        })
    })
}

fn reference(record_name: &str) -> Value {
    json!({ "recordName": record_name, "action": "VALIDATE", "zoneID": { "zoneName": "Notes", "ownerRecordName": "_defaultOwner" } })
}

fn record_json(record_name: &str, record_type: &str, fields: Value) -> Value {
    json!({
        "recordName": record_name,
        "recordType": record_type,
        "fields": fields,
        "pluginFields": {},
        "recordChangeTag": "1",
        "created": { "timestamp": 1752500000000i64, "userRecordName": "_defaultOwner", "deviceID": "2" },
        "modified": { "timestamp": 1752500000000i64, "userRecordName": "_defaultOwner", "deviceID": "2" },
        "deleted": false,
    })
}

/// A table's document holding `grid`: Apple's own baseline table, edited to
/// it a step at a time (rows, then columns, then cells).
fn table_document(grid: &[Vec<String>]) -> Option<String> {
    let mut doc = parse_table_document(&common::payload("table_rev_baseline.json")).ok()?;
    let mut uuids = common::counting_uuids();
    let mut step = |doc: &mut icloud_notes_sync::doc::tables::TableDocument, wanted: Vec<Vec<String>>| {
        apply_table_edit_with(doc, &wanted, &replica(), &mut uuids).ok()
    };
    let current = grid_from_table_document(&doc).ok()?;
    let filler = |r: usize, c: usize| format!("f{r}x{c}");
    let (rows, columns) = (grid.len(), grid[0].len());
    // Rows first, keeping the columns; then columns; all filler text.
    let width = current[0].len();
    let resized_rows: Vec<Vec<String>> = (0..rows).map(|r| (0..width).map(|c| filler(r, c)).collect()).collect();
    let fill_cells: Vec<Vec<String>> = (0..current.len())
        .map(|r| (0..width).map(|c| filler(r, c)).collect())
        .collect();
    step(&mut doc, fill_cells)?;
    if rows != current.len() {
        step(&mut doc, resized_rows)?;
    }
    if columns != width {
        step(
            &mut doc,
            (0..rows)
                .map(|r| (0..columns).map(|c| filler(r, c)).collect())
                .collect(),
        )?;
    }
    step(&mut doc, grid.to_vec())?;
    if grid_from_table_document(&doc).ok()? != grid {
        return None;
    }
    Some(base64_encode(&encode_table_document(&doc).ok()?))
}

/// The zone for `note`: its folder, the note, and every attachment record
/// and file its embeds name. `None` when the note can't be built.
fn zone_for(note: &GenNote, rng: &mut Rng) -> Option<(Server, NoteDocument)> {
    let doc = build_document(note, rng)?;
    let server = Server::default();
    let mut records = vec![record_json(
        "DefaultFolder-CloudKit",
        "Folder",
        json!({ "TitleEncrypted": { "value": base64_encode(b"Notes"), "type": "ENCRYPTED_BYTES" } }),
    )];
    let mut note_json = record_json(
        NOTE,
        "Note",
        json!({
            "TitleEncrypted": { "value": base64_encode(derive_note_title(&doc.text).as_bytes()), "type": "ENCRYPTED_BYTES" },
            "CreationDate": { "value": 1752563940000i64, "type": "TIMESTAMP" },
            "ModificationDate": { "value": 1752564000000i64, "type": "TIMESTAMP" },
            "Deleted": { "value": 0, "type": "INT64" },
            "Folder": { "value": reference("DefaultFolder-CloudKit"), "type": "REFERENCE" },
            "Folders": { "value": [reference("DefaultFolder-CloudKit")], "type": "REFERENCE_LIST" },
            "TextDataEncrypted": { "value": document_base64(&doc), "type": "ENCRYPTED_BYTES" },
        }),
    );
    note_json["parent"] = json!({ "recordName": "DefaultFolder-CloudKit" });
    records.push(note_json);
    for (index, embed) in embeds_of(note).into_iter().enumerate() {
        match embed {
            Embed::File { name, uti } => {
                let url = format!("https://cvws.icloud-content.com/B/asset-{index}");
                let bytes = format!("file {index}").into_bytes();
                records.push(record_json(
                    &embed_id(index),
                    "Attachment",
                    json!({
                        "UTI": { "value": uti, "type": "STRING" },
                        "Media": { "value": reference(&media_id(index)), "type": "REFERENCE" },
                        "Note": { "value": reference(NOTE), "type": "REFERENCE" },
                    }),
                ));
                records.push(record_json(
                    &media_id(index),
                    "Media",
                    json!({
                        "Asset": { "value": { "fileChecksum": format!("sum{index}"), "size": bytes.len(), "downloadURL": url }, "type": "ASSETID" },
                        "FilenameEncrypted": { "value": base64_encode(name.as_bytes()), "type": "ENCRYPTED_BYTES" },
                    }),
                ));
                server.assets.lock().unwrap().insert(url, bytes);
            }
            Embed::Table(grid) => {
                let data = table_document(&grid)?;
                records.push(record_json(
                    &embed_id(index),
                    "Attachment",
                    json!({
                        "UTI": { "value": "com.apple.notes.table", "type": "STRING" },
                        "MergeableDataEncrypted": { "value": data, "type": "ENCRYPTED_BYTES" },
                        "Note": { "value": reference(NOTE), "type": "REFERENCE" },
                    }),
                ));
            }
            Embed::Drawing | Embed::Bare(_) => {}
        }
    }
    *server.records.lock().unwrap() = records;
    Some((server, doc))
}

/// The note `name`'s document in iCloud, decompressed.
fn raw_document(server: &Server, name: &str) -> Vec<u8> {
    let record = server.record(name).unwrap();
    decompress_note_document(&base64_decode(
        record["fields"]["TextDataEncrypted"]["value"].as_str().unwrap(),
    ))
    .unwrap()
}

fn record_of(value: &Value) -> CloudKitRecord {
    serde_json::from_value(value.clone()).unwrap()
}

fn state_of(vault: &Path) -> CloneState {
    read_clone_state(vault).unwrap().unwrap()
}

struct VaultNote {
    file: String,
    text: String,
    body: String,
    frontmatter: String,
    read_only: bool,
    /// Each file attachment of the note, by record name, as its link path
    /// from the note's directory.
    links: HashMap<String, String>,
}

fn vault_note(vault: &Path, record_name: &str, filename_mode: bool) -> Option<VaultNote> {
    let state = state_of(vault);
    let entry = state.notes.get(record_name)?;
    let text = std::fs::read_to_string(vault.join(&entry.file)).ok()?;
    let envelope = split_frontmatter(
        &text,
        SplitOptions {
            filename_as_title: filename_mode,
        },
    );
    let note_dir = posix::dirname(&entry.file);
    let links = state
        .attachments
        .iter()
        .flatten()
        .filter(|(_, a)| a.note_record_name == record_name)
        .map(|(name, a)| (name.clone(), posix::relative(&note_dir, &a.file)))
        .collect();
    Some(VaultNote {
        file: entry.file.clone(),
        text: text.clone(),
        body: envelope.body,
        frontmatter: envelope.frontmatter,
        read_only: entry.unpublishable_reason.as_deref().is_some_and(|r| !r.is_empty()),
        links,
    })
}

fn clone_vault(server: &Arc<Server>, dir: &Path, name: &str, filename_mode: bool) -> Result<PathBuf, String> {
    let vault = dir.join(name);
    run_clone_with(
        &serve(server),
        &vault,
        &mut NoProgress,
        &CloneOptions {
            filename_as_title: filename_mode,
            account: None,
        },
    )
    .map_err(|e| format!("clone failed: {e}"))?;
    Ok(vault)
}

fn push(server: &Arc<Server>, vault: &Path) -> Result<PushResult, String> {
    run_push_with(&serve(server), vault, &mut |_| {}, &PushOptions::default()).map_err(|e| format!("push failed: {e}"))
}

fn classified_on_server(server: &Server, name: &str, filename_mode: bool) -> Box<DecodedNote> {
    match classify_note_record(
        &record_of(&server.record(name).unwrap()),
        &ClassifyOptions {
            title_mode: title_mode(filename_mode),
        },
    ) {
        NoteDecodeResult::Ok(classified) => classified,
        other => panic!("the note in iCloud isn't read: {other:?}"),
    }
}

/// What a note's file says the note is: its embeds (links, tables) as
/// U+FFFC, parsed, with its title put back; an attachment whose link is gone
/// is gone. `None` when push can't read it.
fn file_says(
    classified: &DecodedNote,
    body: &str,
    links: &HashMap<String, String>,
) -> Option<(ParsedNoteMarkdown, Vec<Vec<Vec<String>>>)> {
    let plan = plan_embed_representations(body, &classified.embed_slots, links).ok()?;
    // A link to a file not attached yet needs the file: no edit here adds one.
    if !plan.new_links.is_empty() {
        return None;
    }
    let tables = plan.tables.iter().map(|t| t.block.grid.clone()).collect();
    Some((wanted(classified, &plan.reconstructed_body_text)?, tables))
}

fn server_tables(server: &Server, classified: &DecodedNote) -> Vec<Vec<Vec<String>>> {
    classified
        .embed_slots
        .iter()
        .filter_map(|slot| match slot {
            EmbedSlot::Attachment(r) if r.type_uti == "com.apple.notes.table" => Some(r.attachment_identifier.clone()),
            _ => None,
        })
        .filter_map(|name| {
            let record = server.record(&name)?;
            let data = record["fields"]["MergeableDataEncrypted"]["value"]
                .as_str()?
                .to_string();
            grid_from_table_document(&parse_table_document(&base64_decode(&data)).ok()?).ok()
        })
        .collect()
}

/// A table's cells as its Markdown reads back: whitespace at a cell's edges
/// and a `\r` don't survive a Markdown table.
fn shown(tables: &[Vec<Vec<String>>]) -> Vec<Vec<Vec<String>>> {
    tables
        .iter()
        .map(|grid| {
            icloud_notes_sync::md::table::render_markdown_table(grid)
                .ok()
                .and_then(|md| icloud_notes_sync::md::table::parse_markdown_table(&md).ok())
                .unwrap_or_else(|| grid.clone())
        })
        .collect()
}

/// How the Markdown file in a vault stands for the note as it is in iCloud.
enum FileMatch {
    /// It reads back as the note (text, formatting, tables).
    Exact,
    /// The note is read-only.
    ReadOnly,
    /// It reads back as something else: what.
    Mismatch(String),
}

fn file_matches_server(
    server: &Server,
    vault: &Path,
    name: &str,
    filename_mode: bool,
    label: &str,
) -> Result<FileMatch, String> {
    let local = vault_note(vault, name, filename_mode).ok_or(format!("{label}: the note has no file"))?;
    if local.read_only {
        return Ok(FileMatch::ReadOnly);
    }
    let classified = classified_on_server(server, name, filename_mode);
    // A title the file name can't hold is written in the frontmatter, and
    // reads back as the note's.
    if let Some(title) = icloud_notes_sync::md::frontmatter::read_note_title(&local.frontmatter)
        && title != classified.title_line
    {
        return Ok(FileMatch::Mismatch(format!(
            "{label}: the file's apple-note-title {title:?} isn't the note's {:?}",
            classified.title_line
        )));
    }
    let Some((says, tables)) = file_says(&classified, &local.body, &local.links) else {
        return Ok(FileMatch::Mismatch(format!(
            "{label}: the pulled file can't be read back as the note it is for:\n{:?}",
            local.text
        )));
    };
    let format = classified.format.clone().unwrap();
    if !reads_back_as(&format, &says) {
        return Ok(FileMatch::Mismatch(format!(
            "{label}: the pulled file reads back as another note:\n  file {:?}\n  note {format:?}\n  read back {:?}",
            local.text, says.paragraphs
        )));
    }
    if tables != shown(&server_tables(server, &classified)) {
        return Ok(FileMatch::Mismatch(format!(
            "{label}: the pulled file's tables read back as others:\n  file {:?}\n  in iCloud {:?}",
            local.text,
            server_tables(server, &classified)
        )));
    }
    Ok(FileMatch::Exact)
}

/// After a clone or pull: `None` when the file reads back as the note (go
/// on); otherwise the note must be held back from push, read-only or with
/// every edit refused: an edit is made and pushed, and nothing may go up.
fn settle(
    server: &Arc<Server>,
    vault: &Path,
    name: &str,
    filename_mode: bool,
    label: &str,
) -> Result<Option<Outcome>, String> {
    let (outcome, mismatch) = match file_matches_server(server, vault, name, filename_mode, label)? {
        FileMatch::Exact => return Ok(None),
        FileMatch::ReadOnly => (Outcome::ReadOnly, String::new()),
        FileMatch::Mismatch(why) => (Outcome::Refused, why),
    };
    let local = vault_note(vault, name, filename_mode).unwrap();
    std::fs::write(vault.join(&local.file), format!("{}\nan edit", local.text)).unwrap();
    let before = server.operations.lock().unwrap().len();
    let result = push(server, vault)?;
    let sent: Vec<_> = server.operations.lock().unwrap()[before..].to_vec();
    let refused = result
        .entries
        .iter()
        .find(|e| e.entry.file == local.file)
        .is_some_and(|e| e.entry.resolution == PlanResolution::Refused && e.entry.reason.is_some());
    if !sent.is_empty() || (outcome == Outcome::Refused && !refused) {
        return Err(format!(
            "{mismatch}\nand an edit to it was not held back: {sent:?} {:?}",
            result.entries
        ));
    }
    Ok(Some(outcome))
}

/// Frontmatter keys this tool doesn't own, in YAML shapes Obsidian and
/// others write.
const FRONTMATTER: &[&str] = &[
    "tags: [a, b]",
    "aliases:\n  - x\n  - 'y: z'",
    "# a comment\nkey: value # trailing",
    "anchor: &a 1\nref: *a",
    "multi: |\n  line1\n  line2",
    "folded: >\n  a\n  b",
    "quoted: \"a: b # c\"",
    "single: 'it''s'",
    "empty:",
    "nested:\n  deep:\n    k: v",
    "date: 2024-01-01",
    "num: 0x1F",
    "\"quoted key\": 1",
    "unicodé: ✓",
    "title: My note",
    "apple-note-title-x: 1",
    "spaces:    value",
    "flow: {a: 1, b: [c, d]}",
    "url: https://example.com/a#b",
    "long: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbb",
];

/// `text` with frontmatter keys added: after the note id or before it.
fn add_frontmatter(text: &str, rng: &mut Rng, filename_mode: bool) -> String {
    let envelope = split_frontmatter(
        text,
        SplitOptions {
            filename_as_title: filename_mode,
        },
    );
    let Some(inner) = envelope
        .frontmatter
        .strip_prefix("---\n")
        .and_then(|f| f.split_once("\n---"))
        .map(|(inner, _)| inner.to_string())
    else {
        return text.to_string();
    };
    let extra: Vec<&str> = (0..rng.range(1, 3)).map(|_| *rng.pick(FRONTMATTER)).collect();
    let mut seen = std::collections::HashSet::new();
    let extra: Vec<&str> = extra.into_iter().filter(|e| seen.insert(*e)).collect();
    let yaml = if rng.chance(50) {
        format!("{inner}\n{}", extra.join("\n"))
    } else {
        format!("{}\n{inner}", extra.join("\n"))
    };
    format!("---\n{yaml}\n---\n\n{}", envelope.body)
}

/// A note through a vault and an in-memory iCloud:
///
/// 1. cloned: its file reads back as it, or the note is held back (read
///    only, or an edit to it is refused);
/// 2. frontmatter added to the file: a push sends nothing;
/// 3. changed in iCloud, then pulled: the frontmatter stays as it was and
///    the file reads back as the changed note;
/// 4. its Markdown edited, then pushed: refused (nothing sent), or what is
///    sent is what the edited file says, changing nothing the edit left
///    alone;
/// 5. cloned afresh: the file reads back as the note, or it is held back;
/// 6. a new note's file pushed and cloned afresh ([`check_new_note`]).
fn check_vault(note: &GenNote, seed: u64) -> Checked {
    let mut rng = Rng(seed ^ 0xfa11);
    let Some((server, _)) = zone_for(note, &mut rng) else {
        return Ok(Outcome::Skipped);
    };
    let server = Arc::new(server);
    let dir = tempfile::tempdir().unwrap();
    let fm = note.filename_mode;

    // 1.
    let vault = clone_vault(&server, dir.path(), "a", fm)?;
    if let Some(outcome) = settle(&server, &vault, NOTE, fm, "the clone")? {
        return Ok(outcome);
    }

    // 2.
    let local = vault_note(&vault, NOTE, fm).unwrap();
    let with_frontmatter = add_frontmatter(&local.text, &mut rng, fm);
    std::fs::write(vault.join(&local.file), &with_frontmatter).unwrap();
    let result = push(&server, &vault)?;
    if !server.operations.lock().unwrap().is_empty() {
        return Err(format!(
            "frontmatter added to the file was pushed into the note: {:?}\n  file {with_frontmatter:?}",
            result.entries
        ));
    }
    let frontmatter = vault_note(&vault, NOTE, fm).unwrap().frontmatter;

    // 3.
    let mut remote = parse_note_document(&raw_document(&server, NOTE)).unwrap();
    let remote_text = format!("{}\nRemote {}", remote.text, words(&mut rng, 2, 2));
    apply_text_edit(
        &mut remote,
        &remote_text,
        &ApplyTextEditOptions {
            replica_id: *b"another-device!!",
        },
    )
    .unwrap();
    server.set_field(NOTE, "TextDataEncrypted", json!(document_base64(&remote)));
    run_pull_with(&serve(&server), &vault, &mut NoProgress, &mut |_| {}).map_err(|e| format!("pull failed: {e}"))?;
    let pulled = vault_note(&vault, NOTE, fm).unwrap();
    // The YAML, up to its closing fence: in an in-body vault the envelope
    // also takes the blank lines after it, the note's own empty first lines.
    let yaml = |envelope: &str| envelope.trim_end_matches('\n').to_string();
    if yaml(&pulled.frontmatter) != yaml(&frontmatter) {
        return Err(format!(
            "a pull rewrote the frontmatter:\n  before {frontmatter:?}\n  after  {:?}",
            pulled.frontmatter
        ));
    }
    if let Some(outcome) = settle(&server, &vault, NOTE, fm, "the pull")? {
        return Ok(outcome);
    }

    // 4.
    let before = classified_on_server(&server, NOTE, fm);
    let before_format = before.format.clone().unwrap();
    let before_tables = server_tables(&server, &before);
    let (edited_body, what) = edit_markdown(&pulled.body, &mut rng);
    std::fs::write(vault.join(&pulled.file), format!("{}{edited_body}", pulled.frontmatter)).unwrap();
    // What the file holds now, as push reads it (blank lines that open an
    // in-body vault's body belong to the frontmatter).
    let edited_body = vault_note(&vault, NOTE, fm).unwrap().body;
    let says = file_says(&before, &edited_body, &pulled.links);
    let result = push(&server, &vault)?;
    let entry = result.entries.iter().find(|e| e.entry.file == pulled.file);
    let refused = entry.is_some_and(|e| e.entry.resolution == PlanResolution::Refused);
    let after = classified_on_server(&server, NOTE, fm);
    let after_format = after.format.clone().unwrap_or_default();
    let note_sent = server.operations.lock().unwrap().iter().any(|(_, name)| name == NOTE);
    if refused {
        if note_sent || after_format != before_format {
            return Err(format!("{what}: a refused push changed the note"));
        }
        if entry.and_then(|e| e.entry.reason.as_deref()).is_none_or(str::is_empty) {
            return Err(format!("{what}: a push refused without a reason"));
        }
        return Ok(Outcome::Refused);
    }
    if let Some(failure) = result.failures().first() {
        return Err(format!("{what}: the push failed: {failure}"));
    }
    if !note_sent && after_format == before_format {
        // Nothing to send (an edit that changes nothing the note holds): the
        // file still reads back as the note.
        if let FileMatch::Mismatch(why) = file_matches_server(&server, &vault, NOTE, fm, "the unsent edit")? {
            return Err(format!(
                "{what}: an edit that sent nothing left a file that isn't the note: {why}"
            ));
        }
    }
    let Some((says, tables)) = says else {
        return Err(format!(
            "{what}: a file push can't read was pushed: {:?}\n  edited {edited_body:?}",
            result.entries
        ));
    };
    let (sent_text, sent_format) = decode_sent(
        server.record(NOTE).unwrap()["fields"]["TextDataEncrypted"]["value"]
            .as_str()
            .unwrap(),
    );
    let unsent_matches = !note_sent && reads_back_as(&sent_format, &says);
    if !unsent_matches && (sent_text != says.text || !note_holds_format(&sent_format, &says.paragraphs)) {
        return Err(format!(
            "{what}: the push sent another note than the file says:\n  file {edited_body:?}\n  says {:?}\n  sent {sent_format:?}",
            says.paragraphs
        ));
    }
    if !moves_lines(&what) {
        untouched_paragraphs_kept(&before_format, &sent_format)
            .map_err(|e| format!("{what}: {e}\n  edited {edited_body:?}"))?;
    }
    let now_tables = server_tables(&server, &after);
    if shown(&now_tables) != tables && now_tables != tables {
        return Err(format!(
            "{what}: the tables in iCloud {now_tables:?} aren't the file's {tables:?} (were {before_tables:?})"
        ));
    }

    // 5.
    let fresh = clone_vault(&server, dir.path(), "b", fm)?;
    if let Some(outcome) = settle(&server, &fresh, NOTE, fm, "the fresh clone")? {
        return Ok(outcome);
    }

    // 6.
    check_new_note(&server, &fresh, dir.path(), fm, &mut rng)?;
    Ok(Outcome::Exact)
}

/// A new note's file, generated Markdown under frontmatter this tool doesn't
/// own: pushed, the note made is what the file says (or the push refuses
/// it), and pulled into a fresh clone, its file reads back as the note (or
/// the note is held back).
fn check_new_note(server: &Arc<Server>, vault: &Path, dir: &Path, fm: bool, rng: &mut Rng) -> Result<(), String> {
    let lines: Vec<String> = (0..rng.range(1, 6)).map(|_| markdown_line(rng)).collect();
    let body = lines.join("\n");
    let frontmatter = if rng.chance(50) {
        format!("---\n{}\n---\n\n", rng.pick(FRONTMATTER))
    } else {
        String::new()
    };
    let file = format!("{frontmatter}{body}");
    std::fs::write(vault.join("Notes/New note.md"), &file).unwrap();
    let body = split_frontmatter(&file, SplitOptions { filename_as_title: fm }).body;
    let result = push(server, vault)?;
    let created: Vec<String> = server
        .records
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["recordType"] == "Note" && r["recordName"] != NOTE)
        .map(|r| r["recordName"].as_str().unwrap().to_owned())
        .collect();
    let Some(name) = created.first() else {
        let refused = result
            .entries
            .iter()
            .any(|e| e.entry.file == "Notes/New note.md" && e.entry.resolution == PlanResolution::Refused);
        return if refused || body.trim().is_empty() {
            Ok(())
        } else {
            Err(format!(
                "a new note's file was neither pushed nor refused: {file:?}\n{:?}",
                result.entries
            ))
        };
    };
    let mut says = parse_note_markdown(&body).map_err(|e| format!("{file:?} was pushed, unreadable: {e}"))?;
    if fm {
        let title = icloud_notes_sync::md::title::title_paragraph_from_filename("New note");
        says = icloud_notes_sync::md::title::restore_title_paragraph_text(&title, &says.paragraphs);
    }
    let (sent_text, sent_format) = decode_sent(
        server.record(name).unwrap()["fields"]["TextDataEncrypted"]["value"]
            .as_str()
            .unwrap(),
    );
    if sent_text != says.text || !note_holds_format(&sent_format, &says.paragraphs) {
        return Err(format!(
            "the new note's file {file:?} was pushed as another note:\n  says {:?}\n  sent {sent_format:?}",
            says.paragraphs
        ));
    }
    let fresh = clone_vault(server, dir, "c", fm)?;
    settle(server, &fresh, name, fm, "the new note's fresh clone")?;
    Ok(())
}

/// An in-body vault's file body starts after the frontmatter and every
/// blank line after it, so the empty line a note starts with (as
/// `real_unicode_note.json` does) isn't in what its file reads as: push
/// puts it back, as it does a stripped title, and an edit keeps it.
#[test]
fn an_in_body_note_keeps_the_empty_line_it_starts_with_through_an_edit() {
    let note = GenNote {
        paragraphs: vec![
            GenParagraph {
                kind: Body,
                indent: 0,
                quote: 0,
                done: false,
                start_number: None,
                implicit_body: true,
                pieces: Vec::new(),
            },
            GenParagraph {
                kind: Body,
                indent: 0,
                quote: 0,
                done: false,
                start_number: None,
                implicit_body: true,
                pieces: vec![Piece {
                    text: "Hello".into(),
                    style: InlineStyle::PLAIN,
                    embed: None,
                    hidden: 0,
                }],
            },
        ],
        split_runs: false,
        filename_mode: false,
    };
    let (server, _) = zone_for(&note, &mut Rng(1)).unwrap();
    let server = Arc::new(server);
    let dir = tempfile::tempdir().unwrap();
    let vault = clone_vault(&server, dir.path(), "a", false).unwrap();
    let local = vault_note(&vault, NOTE, false).unwrap();
    std::fs::write(vault.join(&local.file), format!("{}\nmore", local.text)).unwrap();
    push(&server, &vault).unwrap();
    let (text, _) = decode_sent(
        server.record(NOTE).unwrap()["fields"]["TextDataEncrypted"]["value"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(text, "\nHello\nmore");
}

#[test]
fn vault_round_trips_are_exact_or_flagged() {
    run_suite("vault", 150, |rng| gen_note(rng, true), smaller_notes, check_vault);
}

/// The generated cases do reach the edges they are meant to: embeds of every
/// kind, every paragraph kind, nesting, styles.
#[test]
fn the_generator_covers_what_it_is_meant_to() {
    let mut kinds = std::collections::HashSet::new();
    let mut embeds = [0usize; 4];
    let mut styled = 0;
    for seed in seeds("coverage", 300) {
        let note = gen_note(&mut Rng(seed), true);
        for p in &note.paragraphs {
            kinds.insert(p.kind);
            for piece in &p.pieces {
                styled += usize::from(piece.style != InlineStyle::PLAIN);
                match &piece.embed {
                    Some(Embed::File { .. }) => embeds[0] += 1,
                    Some(Embed::Table(_)) => embeds[1] += 1,
                    Some(Embed::Drawing) => embeds[2] += 1,
                    Some(Embed::Bare(_)) => embeds[3] += 1,
                    None => {}
                }
            }
        }
    }
    assert_eq!(kinds.len(), 9, "{kinds:?}");
    assert!(embeds.iter().all(|&n| n > 0), "{embeds:?}");
    assert!(styled > 100);
}
