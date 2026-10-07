//! Home's data: what the terminal knows about where it was launched,
//! and what home draws (`docs/tui.md`, "Home"). Later parts add the rows,
//! the subscriptions and the picker state.

use std::path::PathBuf;

/// What the terminal knows about where it was launched.
pub struct Launch {
    /// The launch directory; `start`'s default workspace.
    pub workspace: PathBuf,
    /// Its project key (`docs/state.md`, "Projects").
    pub project: String,
    /// The launch directory is inside a git repository.
    pub git: bool,
    /// `tui.hover`: with it off, mouse mode 1003 is never sent and nothing
    /// is tinted under the pointer.
    pub hover: bool,
    /// Fiber's version, for the logo.
    pub version: String,
}

/// What home draws, built by [`crate::app::App::home_screen`].
pub(crate) struct HomeScreen {
    /// Fiber's version, for the logo.
    pub(crate) version: String,
    /// The glyph before the name in the one-row logo.
    pub(crate) glyph: String,
    /// The chip row, left to right.
    pub(crate) chips: Vec<String>,
    /// The foot hint, or the quit hint while Ctrl+C is armed.
    pub(crate) foot: String,
    /// The input box shows its placeholder.
    pub(crate) placeholder: bool,
}
