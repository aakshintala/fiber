//! The look: the theme in use, the terminal's colour depth, and the paint
//! pass that turns each frame's role markers into colours (`docs/tui.md`,
//! "Look", "Themes").

use std::ops::RangeInclusive;

use ratatui::buffer::Buffer;
use ratatui::style::Color;

use crate::theme::{ROLES, Rgb, Role, Theme};

/// The theme `tui.theme` names (`docs/configuration.md`, "Keys").
#[derive(Debug, Default)]
pub enum ThemeSetting {
    /// Unset, or `auto`: the theme follows the terminal's light or dark
    /// appearance.
    #[default]
    Follow,
    /// The built-in dark theme.
    Dark,
    /// The built-in light theme.
    Light,
    /// A theme file in Fiber home's `themes/`.
    File {
        /// The theme's name, the file's without `.json`.
        name: String,
        /// The file's contents, or why it could not be read.
        text: Result<String, String>,
    },
}

/// How many colours the terminal draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Depth {
    /// `NO_COLOR`: every role is the terminal's default colour.
    NoColour,
    /// 24-bit colour.
    True,
    /// The xterm 256-colour palette.
    Ansi256,
}

/// Reads an environment variable by name.
pub(crate) type Var<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The terminal's colour depth. `NO_COLOR` set and not empty turns colour
/// off. `COLORTERM` of `truecolor` or `24bit` is truecolour, and so is a
/// `TERM` that names a truecolour terminal, which survives SSH where
/// `COLORTERM` is usually dropped. Anything else is 256 colours.
pub(crate) fn depth(var: Var<'_>) -> Depth {
    if var("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return Depth::NoColour;
    }
    if var("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit") {
        return Depth::True;
    }
    let term = var("TERM").unwrap_or_default();
    if term.ends_with("-direct") || ["xterm-ghostty", "xterm-kitty", "wezterm"].contains(&&*term) {
        return Depth::True;
    }
    Depth::Ansi256
}

/// The xterm-256 entries a role's colour is chosen among at 256 colours
/// (`docs/tui.md`, "Look"): a neutral tint takes the grey ramp, any other
/// tint the colour cube so it keeps its hue (an alert stays red), and a
/// text role any of the 240. 0..=15 are the terminal's own colours and
/// never chosen.
fn among(role: Role) -> RangeInclusive<u8> {
    if role.grey() {
        232..=255
    } else if role.tint() {
        16..=231
    } else {
        16..=255
    }
}

/// The entry among `among` nearest `rgb` by squared distance, the lower
/// index winning a tie.
pub(crate) fn ansi256(rgb: Rgb, among: RangeInclusive<u8>) -> u8 {
    let mut best = (*among.start(), u32::MAX);
    for index in among {
        let distance = squared(rgb, xterm(index));
        if distance < best.1 {
            best = (index, distance);
        }
    }
    best.0
}

/// The colour of xterm-256 entry `index`, from 16: the 6×6×6 cube, then
/// the grey ramp.
fn xterm(index: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |at: u8| LEVELS.get(usize::from(at % 6)).copied().unwrap_or(0);
    match index.checked_sub(232) {
        Some(step) => {
            let grey = step.saturating_mul(10).saturating_add(8);
            (grey, grey, grey)
        }
        None => {
            let cube = index.saturating_sub(16);
            (level(cube / 36), level(cube / 6), level(cube))
        }
    }
}

/// The squared distance between two colours.
fn squared(a: Rgb, b: Rgb) -> u32 {
    let channel = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
    channel(a.0, b.0) + channel(a.1, b.1) + channel(a.2, b.2)
}

/// The terminal's light or dark appearance, as it last reported it
/// (`docs/tui.md`, "Themes").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Appearance {
    /// Dark: the appearance before any report.
    #[default]
    Dark,
    /// Light.
    Light,
}

/// The theme in use, resolved for the terminal's colour depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Look {
    depth: Depth,
    /// Each role's colour, by index.
    colours: [Color; ROLES],
    /// Whether the theme follows the terminal's appearance.
    follow: bool,
    /// The terminal's appearance, as it last reported it.
    appearance: Appearance,
}

impl Default for Look {
    /// The dark theme in truecolour: the theme followed before the terminal
    /// reports its appearance.
    fn default() -> Self {
        Self::resolve(&Theme::DARK, Depth::True, true, Appearance::Dark)
    }
}

