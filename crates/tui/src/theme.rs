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
            Shade::Terminal,                 // text
            Shade::Dim,                      // muted
            Shade::Rgb((0x6e, 0xaa, 0xfe)),  // accent
            Shade::Rgb((0xff, 0x9f, 0x43)),  // heading
            Shade::Rgb((0x6e, 0xaa, 0xfe)),  // success
            Shade::Rgb((0xff, 0x9f, 0x43)),  // warning
            Shade::Rgb((0xff, 0x5d, 0x73)),  // error
            Shade::Rgb((0xff, 0x9f, 0x43)),  // attention
            Shade::Rgb((0x6e, 0xaa, 0xfe)),  // added
            Shade::Rgb((0xff, 0x5d, 0x73)),  // removed
            Shade::Terminal,                 // code_text
            Shade::Rgb((0x6e, 0xaa, 0xfe)),  // keyword
            Shade::Rgb((0xce, 0x91, 0x78)),  // string
            Shade::Rgb((0x7a, 0x7a, 0x8a)),  // comment
            Shade::Rgb((0xb5, 0xce, 0xa8)),  // number
            Shade::Rgb((0x7d, 0xd3, 0xfc)),  // function
            Shade::Rgb((0x7d, 0xd3, 0xfc)),  // type
            Shade::Rgb((0x7d, 0xd3, 0xfc)),  // constant
            Shade::Terminal,                 // operator
            Shade::Rgb((0x7d, 0xd3, 0xfc)),  // info
            Shade::Rgb((0xb3, 0x9d, 0xdb)),  // secondary
            Shade::Rgb((0x3a, 0x3a, 0x4a)),  // rule
            Shade::Rgb((0x80, 0x80, 0x80)),  // scroll
            Shade::Terminal,                 // background
            Shade::Rgb((0x0c, 0x0c, 0x11)),  // panel
            Shade::Rgb((0x1a, 0x1a, 0x22)),  // surface
            Shade::Rgb((0x3a, 0x3a, 0x4a)),  // surface_raised
            Shade::Rgb((0x10, 0x10, 0x17)),  // turn
            Shade::Rgb((0x34, 0x35, 0x41)),  // prompt
            Shade::Rgb((0x18, 0x18, 0x21)),  // code
            Shade::Rgb((0x1f, 0x1a, 0x2e)),  // handoff
            Shade::Rgb((0x1a, 0x1a, 0x22)),  // approval
            Shade::Rgb((0x50, 0x1c, 0x20)),  // alert
            Shade::Rgb((0x1e, 0x1e, 0x26)),  // hover
            Shade::Rgb((0x26, 0x4f, 0x78)),  // selection
            Shade::Rgb((0x5a, 0x4a, 0x1a)),  // match
            Shade::Rgb((0xff, 0x9f, 0x43)),  // match_current
        ],
    };

    /// The built-in light theme. Roles with no ruled light value take
    /// the dark value until ruled (see #1634's case).
    pub(crate) const LIGHT: Theme = Theme {
        colours: [
            Shade::Rgb((0x38, 0x3a, 0x42)), // text
            Shade::Rgb((0xa0, 0xa1, 0xa7)), // muted
            Shade::Rgb((0x01, 0x84, 0xbc)), // accent
            Shade::Rgb((0x40, 0x78, 0xf2)), // heading
            Shade::Rgb((0x50, 0xa1, 0x4f)), // success
            Shade::Rgb((0xc1, 0x84, 0x01)), // warning
            Shade::Rgb((0xe4, 0x56, 0x49)), // error
            Shade::Rgb((0xcc, 0x66, 0x00)), // attention
            Shade::Rgb((0x50, 0xa1, 0x4f)), // added
            Shade::Rgb((0xe4, 0x56, 0x49)), // removed
            Shade::Rgb((0x38, 0x3a, 0x42)), // code_text
            Shade::Rgb((0xa6, 0x26, 0xa4)), // keyword
            Shade::Rgb((0x50, 0xa1, 0x4f)), // string
            Shade::Rgb((0xa0, 0xa1, 0xa7)), // comment
            Shade::Rgb((0x98, 0x68, 0x01)), // number
            Shade::Rgb((0x40, 0x78, 0xf2)), // function
            Shade::Rgb((0xc1, 0x84, 0x01)), // type
            Shade::Rgb((0xe4, 0x56, 0x49)), // constant
            Shade::Rgb((0x01, 0x84, 0xbc)), // operator
            Shade::Rgb((0x7d, 0xd3, 0xfc)), // info
            Shade::Rgb((0xb3, 0x9d, 0xdb)), // secondary
            Shade::Rgb((0x3a, 0x3a, 0x4a)), // rule
            Shade::Rgb((0x80, 0x80, 0x80)), // scroll
            Shade::Rgb((0xfa, 0xfa, 0xfa)), // background
            Shade::Rgb((0x0c, 0x0c, 0x11)), // panel
            Shade::Rgb((0xf0, 0xf0, 0xf1)), // surface
            Shade::Rgb((0xe5, 0xe5, 0xe6)), // surface_raised
            Shade::Rgb((0x10, 0x10, 0x17)), // turn
            Shade::Rgb((0xe2, 0xea, 0xf6)), // prompt
            Shade::Rgb((0xea, 0xea, 0xeb)), // code
            Shade::Rgb((0x1f, 0x1a, 0x2e)), // handoff
            Shade::Rgb((0xe0, 0xe8, 0xf8)), // approval
            Shade::Rgb((0xfc, 0xde, 0xde)), // alert
            Shade::Rgb((0xdb, 0xdb, 0xde)), // hover
            Shade::Rgb((0xd2, 0xde, 0xf5)), // selection
            Shade::Rgb((0xfa, 0xec, 0xb4)), // match
            Shade::Rgb((0xf5, 0xd7, 0x78)), // match_current
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
