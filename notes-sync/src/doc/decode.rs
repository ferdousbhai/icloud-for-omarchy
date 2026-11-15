//! Record classification: the shared skip/decode rules `clone`, `pull` and
//! `push` use. Originally derived from icloud-md.

use std::ops::Range;

use serde_json::Value;

use super::embeds::{AttachmentReference, EmbedSlot, OBJECT_REPLACEMENT_CHARACTER, embed_slots_of, is_table_uti};
use super::encode::TRASH_FOLDER_RECORD_NAME;
use super::format::{
    FormatParagraph, ParagraphKind, decode_note_format, paragraph_projections_equal, trim_trailing_whitespace,
};
use super::text::decode_note_string;
use crate::cloudkit::{CloudKitRecord, FieldValue};
use crate::js::{base64_decode, buffer_to_utf8};
use crate::md::parse::{ParsedNoteMarkdown, parse_note_markdown};
use crate::md::render::render_note_markdown;
use crate::vault::state::TitleMode;

/// `ClassifyNoteOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassifyOptions {
    /// `Filename`: `markdown_text` excludes the title paragraph and the
    /// round-trip gate runs on that stripped projection.
    pub title_mode: TitleMode,
}

/// Why a record can't be synced (`{status: "unsyncable", reason}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsyncableReason {
    /// Body bytes present but don't parse - a durable fact about the record.
    Undecodable,
    /// No `TextDataEncrypted` - a fact about the delivery; never untrack over it.
    MissingBody,
}

impl UnsyncableReason {
    /// The TS string (`"undecodable"` / `"missing-body"`), which appears in
    /// refusal reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            UnsyncableReason::Undecodable => "undecodable",
            UnsyncableReason::MissingBody => "missing-body",
        }
    }
}

/// `OkNoteDecodeResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedNote {
    /// Apple's truncated display title (`TitleEncrypted`).
    pub title: String,
    /// The note's actual first line - what a filename-as-title vault names
    /// the file after.
    pub title_line: String,
    /// Plain visible text (what the CRDT stores; push splices this).
    pub body_text: String,
    /// Markdown with U+FFFC placeholders still in place; `body_text`
    /// verbatim when not publishable.
    pub markdown_text: String,
    /// The whole note's format model (title included); `None` exactly when
    /// the plain-text fallback applies.
    pub format: Option<Vec<FormatParagraph>>,
    /// `markdown_text` omits the title paragraph (filename-as-title vault and
    /// the title holds no embed). TS `titleStripped?: boolean`.
    pub title_stripped: bool,
    pub embed_slots: Vec<EmbedSlot>,
    pub attachments: Vec<AttachmentReference>,
    /// The tables (indexes into `embed_slots`) written as their embed
    /// marker rather than as a Markdown table: one whose Markdown table
    /// couldn't be read back from where it would be written
    /// ([`tables_kept_as_markers`]).
    pub table_markers: Vec<usize>,
    pub publishable: bool,
    pub unpublishable_reason: Option<String>,
}

impl DecodedNote {
    /// What push sends for `parsed` (what the note's file reads as, its
    /// unwritten lines put back): every paragraph whose Markdown the edit
    /// left alone as the note has it, and the others as their Markdown
    /// says. Markdown can't show everything a note holds (a highlight, a
    /// dash list, a style this tool doesn't know), and some of what it
    /// shows reads back as something a little different: a paragraph is
    /// only made what its Markdown says when its Markdown changed, so an
    /// edit changes nothing but what it changed. "Left alone" is measured
    /// against the Markdown this tool writes for the note as it is in
    /// iCloud (`markdown_text`), which is the file as last pulled, or the
    /// remote side of the merge push made.
    pub fn keep_untouched(&self, parsed: ParsedNoteMarkdown) -> ParsedNoteMarkdown {
        let Some(format) = &self.format else {
            return parsed;
        };
        let note = if self.title_stripped { &format[1..] } else { &format[..] };
        let Ok(base) = parse_note_markdown(&self.markdown_text) else {
            return parsed;
        };
        let base = self.with_unwritten_lines(base);
        ParsedNoteMarkdown::from_paragraphs(merge_untouched(note, &base.paragraphs, &parsed.paragraphs))
    }

