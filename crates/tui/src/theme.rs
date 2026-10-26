//! The theme's colour roles and the built-in dark and light themes
//! (`docs/tui.md`, "Themes").
//!
//! Drawing code never holds a colour: [`Role::color`] is a marker, a
//! `Color::Indexed` whose index is the role's, so a rendered line, a cached
//! reply and a test buffer all carry roles. The screen resolves the markers
//! to the theme's colours on the copy of each frame it writes
//! (`crate::look::Look::paint`).

use ratatui::style::{Color, Style};
use serde_json::Value;

/// How many roles a theme gives a colour.
pub(crate) const ROLES: usize = 37;

/// Lifted markers start here: [`Role::lifted`] is `LIFTED_BASE` plus the
/// role's index, so lifts run 64..=100, past every role marker and the
/// colours the paint pass leaves alone.
pub(crate) const LIFTED_BASE: u8 = 64;

/// A colour role: what a colour is for. The order is the doc table's
/// (`docs/tui.md`, "Themes"), and each role's index is its marker's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Role {
    /// The full text colour.
    Text,
    /// What the doc calls dim: READY, labels and hints, grips.
    Muted,
    /// Bullets, the logo's mark, WORKING and its spinner, the steering and
    /// prompt stripes.
    Accent,
    /// Markdown headings.
    Heading,
    /// A call or job that succeeded.
    Success,
    /// RETRYING; the context bar from 60%.
    Warning,
    /// CRASHED, failed lines, the context bar from 85%, "irreversible".
    Error,
    /// NEEDS INPUT, what a card waits on, the approval stripe.
    Attention,
    /// Lines and counts added.
    Added,
    /// Lines and counts removed.
    Removed,
    /// Code with no syntax role.
    CodeText,
    /// Keywords.
    Keyword,
    /// String and character literals.
    String,
    /// Comments.
    Comment,
    /// Number literals.
    Number,
    /// Function and macro names.
    Function,
    /// Type names.
    Type,
    /// Constants: `true`, `null`, `ALL_CAPS` names.
    Constant,
    /// Operators.
    Operator,
    /// Inline code, a call's kind, an approval's tool name and rule prefix,
    /// the input box's ›, "Copied", the context bar's fill.
    Info,
    /// The git branch, the handoff band's label, the question form's
    /// stripe, title and marks, the delegates' ◆.
    Secondary,
    /// Rules, line numbers, a bar's empty cells, the pill, a grip's column
    /// under the pointer, "Chat about this".
    Rule,
    /// The scroll bar's thumb.
    Scroll,
    /// Every cell no surface covers.
    Background,
    /// The rail's and the panel's regions, the narrow layout's status line.
    Panel,
    /// The input box, the panel's cards, the search box.
    Surface,
    /// The card on screen, a hovered card, pickers.
    SurfaceRaised,
    /// A turn's card.
    Turn,
    /// The person's prompt bubble.
    Prompt,
    /// Code blocks.
    Code,
    /// The handoff band.
    Handoff,
    /// The approval panel for a standing ask, and the question form.
    Approval,
    /// The approval panel for a reviewer's escalation.
    Alert,
    /// The click target under the pointer.
    Hover,
    /// Selected text.
    Selection,
    /// A search match.
    Match,
    /// The current search match.
    MatchCurrent,
}

impl Role {
    /// Every role, in index order.
    pub(crate) const ALL: [Role; ROLES] = [
        Self::Text,
        Self::Muted,
        Self::Accent,
        Self::Heading,
        Self::Success,
        Self::Warning,
        Self::Error,
        Self::Attention,
        Self::Added,
        Self::Removed,
        Self::CodeText,
        Self::Keyword,
        Self::String,
        Self::Comment,
        Self::Number,
        Self::Function,
        Self::Type,
        Self::Constant,
        Self::Operator,
        Self::Info,
        Self::Secondary,
        Self::Rule,
        Self::Scroll,
        Self::Background,
        Self::Panel,
        Self::Surface,
        Self::SurfaceRaised,
        Self::Turn,
        Self::Prompt,
        Self::Code,
        Self::Handoff,
        Self::Approval,
        Self::Alert,
        Self::Hover,
        Self::Selection,
        Self::Match,
        Self::MatchCurrent,
    ];

    /// The role's marker: the colour drawing code sets, resolved to the
    /// theme's colour when the frame is written.
    pub(crate) const fn color(self) -> Color {
        Color::Indexed(self as u8)
    }

