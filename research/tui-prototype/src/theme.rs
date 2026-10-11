//! The light theme, 256 colours and NO_COLOR, for #1634.
//!
//! One global transform at the output chokepoint: [`Themed`] wraps the
//! ratatui backend and maps every cell's colours once per draw, so every
//! existing screen (the conversation, the panel, approvals, the question
//! form, search, `--overlay`, `--home`, `--view-case`, `--rail-case`)
//! gets all four cases with no per-site edits. The look is set once at
//! startup and defaults to dark truecolour, which maps every colour to
//! itself, leaving existing captures byte-identical.
//!
//! `--theme CASE` combines with any other flag. `NO_COLOR` set and
//! non-empty selects depth [`Depth::None`] whatever the flag says.

use crate::cases::{Case, Surface};
use ratatui::backend::Backend;
use ratatui::buffer::Cell;
use ratatui::style::Color;
use std::io::{self, Write};
use std::sync::OnceLock;

/// The built-in theme: dark follows the terminal, light paints its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Theme {
    Dark,
    Light,
}

/// How many colours the terminal gets: truecolour, the xterm 256 palette
/// without its 16 ANSI entries, or none at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Depth {
    True,
    C256,
    None,
}

/// One `--theme` look: which theme, at which depth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Look {
    pub theme: Theme,
    pub depth: Depth,
}

impl Look {
    /// The default: dark truecolour, the identity mapping.
    pub(crate) const DARK: Look = Look {
        theme: Theme::Dark,
        depth: Depth::True,
    };
}

/// The look, set once at startup; unset means the default.
static LOOK: OnceLock<Look> = OnceLock::new();

/// Sets the look for the run. The first call wins; tests pass their look
/// to [`map`] directly and never call this.
pub(crate) fn init(look: Look) {
    let _ = LOOK.set(look);
}

/// The run's look, or the default when startup has not set one.
pub(crate) fn get() -> Look {
    LOOK.get().copied().unwrap_or(Look::DARK)
}

/// Whether `NO_COLOR` turns colour off: set and not empty. Takes its
/// reader so tests inject the environment instead of setting it.
pub(crate) fn no_color_set(env: impl Fn(&str) -> Option<String>) -> bool {
    env("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// The look for `--theme CASE`: the case's look, with `NO_COLOR` forcing
/// depth [`Depth::None`]. Unknown cases stay errors, reported before the
/// terminal is touched so no cleanup is owed.
pub(crate) fn resolve(
    name: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> io::Result<Look> {
    let mut look = match name {
        None => Look::DARK,
        Some(n) => crate::cases::lookup(CASES, n).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unknown --theme case {n:?}; one of: {}",
                    crate::cases::names(CASES)
                ),
            )
        })?,
    };
    if no_color_set(env) {
        look.depth = Depth::None;
    }
    Ok(look)
}

const fn hex(v: u32) -> (u8, u8, u8) {
    ((v >> 16) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8)
}

/// Which part of the xterm palette a colour quantizes to: neutral tints
/// take the grey ramp so they stay neutral, hues take the colour cube so
/// an alert stays red, and text takes whichever entry is nearest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ramp {
    Grey,
    Cube,
    Either,
}

