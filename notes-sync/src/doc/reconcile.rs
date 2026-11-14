//! Formatting reconciliation: rewrite a document's attribute runs to carry a
//! desired format model. Originally derived from icloud-md.

use std::collections::{BTreeSet, HashSet};

use super::Result;
use super::document::{NoteDocument, apply_formatting_op};
use super::format::{
    FormatParagraph, InlineStyle, ParagraphKind, decode_note_format, effective_start_number, normalize_spans,
    paragraph_projections_equal,
};
use super::proto::Message;
use super::proto::topotext::{AttributeRun, Font, ParagraphStyle, Todo};

/// `ReconcileResult`: `Ok(changed)`, or `Err(reason)` (`{ok: false, reason}`).
/// A thrown error in TS is the outer `Err` of [`reconcile_note_format`] -
/// push words the two refusals differently.
pub type ReconcileResult = std::result::Result<bool, String>;

const MISALIGNED: &str = "the note's paragraphs don't line up with the edited text - refusing to guess";

/// `reconcileNoteFormat`, minting todo uuids with `randomUUID`
/// (`vault::rt::random_uuid`, so the differential harness's deterministic
/// sequence lines up with icloud-md's).
pub fn reconcile_note_format(
    doc: &mut NoteDocument,
    desired: &[FormatParagraph],
    replica_id: &[u8; 16],
) -> Result<ReconcileResult> {
    reconcile_note_format_with(doc, desired, replica_id, &mut || {
        uuid_string_bytes(&crate::vault::rt::random_uuid())
    })
}

/// `uuidBytes()`: a `randomUUID()` string's hex digits as 16 bytes.
fn uuid_string_bytes(uuid: &str) -> [u8; 16] {
    let hex: Vec<u8> = uuid.bytes().filter(|b| *b != b'-').collect();
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = hex
            .get(2 * i..2 * i + 2)
            .and_then(|p| std::str::from_utf8(p).ok())
            .unwrap_or("00");
        *byte = u8::from_str_radix(pair, 16).unwrap_or(0);
    }
    out
}

/// `reconcileNoteFormat` with an injectable uuid source (called once per
/// rewritten paragraph, in order, exactly where TS calls `randomUUID`).
pub fn reconcile_note_format_with(
    doc: &mut NoteDocument,
    desired: &[FormatParagraph],
    replica_id: &[u8; 16],
    mint_uuid: &mut dyn FnMut() -> [u8; 16],
) -> Result<ReconcileResult> {
    let current = match decode_note_format(&doc.text, &doc.attribute_runs) {
        Ok(paragraphs) => paragraphs,
        Err(reason) => return Ok(Err(reason)),
    };
    if current.len() != desired.len() || current.iter().zip(desired).any(|(c, d)| c.text != d.text) {
        return Ok(Err(MISALIGNED.into()));
    }
    let last = current.len().saturating_sub(1);

    let mut uuid_owners: HashSet<Vec<u8>> = HashSet::new();
    let mut needs_fresh_todo_uuid: HashSet<usize> = HashSet::new();
    for (i, paragraph) in current.iter().enumerate() {
        if paragraph.kind != ParagraphKind::TodoList || desired[i].kind != ParagraphKind::TodoList {
            continue;
        }
        let Some(uuid) = todo_uuid_of_paragraph(doc, paragraph, i == last) else {
            continue;
        };
        if uuid.is_empty() {
            continue;
        }
        if !uuid_owners.insert(uuid) {
            needs_fresh_todo_uuid.insert(i);
        }
    }

    let mut needs_start_repair: HashSet<usize> = HashSet::new();
    for (i, paragraph) in current.iter().enumerate() {
        if paragraph.kind == ParagraphKind::NumberedList
            && desired[i].kind == ParagraphKind::NumberedList
            && has_explicit_zero_start(doc, paragraph, i == last)
        {
            needs_start_repair.insert(i);
        }
    }

    let own_styles = own_styles_of_mixed_paragraphs(doc, &current);

    let changed: Vec<usize> = (0..desired.len())
        // The note's last paragraph, when empty, has no character to carry
        // a style (`note_holds_format`): nothing is written for it.
        .filter(|&i| !(i == last && current[i].text.is_empty()))
        .filter(|&i| {
            needs_fresh_todo_uuid.contains(&i)
                || needs_start_repair.contains(&i)
                || own_styles[i].is_some()
                || !paragraph_projections_equal(
                    &current[i],
                    &desired[i],
                    i.checked_sub(1).map(|p| &current[p]),
                    i.checked_sub(1).map(|p| &desired[p]),
                )
        })
        .collect();
    if changed.is_empty() {
        return Ok(Ok(false));
    }

    let plans: Vec<ParagraphPlan> = changed
        .iter()
        .map(|&index| {
            build_paragraph_plan(
                &current[index],
                &desired[index],
                index == desired.len() - 1,
                needs_fresh_todo_uuid.contains(&index),
                mint_uuid(),
                own_styles[index].clone(),
            )
        })
        .collect();
    doc.attribute_runs = rewrite_attribute_runs(&doc.attribute_runs, &plans)?;
    let ranges: Vec<(usize, usize)> = plans.iter().map(|p| (p.start, p.end)).collect();
    apply_formatting_op(doc, &ranges, replica_id)?;
    Ok(Ok(true))
}