    /// `parsed`, what the note's file reads as, with the lines the file
    /// can't hold put back as the note has them. An in-body vault's file
    /// body starts after the frontmatter and every blank line after it
    /// (`split_frontmatter`), so the empty lines a note starts with are in
    /// the note but never in what its file reads as. A filename-as-title
    /// note that is only its title has an empty file, which reads as one
    /// empty line rather than none. (The title of a stripped note is put
    /// back by the caller.)
    pub fn with_unwritten_lines(&self, parsed: ParsedNoteMarkdown) -> ParsedNoteMarkdown {
        let plain_empty =
            |p: &FormatParagraph| p.text.is_empty() && p.kind == ParagraphKind::Body && p.block_quote_level == 0;
        if self.title_stripped {
            let title_only = self.format.as_ref().is_some_and(|f| f.len() == 1);
            return if title_only && parsed.paragraphs.len() == 1 && plain_empty(&parsed.paragraphs[0]) {
                ParsedNoteMarkdown::from_paragraphs(Vec::new())
            } else {
                parsed
            };
        }
        // Each empty line the Markdown starts with is an empty paragraph
        // (one in a quote, a list or a heading has its marker).
        let leading = self
            .markdown_text
            .split('\n')
            .take_while(|line| line.is_empty())
            .count();
        let kept = parsed.paragraphs.iter().take_while(|p| plain_empty(p)).count();
        if leading <= kept {
            return parsed;
        }
        let empty = FormatParagraph {
            kind: ParagraphKind::Body,
            indent: 0,
            block_quote_level: 0,
            done: None,
            start_number: 0,
            text: String::new(),
            spans: Vec::new(),
            start: 0,
        };
        let mut paragraphs = vec![empty; leading - kept];
        paragraphs.extend(parsed.paragraphs);
        ParsedNoteMarkdown::from_paragraphs(paragraphs)
    }
}

/// `NoteDecodeResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteDecodeResult {
    Deleted,
    Unsyncable(UnsyncableReason),
    Ok(Box<DecodedNote>),
}

/// The reason engines up to 0.10.1 gave a note whose text lives in a
/// `TextDataAsset` (upstream icloud-md PR #29, verbatim), which they could
/// read but not write. Such a note is written back now; a vault's state may
/// still carry the reason, which is dropped when the state is read.
pub const TEXT_DATA_ASSET_UNPUBLISHABLE_REASON: &str =
    "is so large that Apple keeps its text in a separate file, which can't be written back yet";

/// The largest `TextDataEncrypted` (base64) decoded: CloudKit caps an inline
/// field near 1 MB, so only an inlined `TextDataAsset` comes close, and a
/// body past this is undecodable rather than inflated.
pub const MAX_TEXT_DATA_BASE64: usize = 16 << 20;

