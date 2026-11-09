//! The feedback seam (`docs/architecture.md`, "The call rules"): the
//! report builder lives in `cli`, which `tui` may not call, so `contract`
//! defines the writer type and `main` injects it. Vocabulary only: no
//! behaviour beyond the types both sides name.

use std::path::PathBuf;
use std::sync::Arc;

use crate::SessionId;

/// Writes a feedback report for `description` and the session on screen,
/// when there is one, with the screen's captured frame shapes, and
/// returns its path with the issue link: the injected writer `main`
/// supplies, so the TUI names this seam and never `cli`.
pub type WriteFeedback = Arc<
    dyn Fn(&str, Option<&SessionId>, &[FrameShape]) -> Result<FeedbackDone, String> + Send + Sync,
>;

/// How many painted frames the screen keeps for a feedback report: 20
/// wheel steps at 3 rows cover 60 rows, more than a 48-row screen, so a
/// full screen of scrolling stays in the ring. Picked, not measured.
pub const RING_DEPTH: usize = 20;

/// One colour channel of a shaped cell: the 16 ANSI names, an 8-bit
/// index, a truecolour triple, or the terminal default. Closed, so the
/// report renderer matches it exhaustively and no other value can reach
/// the wire. Holds no glyph and no string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeColor {
    /// The terminal default.
    Reset,
    /// ANSI black.
    Black,
    /// ANSI red.
    Red,
    /// ANSI green.
    Green,
    /// ANSI yellow.
    Yellow,
    /// ANSI blue.
    Blue,
    /// ANSI magenta.
    Magenta,
    /// ANSI cyan.
    Cyan,
    /// ANSI white.
    Gray,
    /// ANSI bright black.
    DarkGray,
    /// ANSI bright red.
    LightRed,
    /// ANSI bright green.
    LightGreen,
    /// ANSI bright yellow.
    LightYellow,
    /// ANSI bright blue.
    LightBlue,
    /// ANSI bright magenta.
    LightMagenta,
    /// ANSI bright cyan.
    LightCyan,
    /// ANSI bright white.
    White,
    /// An 8-bit colour index.
    Indexed(u8),
    /// A truecolour triple.
    Rgb(u8, u8, u8),
}

/// One shaped cell's painted style: foreground, background, underline
/// colour and modifier bits, each the value the paint pass left on the
/// cell. Holds no glyph and no string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleValue {
    /// The cell's painted foreground.
    pub fg: ShapeColor,
    /// The cell's painted background.
    pub bg: ShapeColor,
    /// The cell's painted underline colour.
    pub ul: ShapeColor,
    /// The cell's modifier bits, passed through opaquely.
    pub mods: u16,
}

/// What a shaped cell holds: blank or content, a wide lead's
/// continuation, or a cell the diff omits. Closed, so the report
/// renderer matches it exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    /// A blank narrow cell.
    Blank,
    /// A content narrow cell.
    Content,
    /// A column continuing a wide lead.
    Continuation,
    /// A cell the diff omits.
    Exempt,
}

/// One shaped cell: its kind, whether it leads a wide cell, and its
/// palette slot. Fields stay private so only the constructor builds
/// one: no string or glyph can be smuggled into a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeCell {
    kind: ShapeKind,
    wide: bool,
    style: usize,
}

impl ShapeCell {
    /// Builds the cell the encoder classified: its kind, whether it
    /// leads a wide cell, and its palette slot.
    pub fn new(kind: ShapeKind, wide: bool, style: usize) -> Self {
        Self { kind, wide, style }
    }

    /// What the cell holds.
    pub fn kind(&self) -> ShapeKind {
        self.kind
    }

    /// Whether the cell leads a wide cell.
    pub fn wide(&self) -> bool {
        self.wide
    }

    /// The cell's slot in its frame's palette.
    pub fn style(&self) -> usize {
        self.style
    }
}

/// One shaped row: its cells, in column order. Built only through the
/// constructor, so no string or glyph can be smuggled into a shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeRow {
    cells: Vec<ShapeCell>,
}

impl ShapeRow {
    /// Builds a row from its cells, in column order.
    pub fn new(cells: impl IntoIterator<Item = ShapeCell>) -> Self {
        Self {
            cells: cells.into_iter().collect(),
        }
    }

    /// The row's cells, in column order.
    pub fn cells(&self) -> &[ShapeCell] {
        &self.cells
    }
}

/// One captured frame's shape: its geometry, the scroll state that
/// produced it, its palette and its rows, oldest first in the ring.
/// Holds no glyph and no string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameShape {
    /// The frame's width in cells.
    pub width: u16,
    /// The frame's height in cells.
    pub height: u16,
    /// The top wrapped row while scrolled up; `None` follows.
    pub top: Option<usize>,
    /// Whether newer rows wait below the viewport.
    pub has_new: bool,
    /// The frame's styles by first appearance.
    pub palette: Vec<StyleValue>,
    /// The frame's rows, top first.
    pub rows: Vec<ShapeRow>,
}

/// Classifies one cell's width for the shape alphabet, from flags and a
/// width alone, never a symbol string: widths 0 and 1 are narrow, 2 is
/// wide with one continuation, and 3 or more is wide with `width - 1`
/// continuations. Exempt short-circuits with no trailing: the diff
/// omits these cells. Returns the kind, whether the cell leads a wide
/// cell, and how many continuations follow it.
pub fn encode_cell(width: u16, blank: bool, exempt: bool) -> (ShapeKind, bool, u16) {
    if exempt {
        return (ShapeKind::Exempt, false, 0);
    }
    let kind = if blank {
        ShapeKind::Blank
    } else {
        ShapeKind::Content
    };
    match width {
        0 | 1 => (kind, false, 0),
        2 => (kind, true, 1),
        _ => (kind, true, width - 1),
    }
}

/// Reads the `doctor` report the headless door prints and the `/doctor`
/// view shows: the injected reader `main` supplies, so the TUI names
/// this seam and never `cli`. The failure carries the capture's text, as
/// the headless door prints it.
pub type ReadDoctor = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// What a feedback write returns: the report's path and its issue link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackDone {
    /// The report file written under the current directory.
    pub path: PathBuf,
    /// The new-issue link carrying the report, or its description.
    pub link: String,
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod tests;
