//! Home's logo: a copy of real Fiber's pixel logo (`crates/tui/src/logo.rs`:
//! `fiber` in pixel letters drawn with half blocks, four rows tall, with
//! each letter's counter shaded) and the kitty-graphics image that replaces
//! it where the terminal speaks the protocol. Its own job is the logo, so
//! `home.rs` does not carry it.

use crate::{BLUE, CYAN, HD, SX_COM, SX_KW, SX_STR, dim, fg, sp};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

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
const WAVE_COLOUR: Color = BLUE;

/// Each letter's step of the name's gradient, one per letter: heading,
/// accent, string, type, keyword, the order of `GRADIENT` in real Fiber's
/// `logo.rs`.
const GRADIENT: [Color; 5] = [HD, BLUE, SX_STR, CYAN, SX_KW];

/// The counters' shade: the prototype's muted grey. A half-block
/// background needs a colour, not the DIM modifier.
const COUNTER_SHADE: Color = SX_COM;

/// The four-row logo's size in cells: the image's cell count depends on it.
pub(crate) const CELLS_W: u16 = 32;
pub(crate) const CELLS_H: u16 = 4;

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

/// A pixel's mark for the half-block rule: ink or a counter is `#`.
fn mark_of(pixel: Pixel) -> char {
    match pixel {
        Pixel::Empty => ' ',
        Pixel::Wave | Pixel::Ink(_) | Pixel::Counter(_) => '#',
    }
}

/// The logo's width in cells: one cell per pixel column.
pub(crate) fn pixel_width() -> usize {
    pixels().len()
}

/// The four-row logo: exactly 4 rows, each `CELLS_W` cells of logo then,
/// on row 4 only, one blank and the version dim. One span per cell. With
/// `image`, the logo's cells are blank spaces (the mask is transparent in
/// its holes, so pixel letters would show through under the image) while
/// the version still reads dim past them.
pub(crate) fn rows(version: &str, image: bool) -> Vec<Vec<Span<'static>>> {
    let columns = pixels();
    // The pixel columns fill exactly the cells the image places.
    debug_assert_eq!(pixel_width(), CELLS_W as usize);
    let mut out = vec![Vec::new(); CELLS_H as usize];
    for (r, line) in out.iter_mut().enumerate() {
        for column in &columns {
            if image {
                line.push(Span::raw(" "));
                continue;
            }
            let upper = column.get(r * 2).copied().unwrap_or(Pixel::Empty);
            let lower = column.get(r * 2 + 1).copied().unwrap_or(Pixel::Empty);
            // One colour is one pixel colour on both halves: the same
            // ink twice, or nothing against anything.
            let same = upper.colour() == lower.colour();
            let mark = cell(mark_of(upper), mark_of(lower), same);
            let style = match (upper.colour(), lower.colour()) {
                (None, None) => Style::new(),
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
            line.push(Span::styled(mark.to_string(), style));
        }
        if r == CELLS_H as usize - 1 {
            line.push(Span::raw(" "));
            // One span per cell, so tests and painters can address each column.
            for ch in version.chars() {
                line.push(Span::styled(ch.to_string(), dim()));
            }
        }
    }
    out
}

