//! Where the session screen's regions go: the rail on the left, the
//! conversation column, and the panel on the right, each from the screen's
//! width alone (`docs/tui.md`, "Layout", "The narrow layout", "Shedding").

use ratatui::layout::Rect;

/// The conversation column's minimum width: below it the panel goes away
/// (`docs/tui.md`, "The narrow layout").
pub(crate) const CONVERSATION_MIN: u16 = 84;
/// The rail's floor, in columns.
pub(crate) const RAIL_FLOOR: u16 = 22;
/// The rail's ceiling, in columns.
pub(crate) const RAIL_CEILING: u16 = 48;
/// The panel's floor, in columns.
pub(crate) const PANEL_FLOOR: u16 = 30;
/// The panel's ceiling, in columns.
pub(crate) const PANEL_CEILING: u16 = 60;
/// The rail's default share of the screen's width, in percent: `tui.rail.width`'s
/// built-in default, kept here so the layout and the launch description read
/// one source.
pub const DEFAULT_RAIL_SHARE: f64 = 15.0;
/// The panel's default share of the screen's width, in percent: `tui.panel.width`'s
/// built-in default, kept here so the layout and the launch description read
/// one source.
pub const DEFAULT_PANEL_SHARE: f64 = 21.0;
/// Below this many columns or rows the screen is one line saying so
/// (`docs/tui.md`, "Shedding").
pub(crate) const FLOOR: (u16, u16) = (40, 10);
/// The blank column the conversation's rows keep on their left while the
/// layout applies (`docs/tui.md`, "Layout").
pub(crate) const GUTTER: u16 = 1;
/// `area` with its first `gutter` columns dropped. The gutter is clamped
/// to `area.width`; the result's `right()` equals `area.right()`.
pub(crate) fn past_gutter(area: Rect, gutter: u16) -> Rect {
    let gutter = gutter.min(area.width);
    Rect {
        x: area.x.saturating_add(gutter),
        width: area.width.saturating_sub(gutter),
        ..area
    }
}

/// The rail's and the panel's shares of the screen's width, in percent
/// (`tui.rail.width`, `tui.panel.width`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shares {
    /// The rail's share.
    pub(crate) rail: f64,
    /// The panel's share.
    pub(crate) panel: f64,
}

/// Which regions the session screen wants, before width decides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Want {
    /// The rail: the person has not hidden it.
    pub(crate) rail: bool,
    /// The rail is wanted, hidden or not: a rail not drawn leaves its
    /// grip.
    pub(crate) rail_by_count: bool,
    /// The panel: the person has not hidden it.
    pub(crate) panel: bool,
}

/// The session screen's terminal geometry and its placed regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The rail computed by [`split`], before any extension placement.
    pub(crate) rail: Option<Rect>,
    /// The hidden rail's grip: column 0 while the rail is wanted and not drawn.
    pub(crate) grip: Option<Rect>,
    /// The conversation column computed by [`split`].
    pub(crate) split_column: Rect,
    /// The panel computed by [`split`], before any extension placement.
    pub(crate) panel: Option<Rect>,
    /// The panel is wanted and the screen is too narrow for it: the narrow
    /// layout's rows take its place.
    pub(crate) narrow: bool,
    /// Rectangles for the placed regions in this frame.
    regions: RegionMap,
}

impl Layout {
    /// A layout with the full column reserved for conversation.
    pub(crate) fn bare(column: Rect) -> Self {
        let mut regions = RegionMap::default();
        regions.set(Region::Conversation, Some(column));
        Self {
            rail: None,
            grip: None,
            split_column: column,
            panel: None,
            narrow: false,
            regions,
        }
    }

    /// The full-height column derived from placed regions, or the terminal
    /// split column when no column region is shown.
    pub(crate) fn column(self) -> Rect {
        let Some(column) = self.regions.shown().find_map(|(region, rect)| {
            (!matches!(region, Region::Rail | Region::Panel)).then_some(rect)
        }) else {
            return self.split_column;
        };
        let height = self
            .regions
            .shown()
            .map(|(_, rect)| rect.bottom())
            .max()
            .unwrap_or(0);
        Rect::new(column.x, 0, column.width, height)
    }

    /// The placed rectangles for this layout.
    pub(crate) fn regions(self) -> RegionMap {
        self.regions
    }

