//! One turn's card as surfaces: the prompt bubble above, the entries and
//! the ending on the turn tint with half-block edges, broken at a
//! handoff's band (`docs/tui.md`, "Look", "Turns", "Handoff"). A child of
//! `turn`, so it reads the card's fields.

use contract::events::TurnOutcome;
use jiff::tz::TimeZone;

use super::{Ending, Entry, Turn};
use crate::format;
use crate::rows::Rows;
use crate::surface::{self, Edges};
use crate::theme::Role;

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
        crate::work::add(|work| work.turn_rows += 1);
        let prompts_at = out.len();
        for prompt in &self.prompts {
            let before = out.len();
            super::images::bubble_rows(prompt, width, layout, out);
            if out.len() > before {
                out.gap(before);
                if let Some(time) = crate::local_time::time_of_day(self.started, zone) {
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
        for entry in &self.entries {
            if let Entry::Band(band) = entry {
                if !broken {
                    first = piece_drew;
                    broken = true;
                }
                out.on_surface(from, width, Role::Turn, Edges { top, bottom: true });
                let band_at = out.len();
                band.rows(out);
                out.on_surface(
                    band_at,
                    width,
                    Role::Surface,
                    Edges {
                        top: false,
                        bottom: false,
                    },
                );
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
                        group.rows(
                            self.is_open() && self.open_group == Some(*at),
                            inner,
                            layout,
                            out,
                        );
                    }
                }
                Entry::Aside(aside) => aside.rows(out),
                Entry::Band(_) => {}
                Entry::Answers(answered) => answered.rows(out),
            });
            piece_drew = piece_drew || drew;
            if drew {
                separates = true;
                after_band = false;
            }
        }
        if let Some((ending, ts)) = &self.ended {
            let drew = block(out, inner, pad, separates, |out| {
                let head = match ending {
                    Ending::Done(done) => {
                        if let (TurnOutcome::Failed, Some(error)) = (done.outcome, &done.error) {
                            format::failure(error, out);
                        }
                        match done.outcome {
                            TurnOutcome::Completed => "▣ completed",
                            TurnOutcome::Interrupted => "▣ interrupted",
                            TurnOutcome::Failed => "▣ failed",
                        }
                    }
                    Ending::CutShort => "▣ cut short: Fiber stopped",
                };
                let ms = ts.saturating_sub(self.started);
                let closing = format::closing(head, ms, self.calls, &self.spend.usage());
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
        if led && !first && !last && !after_band && self.is_open() {
            out.gap(out.len());
        }
        Pieces { first, last }
    }
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