impl Look {
    /// The look for `setting` on the terminal `var` describes, and the
    /// notice to show when `setting` names a theme file that cannot be
    /// read or parsed; the theme then follows the terminal's appearance.
    /// Before the terminal reports its appearance, it is dark.
    pub(crate) fn new(setting: ThemeSetting, var: Var<'_>) -> (Look, Option<String>) {
        let depth = depth(var);
        let (theme, follow, notice) = match setting {
            ThemeSetting::Follow => (Theme::DARK, true, None),
            ThemeSetting::Dark => (Theme::DARK, false, None),
            ThemeSetting::Light => (Theme::LIGHT, false, None),
            ThemeSetting::File { name, text } => match text.and_then(|text| Theme::parse(&text)) {
                Ok(theme) => (theme, false, None),
                Err(reason) => (
                    Theme::DARK,
                    true,
                    Some(format!(
                        "Theme \"{name}\": {reason}; following the terminal's appearance."
                    )),
                ),
            },
        };
        (
            Self::resolve(&theme, depth, follow, Appearance::Dark),
            notice,
        )
    }

    /// Records the terminal's reported `appearance`, re-resolving the
    /// theme while it is followed: whether the colours changed
    /// (`docs/tui.md`, "Themes").
    pub(crate) fn appearance(&mut self, appearance: Appearance) -> bool {
        let changed = self.follow && appearance != self.appearance;
        self.appearance = appearance;
        if changed {
            let theme = match appearance {
                Appearance::Dark => Theme::DARK,
                Appearance::Light => Theme::LIGHT,
            };
            self.colours = Self::colours(&theme, self.depth);
        }
        changed
    }

    /// The terminal's appearance, as it last reported it.
    pub(crate) fn reported(&self) -> Appearance {
        self.appearance
    }

    /// The look of `theme` at `depth`, following the terminal when
    /// `follow`.
    fn resolve(theme: &Theme, depth: Depth, follow: bool, appearance: Appearance) -> Look {
        Look {
            depth,
            colours: Self::colours(theme, depth),
            follow,
            appearance,
        }
    }

    /// Each role's colour in `theme` at `depth`.
    fn colours(theme: &Theme, depth: Depth) -> [Color; ROLES] {
        Role::ALL.map(|role| {
            let rgb = theme.rgb(role);
            match depth {
                Depth::NoColour => Color::Reset,
                Depth::True => Color::Rgb(rgb.0, rgb.1, rgb.2),
                Depth::Ansi256 => Color::Indexed(ansi256(rgb, among(role))),
            }
        })
    }

    /// The colour `role` resolves to.
    pub(crate) fn colour(&self, role: Role) -> Color {
        self.colours
            .get(usize::from(role as u8))
            .copied()
            .unwrap_or(Color::Reset)
    }

    /// Resolves every role marker in `buf` to its colour; any other colour
    /// is left as it is. With no colour, a half-block edge (▄ or ▀ drawn in
    /// a tint) becomes a blank cell: with no tint there is no surface to
    /// edge, and the row stays so row counts do not change. Runs once per
    /// frame, on the copy written to the terminal.
    pub(crate) fn paint(&self, buf: &mut Buffer) {
        for cell in &mut buf.content {
            let edge = self.depth == Depth::NoColour
                && matches!(cell.symbol(), "▄" | "▀")
                && role_of(cell.fg).is_some_and(Role::tint);
            if edge {
                cell.set_symbol(" ");
            }
            cell.fg = self.resolved(cell.fg);
            cell.bg = self.resolved(cell.bg);
        }
    }

    /// `colour` with a role marker resolved.
    fn resolved(&self, colour: Color) -> Color {
        role_of(colour).map_or(colour, |role| self.colour(role))
    }
}

/// The role `colour` marks, if it is a marker.
fn role_of(colour: Color) -> Option<Role> {
    match colour {
        Color::Indexed(index) => Role::from_index(index),
        Color::Reset
        | Color::Black
        | Color::Red
        | Color::Green
        | Color::Yellow
        | Color::Blue
        | Color::Magenta
        | Color::Cyan
        | Color::Gray
        | Color::DarkGray
        | Color::LightRed
        | Color::LightGreen
        | Color::LightYellow
        | Color::LightBlue
        | Color::LightMagenta
        | Color::LightCyan
        | Color::White
        | Color::Rgb(..) => None,
    }
}

#[cfg(test)]
#[path = "look_tests.rs"]
mod tests;
