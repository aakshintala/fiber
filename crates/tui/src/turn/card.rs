//! One turn's card as surfaces: the prompt bubble above, the entries and
//! the ending on the turn tint with half-block edges, broken at a
//! handoff's band (`docs/tui.md`, "Look", "Turns", "Handoff"). A child of
//! `turn`, so it reads the card's fields.

use contract::Seq;
use contract::events::{TurnCompleted, TurnOutcome};
use jiff::tz::TimeZone;

use super::{Ending, Entry, Turn};
use crate::format;
use crate::rows::Rows;
use crate::surface::{self, Edges};
use crate::theme::Role;

/// The whole-turn totals a page's draw supplies for the turn they
/// close: the draw renders them without cloning or mutating the card.
pub(crate) struct DrawSummary<'a> {
    pub(crate) started: u64,
    pub(crate) calls: u64,
    pub(crate) spend: &'a format::Spend,
    pub(crate) ended: Option<&'a (TurnCompleted, u64)>,
}

/// Whether the card's first piece and its last piece drew any row, edges
/// not counted; one piece with no band. A card with no piece row: both
/// false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pieces {
    pub(crate) first: bool,
    pub(crate) last: bool,
}

/// Draws one block's rows with `draw` at the card's inner width, padding
/// them past it; a blank row separates it from the block before when both
/// drew, or from the piece's withheld top edge. Whether this block drew.
fn block(
    out: &mut Rows,
    inner: u16,
    pad: u16,
    separates: bool,
    draw: impl FnOnce(&mut Rows),
) -> bool {
    let at = out.len();
    draw(out);
    if out.len() == at {
        return false;
    }
    out.pad(at, pad, inner);
    if separates {
        out.gap(at);
    }
    true
}

impl Turn {
    /// Notes `seq`, the line just folded into the card, as the source of
    /// what it began: the bubble on the card's first line, each entry it
    /// added, and the ending once the card closes. Entries a line with
    /// no `seq` added take the next line's that has one.
    pub(crate) fn note_seq(&mut self, seq: Option<Seq>) {
        if self.first_seq.is_none() {
            self.first_seq = seq;
        }
        self.entry_seqs.resize(self.entries.len(), None);
        if let Some(seq) = seq {
            for slot in self
                .entry_seqs
                .iter_mut()
                .rev()
                .take_while(|slot| slot.is_none())
            {
                *slot = Some(seq);
            }
        }
        if self.ended.is_some() && self.end_seq.is_none() {
            self.end_seq = seq;
        }
    }

    /// The bubble and its time row, then the card's pieces on `turn`,
    /// each broken at a handoff's band; `edges` says whether the first
    /// piece draws its top edge and the last its bottom one
    /// (`docs/tui.md`, "Look", "Turns", "Handoff"). `layout` names
    /// the session's images, so each draws as its one clickable line
    /// (`docs/tui.md`, "Images").
    pub(crate) fn rows(
        &self,
        width: u16,
        zone: &TimeZone,
        edges: Edges,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) -> Pieces {
        self.rows_with_summary(width, zone, edges, layout, None, out)
    }