/// `classifyNoteRecord`.
///
/// A note whose text Apple moved into a `TextDataAsset` (upstream PR #29)
/// reads like any other once `Database::inline_asset_bodies` has fetched
/// it, and is written back like any other: `Database::update_records`
/// uploads its new text as an asset.
pub fn classify_note_record(record: &CloudKitRecord, options: &ClassifyOptions) -> NoteDecodeResult {
    if is_deleted(record) {
        return NoteDecodeResult::Deleted;
    }
    let Some(Value::String(text_value)) = record.fields.get("TextDataEncrypted").map(|f| &f.value) else {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::MissingBody);
    };
    if text_value.len() > MAX_TEXT_DATA_BASE64 {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::Undecodable);
    }
    let Ok(string) = decode_note_string(&base64_decode(text_value)) else {
        return NoteDecodeResult::Unsyncable(UnsyncableReason::Undecodable);
    };
    let body_text = string.string.clone().unwrap_or_default();
    let title = decode_title_field(record.fields.get("TitleEncrypted"));
    let title_line = body_text.split('\n').next().unwrap_or("").to_string();

    // Runs past the end of the text: nothing says which characters they
    // mean, so neither the embeds nor the formatting can be read.
    let Some(embed_slots) = embed_slots_of(&string) else {
        return NoteDecodeResult::Ok(Box::new(DecodedNote {
            title,
            title_line,
            body_text: body_text.clone(),
            markdown_text: body_text,
            format: None,
            title_stripped: false,
            embed_slots: Vec::new(),
            attachments: Vec::new(),
            table_markers: Vec::new(),
            publishable: false,
            unpublishable_reason: Some(OVERSHOOTING_RUNS_REASON.into()),
        }));
    };
    let attachments: Vec<AttachmentReference> = embed_slots
        .iter()
        .filter_map(|slot| match slot {
            EmbedSlot::Attachment(reference) => Some(reference.clone()),
            EmbedSlot::Unknown { .. } => None,
        })
        .collect();
    let base = DecodedNote {
        title,
        title_line,
        body_text: body_text.clone(),
        markdown_text: body_text.clone(),
        format: None,
        title_stripped: false,
        embed_slots,
        attachments,
        table_markers: Vec::new(),
        publishable: false,
        unpublishable_reason: None,
    };

    // Runs that end within the text (as `embed_slots_of` found) decode.
    let Ok(paragraphs) = decode_note_format(&body_text, &string.attribute_run) else {
        return NoteDecodeResult::Ok(Box::new(DecodedNote {
            unpublishable_reason: Some(OVERSHOOTING_RUNS_REASON.into()),
            ..base
        }));
    };
    let strip_title = options.title_mode == TitleMode::Filename
        && paragraphs
            .first()
            .is_some_and(|title| !title.text.contains(OBJECT_REPLACEMENT_CHARACTER));
    let projected = if strip_title {
        paragraphs[1..].to_vec()
    } else {
        paragraphs.clone()
    };

    if strip_title && projected.is_empty() {
        return NoteDecodeResult::Ok(Box::new(DecodedNote {
            markdown_text: String::new(),
            format: Some(paragraphs),
            title_stripped: true,
            publishable: true,
            ..base
        }));
    }

    // The Markdown that reads back as the note, or as near to it as Markdown
    // comes: push keeps what it doesn't show as the note has it until the
    // paragraph is edited (`keep_untouched`).
    let rendered = render_note_markdown(&projected);
    let trimmed: Vec<FormatParagraph> = projected.iter().map(trim_trailing_whitespace).collect();
    let table_markers = tables_kept_as_markers(&trimmed, &rendered, &base.embed_slots);
    NoteDecodeResult::Ok(Box::new(DecodedNote {
        markdown_text: rendered,
        format: Some(paragraphs),
        title_stripped: strip_title,
        table_markers,
        publishable: true,
        ..base
    }))
}

/// Why a note whose formatting runs overshoot its text is read-only (it
/// follows "this note"): which characters they mean can't be told, so
/// neither its formatting nor its embeds could be kept through an edit. Its
/// file is its plain text.
pub const OVERSHOOTING_RUNS_REASON: &str =
    "has formatting that runs past the end of its text, so which text it belongs to can't be told";