struct SpanInterval {
    start: usize,
    end: usize,
    style: InlineStyle,
}

struct ParagraphPlan<'a> {
    current: &'a FormatParagraph,
    desired: &'a FormatParagraph,
    start: usize,
    end: usize,
    current_spans: Vec<SpanInterval>,
    desired_spans: Vec<SpanInterval>,
    todo_uuid: [u8; 16],
    force_fresh_todo_uuid: bool,
    /// The paragraph's own style, when some of its runs carry another
    /// ([`own_styles_of_mixed_paragraphs`]).
    own_style: Option<Option<ParagraphStyle>>,
}

fn text_len(p: &FormatParagraph) -> usize {
    crate::js::len16(&p.text)
}

fn build_paragraph_plan<'a>(
    current: &'a FormatParagraph,
    desired: &'a FormatParagraph,
    is_last: bool,
    force_fresh_todo_uuid: bool,
    todo_uuid: [u8; 16],
    own_style: Option<Option<ParagraphStyle>>,
) -> ParagraphPlan<'a> {
    let start = current.start;
    let end = start + text_len(current) + usize::from(!is_last);
    // A monospaced paragraph shows none of the styling its runs carry
    // (`normalize_spans`); made another kind it shows all of it, so that is
    // what the wanted styling is compared with.
    let shown;
    let styled = if current.kind == ParagraphKind::Monospaced && desired.kind != ParagraphKind::Monospaced {
        shown = FormatParagraph {
            kind: desired.kind,
            ..current.clone()
        };
        &shown
    } else {
        current
    };
    ParagraphPlan {
        current,
        desired,
        start,
        end,
        current_spans: span_intervals(styled, end),
        desired_spans: span_intervals(desired, end),
        todo_uuid,
        force_fresh_todo_uuid,
        own_style,
    }
}

/// Runs overlapping the paragraph's range (newline included), in order.
fn overlapping_runs<'a>(
    doc: &'a NoteDocument,
    paragraph: &FormatParagraph,
    is_last: bool,
) -> impl Iterator<Item = &'a AttributeRun> {
    let start = paragraph.start;
    let end = start + text_len(paragraph) + usize::from(!is_last);
    let mut offset = 0usize;
    doc.attribute_runs.iter().filter(move |run| {
        let run_start = offset;
        let run_end = offset + run.len() as usize;
        offset = run_end;
        run_start < end && run_end > start
    })
}

/// What a run's paragraph style says of the paragraph, as far as the format
/// model reads it (`decode_note_format`).
#[derive(PartialEq)]
struct ParagraphFacts<'a> {
    style: u32,
    indent: i32,
    block_quote_level: u32,
    todo: Option<(&'a [u8], bool)>,
    start_number: u32,
}