/// A dark value's light value, each side, and whether the role is neutral.
/// Keyed by (dark rgb, `is_bg`) because a background orange carries black
/// text while a foreground orange must read on a light tint.
fn light_for(dark: (u8, u8, u8), is_bg: bool) -> Option<((u8, u8, u8), Ramp)> {
    let (fg, bg, ramp) = match hex_of(dark) {
        // error, removed
        0xff5d73 => (0xcf222e, 0xcf222e, Ramp::Cube),
        // accent, success, added, keyword
        0x6eaafe => (0x0969da, 0x0969da, Ramp::Cube),
        // heading, warning, attention as text; the current match behind
        0xff9f43 if !is_bg => (0xbc4c00, 0xbc4c00, Ramp::Cube),
        0xff9f43 => (0xbc4c00, 0xffd67e, Ramp::Cube),
        // secondary
        0xb39ddb => (0x6f42c1, 0x6f42c1, Ramp::Cube),
        // function, type, constant, info
        0x7dd3fc => (0x0b7285, 0x0b7285, Ramp::Cube),
        // rule, surface_raised
        0x3a3a4a => (0xd0d7de, 0xd0d7de, Ramp::Grey),
        // panel
        0x0c0c11 => (0xf6f8fa, 0xf6f8fa, Ramp::Grey),
        // surface, input box, approval
        0x1a1a22 => (0xeaeef2, 0xeaeef2, Ramp::Grey),
        // prompt bubble
        0x343541 => (0xdde3ea, 0xdde3ea, Ramp::Grey),
        // turn card
        0x101017 => (0xf3f5f8, 0xf3f5f8, Ramp::Grey),
        // code block
        0x181821 => (0xeff1f4, 0xeff1f4, Ramp::Grey),
        // handoff band
        0x1f1a2e => (0xefe9f9, 0xefe9f9, Ramp::Cube),
        // string literals
        0xce9178 => (0xa31515, 0xa31515, Ramp::Cube),
        // number literals
        0xb5cea8 => (0x116329, 0x116329, Ramp::Cube),
        // comments
        0x7a7a8a => (0x6e7781, 0x6e7781, Ramp::Cube),
        // scroll thumb
        0x808080 => (0xb0b7bf, 0xb0b7bf, Ramp::Cube),
        // search match behind white text
        0x5a4a1a => (0xfff1a8, 0xfff1a8, Ramp::Cube),
        // selected text behind
        0x264f78 => (0xb6d7ff, 0xb6d7ff, Ramp::Cube),
        // alert: red in both built-in themes
        0x501c20 => (0xffd7d9, 0xffd7d9, Ramp::Cube),
        // hover tint
        0x1e1e26 => (0xe3e7ec, 0xe3e7ec, Ramp::Grey),
        _ => return None,
    };
    Some((if is_bg { hex(bg) } else { hex(fg) }, ramp))
}

/// Packs three channels back into one `0xrrggbb` for the table above.
const fn hex_of(dark: (u8, u8, u8)) -> u32 {
    ((dark.0 as u32) << 16) | ((dark.1 as u32) << 8) | dark.2 as u32
}

/// A computed tint's ramp: a near-grey (channels within `0x10`) takes the
/// grey ramp, anything with more hue takes the cube.
fn fallback_ramp((r, g, b): (u8, u8, u8)) -> Ramp {
    if r.max(g).max(b) - r.min(g).min(b) <= 0x10 {
        Ramp::Grey
    } else {
        Ramp::Cube
    }
}

fn rgb_to_hsl((r, g, b): (u8, u8, u8)) -> (f64, f64, f64) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb((h, s, l): (f64, f64, f64)) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h * 6.0) as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let up = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (up(r), up(g), up(b))
}