    /// Replaces the region map while retaining terminal geometry.
    pub(crate) fn with_regions(mut self, regions: RegionMap) -> Self {
        self.regions = regions;
        self
    }

    /// The rail's placed rectangle, if shown.
    pub(crate) fn rail(self) -> Option<Rect> {
        self.regions.get(Region::Rail)
    }

    /// The panel's placed rectangle, if shown.
    pub(crate) fn panel(self) -> Option<Rect> {
        self.regions.get(Region::Panel)
    }
}

/// A built-in region the layout slot may place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum Region {
    /// The session rail.
    Rail,
    /// The conversation area.
    Conversation,
    /// The working line.
    WorkingLine,
    /// The steering queue.
    SteeringQueue,
    /// The input box.
    InputBox,
    /// The side panel.
    Panel,
}

impl Region {
    /// The regions in the layout input's stable order.
    pub const ALL: [Self; 6] = [
        Self::Rail,
        Self::Conversation,
        Self::WorkingLine,
        Self::SteeringQueue,
        Self::InputBox,
        Self::Panel,
    ];

    /// The Lua and documentation name for this region.
    pub fn name(self) -> &'static str {
        match [
            "rail",
            "conversation",
            "working_line",
            "steering_queue",
            "input_box",
            "panel",
        ]
        .get(self as usize)
        {
            Some(name) => name,
            None => "",
        }
    }

    /// The built-in region named by `name`, if any.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|region| region.name() == name)
    }
}

/// Rectangles for the built-in regions shown in one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RegionMap {
    rects: [Option<Rect>; Region::ALL.len()],
}

impl RegionMap {
    /// The rectangle for `region`, if it is shown.
    pub(crate) fn get(self, region: Region) -> Option<Rect> {
        self.rects.get(region as usize).copied().flatten()
    }

    /// The shown regions, in the layout input's stable order.
    pub(crate) fn shown(self) -> impl Iterator<Item = (Region, Rect)> {
        Region::ALL
            .into_iter()
            .filter_map(move |region| self.get(region).map(|rect| (region, rect)))
    }

    /// Sets `region` to `rect`, or removes it when `rect` is absent.
    pub(crate) fn set(&mut self, region: Region, rect: Option<Rect>) {
        if let Some(slot) = self.rects.get_mut(region as usize) {
            *slot = rect;
        }
    }
}

/// The one line a screen below [`FLOOR`] shows, naming its size; `None`
/// at the floor or above.
pub(crate) fn floor_line(width: u16, height: u16) -> Option<String> {
    let (columns, rows) = FLOOR;
    (width < columns || height < rows)
        .then(|| format!("Fiber needs {columns}×{rows} · now {width}×{height}"))
}

/// A draggable edge: the rail's last column, the panel's first, or the
/// hidden rail's grip (`docs/tui.md`, "Layout", "Shedding").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Rail,
    Panel,
    Grip,
}

/// The edge in column `col` of `layout`, if any.
pub(crate) fn edge_at(layout: &Layout, col: u16) -> Option<Edge> {
    if layout
        .rail()
        .is_some_and(|rail| col == rail.right().saturating_sub(1))
    {
        return Some(Edge::Rail);
    }
    if layout.panel().is_some_and(|panel| col == panel.x) {
        return Some(Edge::Panel);
    }
    if layout.grip.is_some_and(|grip| col == grip.x) {
        return Some(Edge::Grip);
    }
    None
}

/// The edge's column as a rect, full height; `None` when that region is
/// not drawn.
pub(crate) fn edge_rect(layout: &Layout, edge: Edge) -> Option<Rect> {
    match edge {
        Edge::Rail => layout
            .rail()
            .map(|rail| Rect::new(rail.right().saturating_sub(1), rail.y, 1, rail.height)),
        Edge::Panel => layout
            .panel()
            .map(|panel| Rect::new(panel.x, panel.y, 1, panel.height)),
        Edge::Grip => layout.grip,
    }
}