/// The one-row logo for short screens: `⌇ fiber 0.0.1`, the ⌇ in accent,
/// each letter of the name bold in its gradient colour, the version dim.
pub(crate) fn one_row(version: &str) -> Vec<Span<'static>> {
    let mut out = vec![sp("⌇", fg(BLUE)), sp(" ", Style::new())];
    for (k, ch) in "fiber".chars().enumerate() {
        out.push(sp(
            ch.to_string(),
            fg(GRADIENT[k]).add_modifier(Modifier::BOLD),
        ));
    }
    out.push(sp(" ", Style::new()));
    // One span per cell, so tests and painters can address each column.
    for ch in version.chars() {
        out.push(sp(ch.to_string(), dim()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn painted(version_rows: Vec<Vec<Span<'static>>>) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 4));
        for (y, line) in version_rows.iter().enumerate() {
            for (x, s) in line.iter().enumerate() {
                let c = &mut buf[(x as u16, y as u16)];
                c.set_symbol(s.content.as_ref());
                c.set_style(s.style);
            }
        }
        buf
    }

    #[test]
    fn the_wave_is_accent_half_blocks() {
        let buf = painted(rows("0.0.1", false));
        assert_eq!(buf[(0, 0)].symbol(), "▀");
        assert_eq!(buf[(0, 0)].fg, BLUE);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
        assert_eq!(buf[(1, 0)].symbol(), "▄");
        assert_eq!(buf[(1, 0)].fg, BLUE);
        assert_eq!(buf[(0, 3)].symbol(), "▀");
        assert_eq!(buf[(1, 3)].symbol(), "▄");
    }

    #[test]
    fn full_ink_columns_are_full_blocks() {
        let buf = painted(rows("0.0.1", false));
        for r in 0..4 {
            assert_eq!(buf[(3, r)].symbol(), "█", "f's first column, row {r}");
            assert_eq!(buf[(3, r)].fg, HD, "f's first column, row {r}");
        }
        for r in 0..4 {
            assert_eq!(buf[(11, r)].symbol(), "█", "i's middle column, row {r}");
            assert_eq!(buf[(11, r)].fg, BLUE, "i's middle column, row {r}");
        }
    }

    #[test]
    fn counters_are_shaded_under_ink() {
        let buf = painted(rows("0.0.1", false));
        assert_eq!(buf[(16, 0)].symbol(), "▀");
        assert_eq!(buf[(16, 0)].fg, SX_STR);
        assert_eq!(buf[(16, 0)].bg, SX_COM);
        assert_eq!(buf[(16, 1)].symbol(), "▀");
        assert_eq!(buf[(16, 1)].fg, SX_COM);
        assert_eq!(buf[(16, 1)].bg, SX_STR);
        assert_eq!(buf[(16, 2)].symbol(), "█");
        assert_eq!(buf[(16, 2)].fg, SX_COM);
        assert_eq!(buf[(16, 3)].symbol(), "▀");
        assert_eq!(buf[(16, 3)].fg, SX_COM);
        assert_eq!(buf[(16, 3)].bg, SX_STR);
    }

    #[test]
    fn gaps_are_blank() {
        let buf = painted(rows("0.0.1", false));
        for x in [2, 8, 14, 20, 26] {
            for r in 0..4 {
                assert_eq!(buf[(x, r)].symbol(), " ", "gap column {x}, row {r}");
            }
        }
    }

    #[test]
    fn the_version_is_dim_past_one_blank() {
        let buf = painted(rows("0.0.1", false));
        let got = painted(rows("0.0.1", false));
        assert_eq!(got[(32, 3)].symbol(), " ");
        let mut text = String::new();
        for x in 33..38 {
            text.push_str(buf[(x, 3)].symbol());
            assert!(
                buf[(x, 3)].modifier.contains(Modifier::DIM),
                "version not dim at {x}"
            );
        }
        assert_eq!(text, "0.0.1");
        for r in 0..3 {
            assert_eq!(got[(32, r)].symbol(), " ", "row {r} runs past the logo");
        }
    }

    #[test]
    fn the_logo_is_32_cells_wide() {
        assert_eq!(pixel_width(), 32);
    }

    #[test]
    fn one_row_logo() {
        let line = one_row("0.0.1");
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        for (x, s) in line.iter().enumerate() {
            let c = &mut buf[(x as u16, 0)];
            c.set_symbol(s.content.as_ref());
            c.set_style(s.style);
        }
        let mut text = String::new();
        for x in 0..13 {
            text.push_str(buf[(x, 0)].symbol());
        }
        assert_eq!(text, "⌇ fiber 0.0.1");
        assert_eq!(buf[(0, 0)].fg, BLUE);
        for (k, x) in (2..7).enumerate() {
            assert_eq!(buf[(x, 0)].fg, GRADIENT[k], "letter at {x}");
            assert!(
                buf[(x, 0)].modifier.contains(Modifier::BOLD),
                "letter at {x} not bold"
            );
        }
        for x in 8..13 {
            assert!(
                buf[(x, 0)].modifier.contains(Modifier::DIM),
                "version not dim at {x}"
            );
        }
    }
}