/// Lightness inversion in HSL, keeping the hue: unlisted colours are
/// computed tints (`lift()`, state stripes, literals), so a dark tint
/// comes back light and a light one dark.
fn invert((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let (h, s, l) = rgb_to_hsl((r, g, b));
    hsl_to_rgb((h, s, 1.0 - l))
}

fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// The xterm colour cube's channel levels.
const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The nearest cube level to a channel; ties fall to the lower level.
fn level(v: u8) -> u8 {
    let mut best = 0;
    for (i, &l) in LEVELS.iter().enumerate() {
        if (v as i16 - l as i16).abs() < (v as i16 - LEVELS[best] as i16).abs() {
            best = i;
        }
    }
    best as u8
}

/// The nearest xterm palette entry from 16 to 255: the grey ramp for
/// neutrals, the colour cube for hues, either for text. Squared RGB
/// distance throughout.
fn nearest((r, g, b): (u8, u8, u8), ramp: Ramp) -> u8 {
    let dist = |v: u8| {
        let (dr, dg, db) = (r as i32 - v as i32, g as i32 - v as i32, b as i32 - v as i32);
        (dr * dr + dg * dg + db * db) as u32
    };
    let grey = (0..=23)
        .map(|i| (232 + i, dist(8 + 10 * i)))
        .min_by_key(|&(_, d)| d)
        .map(|(x, _)| x)
        .unwrap_or(255);
    let (lr, lg, lb) = (level(r), level(g), level(b));
    let cube = 16 + 36 * lr + 6 * lg + lb;
    let cubed = {
        let (cr, cg, cb) = (LEVELS[lr as usize], LEVELS[lg as usize], LEVELS[lb as usize]);
        let (dr, dg, db) = (r as i32 - cr as i32, g as i32 - cg as i32, b as i32 - cb as i32);
        (dr * dr + dg * dg + db * db) as u32
    };
    match ramp {
        Ramp::Grey => grey,
        Ramp::Cube => cube,
        Ramp::Either => {
            if dist(8 + 10 * (grey - 232)) <= cubed {
                grey
            } else {
                cube
            }
        }
    }
}

/// Maps one colour under a look: the theme's value, then the depth's.
/// Dark truecolour is the identity; `NO_COLOR` depth maps everything to
/// the default; modifiers are untouched (the caller keeps the cell's).
pub(crate) fn map(look: Look, colour: Color, is_bg: bool) -> Color {
    if look == Look::DARK {
        return colour;
    }
    if look.depth == Depth::None {
        return Color::Reset;
    }
    let (v, ramp): ((u8, u8, u8), Ramp) = match colour {
        // The terminal's own colours stay the default at 256 colours.
        Color::Reset if look.depth == Depth::C256 => return Color::Reset,
        Color::Reset if look.theme == Theme::Dark => return Color::Reset,
        Color::Reset if !is_bg => (hex(0x1f2328), Ramp::Either),
        Color::Reset => (hex(0xffffff), Ramp::Grey),
        // Ink never comes from the 16 ANSI colours: black stays black
        // in both themes, white stays white in dark and reads as the
        // theme's text in light, and both quantize as text.
        Color::Black => (hex(0x000000), Ramp::Either),
        Color::White if look.theme == Theme::Dark => (hex(0xffffff), Ramp::Either),
        Color::White => (hex(0x1f2328), Ramp::Either),
        Color::Rgb(r, g, b) if look.theme == Theme::Dark => ((r, g, b), fallback_ramp((r, g, b))),
        Color::Rgb(r, g, b) => match light_for((r, g, b), is_bg) {
            Some((v, ramp)) => (v, ramp),
            None => {
                let v = (r, g, b);
                (invert(v), fallback_ramp(v))
            }
        },
        // Already indexed: nothing to quantize.
        other => return other,
    };
    if look.depth == Depth::True {
        Color::Rgb(v.0, v.1, v.2)
    } else {
        Color::Indexed(nearest(v, ramp))
    }
}

/// Maps a buffer cell under a look: foreground, background and underline
/// colour go through [`map`]; symbol, modifiers and skip stay.
pub(crate) fn map_cell(look: Look, cell: &Cell) -> Cell {
    let mut out = cell.clone();
    out.fg = map(look, cell.fg, false);
    out.bg = map(look, cell.bg, true);
    out.underline_color = map(look, cell.underline_color, false);
    out
}

/// Whether `clear` must first paint the background: under the light
/// theme unpainted cells would otherwise show the terminal's own (dark)
/// background. Erased cells take the current background on BCE
/// terminals, so the wrapper sets it, clears, and resets it.
pub(crate) fn needs_clear(look: Look) -> bool {
    look.theme == Theme::Light && look.depth == Depth::True
}

/// The backend wrapper: every `draw` maps each cell through [`map_cell`],
/// which is also the OSC 8 links path, since it calls `Backend::draw`.
/// Everything else passes through untouched.
pub(crate) struct Themed<B>(B);

impl<B> Themed<B> {
    pub(crate) fn new(inner: B) -> Self {
        Self(inner)
    }
}

impl<B> Backend for Themed<B>
where
    B: Backend<Error = io::Error> + Write,
{
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let look = get();
        if look == Look::DARK {
            return self.0.draw(content);
        }
        let cells: Vec<(u16, u16, Cell)> = content
            .map(|(x, y, c)| (x, y, map_cell(look, c)))
            .collect();
        self.0.draw(cells.iter().map(|(x, y, c)| (*x, *y, c)))
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.0.get_cursor_position()
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        if needs_clear(get()) {
            write!(self.0, "\x1b[48;2;255;255;255m")?;
        }
        self.0.clear()?;
        if needs_clear(get()) {
            write!(self.0, "\x1b[49m")?;
        }
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        if needs_clear(get())
            && matches!(clear_type, ratatui::backend::ClearType::All)
        {
            write!(self.0, "\x1b[48;2;255;255;255m")?;
        }
        self.0.clear_region(clear_type)?;
        if needs_clear(get())
            && matches!(clear_type, ratatui::backend::ClearType::All)
        {
            write!(self.0, "\x1b[49m")?;
        }
        Ok(())
    }

    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.0.size()
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.0.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Backend::flush(&mut self.0)
    }
}

