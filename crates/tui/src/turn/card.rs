//! One turn's card as surfaces: the entries and the ending on the surface
//! tint with half-block edges, broken at a handoff's band
//! (`docs/tui.md`, "Look", "Turns", "Handoff"). A child of `turn`, so it
//! reads the card's fields.

use contract::events::TurnOutcome;
use jiff::tz::TimeZone;

use super::{Ending, Entry, Turn};
use crate::format;
use crate::rows::Rows;
use crate::surface::Edges;
use crate::theme::Role;

/// Whether the card's first piece and its last piece drew any row, edges
/// not counted; one piece with no band. A card with no piece row: both
/// false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pieces {
    pub(crate) first: bool,
    pub(crate) last: bool,
}

impl Turn {
    /// The bubble and its time row, then the card's pieces on `surface`,
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
        for prompt in &self.prompts {
            let before = out.len();
            super::images::bubble_rows(prompt, width, layout, out);
            if out.len() > before
                && let Some(time) = crate::local_time::time_of_day(self.started, zone)
            {
                out.push((format::dim(time).right_aligned(), None));
            }
        }
        let mut from = out.len();
        let mut top = edges.top;
        let mut first = false;
        let mut broken = false;
        for entry in &self.entries {
            if let Entry::Band(band) = entry {
                if !broken {
                    first = out.len() > from;
                    broken = true;
                }
                out.on_surface(from, width, Role::Surface, Edges { top, bottom: true });
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
                continue;
            }
            match entry {
                Entry::Reply { reply, .. } => reply.rows(width, out),
                Entry::Steer(steered) => steered.rows(width, zone, layout, out),
                Entry::Group(at) => {
                    if let Some(group) = self.groups.get(*at) {
                        group.rows(
                            self.is_open() && self.open_group == Some(*at),
                            width,
                            layout,
                            out,
                        );
                    }
                }
                Entry::Aside(aside) => aside.rows(out),
                Entry::Band(_) => {}
                Entry::Answers(answered) => answered.rows(out),
            }
        }
        if let Some((ending, ts)) = &self.ended {
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
        }
        let last = out.len() > from;
        if !broken {
            first = last;
        }
        out.on_surface(
            from,
            width,
            Role::Surface,
            Edges {
                top,
                bottom: edges.bottom,
            },
        );
        Pieces { first, last }
    }
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
