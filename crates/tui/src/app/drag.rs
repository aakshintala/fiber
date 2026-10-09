//! Dragging the rail's and the panel's edges: the drag in progress and
//! the shares a release queued to save (`docs/tui.md`, "Layout",
//! "Shedding"). Drawing lives in `crate::view::drag`.

use super::App;
use crate::keys::{Button, Mouse, MouseKind};
use crate::layout::{self, Edge};

/// A share waiting to be saved: its configuration key and its percent.
type QueuedSave = (&'static str, f64);

/// The drag in progress and the shares waiting to be saved.
#[derive(Debug, Default)]
pub(crate) struct DragState {
    drag: Option<Drag>,
    saves: Vec<QueuedSave>,
}

/// One drag: its edge, the share when it started, and whether the rail
/// was last dragged below its floor.
#[derive(Debug, Clone, Copy)]
struct Drag {
    edge: Edge,
    before: f64,
    below: bool,
}

impl App {
    /// Every mouse report, before clicks and selection see it: a left
    /// press on an edge starts a drag, a left drag moves it, the release
    /// ends it.
    pub(crate) fn on_drag(&mut self, mouse: &Mouse) {
        match mouse.kind {
            MouseKind::Press(Button::Left) => self.press_edge(mouse.col),
            MouseKind::Drag(Button::Left) => {
                if self.drag.drag.is_some() {
                    self.drag_to(mouse.col);
                }
            }
            MouseKind::Release => {
                if self.drag.drag.is_some() {
                    self.end_drag();
                }
            }
            MouseKind::Press(_)
            | MouseKind::Drag(_)
            | MouseKind::Motion
            | MouseKind::WheelUp
            | MouseKind::WheelDown => {}
        }
    }

    /// A left press starts a drag on the edge in column `col`, if any,
    /// and replaces a drag whose release never came. The pages rewrap at
    /// the new column width before the frame.
    fn press_edge(&mut self, col: u16) {
        self.drag.drag = self.chrome.layout().and_then(|layout| {
            let edge = layout::edge_at(&layout, col)?;
            let before = match edge {
                Edge::Rail | Edge::Grip => self.home.as_ref()?.launch.rail_share,
                Edge::Panel => self.home.as_ref()?.launch.panel_share,
            };
            Some(Drag {
                edge,
                before,
                below: false,
            })
        });
        self.settle();
    }

    /// A left drag moves the drag to column `col`, writing the live share
    /// so the split follows the pointer. Dragging the grip out past the
    /// rail's floor shows the rail, hiding the panel when both do not fit
    /// (`docs/tui.md`, "Shedding"), and the drag follows as the rail's
    /// edge. Below the floor the split keeps the share while the drag
    /// runs; the release hides the rail.
    fn drag_to(&mut self, col: u16) {
        let Some(edge) = self.drag.drag.map(|drag| drag.edge) else {
            self.settle();
            return;
        };
        let Some(layout) = self.chrome.layout() else {
            self.settle();
            return;
        };
        let screen = self.screen.width();
        let width = layout::dragged(edge, col, screen, &layout);
        match edge {
            Edge::Rail => {
                let below = width < layout::RAIL_FLOOR;
                if let Some(home) = self.home.as_mut() {
                    home.launch.rail_share = layout::share_for(width, screen);
                }
                if let Some(drag) = self.drag.drag.as_mut() {
                    drag.below = below;
                }
            }
            Edge::Grip => {
                if width >= layout::RAIL_FLOOR {
                    if let Some(home) = self.home.as_mut() {
                        home.launch.rail_share = layout::share_for(width, screen);
                    }
                    self.show_rail();
                    if let Some(drag) = self.drag.drag.as_mut() {
                        drag.edge = Edge::Rail;
                    }
                }
            }
            Edge::Panel => {
                if let Some(home) = self.home.as_mut() {
                    home.launch.panel_share = layout::share_for(width, screen);
                }
            }
        }
        self.settle();
    }

    /// The release ends the drag. A rail dragged below its floor hides
    /// until shown and keeps the share it started with; a grip that never
    /// reached the floor changes nothing; otherwise the share the drag
    /// moved to waits to be saved.
    #[allow(
        clippy::float_cmp,
        reason = "both shares come from `share_for`, so an unmoved drag compares exactly equal"
    )]
    fn end_drag(&mut self) {
        let Some(drag) = self.drag.drag.take() else {
            self.settle();
            return;
        };
        if drag.edge == Edge::Rail && drag.below {
            self.chrome.hide_rail();
            if let Some(home) = self.home.as_mut() {
                home.launch.rail_share = drag.before;
            }
            self.settle();
            return;
        }
        if drag.edge == Edge::Grip {
            self.settle();
            return;
        }
        let share = match drag.edge {
            Edge::Rail => self.home.as_ref().map(|home| home.launch.rail_share),
            Edge::Panel => self.home.as_ref().map(|home| home.launch.panel_share),
            Edge::Grip => None,
        };
        let key = match drag.edge {
            Edge::Rail | Edge::Grip => "tui.rail.width",
            Edge::Panel => "tui.panel.width",
        };
        if let Some(share) = share
            && share != drag.before
        {
            self.drag.saves.push((key, share));
        }
        self.settle();
    }

    /// The edge being dragged and its live share, for the pill. A grip
    /// below the floor has no share yet, so it draws none.
    pub(crate) fn dragging(&self) -> Option<(Edge, f64)> {
        match self.drag.drag?.edge {
            Edge::Rail => Some((Edge::Rail, self.home.as_ref()?.launch.rail_share)),
            Edge::Panel => Some((Edge::Panel, self.home.as_ref()?.launch.panel_share)),
            Edge::Grip => None,
        }
    }

    /// The shares a release queued, in order, emptied.
    pub(crate) fn take_saves(&mut self) -> Vec<QueuedSave> {
        std::mem::take(&mut self.drag.saves)
    }

    /// Whether the pointer shape is the resize arrow: a drag runs, or
    /// `at` is on an edge column.
    pub(crate) fn over_edge(&self, at: Option<(u16, u16)>) -> bool {
        if self.drag.drag.is_some() {
            return true;
        }
        let Some(layout) = self.chrome.layout() else {
            return false;
        };
        at.is_some_and(|(col, _)| layout::edge_at(&layout, col).is_some())
    }
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;
