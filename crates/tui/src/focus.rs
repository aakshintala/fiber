//! Navigate mode's focus order, as a function of the frame's targets and
//! regions alone (`docs/tui.md`, "Moving through the conversation"): every
//! click target on screen is a focus stop, ordered top to bottom and left
//! to right within its area.

use ratatui::layout::{Position, Rect};

use crate::mouse::{Target, TargetId};

/// A focus area: Tab moves conversation, panel, rail, then the
/// conversation again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Area {
    Conversation,
    Panel,
    Rail,
}

/// Where the panel and the rail are drawn, while shown (`docs/tui.md`,
/// "Layout"); #669 sets them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Regions {
    pub(crate) panel: Option<Rect>,
    pub(crate) rail: Option<Rect>,
}

impl Regions {
    /// The area holding `rect`'s top-left cell: the panel's, then the
    /// rail's, else the conversation.
    pub(crate) fn area(&self, rect: Rect) -> Area {
        let at = Position::new(rect.x, rect.y);
        if self.panel.is_some_and(|panel| panel.contains(at)) {
            Area::Panel
        } else if self.rail.is_some_and(|rail| rail.contains(at)) {
            Area::Rail
        } else {
            Area::Conversation
        }
    }
}

/// Each id once, in draw order, with its topmost-leftmost rect: what
/// `order` sorts and `area_of` tests.
fn positions(targets: &[Target]) -> Vec<(TargetId, Rect)> {
    let mut out: Vec<(TargetId, Rect)> = Vec::new();
    for target in targets {
        match out.iter_mut().find(|(id, _)| *id == target.id) {
            Some((_, rect)) => {
                if (target.rect.y, target.rect.x) < (rect.y, rect.x) {
                    *rect = target.rect;
                }
            }
            None => out.push((target.id, target.rect)),
        }
    }
    out
}

/// The stops of `area`: each id among `targets` once, at its
/// topmost-leftmost rect, in (y, x) order, ties in draw order.
pub(crate) fn order(targets: &[Target], regions: &Regions, area: Area) -> Vec<TargetId> {
    let mut stops = positions(targets);
    // The sort is stable, so ties keep draw order.
    stops.sort_by_key(|(_, rect)| (rect.y, rect.x));
    stops
        .into_iter()
        .filter(|(_, rect)| regions.area(*rect) == area)
        .map(|(id, _)| id)
        .collect()
}

/// The area of `id`'s topmost-leftmost rect; None when `id` is not among
/// `targets`.
pub(crate) fn area_of(targets: &[Target], regions: &Regions, id: TargetId) -> Option<Area> {
    positions(targets)
        .into_iter()
        .find(|(seen, _)| *seen == id)
        .map(|(_, rect)| regions.area(rect))
}

/// The first area after `from` in Tab order (Conversation, Panel, Rail,
/// cyclic) whose order is not empty, trying the two others; None when
/// neither has a stop.
pub(crate) fn next_area(targets: &[Target], regions: &Regions, from: Area) -> Option<Area> {
    let after = match from {
        Area::Conversation => [Area::Panel, Area::Rail],
        Area::Panel => [Area::Rail, Area::Conversation],
        Area::Rail => [Area::Conversation, Area::Panel],
    };
    after
        .into_iter()
        .find(|area| !order(targets, regions, *area).is_empty())
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