impl<B: Write> Write for Themed<B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Every `--theme` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "light",
        help: "the light theme in truecolour",
        check: "run `--static --theme light`: the conversation, panel, approval and question form on light tints with dark text; rules and comments dim but legible, the alert red.",
        build: || Look {
            theme: Theme::Light,
            depth: Depth::True,
        },
    },
    Case {
        name: "dark-256",
        help: "the dark theme forced to 256 colours",
        check: "run `--static --theme dark-256`: the same screen through the xterm palette; neutral tints grey, hues kept, the alert still red.",
        build: || Look {
            theme: Theme::Dark,
            depth: Depth::C256,
        },
    },
    Case {
        name: "light-256",
        help: "the light theme forced to 256 colours",
        check: "run `--static --theme light-256`: the light screen through the xterm palette; dark text on light tints, the alert still red.",
        build: || Look {
            theme: Theme::Light,
            depth: Depth::C256,
        },
    },
    Case {
        name: "dark-no-color",
        help: "the dark screen with colour off (also NO_COLOR=1)",
        check: "run `--static --theme dark-no-color`, or with NO_COLOR=1: no colour anywhere, only bold, dim, reversed and underline; edges blank but rows kept.",
        build: || Look {
            theme: Theme::Dark,
            depth: Depth::None,
        },
    },
];

