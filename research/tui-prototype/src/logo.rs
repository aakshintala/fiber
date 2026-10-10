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

/// The four-row logo: exactly 4 rows, each `CELLS_W` cells of logo then,
/// on row 4 only, one blank and the version dim. One span per cell. With
/// `image`, the logo's cells are blank spaces (the mask is transparent in
/// its holes, so pixel letters would show through under the image) while
/// the version still reads dim past them.
pub(crate) fn rows(version: &str, image: bool) -> Vec<Vec<Span<'static>>> {
    let columns = pixels();
    // The pixel columns fill exactly the cells the image places.
    debug_assert_eq!(columns.len(), CELLS_W as usize);
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

// ============================================================ the kitty image
// The escape fields below (`a=t/p/d`, `f=32`, `s`, `v`, `i`, `p`, `c`,
// `r`, `C=1`, `q=2`, `m`, `d=i/I`, 4096-byte chunks) come from the kitty
// graphics protocol specification, not from a probe of it. They are
// provisional until the owner's live run in Ghostty, kitty and WezTerm.

/// The alpha mask real Fiber builds from the font with `cargo xtask
/// logo-mask` and checks in: 640 by 160 pixels of 8-bit alpha, row-major.
/// A wrong length fails to compile.
const MASK: &[u8; 102_400] = include_bytes!("../assets/logo-mask.bin");
/// The mask's size in pixels.
const W: usize = 640;
const H: usize = 160;
/// The wave's width in mask pixels (`WAVE_PX` in `xtask/src/logo.rs`).
const WAVE_PX: usize = 60;
/// Each letter's first mask column, read off the checked-in mask: its
/// all-zero column runs are 60..=103, 188..=208, 292..=308, 389..=408,
/// 490..=513 and 595..=639, so f, i, b, e, r occupy 104..=187, 209..=291,
/// 309..=388, 409..=489, 514..=594. Columns 60..=103 have no ink, so
/// their tint never shows.
const LETTER_STARTS: [usize; 5] = [104, 209, 309, 409, 514];

/// Whether this terminal speaks kitty graphics: the kitty, Ghostty or
/// WezTerm terminal, never under tmux or screen. Reads the environment,
/// never a terminal query, so the first frame never waits on it.
/// iTerm2 and Sixel are out; home draws the pixel logo there.
pub(crate) fn supported(env: impl Fn(&str) -> Option<String>) -> bool {
    if env("TMUX").is_some() || env("STY").is_some() {
        return false;
    }
    let term = env("TERM").unwrap_or_default();
    if term.starts_with("tmux") || term.starts_with("screen") {
        return false;
    }
    if env("KITTY_WINDOW_ID").is_some() {
        return true;
    }
    if term == "xterm-kitty" || term == "xterm-ghostty" {
        return true;
    }
    matches!(
        env("TERM_PROGRAM").as_deref(),
        Some("ghostty") | Some("WezTerm")
    )
}

/// A mask column's tint: the wave is accent blue, each letter its step of
/// the name's gradient.
fn tint(x: usize) -> Color {
    if x < WAVE_PX {
        return WAVE_COLOUR;
    }
    GRADIENT[LETTER_STARTS.partition_point(|&s| s <= x).saturating_sub(1)]
}

fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