    /// The role's name, as a theme file and an extension's span spell it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Accent => "accent",
            Self::Heading => "heading",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Attention => "attention",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::CodeText => "code_text",
            Self::Keyword => "keyword",
            Self::String => "string",
            Self::Comment => "comment",
            Self::Number => "number",
            Self::Function => "function",
            Self::Type => "type",
            Self::Constant => "constant",
            Self::Operator => "operator",
            Self::Info => "info",
            Self::Secondary => "secondary",
            Self::Rule => "rule",
            Self::Scroll => "scroll",
            Self::Background => "background",
            Self::Panel => "panel",
            Self::Surface => "surface",
            Self::SurfaceRaised => "surface_raised",
            Self::Turn => "turn",
            Self::Prompt => "prompt",
            Self::Code => "code",
            Self::Handoff => "handoff",
            Self::Approval => "approval",
            Self::Alert => "alert",
            Self::Hover => "hover",
            Self::Selection => "selection",
            Self::Match => "match",
            Self::MatchCurrent => "match_current",
        }
    }

    /// The role named `name`, spelled exactly as [`Role::name`] spells it.
    pub(crate) fn from_name(name: &str) -> Option<Role> {
        Self::ALL.into_iter().find(|role| role.name() == name)
    }

    /// A background role: a tint a surface sits on.
    pub(crate) fn tint(self) -> bool {
        match self {
            Self::Background
            | Self::Panel
            | Self::Surface
            | Self::SurfaceRaised
            | Self::Turn
            | Self::Prompt
            | Self::Code
            | Self::Handoff
            | Self::Approval
            | Self::Alert
            | Self::Hover
            | Self::Selection
            | Self::Match
            | Self::MatchCurrent => true,
            Self::Text
            | Self::Muted
            | Self::Accent
            | Self::Heading
            | Self::Success
            | Self::Warning
            | Self::Error
            | Self::Attention
            | Self::Added
            | Self::Removed
            | Self::CodeText
            | Self::Keyword
            | Self::String
            | Self::Comment
            | Self::Number
            | Self::Function
            | Self::Type
            | Self::Constant
            | Self::Operator
            | Self::Info
            | Self::Secondary
            | Self::Rule
            | Self::Scroll => false,
        }
    }

    /// A neutral tint, which takes the grey ramp at 256 colours
    /// (`docs/tui.md`, "Look").
    pub(crate) fn grey(self) -> bool {
        match self {
            Self::Background
            | Self::Panel
            | Self::Surface
            | Self::SurfaceRaised
            | Self::Turn
            | Self::Prompt
            | Self::Code
            | Self::Approval
            | Self::Rule
            | Self::Hover => true,
            Self::Text
            | Self::Muted
            | Self::Accent
            | Self::Heading
            | Self::Success
            | Self::Warning
            | Self::Error
            | Self::Attention
            | Self::Added
            | Self::Removed
            | Self::CodeText
            | Self::Keyword
            | Self::String
            | Self::Comment
            | Self::Number
            | Self::Function
            | Self::Type
            | Self::Constant
            | Self::Operator
            | Self::Info
            | Self::Secondary
            | Self::Scroll
            | Self::Handoff
            | Self::Alert
            | Self::Selection
            | Self::Match
            | Self::MatchCurrent => false,
        }
    }

    /// The role whose marker index is `index`, if one is.
    pub(crate) fn from_index(index: u8) -> Option<Role> {
        Self::ALL.get(usize::from(index)).copied()
    }

    /// The role `marker` holds, if it holds one: what a built-in row's
    /// span names when the seam reads it back. Only the theme names
    /// colours (`docs/tui.md`, "Themes").
    pub(crate) fn of_marker(marker: Option<Color>) -> Option<Role> {
        match marker {
            Some(Color::Indexed(index)) => Self::from_index(index),
            _ => None,
        }
    }

    /// This role's hover-lift marker: what a hovered or focused click
    /// target takes on this role's tint (`docs/tui.md`, palette,
    /// `hover`). The look resolves it to the tint 0x14 lighter on each
    /// channel.
    pub(crate) fn lifted(self) -> Color {
        Color::Indexed(LIFTED_BASE.saturating_add(self as u8))
    }

    /// The role a hover-lift marker lifts, if one is: a marker outside
    /// 64..=100 lifts nothing, so [`Role::from_index`] never matches a
    /// lifted marker.
    pub(crate) fn from_lifted(index: u8) -> Option<Role> {
        index.checked_sub(LIFTED_BASE).and_then(Self::from_index)
    }
}

/// The foreground of `role`.
pub(crate) fn style(role: Role) -> Style {
    Style::new().fg(role.color())
}