/// The tables (indexes into `slots`) whose Markdown table wouldn't read back
/// from where it is written, in place of its U+FFFC: each is written as its
/// embed marker instead, which stays in its place through any edit around
/// it. A table reads back when its placeholder is a plain line of its own
/// (no text before or after it, no list, heading, quote, code or styling
/// around it, any of which keeps the table from being found where it is
/// written, or ends its last row early), and the line after it can't be read
/// as one more of its rows. `paragraphs` are the ones `rendered` writes,
/// their embeds numbered from the first of `slots`.
fn tables_kept_as_markers(paragraphs: &[FormatParagraph], rendered: &str, slots: &[EmbedSlot]) -> Vec<usize> {
    let is_table = |slot: usize| matches!(slots.get(slot), Some(EmbedSlot::Attachment(reference)) if is_table_uti(&reference.type_uti));
    let placeholder = OBJECT_REPLACEMENT_CHARACTER.to_string();
    let mut markers = Vec::new();
    let mut slot = 0;
    for paragraph in paragraphs {
        for _ in paragraph.text.chars().filter(|&c| c == OBJECT_REPLACEMENT_CHARACTER) {
            if is_table(slot)
                && !(paragraph.kind == ParagraphKind::Body
                    && paragraph.block_quote_level == 0
                    && paragraph.text == placeholder)
            {
                markers.push(slot);
            }
            slot += 1;
        }
    }
    let reads_as_row = |line: &str| {
        let line = line.trim();
        line.len() >= 2 && line.starts_with('|') && line.ends_with('|')
    };
    let lines: Vec<&str> = rendered.split('\n').collect();
    let mut slot = 0;
    for (index, line) in lines.iter().enumerate() {
        for _ in line.chars().filter(|&c| c == OBJECT_REPLACEMENT_CHARACTER) {
            // The line after it: a row of its own, or the header of a table
            // written right below it.
            let row_after = lines
                .get(index + 1)
                .is_some_and(|next| reads_as_row(next) || (*next == placeholder && is_table(slot + 1)));
            if is_table(slot) && (*line != placeholder || row_after) {
                markers.push(slot);
            }
            slot += 1;
        }
    }
    markers.sort_unstable();
    markers.dedup();
    markers
}

/// [`DecodedNote::keep_untouched`] on paragraphs: `note` (the note's own,
/// as iCloud has them), `base` (what the Markdown written for them reads
/// back as) and `edited` (what the edited Markdown reads as).
///
/// `base` is lined up with `note`: a paragraph that reads back as itself
/// is a pair, and what lies between two pairs is one stretch (paragraph by
/// paragraph when they have the same texts). `edited` is lined up with
/// `base`. A stretch whose every `base` paragraph is in `edited`, unchanged
/// and together, is sent as `note` has it; any other is sent as `edited`
/// has it.
fn merge_untouched(
    note: &[FormatParagraph],
    base: &[FormatParagraph],
    edited: &[FormatParagraph],
) -> Vec<FormatParagraph> {
    let same = |a: &FormatParagraph, b: &FormatParagraph| paragraph_projections_equal(a, b, None, None);
    let text = |p: &FormatParagraph| trim_trailing_whitespace(p).text;
    let placeholders = |paragraphs: &[FormatParagraph]| {
        paragraphs
            .iter()
            .map(|p| p.text.matches(OBJECT_REPLACEMENT_CHARACTER).count())
            .sum::<usize>()
    };

    // Stretches of `base` and `note` that stand for each other, in order.
    let mut stretches: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let (mut b, mut n) = (0, 0);
    let pairs = common_subsequence(base, note, &same);
    for &(pair_b, pair_n) in pairs.iter().chain(std::iter::once(&(base.len(), note.len()))) {
        let (gap_b, gap_n) = (b..pair_b, n..pair_n);
        if gap_b.len() == gap_n.len()
            && gap_b
                .clone()
                .zip(gap_n.clone())
                .all(|(i, j)| text(&base[i]) == text(&note[j]))
        {
            stretches.extend(gap_b.zip(gap_n).map(|(i, j)| (i..i + 1, j..j + 1)));
        } else if !gap_b.is_empty() || !gap_n.is_empty() {
            stretches.push((gap_b, gap_n));
        }
        if pair_b < base.len() {
            stretches.push((pair_b..pair_b + 1, pair_n..pair_n + 1));
        }
        (b, n) = (pair_b + 1, pair_n + 1);
    }

    // Where each `base` paragraph is in `edited`, unchanged.
    let mut in_edited: Vec<Option<usize>> = vec![None; base.len()];
    for (i, j) in common_subsequence(base, edited, &same) {
        in_edited[i] = Some(j);
    }
    // `edited`'s position just after `base`'s paragraph `i - 1` (0 at the
    // start), when that paragraph is in it unchanged.
    let after = |i: usize| {
        if i == 0 {
            Some(0)
        } else {
            in_edited[i - 1].map(|j| j + 1)
        }
    };

    let mut out: Vec<FormatParagraph> = Vec::new();
    let mut next = 0;
    for (base_range, note_range) in stretches {
        let kept = placeholders(&base[base_range.clone()]) == placeholders(&note[note_range.clone()]);
        if base_range.is_empty() {
            // Paragraphs the note has and the Markdown doesn't show: kept
            // when nothing was put or changed where they are.
            let before = after(base_range.start);
            let at = if base_range.start == base.len() {
                Some(edited.len())
            } else {
                in_edited[base_range.start]
            };
            if kept && before.is_some() && before == at {
                out.extend(edited[next..before.unwrap_or(next).max(next)].iter().cloned());
                next = next.max(before.unwrap_or(next));
                out.extend(note[note_range].iter().cloned());
            }
            continue;
        }
        let found: Vec<Option<usize>> = base_range.clone().map(|i| in_edited[i]).collect();
        let together =
            found.iter().all(Option::is_some) && found.windows(2).all(|pair| pair[1] == pair[0].map(|j| j + 1));
        if kept && together {
            let (first, last) = (found[0].unwrap_or(next), found[found.len() - 1].unwrap_or(next));
            out.extend(edited[next..first.max(next)].iter().cloned());
            out.extend(note[note_range].iter().cloned());
            next = next.max(last + 1);
        } else if let Some(last) = found.iter().flatten().max() {
            out.extend(edited[next..(last + 1).max(next)].iter().cloned());
            next = next.max(last + 1);
        }
    }
    out.extend(edited[next.min(edited.len())..].iter().cloned());
    out
}