fn paragraph_facts(style: Option<&ParagraphStyle>) -> ParagraphFacts<'_> {
    let kind = style.and_then(|ps| ps.style).unwrap_or(ParagraphKind::Body.style());
    let is = |wanted: ParagraphKind| kind == wanted.style();
    ParagraphFacts {
        style: kind,
        indent: style.and_then(|ps| ps.indent).unwrap_or(0),
        block_quote_level: style.and_then(|ps| ps.block_quote_level).unwrap_or(0),
        todo: style
            .and_then(|ps| ps.todo.as_ref())
            .filter(|_| is(ParagraphKind::TodoList))
            .map(|todo| (todo.todo_uuid.as_deref().unwrap_or_default(), todo.done == Some(1))),
        start_number: style
            .and_then(|ps| ps.starting_list_item_number)
            .filter(|_| is(ParagraphKind::NumberedList))
            .map_or(1, effective_start_number),
    }
}

/// A paragraph is styled by the run that covers its line break (its last
/// character, for the note's last paragraph): that is the one
/// `decode_note_format` reads, and in a note written by Notes every run of
/// the paragraph says the same. Text typed at the start of a line joins the
/// run before it (`adjust_attribute_runs`), which is the line above's: that
/// run then says the paragraph is a title, say, where the paragraph is body
/// text. For each such paragraph, the style of its own run (`Some(None)`: a
/// run without one), which its other runs are given.
fn own_styles_of_mixed_paragraphs(
    doc: &NoteDocument,
    paragraphs: &[FormatParagraph],
) -> Vec<Option<Option<ParagraphStyle>>> {
    let runs = &doc.attribute_runs;
    let mut offset = 0usize;
    let ends: Vec<usize> = runs
        .iter()
        .map(|run| {
            offset += run.len() as usize;
            offset
        })
        .collect();
    paragraphs
        .iter()
        .enumerate()
        .map(|(index, paragraph)| {
            let start = paragraph.start;
            let end = start + text_len(paragraph) + usize::from(index + 1 < paragraphs.len());
            let first = ends.partition_point(|&run_end| run_end <= start);
            let own = ends.partition_point(|&run_end| run_end < end);
            // An empty last paragraph has no run, and runs shorter than the
            // text leave none over the paragraph's end: nothing to go by.
            if end == start || own >= runs.len() {
                return None;
            }
            let own_style = runs[own].paragraph_style.as_ref();
            let facts = paragraph_facts(own_style);
            runs[first..own]
                .iter()
                .any(|run| paragraph_facts(run.paragraph_style.as_ref()) != facts)
                .then(|| own_style.cloned())
        })
        .collect()
}

fn has_explicit_zero_start(doc: &NoteDocument, paragraph: &FormatParagraph, is_last: bool) -> bool {
    overlapping_runs(doc, paragraph, is_last).any(|run| {
        run.paragraph_style
            .as_ref()
            .is_some_and(|ps| ps.starting_list_item_number == Some(0))
    })
}

fn todo_uuid_of_paragraph(doc: &NoteDocument, paragraph: &FormatParagraph, is_last: bool) -> Option<Vec<u8>> {
    overlapping_runs(doc, paragraph, is_last)
        .find_map(|run| run.paragraph_style.as_ref().and_then(|ps| ps.todo.as_ref()))
        .map(|todo| todo.todo_uuid.clone().unwrap_or_default())
}

fn span_intervals(paragraph: &FormatParagraph, paragraph_end: usize) -> Vec<SpanInterval> {
    let mut out: Vec<SpanInterval> = Vec::new();
    let mut at = paragraph.start;
    for span in normalize_spans(paragraph) {
        if span.length == 0 {
            continue;
        }
        out.push(SpanInterval {
            start: at,
            end: at + span.length,
            style: span.style,
        });
        at += span.length;
    }
    match out.last_mut() {
        Some(last) if last.end < paragraph_end => last.end = paragraph_end,
        Some(_) => {}
        None if paragraph_end > at => out.push(SpanInterval {
            start: at,
            end: paragraph_end,
            style: InlineStyle::PLAIN,
        }),
        None => {}
    }
    out
}

