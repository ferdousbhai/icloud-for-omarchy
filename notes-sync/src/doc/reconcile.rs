//! Formatting reconciliation: rewrite a document's attribute runs to carry a
//! desired format model. Ports icloud-md `src/notes/formatReconcile.ts`.

use std::collections::{BTreeSet, HashSet};

use super::Result;
use super::document::{NoteDocument, apply_formatting_op};
use super::format::{
    FormatParagraph, InlineStyle, ParagraphKind, decode_note_format, normalize_spans, paragraph_projections_equal,
};
use super::proto::Message;
use super::proto::topotext::{AttributeRun, Font, ParagraphStyle, Todo};

/// `ReconcileResult`: `Ok(changed)`, or `Err(reason)` (`{ok: false, reason}`).
/// A thrown error in TS is the outer `Err` of [`reconcile_note_format`] -
/// push words the two refusals differently.
pub type ReconcileResult = std::result::Result<bool, String>;

const MISALIGNED: &str = "the note's paragraphs don't line up with the edited text - refusing to guess";

/// `reconcileNoteFormat`, minting todo uuids with `randomUUID`.
pub fn reconcile_note_format(
    doc: &mut NoteDocument,
    desired: &[FormatParagraph],
    replica_id: &[u8; 16],
) -> Result<ReconcileResult> {
    reconcile_note_format_with(doc, desired, replica_id, &mut || *uuid::Uuid::new_v4().as_bytes())
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

    let changed: Vec<usize> = (0..desired.len())
        .filter(|&i| {
            needs_fresh_todo_uuid.contains(&i)
                || needs_start_repair.contains(&i)
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
}

fn text_len(p: &FormatParagraph) -> usize {
    super::js::utf16_len(&p.text)
}

fn build_paragraph_plan<'a>(
    current: &'a FormatParagraph,
    desired: &'a FormatParagraph,
    is_last: bool,
    force_fresh_todo_uuid: bool,
    todo_uuid: [u8; 16],
) -> ParagraphPlan<'a> {
    let start = current.start;
    let end = start + text_len(current) + usize::from(!is_last);
    ParagraphPlan {
        current,
        desired,
        start,
        end,
        current_spans: span_intervals(current, end),
        desired_spans: span_intervals(desired, end),
        todo_uuid,
        force_fresh_todo_uuid,
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
    overlay_paragraph_style(piece, plan);
    let current = style_at(&plan.current_spans, piece_start);
    let desired = style_at(&plan.desired_spans, piece_start);
    if current != desired {
        overlay_inline_style(piece, &current, &desired);
    }
}

fn effective_start(start_number: u32) -> u32 {
    if start_number == 0 { 1 } else { start_number }
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
        && effective_start(current.start_number) != effective_start(desired.start_number);
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
        ps.starting_list_item_number = if effective_start(desired.start_number) == 1 {
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
            && effective_start(desired.start_number) != 1)
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
        piece.font = font_name.map(|name| Font {
            name: Some(name.into()),
            ..Default::default()
        });
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