/// The pairs `(i, j)` of a longest common subsequence of `a` and `b` under
/// `same`, in order. Past their common start and end, a stretch too long to
/// compare every pair of is taken as having nothing in common.
fn common_subsequence(
    a: &[FormatParagraph],
    b: &[FormatParagraph],
    same: &dyn Fn(&FormatParagraph, &FormatParagraph) -> bool,
) -> Vec<(usize, usize)> {
    const MOST_PAIRS: usize = 4_000_000;
    let prefix = a.iter().zip(b).take_while(|(x, y)| same(x, y)).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| same(x, y))
        .count();
    let (middle_a, middle_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut pairs: Vec<(usize, usize)> = (0..prefix).map(|i| (i, i)).collect();
    let (n, m) = (middle_a.len(), middle_b.len());
    if n > 0 && m > 0 && n.saturating_mul(m) <= MOST_PAIRS {
        let mut lengths = vec![0u32; (n + 1) * (m + 1)];
        let at = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lengths[at(i, j)] = if same(&middle_a[i], &middle_b[j]) {
                    lengths[at(i + 1, j + 1)] + 1
                } else {
                    lengths[at(i + 1, j)].max(lengths[at(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if same(&middle_a[i], &middle_b[j]) && lengths[at(i, j)] == lengths[at(i + 1, j + 1)] + 1 {
                pairs.push((prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if lengths[at(i + 1, j)] >= lengths[at(i, j + 1)] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    pairs.extend((0..suffix).map(|k| (a.len() - suffix + k, b.len() - suffix + k)));
    pairs
}

/// `isDeleted`: the record-level `deleted` flag, a non-zero `Deleted` field,
/// or a note parented to the Trash folder.
pub fn is_deleted(record: &CloudKitRecord) -> bool {
    if record.deleted == Some(true) {
        return true;
    }
    if let Some(Value::Number(n)) = record.fields.get("Deleted").map(|f| &f.value)
        && n.as_f64() != Some(0.0)
    {
        return true;
    }
    matches!(
        record.fields.get("Folder").map(|f| &f.value),
        Some(Value::Object(folder)) if folder.get("recordName") == Some(&Value::String(TRASH_FOLDER_RECORD_NAME.into()))
    )
}

/// `decodeTitleField`.
fn decode_title_field(field: Option<&FieldValue>) -> String {
    match field.map(|f| &f.value) {
        Some(Value::String(value)) => buffer_to_utf8(&base64_decode(value)),
        _ => String::new(),
    }
}