fn rewrite_attribute_runs(runs: &[AttributeRun], plans: &[ParagraphPlan]) -> Result<Vec<AttributeRun>> {
    let mut boundaries: BTreeSet<usize> = BTreeSet::new();
    for plan in plans {
        boundaries.insert(plan.start);
        boundaries.insert(plan.end);
        for interval in plan.current_spans.iter().chain(&plan.desired_spans) {
            boundaries.insert(interval.start);
            boundaries.insert(interval.end);
        }
    }

    // (run, minted here) - only minted pieces may merge.
    let mut out: Vec<(AttributeRun, bool)> = Vec::new();
    let mut offset = 0usize;
    for run in runs {
        let run_start = offset;
        let run_end = offset + run.len() as usize;
        offset = run_end;
        if !plans.iter().any(|p| run_start < p.end && run_end > p.start) {
            out.push((run.clone(), false));
            continue;
        }
        let mut cuts = vec![run_start];
        cuts.extend(boundaries.iter().copied().filter(|&b| b > run_start && b < run_end));
        cuts.push(run_end);
        for pair in cuts.windows(2) {
            let (piece_start, piece_end) = (pair[0], pair[1]);
            let mut piece = run.clone();
            piece.length = Some((piece_end - piece_start) as u32);
            if let Some(plan) = plans.iter().find(|p| piece_start >= p.start && piece_end <= p.end) {
                overlay_piece(&mut piece, plan, piece_start);
            }
            out.push((piece, true));
        }
    }
    merge_encodable_equal_runs(out)
}

fn style_at(intervals: &[SpanInterval], at: usize) -> InlineStyle {
    intervals
        .iter()
        .find(|i| at >= i.start && at < i.end)
        .map_or(InlineStyle::PLAIN, |i| i.style.clone())
}

fn overlay_piece(piece: &mut AttributeRun, plan: &ParagraphPlan, piece_start: usize) {
    if let Some(own) = &plan.own_style
        && paragraph_facts(piece.paragraph_style.as_ref()) != paragraph_facts(own.as_ref())
    {
        piece.paragraph_style = own.clone();
    }
    overlay_paragraph_style(piece, plan);
    let current = style_at(&plan.current_spans, piece_start);
    let desired = style_at(&plan.desired_spans, piece_start);
    if current != desired {
        overlay_inline_style(piece, &current, &desired);
    }
}

fn done_flag(paragraph: &FormatParagraph) -> u32 {
    u32::from(paragraph.done == Some(true))
}

fn overlay_paragraph_style(piece: &mut AttributeRun, plan: &ParagraphPlan) {
    let (current, desired) = (plan.current, plan.desired);
    if current.kind.projected() != desired.kind.projected() {
        piece.paragraph_style = Some(fresh_paragraph_style(desired, piece, plan));
        return;
    }
    let need_indent = desired.kind.is_list() && current.indent != desired.indent;
    let need_quote = current.block_quote_level != desired.block_quote_level;
    let need_done =
        desired.kind == ParagraphKind::TodoList && current.done.unwrap_or(false) != desired.done.unwrap_or(false);
    let need_start = desired.kind == ParagraphKind::NumberedList
        && effective_start_number(current.start_number) != effective_start_number(desired.start_number);
    let need_identity = desired.kind == ParagraphKind::TodoList && plan.force_fresh_todo_uuid;
    let need_start_repair = piece
        .paragraph_style
        .as_ref()
        .is_some_and(|ps| ps.starting_list_item_number == Some(0));
    if !need_indent && !need_quote && !need_done && !need_start && !need_identity && !need_start_repair {
        return;
    }
    let ps = piece.paragraph_style.get_or_insert_with(|| ParagraphStyle {
        style: Some(3),
        alignment: Some(4),
        ..Default::default()
    });
    if need_indent {
        ps.indent = Some(desired.indent);
    }
    if need_quote {
        ps.block_quote_level = Some(desired.block_quote_level);
    }
    if need_start {
        ps.starting_list_item_number = if effective_start_number(desired.start_number) == 1 {
            None
        } else {
            Some(desired.start_number)
        };
    }
    if ps.starting_list_item_number == Some(0) {
        ps.starting_list_item_number = None;
    }
    if need_identity {
        ps.todo = Some(Todo {
            todo_uuid: Some(plan.todo_uuid.to_vec()),
            done: Some(done_flag(desired)),
            ..Default::default()
        });
    } else if need_done {
        match &mut ps.todo {
            Some(todo) => todo.done = Some(done_flag(desired)),
            None => {
                ps.todo = Some(Todo {
                    todo_uuid: Some(plan.todo_uuid.to_vec()),
                    done: Some(done_flag(desired)),
                    ..Default::default()
                })
            }
        }
    }
}