/// The width a drag of `edge` to column `col` gives on a `screen`-wide
/// layout, before the rail's floor: the rail (and the grip) `col + 1`,
/// the panel `screen - col` kept at its floor; each kept at its ceiling
/// and so the conversation keeps CONVERSATION_MIN beside what else is
/// drawn.
#[allow(
    clippy::manual_clamp,
    reason = "a resize can cross the bounds, where `clamp` panics"
)]
pub(crate) fn dragged(edge: Edge, col: u16, screen: u16, layout: &Layout) -> u16 {
    match edge {
        Edge::Rail => {
            let panel = layout.panel().map_or(0, |panel| panel.width);
            col.saturating_add(1).min(RAIL_CEILING).min(
                screen
                    .saturating_sub(CONVERSATION_MIN)
                    .saturating_sub(panel),
            )
        }
        Edge::Grip => col
            .saturating_add(1)
            .min(RAIL_CEILING)
            .min(screen.saturating_sub(CONVERSATION_MIN)),
        Edge::Panel => {
            let left = layout.rail().map_or_else(
                || layout.grip.map_or(0, |grip| grip.width),
                |rail| rail.width,
            );
            screen
                .saturating_sub(col)
                .max(PANEL_FLOOR)
                .min(PANEL_CEILING)
                .min(screen.saturating_sub(CONVERSATION_MIN).saturating_sub(left))
        }
    }
}

/// `width` as a share of `screen`, in percent rounded to one decimal.
pub(crate) fn share_for(width: u16, screen: u16) -> f64 {
    (f64::from(width) * 1000.0 / f64::from(screen)).round() / 10.0
}

/// A region's width: `share` percent of `width`, rounded, kept from `floor`
/// to `ceiling`. `round` takes a half away from zero, which for a
/// width is half up.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to floor..=ceiling, both u16, before the cast"
)]
pub(crate) fn share_of(width: u16, share: f64, floor: u16, ceiling: u16) -> u16 {
    let target = f64::from(width) * share / 100.0;
    target.round().clamp(f64::from(floor), f64::from(ceiling)) as u16
}

/// Splits a `width` by `height` screen. The rail sheds before the panel:
/// the panel's fit never counts the rail. A wanted rail that is not
/// drawn leaves a one-column grip at the left edge, and the panel shows
/// only while the conversation keeps its minimum beside the grip.
pub(crate) fn split(width: u16, height: u16, shares: &Shares, want: &Want) -> Layout {
    let panel_w = share_of(width, shares.panel, PANEL_FLOOR, PANEL_CEILING);
    let rail_w = share_of(width, shares.rail, RAIL_FLOOR, RAIL_CEILING);
    let fits = |taken: u16| {
        width
            .checked_sub(taken)
            .is_some_and(|left| left >= CONVERSATION_MIN)
    };
    let panel_fits = want.panel && fits(panel_w);
    let beside = if panel_fits { panel_w } else { 0 };
    let rail_shown = want.rail && fits(rail_w.saturating_add(beside));
    let grip_w = u16::from(want.rail_by_count && !rail_shown);
    let panel_shown = want.panel && fits(grip_w.saturating_add(panel_w));
    let left = if rail_shown { rail_w } else { grip_w };
    let right = if panel_shown { panel_w } else { 0 };
    let rail = rail_shown.then(|| Rect::new(0, 0, rail_w, height));
    let grip = (grip_w > 0).then(|| Rect::new(0, 0, grip_w, height));
    let panel = panel_shown.then(|| Rect::new(width.saturating_sub(right), 0, right, height));
    let column = Rect::new(
        left,
        0,
        width.saturating_sub(left).saturating_sub(right),
        height,
    );
    let mut regions = RegionMap::default();
    regions.set(Region::Rail, rail);
    regions.set(Region::Conversation, Some(column));
    regions.set(Region::Panel, panel);
    Layout {
        rail,
        grip,
        split_column: column,
        panel,
        narrow: want.panel && !panel_shown,
        regions,
    }
}

/// The session screen's rail width and conversation column range at
/// `width` by `height` with the default shares and wants: the rail 15%,
/// the panel 21%, both wanted. The rail width is 0 while the rail is not
/// drawn; the range is the conversation column's columns.
pub fn session_columns(width: u16, height: u16) -> (u16, std::ops::Range<u16>) {
    let shares = Shares {
        rail: DEFAULT_RAIL_SHARE,
        panel: DEFAULT_PANEL_SHARE,
    };
    let want = Want {
        rail: true,
        rail_by_count: true,
        panel: true,
    };
    let layout = split(width, height, &shares, &want);
    let rail = layout.rail().map_or(0, |rail| rail.width);
    let start = layout.column().x;
    let end = start.saturating_add(layout.column().width);
    (rail, start..end)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
