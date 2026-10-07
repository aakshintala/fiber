//! Home's pixel logo: `fiber` in pixel letters drawn with half blocks,
//! four rows tall, with each letter's counter shaded (`docs/tui.md`,
//! "The logo"). Two pixel rows make one cell. Where the screen is too
//! short for four rows, home draws the one-row logo instead.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// The wave before the name, two pixels wide and eight tall.
const WAVE: [&str; 8] = ["# ", " #", "# ", " #", "# ", " #", "# ", " #"];
/// The `f`, five pixels wide and eight tall.
const F: [&str; 8] = [
    "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#    ", "#    ",
];
/// The `i`, five pixels wide and eight tall.
const I: [&str; 8] = [
    "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "#####",
];
/// The `b`, with its counter shaded.
const B: [&str; 8] = [
    "#### ", "#..# ", "#..# ", "#### ", "#..# ", "#..# ", "#..# ", "#### ",
];
/// The `e`, with its counter shaded.
const E: [&str; 8] = [
    "#####", "#    ", "#..# ", "#..# ", "#####", "#    ", "#    ", "#####",
];
/// The `r`, five pixels wide and eight tall.
const R: [&str; 8] = [
    "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #", "#   #",
];

/// The letters after the wave, left to right.
const LETTERS: [&[&str; 8]; 5] = [&F, &I, &B, &E, &R];

/// One blank pixel column sits between the wave and each letter.
const GAP: usize = 1;

/// The wave's colour: the accent colour.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const WAVE_COLOUR: Color = Color::Rgb(86, 182, 194);

/// Each letter's step of the name's gradient, one per letter.
/// debt: fixed colours, not theme roles; upgrade when colour roles land
/// (see #685).
const GRADIENT: [Color; 5] = [
    Color::Rgb(97, 175, 239),
    Color::Rgb(86, 182, 194),
    Color::Rgb(152, 195, 121),
    Color::Rgb(229, 192, 123),
    Color::Rgb(198, 120, 221),
];

/// The counters' shade.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const COUNTER_SHADE: Color = Color::Rgb(106, 115, 130);

/// What a pixel is: empty, ink of one glyph, or a letter's counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pixel {
    /// No ink.
    Empty,
    /// The wave's ink.
    Wave,
    /// A letter's ink, by its index into [`LETTERS`].
    Ink(usize),
    /// A letter's counter, by its index into [`LETTERS`].
    Counter(usize),
}

impl Pixel {
    /// The pixel's colour, if it has ink.
    fn colour(self) -> Option<Color> {
        match self {
            Pixel::Empty => None,
            Pixel::Wave => Some(WAVE_COLOUR),
            Pixel::Ink(at) => GRADIENT.get(at).copied(),
            Pixel::Counter(_) => Some(COUNTER_SHADE),
        }
    }
}

/// Two pixel rows make one cell by the half-block rule: both empty is a
/// space; both ink of one colour is █; only the top is ▀ in its colour;
/// only the bottom is ▄ in its colour; two different non-empty pixels
/// are ▀, the top as foreground and the bottom as background.
fn cell(top: char, bottom: char, same: bool) -> char {
    let lit = |pixel: char| pixel != ' ';
    match (lit(top), lit(bottom)) {
        (false, false) => ' ',
        (true, true) if same && top == bottom => '█',
        (true, true) => '▀',
        (true, false) => '▀',
        (false, true) => '▄',
    }
}

/// The logo's pixels: the wave then the letters, one blank column apart,
/// eight rows tall.
fn pixels() -> Vec<Vec<Pixel>> {
    let mut columns: Vec<Vec<Pixel>> = Vec::new();
    // `at` is the letter's index into [`LETTERS`]; `None` is the wave.
    let mut glyph = |bitmap: &[&str; 8], at: Option<usize>| {
        let base = columns.len();
        for (row, line) in bitmap.iter().enumerate() {
            for (col, mark) in line.chars().enumerate() {
                let pixel = match (mark, at) {
                    ('#', None) => Pixel::Wave,
                    ('#', Some(letter)) => Pixel::Ink(letter),
                    ('.', Some(letter)) => Pixel::Counter(letter),
                    _ => Pixel::Empty,
                };
                if row == 0 {
                    columns.push(vec![pixel]);
                } else if let Some(column) = columns.get_mut(base + col) {
                    column.push(pixel);
                }
            }
        }
        for _ in 0..GAP {
            columns.push(vec![Pixel::Empty; bitmap.len()]);
        }
    };
    glyph(&WAVE, None);
    for (at, letter) in LETTERS.iter().enumerate() {
        glyph(letter, Some(at));
    }
    for _ in 0..GAP {
        columns.pop();
    }
    columns
}

/// The four-row logo's width in cells: one cell per pixel column, a
/// blank cell, and the version on the fourth row.
pub(super) fn width_cells(version: &str) -> usize {
    pixels().len() + 1 + crate::format::width(version)
}

/// Draws the four-row logo at `x`, `y`: the pixel letters, with the
/// version dim on the fourth row.
pub(super) fn draw(buf: &mut Buffer, x: u16, y: u16, version: &str) {
    let columns = pixels();
    for (at, column) in columns.iter().enumerate() {
        let col = x.saturating_add(row_as_u16(at));
        for row in 0..4 {
            let upper = column.get(row * 2).copied().unwrap_or(Pixel::Empty);
            let lower = column.get(row * 2 + 1).copied().unwrap_or(Pixel::Empty);
            // One colour is one pixel colour on both halves: the same
            // ink twice, or nothing against anything.
            let same = upper.colour() == lower.colour();
            let mark = cell(mark_of(upper), mark_of(lower), same);
            let style = match (upper.colour(), lower.colour()) {
                (None, None) => Style::default(),
                (Some(top), None) => Style::new().fg(top),
                (None, Some(bottom)) => Style::new().fg(bottom),
                (Some(top), Some(bottom)) => {
                    if mark == '█' {
                        Style::new().fg(top)
                    } else {
                        Style::new().fg(top).bg(bottom)
                    }
                }
            };
            if let Some(into) = buf.cell_mut((col, y.saturating_add(row_as_u16(row)))) {
                into.set_symbol(&mark.to_string());
                into.set_style(style);
            }
        }
    }
    let version_x = x.saturating_add(row_as_u16(columns.len() + 1));
    buf.set_stringn(
        version_x,
        y.saturating_add(3),
        version,
        version.len(),
        Style::new().add_modifier(Modifier::DIM),
    );
}

/// A pixel's mark for the half-block rule: ink or a counter is `#`.
fn mark_of(pixel: Pixel) -> char {
    match pixel {
        Pixel::Empty => ' ',
        Pixel::Wave | Pixel::Ink(_) | Pixel::Counter(_) => '#',
    }
}

/// A pixel row as a cell row: never past the area's last row.
fn row_as_u16(row: usize) -> u16 {
    u16::try_from(row).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;