fn fresh_paragraph_style(desired: &FormatParagraph, piece: &AttributeRun, plan: &ParagraphPlan) -> ParagraphStyle {
    let todo = (desired.kind == ParagraphKind::TodoList).then(|| {
        // `piece.paragraphStyle?.todo?.todoUUID ?? plan.todoUuid`: a present
        // todo with no uuid reads as protobuf-es's empty default, not undefined.
        let existing = if plan.force_fresh_todo_uuid {
            None
        } else {
            piece
                .paragraph_style
                .as_ref()
                .and_then(|ps| ps.todo.as_ref())
                .map(|todo| todo.todo_uuid.clone().unwrap_or_default())
        };
        Todo {
            todo_uuid: Some(existing.unwrap_or_else(|| plan.todo_uuid.to_vec())),
            done: Some(done_flag(desired)),
            ..Default::default()
        }
    });
    ParagraphStyle {
        style: Some(desired.kind.projected().style()),
        alignment: Some(4),
        writing_direction: Some(0),
        indent: Some(if desired.kind.is_list() { desired.indent } else { 0 }),
        todo,
        paragraph_hints: Some(0),
        starting_list_item_number: (desired.kind == ParagraphKind::NumberedList
            && effective_start_number(desired.start_number) != 1)
            .then_some(desired.start_number),
        block_quote_level: Some(desired.block_quote_level),
        uuid: Some(Vec::new()),
        ..Default::default()
    }
}

fn overlay_inline_style(piece: &mut AttributeRun, current: &InlineStyle, desired: &InlineStyle) {
    if current.bold != desired.bold || current.italic != desired.italic {
        piece.font_hints = Some(u32::from(desired.bold) | (u32::from(desired.italic) << 1));
        let font_name = match (desired.bold, desired.italic) {
            (true, true) => Some("SFUIText-BoldItalic"),
            (true, false) => Some("SFUIText-Bold"),
            (false, true) => Some("SFUIText-LightItalic"),
            (false, false) => None,
        };
        // Only the font's name says bold or italic: its size (which the
        // Markdown doesn't show) stays.
        let font = Font {
            name: font_name.map(str::to_string),
            ..piece.font.take().unwrap_or_default()
        };
        piece.font = (font != Font::default()).then_some(font);
    }
    if current.strikethrough != desired.strikethrough {
        piece.strikethrough = Some(u32::from(desired.strikethrough));
    }
    if current.underline != desired.underline {
        piece.underline = Some(u32::from(desired.underline));
    }
    if current.link != desired.link {
        piece.link = Some(desired.link.clone());
    }
}

fn merge_encodable_equal_runs(runs: Vec<(AttributeRun, bool)>) -> Result<Vec<AttributeRun>> {
    let mut out: Vec<(AttributeRun, bool)> = Vec::new();
    for (run, rewritten) in runs {
        if let Some((previous, previous_rewritten)) = out.last_mut()
            && *previous_rewritten
            && rewritten
            && previous.attachment_info.is_none()
            && run.attachment_info.is_none()
            && same_fields_ignoring_length(previous, &run)?
        {
            previous.length = Some(previous.len() + run.len());
            continue;
        }
        out.push((run, rewritten));
    }
    Ok(out.into_iter().map(|(run, _)| run).collect())
}

fn same_fields_ignoring_length(a: &AttributeRun, b: &AttributeRun) -> Result<bool> {
    let mut a = a.clone();
    let mut b = b.clone();
    a.length = Some(0);
    b.length = Some(0);
    Ok(a.encode()? == b.encode()?)
}