/// `--theme`, for `--help` and `check/theme.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--theme",
    file: "theme",
    title: "Themes",
    docs: || crate::cases::docs(CASES),
};

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: Look = Look {
        theme: Theme::Light,
        depth: Depth::True,
    };
    const DARK_256: Look = Look {
        theme: Theme::Dark,
        depth: Depth::C256,
    };
    const LIGHT_256: Look = Look {
        theme: Theme::Light,
        depth: Depth::C256,
    };
    const NONE: Look = Look {
        theme: Theme::Dark,
        depth: Depth::None,
    };

    fn val(v: u32) -> Color {
        Color::Rgb((v >> 16) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8)
    }

    /// Every role in docs/tui.md's role table, in its order, with the
    /// code's real const or literal and the expected light value.
    /// Roles sharing a dark value share the light value.
    #[test]
    fn the_light_table_covers_every_role() {
        let roles: &[(&str, Color, bool, u32)] = &[
            ("text", Color::Reset, false, 0x1f2328),
            ("muted", Color::Reset, false, 0x1f2328),
            ("accent", crate::BLUE, false, 0x0969da),
            ("heading", crate::HD, false, 0xbc4c00),
            ("success", crate::BLUE, false, 0x0969da),
            ("warning", crate::ORANGE, false, 0xbc4c00),
            ("error", crate::RED, false, 0xcf222e),
            ("attention", crate::ORANGE, false, 0xbc4c00),
            ("added", crate::BLUE, false, 0x0969da),
            ("removed", crate::RED, false, 0xcf222e),
            ("code_text", Color::Reset, false, 0x1f2328),
            ("keyword", crate::SX_KW, false, 0x0969da),
            ("string", crate::SX_STR, false, 0xa31515),
            ("comment", crate::SX_COM, false, 0x6e7781),
            ("number", crate::SX_NUM, false, 0x116329),
            ("function", crate::SX_FN, false, 0x0b7285),
            ("type", crate::CYAN, false, 0x0b7285),
            ("constant", crate::CYAN, false, 0x0b7285),
            ("operator", Color::Reset, false, 0x1f2328),
            ("info", crate::CYAN, false, 0x0b7285),
            ("secondary", crate::PURPLE, false, 0x6f42c1),
            ("rule", crate::SEL, false, 0xd0d7de),
            ("scroll", val(0x808080), false, 0xb0b7bf),
            ("background", Color::Reset, true, 0xffffff),
            ("panel", crate::BP, true, 0xf6f8fa),
            ("surface", crate::BC, true, 0xeaeef2),
            ("surface_raised", crate::SEL, true, 0xd0d7de),
            ("turn", crate::BW, true, 0xf3f5f8),
            ("prompt", crate::BU, true, 0xdde3ea),
            ("code", crate::BK, true, 0xeff1f4),
            ("handoff", crate::BH, true, 0xefe9f9),
            ("approval", crate::BI, true, 0xeaeef2),
            ("alert", val(0x501c20), true, 0xffd7d9),
            ("hover", val(0x1e1e26), true, 0xe3e7ec),
            ("selection", val(0x264f78), true, 0xb6d7ff),
            ("match", val(0x5a4a1a), true, 0xfff1a8),
            ("match_current", crate::ORANGE, true, 0xffd67e),
        ];
        for (role, dark, is_bg, light) in roles {
            assert_eq!(
                map(LIGHT, *dark, *is_bg),
                val(*light),
                "role {role} maps wrong"
            );
        }
    }

    #[test]
    fn reset_black_and_white_hold_contrast() {
        // Match text is white on a dark gold; under light it reads dark
        // on pale gold.
        assert_eq!(map(LIGHT, Color::White, false), val(0x1f2328));
        assert_eq!(map(LIGHT, val(0x5a4a1a), true), val(0xfff1a8));
        // The current match is black on orange; under light it stays
        // dark on a light orange.
        assert_eq!(map(LIGHT, Color::Black, false), val(0x000000));
        assert_eq!(map(LIGHT, crate::ORANGE, false), val(0xbc4c00));
        // The keymap's selected tab is dark ink on white; under light it
        // stays inverse: light ink on a dark tab.
        assert_eq!(map(LIGHT, Color::White, true), val(0x1f2328));
    }

    #[test]
    fn the_fallback_inverts_lightness_keeping_hue() {
        // Computed tints have no table entry: a dark tint comes back
        // light, a light one dark, with the hue kept.
        for dark in [0x262014u32, 0x241a10, 0x121217, 0x201114, 0x2e2e36] {
            let (r, g, b) = hex(dark);
            let before = rgb_to_hsl((r, g, b)).2;
            let out = rgb(map(LIGHT, val(dark), true)).expect("fallback stays rgb");
            let after = rgb_to_hsl(out).2;
            assert!(after > before, "no lighter for {dark:#x}");
            assert!(after > 0.5, "not light for {dark:#x}");
        }
        let light = (0xd0u8, 0xd7u8, 0xdeu8);
        let out = invert(light);
        assert!(rgb_to_hsl(out).2 < rgb_to_hsl(light).2);
        // Hue kept: a brown stays brownish, not grey or blue.
        let (h, _, _) = rgb_to_hsl(hex(0x262014));
        let (h2, _, _) = rgb_to_hsl(invert(hex(0x262014)));
        assert!((h - h2).abs() < 0.01, "hue moved {h} -> {h2}");
    }

    #[test]
    fn grey_cube_boundary_sends_neutrals_to_grey() {
        // #1a1a22 is the surface tint: neutral takes the grey ramp, the
        // same value as non-neutral takes the colour cube.
        assert_eq!(nearest(hex(0x1a1a22), Ramp::Grey), 234);
        assert_eq!(nearest(hex(0x1a1a22), Ramp::Cube), 16);
        // Through the mapper: surfaces land on grey, accents on the cube.
        assert_eq!(map(DARK_256, val(0x1a1a22), true), Color::Indexed(234));
        assert_eq!(map(DARK_256, val(0x6eaafe), false), Color::Indexed(75));
        // The spread rule at its edge: 0x10 takes grey, 0x11 the cube.
        assert_eq!(fallback_ramp((0x20, 0x30, 0x30)), Ramp::Grey);
        assert_eq!(fallback_ramp((0x20, 0x30, 0x31)), Ramp::Cube);
        assert_eq!(fallback_ramp((0x20, 0x20, 0x31)), Ramp::Cube);
    }

    #[test]
    fn cube_levels_and_primaries() {
        // Just below, at and just above two cube levels.
        for (v, want) in [(94u8, 1u8), (95, 1), (96, 1), (134, 2), (135, 2), (136, 2)] {
            assert_eq!(level(v), want, "level for {v}");
        }
        // Ties fall to the lower level: 47 is midway between 0 and 95.
        assert_eq!(level(47), 0);
        assert_eq!(level(48), 1);
        assert_eq!(map(DARK_256, val(0xff0000), false), Color::Indexed(196));
        assert_eq!(map(DARK_256, val(0x00ff00), false), Color::Indexed(46));
        assert_eq!(map(DARK_256, val(0x0000ff), false), Color::Indexed(21));
    }

    #[test]
    fn no_colour_maps_everything_to_reset() {
        for colour in [
            Color::Reset,
            Color::Black,
            Color::White,
            val(0x6eaafe),
            val(0x1a1a22),
            Color::Indexed(75),
        ] {
            for is_bg in [false, true] {
                assert_eq!(map(NONE, colour, is_bg), Color::Reset);
            }
        }
    }

    #[test]
    fn dark_truecolour_is_the_identity() {
        for colour in [
            Color::Reset,
            Color::Black,
            Color::White,
            val(0x6eaafe),
            val(0x1a1a22),
            val(0x262014),
            Color::Indexed(75),
        ] {
            for is_bg in [false, true] {
                assert_eq!(map(Look::DARK, colour, is_bg), colour);
            }
        }
    }

    #[test]
    fn dark_256_keeps_defaults_and_kills_ansi() {
        // The terminal's own colours stay the default at 256 colours.
        assert_eq!(map(DARK_256, Color::Reset, false), Color::Reset);
        assert_eq!(map(DARK_256, Color::Reset, true), Color::Reset);
        // Its 16 ANSI colours never reach the screen.
        assert_eq!(map(DARK_256, Color::Black, false), Color::Indexed(16));
        assert_eq!(map(DARK_256, Color::White, false), Color::Indexed(231));
        // Text takes whichever entry is nearest, even the cube.
        assert_eq!(map(LIGHT_256, Color::Reset, false), Color::Reset);
    }

    #[test]
    fn no_color_reads_the_environment() {
        let unset = |_: &str| None;
        let empty = |_: &str| Some(String::new());
        let one = |_: &str| Some("1".to_string());
        assert!(!no_color_set(unset));
        assert!(!no_color_set(empty));
        assert!(no_color_set(one));
        assert!(!no_color_set(|k| (k == "OTHER").then(|| "1".to_string())));
    }

    #[test]
    fn resolve_wires_flag_and_env() {
        let no_env = |_: &str| None;
        assert_eq!(resolve(None, no_env).unwrap(), Look::DARK);
        assert_eq!(
            resolve(Some("light"), no_env).unwrap(),
            Look {
                theme: Theme::Light,
                depth: Depth::True,
            }
        );
        assert_eq!(
            resolve(Some("dark-256"), no_env).unwrap(),
            Look {
                theme: Theme::Dark,
                depth: Depth::C256,
            }
        );
        // NO_COLOR forces depth None whatever the flag says.
        let no_color = |_: &str| Some("1".to_string());
        assert_eq!(resolve(None, no_color).unwrap(), NONE);
        assert_eq!(
            resolve(Some("light"), no_color).unwrap(),
            Look {
                theme: Theme::Light,
                depth: Depth::None,
            }
        );
        assert!(resolve(Some("typo"), no_env).is_err());
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 4);
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    #[test]
    fn only_light_truecolour_paints_unpainted_cells() {
        for theme in [Theme::Dark, Theme::Light] {
            for depth in [Depth::True, Depth::C256, Depth::None] {
                let look = Look { theme, depth };
                assert_eq!(
                    needs_clear(look),
                    look == Look {
                        theme: Theme::Light,
                        depth: Depth::True,
                    }
                );
            }
        }
        // The painted background is the background role's light value.
        assert_eq!(map(LIGHT, Color::Reset, true), Color::Rgb(255, 255, 255));
    }
}