/// A colour in 24 bits.
pub(crate) type Rgb = (u8, u8, u8);

/// A theme value: the terminal's own default colour, the default
/// foreground drawn dim, or a fixed colour (`docs/tui.md`, "Themes").
/// The default foreground is SGR 39 as a foreground and SGR 49 as a
/// background; both resolve to `Color::Reset` at every depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shade {
    /// The terminal's default colour.
    Terminal,
    /// The default foreground drawn dim.
    Dim,
    /// A fixed colour.
    Rgb(Rgb),
}

/// A theme: one colour per role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Theme {
    colours: [Shade; ROLES],
}

impl Theme {
    /// The built-in dark theme (`docs/tui.md`, "Themes").
    pub(crate) const DARK: Theme = Theme {
        colours: [
            Shade::Terminal,                // text
            Shade::Dim,                     // muted
            Shade::Rgb((0x6e, 0xaa, 0xfe)), // accent
            Shade::Rgb((0xff, 0x9f, 0x43)), // heading
            Shade::Rgb((0x6e, 0xaa, 0xfe)), // success
            Shade::Rgb((0xff, 0x9f, 0x43)), // warning
            Shade::Rgb((0xff, 0x5d, 0x73)), // error
            Shade::Rgb((0xff, 0x9f, 0x43)), // attention
            Shade::Rgb((0x73, 0xc9, 0x91)), // added
            Shade::Rgb((0xff, 0x5d, 0x73)), // removed
            Shade::Terminal,                // code_text
            Shade::Rgb((0x6e, 0xaa, 0xfe)), // keyword
            Shade::Rgb((0xce, 0x91, 0x78)), // string
            Shade::Rgb((0x7a, 0x7a, 0x8a)), // comment
            Shade::Rgb((0xb5, 0xce, 0xa8)), // number
            Shade::Rgb((0x7d, 0xd3, 0xfc)), // function
            Shade::Rgb((0x7d, 0xd3, 0xfc)), // type
            Shade::Rgb((0x7d, 0xd3, 0xfc)), // constant
            Shade::Terminal,                // operator
            Shade::Rgb((0x7d, 0xd3, 0xfc)), // info
            Shade::Rgb((0xb3, 0x9d, 0xdb)), // secondary
            Shade::Rgb((0x3a, 0x3a, 0x4a)), // rule
            Shade::Rgb((0x80, 0x80, 0x80)), // scroll
            Shade::Terminal,                // background
            Shade::Rgb((0x0c, 0x0c, 0x11)), // panel
            Shade::Rgb((0x1a, 0x1a, 0x22)), // surface
            Shade::Rgb((0x3a, 0x3a, 0x4a)), // surface_raised
            Shade::Rgb((0x10, 0x10, 0x17)), // turn
            Shade::Rgb((0x34, 0x35, 0x41)), // prompt
            Shade::Rgb((0x18, 0x18, 0x21)), // code
            Shade::Rgb((0x1f, 0x1a, 0x2e)), // handoff
            Shade::Rgb((0x1a, 0x1a, 0x22)), // approval
            Shade::Rgb((0x50, 0x1c, 0x20)), // alert
            Shade::Rgb((0x1e, 0x1e, 0x26)), // hover
            Shade::Rgb((0x26, 0x4f, 0x78)), // selection
            Shade::Rgb((0x5a, 0x4a, 0x1a)), // match
            Shade::Rgb((0xff, 0x9f, 0x43)), // match_current
        ],
    };

