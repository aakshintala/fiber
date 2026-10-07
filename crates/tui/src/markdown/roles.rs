//! The colour roles a reply draws with (`docs/tui.md`, "Look").
//!
//! debt: dark-theme truecolour values, not a theme; upgrade when themes land
//! (see #685), which points each role at the theme's colour.

use ratatui::style::Color;

/// A named colour role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Headings.
    Heading,
    /// Bullets and list numbers.
    Accent,
    /// A reply's text: the full text colour.
    Text,
    /// Line numbers, rules and block quote bars.
    Dim,
    /// A code block's background, and inline code's.
    CodeTint,
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
}

impl Role {
    /// The role's colour.
    pub(crate) const fn color(self) -> Color {
        match self {
            Self::Heading | Self::Function => Color::Rgb(97, 175, 239),
            Self::Accent | Self::Operator => Color::Rgb(86, 182, 194),
            Self::Text => Color::Rgb(220, 223, 228),
            Self::Dim => Color::Rgb(127, 132, 142),
            Self::CodeTint => Color::Rgb(33, 37, 43),
            Self::CodeText => Color::Rgb(171, 178, 191),
            Self::Keyword => Color::Rgb(198, 120, 221),
            Self::String => Color::Rgb(152, 195, 121),
            Self::Comment => Color::Rgb(106, 115, 130),
            Self::Number => Color::Rgb(209, 154, 102),
            Self::Type => Color::Rgb(229, 192, 123),
            Self::Constant => Color::Rgb(224, 108, 117),
        }
    }
}