/// The image's bytes: straight (not premultiplied) RGBA, row-major. The
/// image has no shaded counters: the mask's counters are holes (alpha 0),
/// so they stay transparent. Counter shading is the pixel logo's.
pub(crate) fn rgba() -> Vec<u8> {
    let mut out = Vec::with_capacity(W * H * 4);
    for (i, &a) in MASK.iter().enumerate() {
        let (r, g, b) = rgb_of(tint(i % W));
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

/// Sends the image once per run: RGBA `f=32`, image id 1, base64 in
/// chunks of at most 4096 bytes, `m=1` on all but the last chunk.
pub(crate) fn transmit() -> String {
    // Base64 grows by 4/3, so 4096-byte payloads stay a multiple of 4
    // except the last one.
    const CHUNK: usize = 4096;
    let b64 = crate::b64(&rgba());
    let mut out = String::new();
    let mut it = b64.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = it.next() {
        let last = it.peek().is_none();
        let m = if last { 0 } else { 1 };
        if first {
            out.push_str(&format!(
                "\x1b_Ga=t,f=32,s={W},v={H},i=1,q=2,m={m};{}",
                String::from_utf8_lossy(chunk)
            ));
            first = false;
        } else {
            out.push_str(&format!("\x1b_Gm={m};{}", String::from_utf8_lossy(chunk)));
        }
        out.push_str("\x1b\\");
    }
    out
}

/// Puts image 1 over the logo's cells at `x`, `y`: 32 columns by 4 rows,
/// replacing placement 1, in column mode so text never shifts.
pub(crate) fn place(x: u16, y: u16) -> String {
    format!(
        "\x1b[{};{}H\x1b_Ga=p,i=1,p=1,c=32,r=4,C=1,q=2\x1b\\",
        y + 1,
        x + 1
    )
}

/// Hides the image's placements, leaving the transmitted image for reuse.
pub(crate) const HIDE: &str = "\x1b_Ga=d,d=i,i=1,q=2\x1b\\";
/// Frees the image on the way out of home.
pub(crate) const FREE: &str = "\x1b_Ga=d,d=I,i=1,q=2\x1b\\";

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

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|&&(kk, _)| kk == k)
                .map(|&(_, v)| v.to_string())
        }
    }

    #[test]
    fn support_follows_the_environment() {
        // Each row: the environment, then whether the image is supported.
        let cases: &[(&[(&str, &str)], bool)] = &[
            (&[], false),
            (&[("KITTY_WINDOW_ID", "1")], true),
            (&[("TERM", "xterm-kitty")], true),
            (&[("TERM", "xterm-ghostty")], true),
            (&[("TERM", "xterm-256color")], false),
            (&[("TERM_PROGRAM", "ghostty")], true),
            (&[("TERM_PROGRAM", "WezTerm")], true),
            (&[("TERM_PROGRAM", "iTerm.app")], false),
            (&[("TERM_PROGRAM", "ghostty"), ("TMUX", "x")], false),
            (&[("TERM_PROGRAM", "ghostty"), ("STY", "x")], false),
            (
                &[("KITTY_WINDOW_ID", "1"), ("TERM", "screen-256color")],
                false,
            ),
            (
                &[("KITTY_WINDOW_ID", "1"), ("TERM", "tmux-256color")],
                false,
            ),
        ];
        for (pairs, want) in cases {
            assert_eq!(supported(env_of(pairs)), *want, "env {pairs:?}");
        }
    }

    fn column_has_ink(x: usize) -> bool {
        (0..H).any(|y| MASK[y * W + x] > 0)
    }

    fn column_is_clear(x: usize) -> bool {
        (0..H).all(|y| MASK[y * W + x] == 0)
    }

    #[test]
    fn letter_starts_match_the_mask() {
        for &s in &LETTER_STARTS {
            assert!(column_is_clear(s - 1), "column {} has ink", s - 1);
            assert!(column_has_ink(s), "column {s} is clear");
        }
        assert!(column_has_ink(59), "the wave ends before column 59");
        for x in 60..104 {
            assert!(column_is_clear(x), "column {x} has ink");
        }
    }

    #[test]
    fn tint_boundaries() {
        // The wave is accent blue; each letter its gradient step. HD reads
        // as ORANGE and SX_KW as BLUE in value; the table names the constants.
        let cases: &[(usize, Color)] = &[
            (0, BLUE),
            (59, BLUE),
            (60, HD),
            (103, HD),
            (104, HD),
            (208, HD),
            (209, BLUE),
            (308, BLUE),
            (309, SX_STR),
            (408, SX_STR),
            (409, CYAN),
            (513, CYAN),
            (514, SX_KW),
            (639, SX_KW),
        ];
        for &(x, want) in cases {
            assert_eq!(tint(x), want, "column {x}");
        }
    }

    #[test]
    fn rgba_carries_tint_and_alpha() {
        let bytes = rgba();
        assert_eq!(bytes.len(), 409_600);
        for &x in &[30, 150, 550] {
            let y = (0..H).find(|&y| MASK[y * W + x] > 0).unwrap();
            let (r, g, b) = rgb_of(tint(x));
            let at = (y * W + x) * 4;
            assert_eq!(&bytes[at..at + 4], &[r, g, b, MASK[y * W + x]]);
        }
        let clear = (0..H * W).find(|&i| MASK[i] == 0).unwrap();
        assert_eq!(bytes[clear * 4 + 3], 0);
    }

    #[test]
    fn transmit_chunks() {
        let t = transmit();
        assert!(
            t.starts_with("\x1b_Ga=t,f=32,s=640,v=160,i=1,q=2,m=1;"),
            "the first chunk does not transmit"
        );
        // Every chunk ends at its terminator; nothing trails the last one.
        let mut parts: Vec<&str> = t.split("\x1b\\").collect();
        assert_eq!(parts.pop(), Some(""));
        let (first, last) = (parts[0], parts[parts.len() - 1]);
        assert!(first.starts_with("\x1b_Ga=t,"), "first chunk: {first:?}");
        for c in &parts[1..parts.len() - 1] {
            assert!(c.starts_with("\x1b_Gm=1;"), "a middle chunk: {c:?}");
        }
        assert!(last.starts_with("\x1b_Gm=0;"), "the last chunk: {last:?}");
        // Base64 carries no `;`, so the payload follows the last one.
        let payloads: Vec<&str> = parts
            .iter()
            .map(|c| c.split(';').next_back().unwrap())
            .collect();
        for p in &payloads[..payloads.len() - 1] {
            assert!(p.len() <= 4096, "a chunk carries more than 4096 bytes");
            assert_eq!(p.len() % 4, 0, "a chunk is not base64-aligned");
        }
        assert!(payloads.last().unwrap().len() <= 4096);
        let joined: String = payloads.concat();
        assert_eq!(joined, crate::b64(&rgba()));
        assert_eq!(joined.len(), 546_136);
    }

    #[test]
    fn place_moves_then_places() {
        assert_eq!(
            place(38, 2),
            "\x1b[3;39H\x1b_Ga=p,i=1,p=1,c=32,r=4,C=1,q=2\x1b\\"
        );
        assert_eq!(HIDE, "\x1b_Ga=d,d=i,i=1,q=2\x1b\\");
        assert_eq!(FREE, "\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    }

    #[test]
    fn image_rows_blank_the_letters() {
        let buf = painted(rows("0.0.1", true));
        for y in 0..4 {
            for x in 0..32 {
                assert_eq!(buf[(x, y)].symbol(), " ", "cell ({x}, {y})");
                assert_eq!(buf[(x, y)].fg, Color::Reset, "cell ({x}, {y})");
                assert_eq!(buf[(x, y)].bg, Color::Reset, "cell ({x}, {y})");
            }
        }
        let mut text = String::new();
        for x in 33..38 {
            text.push_str(buf[(x, 3)].symbol());
            assert!(
                buf[(x, 3)].modifier.contains(Modifier::DIM),
                "version not dim at {x}"
            );
        }
        assert_eq!(text, "0.0.1");
    }
}