    /// The built-in light theme (`docs/tui.md`, "Themes").
    pub(crate) const LIGHT: Theme = Theme {
        colours: [
            Shade::Rgb((0x1f, 0x23, 0x28)), // text
            Shade::Dim,                     // muted
            Shade::Rgb((0x09, 0x69, 0xda)), // accent
            Shade::Rgb((0xbc, 0x4c, 0x00)), // heading
            Shade::Rgb((0x09, 0x69, 0xda)), // success
            Shade::Rgb((0xbc, 0x4c, 0x00)), // warning
            Shade::Rgb((0xcf, 0x22, 0x2e)), // error
            Shade::Rgb((0xbc, 0x4c, 0x00)), // attention
            Shade::Rgb((0x1a, 0x7f, 0x37)), // added
            Shade::Rgb((0xcf, 0x22, 0x2e)), // removed
            Shade::Rgb((0x1f, 0x23, 0x28)), // code_text
            Shade::Rgb((0x09, 0x69, 0xda)), // keyword
            Shade::Rgb((0xa3, 0x15, 0x15)), // string
            Shade::Rgb((0x6e, 0x77, 0x81)), // comment
            Shade::Rgb((0x11, 0x63, 0x29)), // number
            Shade::Rgb((0x0b, 0x72, 0x85)), // function
            Shade::Rgb((0x0b, 0x72, 0x85)), // type
            Shade::Rgb((0x0b, 0x72, 0x85)), // constant
            Shade::Rgb((0x1f, 0x23, 0x28)), // operator
            Shade::Rgb((0x0b, 0x72, 0x85)), // info
            Shade::Rgb((0x6f, 0x42, 0xc1)), // secondary
            Shade::Rgb((0xd0, 0xd7, 0xde)), // rule
            Shade::Rgb((0xb0, 0xb7, 0xbf)), // scroll
            Shade::Rgb((0xff, 0xff, 0xff)), // background
            Shade::Rgb((0xf6, 0xf8, 0xfa)), // panel
            Shade::Rgb((0xea, 0xee, 0xf2)), // surface
            Shade::Rgb((0xd0, 0xd7, 0xde)), // surface_raised
            Shade::Rgb((0xf3, 0xf5, 0xf8)), // turn
            Shade::Rgb((0xdd, 0xe3, 0xea)), // prompt
            Shade::Rgb((0xef, 0xf1, 0xf4)), // code
            Shade::Rgb((0xef, 0xe9, 0xf9)), // handoff
            Shade::Rgb((0xea, 0xee, 0xf2)), // approval
            Shade::Rgb((0xff, 0xd7, 0xd9)), // alert
            Shade::Rgb((0xe3, 0xe7, 0xec)), // hover
            Shade::Rgb((0xb6, 0xd7, 0xff)), // selection
            Shade::Rgb((0xff, 0xf1, 0xa8)), // match
            Shade::Rgb((0xff, 0xd6, 0x7e)), // match_current
        ],
    };

    /// The shade `role` takes.
    pub(crate) fn shade(&self, role: Role) -> Shade {
        self.colours
            .get(usize::from(role as u8))
            .copied()
            .unwrap_or(Shade::Terminal)
    }

    /// Reads a theme file (`docs/tui.md`, "Themes"): an object with
    /// `base`, `dark` or `light`, which gives every role the file leaves
    /// out, and `roles`, role names to `#rrggbb`. Any other key, an unknown
    /// role or a value that is not `#rrggbb` refuses the whole file; the
    /// error says why.
    pub(crate) fn parse(text: &str) -> Result<Theme, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|error| format!("not JSON: {error}"))?;
        let Value::Object(file) = value else {
            return Err("not a JSON object".to_owned());
        };
        if let Some(key) = file.keys().find(|key| *key != "base" && *key != "roles") {
            return Err(format!("unknown key \"{key}\""));
        }
        let mut theme = match file.get("base") {
            None => return Err("no \"base\"".to_owned()),
            Some(Value::String(base)) if base == "dark" => Self::DARK,
            Some(Value::String(base)) if base == "light" => Self::LIGHT,
            Some(other) => return Err(format!("\"base\" is {other}, not \"dark\" or \"light\"")),
        };
        let roles = match file.get("roles") {
            None => return Ok(theme),
            Some(Value::Object(roles)) => roles,
            Some(_) => return Err("\"roles\" is not an object".to_owned()),
        };
        for (name, value) in roles {
            let role = Role::ALL
                .into_iter()
                .find(|role| role.name() == name)
                .ok_or_else(|| format!("unknown role \"{name}\""))?;
            let rgb = value
                .as_str()
                .and_then(hex)
                .ok_or_else(|| format!("role \"{name}\": {value} is not #rrggbb"))?;
            if let Some(slot) = theme.colours.get_mut(usize::from(role as u8)) {
                *slot = Shade::Rgb(rgb);
            }
        }
        Ok(theme)
    }
}

/// One hovered cell's background: the cell's tint role as its lifted
/// marker, or the hover tint with no tint under it (`docs/tui.md`,
/// palette, `hover`). The page itself counts as no tint: a target drawn
/// straight on the background takes the flat hover in every theme.
pub(crate) fn hover_bg(bg: Color) -> Color {
    if let Color::Indexed(index) = bg
        && let Some(role) = Role::from_index(index)
        && role.tint()
        && !matches!(role, Role::Background)
    {
        return role.lifted();
    }
    Role::Hover.color()
}

/// `#rrggbb`, either case, as a colour.
fn hex(text: &str) -> Option<Rgb> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| {
        digits
            .get(at..at.saturating_add(2))
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
    };
    Some((channel(0)?, channel(2)?, channel(4)?))
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