    /// The same card with the whole-turn totals `summary` names drawn in
    /// place of the card's own, for the turn the summary closes; any
    /// other turn draws its own fields. The view is borrowed, so recounts
    /// neither clone nor mutate the open turn.
    pub(crate) fn rows_with_summary(
        &self,
        width: u16,
        zone: &TimeZone,
        edges: Edges,
        layout: &crate::image::Layout,
        summary: Option<DrawSummary<'_>>,
        out: &mut Rows,
    ) -> Pieces {
        crate::work::add(|work| work.turn_rows += 1);
        let started = summary.as_ref().map_or(self.started, |view| view.started);
        let calls = summary.as_ref().map_or(self.calls, |view| view.calls);
        let spend: &format::Spend = summary.as_ref().map_or(&self.spend, |view| view.spend);
        let open = summary.as_ref().and_then(|view| view.ended).is_none() && self.is_open();
        let prompts_at = out.len();
        out.set_source(self.first_seq);
        for prompt in &self.prompts {
            let before = out.len();
            super::images::bubble_rows(prompt, width, layout, out);
            if out.len() > before {
                out.gap(before);
                if let Some(time) = crate::local_time::time_of_day(started, zone) {
                    out.push((format::dim(format!("you {time} ")).right_aligned(), None));
                }
            }
        }
        let card_at = out.len();
        let led = card_at > prompts_at;
        let inner = surface::inset(width);
        let pad = width.saturating_sub(inner) / 2;
        let mut from = out.len();
        let mut top = edges.top;
        let mut first = false;
        let mut broken = false;
        // Whether the next drawn block separates from the block before
        // or from the piece's withheld top edge with a blank row.
        let mut separates = !top;
        let mut piece_drew = false;
        // Whether a band drew since the last block: no blank row goes
        // next to a handoff band (`docs/tui.md`, "Turns", "Handoff").
        let mut after_band = false;
        for (index, entry) in self.entries.iter().enumerate() {
            out.set_source(self.entry_seqs.get(index).copied().flatten());
            if let Entry::Band(band) = entry {
                if !broken {
                    first = piece_drew;
                    broken = true;
                }
                out.on_surface(from, width, Role::Turn, Edges { top, bottom: true });
                band.rows(width, zone, out);
                from = out.len();
                top = true;
                separates = false;
                piece_drew = false;
                after_band = true;
                continue;
            }
            let gap_before = separates;
            let drew = block(out, inner, pad, gap_before, |out| match entry {
                Entry::Reply { reply, .. } => reply.rows(inner, out),
                Entry::Steer(steered) => steered.rows(inner, zone, layout, out),
                Entry::Group(at) => {
                    if let Some(group) = self.groups.get(*at) {
                        group.rows(open && self.open_group == Some(*at), inner, layout, out);
                    }
                }
                Entry::Aside(aside) => aside.rows(inner, out),
                Entry::Band(_) => {}
                Entry::Answers(answered) => answered.rows(inner, zone, out),
            });
            piece_drew = piece_drew || drew;
            if drew {
                separates = true;
                after_band = false;
            }
        }
        let summary_ended = summary.as_ref().and_then(|view| view.ended);
        if summary_ended.is_some() || self.ended.is_some() {
            out.set_source(self.end_seq);
            let drew = block(out, inner, pad, separates, |out| {
                let head = match summary_ended {
                    Some((done, _)) => {
                        if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error) {
                            format::failure(error, out);
                        }
                        match done.outcome {
                            TurnOutcome::Completed => "▣ completed",
                            TurnOutcome::Interrupted => "▣ interrupted",
                            TurnOutcome::Failed => "▣ failed",
                        }
                    }
                    None => match &self.ended {
                        Some((Ending::Done(done), _)) => {
                            if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error)
                            {
                                format::failure(error, out);
                            }
                            match done.outcome {
                                TurnOutcome::Completed => "▣ completed",
                                TurnOutcome::Interrupted => "▣ interrupted",
                                TurnOutcome::Failed => "▣ failed",
                            }
                        }
                        _ => "▣ cut short: Fiber stopped",
                    },
                };
                let ts = summary_ended.map_or_else(
                    || self.ended.as_ref().map_or(0, |(_, ts)| *ts),
                    |(_, ts)| *ts,
                );
                let ms = ts.saturating_sub(started);
                let closing = format::closing(head, ms, calls, &spend.usage());
                out.push((format::dim(closing), None));
            });
            piece_drew = piece_drew || drew;
            if drew {
                after_band = false;
            }
        }
        let last = piece_drew;
        if !broken {
            first = last;
        }
        out.on_surface(
            from,
            width,
            Role::Turn,
            Edges {
                top,
                bottom: edges.bottom,
            },
        );
        if led && (first || last) {
            out.gap(card_at);
        }
        // A bubble whose card draws nothing yet, on a turn still
        // running: the blank row one fold has ahead of its card, in the
        // same place the blank before the card takes once it draws
        // (`docs/tui.md`, "Turns", "History and paging").
        if led && !last && !after_band {
            out.gap(out.len());
        }
        out.set_source(None);
        Pieces { first, last }
    }
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
