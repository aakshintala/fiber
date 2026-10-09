//! The theme's colour roles and the built-in dark and light themes
//! (`docs/tui.md`, "Themes").
//!
//! Drawing code never holds a colour: [`Role::color`] is a marker, a
//! `Color::Indexed` whose index is the role's, so a rendered line, a cached
//! reply and a test buffer all carry roles. The screen resolves the markers
//! to the theme's colours on the copy of each frame it writes
//! (`crate::look::Look::paint`).

use ratatui::style::Color;
use serde_json::Value;

/// How many roles a theme gives a colour.
pub(crate) const ROLES: usize = 37;

/// A colour role: what a colour is for. The order is the doc table's
/// (`docs/tui.md`, "Themes"), and each role's index is its marker's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Role {
    /// The full text colour.
    Text,
    /// Dim text: READY, line numbers, rules, grips.
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
    /// Inline code, a call's kind, the input box's ›.
    Info,
    /// The git branch, the handoff band's label, the delegates' ◆.
    Secondary,
    /// Rules, line numbers, a bar's empty cells.
    Rule,
    /// The scroll bar's thumb.
    Scroll,
    /// Every cell no surface covers.
    Background,
    /// The rail's and the panel's regions.
    Panel,
    /// The input box, cards, notices, the handoff band.
    Surface,
    /// The card on screen, a hovered card, pickers.
    SurfaceRaised,
    /// A turn's card.
    Turn,
    /// The person's prompt bubble.
    Prompt,
    /// Code blocks and inline code.
    Code,
    /// The handoff band.
    Handoff,
    /// The approval panel for a standing ask.
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
            | Self::Hover
            | Self::Selection => true,
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
            | Self::Scroll
            | Self::Handoff
            | Self::Approval
            | Self::Alert
            | Self::Match
            | Self::MatchCurrent => false,
        }
    }

    /// The role whose marker index is `index`, if one is.
    pub(crate) fn from_index(index: u8) -> Option<Role> {
        Self::ALL.get(usize::from(index)).copied()
    }
}

/// A colour in 24 bits.
pub(crate) type Rgb = (u8, u8, u8);

/// A theme: one colour per role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Theme {
    colours: [Rgb; ROLES],
}

impl Theme {
    /// The built-in dark theme.
    pub(crate) const DARK: Theme = Theme {
        colours: [
            (0xdc, 0xdf, 0xe4), // text
            (0x7f, 0x84, 0x8e), // muted
            (0x56, 0xb6, 0xc2), // accent
            (0x61, 0xaf, 0xef), // heading
            (0x98, 0xc3, 0x79), // success
            (0xe5, 0xc0, 0x7b), // warning
            (0xe0, 0x6c, 0x75), // error
            (0xd1, 0x9a, 0x66), // attention
            (0x98, 0xc3, 0x79), // added
            (0xe0, 0x6c, 0x75), // removed
            (0xab, 0xb2, 0xbf), // code_text
            (0xc6, 0x78, 0xdd), // keyword
            (0x98, 0xc3, 0x79), // string
            (0x6a, 0x73, 0x82), // comment
            (0xd1, 0x9a, 0x66), // number
            (0x61, 0xaf, 0xef), // function
            (0xe5, 0xc0, 0x7b), // type
            (0xe0, 0x6c, 0x75), // constant
            (0x56, 0xb6, 0xc2), // operator
            (0x7d, 0xd3, 0xfc), // info
            (0xb3, 0x9d, 0xdb), // secondary
            (0x3a, 0x3a, 0x4a), // rule
            (0x80, 0x80, 0x80), // scroll
            (0x1e, 0x21, 0x27), // background
            (0x0c, 0x0c, 0x11), // panel
            (0x28, 0x2c, 0x34), // surface
            (0x32, 0x38, 0x42), // surface_raised
            (0x10, 0x10, 0x17), // turn
            (0x26, 0x34, 0x4a), // prompt
            (0x21, 0x25, 0x2b), // code
            (0x1f, 0x1a, 0x2e), // handoff
            (0x1c, 0x28, 0x40), // approval
            (0x50, 0x1c, 0x20), // alert
            (0x3e, 0x44, 0x51), // hover
            (0x37, 0x41, 0x5a), // selection
            (0x5a, 0x4a, 0x1e), // match
            (0x82, 0x64, 0x1e), // match_current
        ],
    };

    /// The built-in light theme.
    pub(crate) const LIGHT: Theme = Theme {
        colours: [
            (0x38, 0x3a, 0x42), // text
            (0xa0, 0xa1, 0xa7), // muted
            (0x01, 0x84, 0xbc), // accent
            (0x40, 0x78, 0xf2), // heading
            (0x50, 0xa1, 0x4f), // success
            (0xc1, 0x84, 0x01), // warning
            (0xe4, 0x56, 0x49), // error
            (0xcc, 0x66, 0x00), // attention
            (0x50, 0xa1, 0x4f), // added
            (0xe4, 0x56, 0x49), // removed
            (0x38, 0x3a, 0x42), // code_text
            (0xa6, 0x26, 0xa4), // keyword
            (0x50, 0xa1, 0x4f), // string
            (0xa0, 0xa1, 0xa7), // comment
            (0x98, 0x68, 0x01), // number
            (0x40, 0x78, 0xf2), // function
            (0xc1, 0x84, 0x01), // type
            (0xe4, 0x56, 0x49), // constant
            (0x01, 0x84, 0xbc), // operator
            (0x7d, 0xd3, 0xfc), // info
            (0xb3, 0x9d, 0xdb), // secondary
            (0x3a, 0x3a, 0x4a), // rule
            (0x80, 0x80, 0x80), // scroll
            (0xfa, 0xfa, 0xfa), // background
            (0x0c, 0x0c, 0x11), // panel
            (0xf0, 0xf0, 0xf1), // surface
            (0xe5, 0xe5, 0xe6), // surface_raised
            (0x10, 0x10, 0x17), // turn
            (0xe2, 0xea, 0xf6), // prompt
            (0xea, 0xea, 0xeb), // code
            (0x1f, 0x1a, 0x2e), // handoff
            (0xe0, 0xe8, 0xf8), // approval
            (0xfc, 0xde, 0xde), // alert
            (0xdb, 0xdb, 0xde), // hover
            (0xd2, 0xde, 0xf5), // selection
            (0xfa, 0xec, 0xb4), // match
            (0xf5, 0xd7, 0x78), // match_current
        ],
    };

    /// The colour `role` takes.
    pub(crate) fn rgb(&self, role: Role) -> Rgb {
        self.colours
            .get(usize::from(role as u8))
            .copied()
            .unwrap_or_default()
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
                *slot = rgb;
            }
        }
        Ok(theme)
    }
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
