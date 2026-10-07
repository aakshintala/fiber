//! The session screen's chrome: where the rail, the conversation column
//! and the panel go, the header naming the session, and the person's hide
//! of the panel (`docs/tui.md`, "Layout", "Shedding", "Naming the
//! session"). [`Chrome`] owns that state; the app hands it the screen's
//! size and home's counts on every settle. The layout applies only while
//! the app has home state and is not on home; without home the
//! conversation is the whole screen.

use super::{App, Effect};
use crate::focus::Regions;
use crate::layout::{self, Layout, Shares, Want};

/// What the app knows about home when it lays the screen out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HomeState {
    /// The rail's and the panel's shares from the launch description.
    pub(crate) shares: Shares,
    /// How many sessions are live.
    pub(crate) live: usize,
    /// Home draws: no layout applies.
    pub(crate) on_home: bool,
}

/// The screen's regions, as the latest settle laid them out, and what the
/// person chose to show.
#[derive(Debug, Default)]
pub(crate) struct Chrome {
    /// ⌥P or `/panel` hid the panel.
    panel_hidden: bool,
    /// The session screen's regions; `None` without home state, on home,
    /// or below the floor.
    layout: Option<Layout>,
    /// The floor line, while the screen is below the floor with home
    /// state.
    floor: Option<String>,
}

impl Chrome {
    /// Shows or hides the panel.
    pub(crate) fn toggle_panel(&mut self) {
        self.panel_hidden = !self.panel_hidden;
    }

    /// Lays a `width` by `height` screen out, returning whether the
    /// regions moved. The rail is wanted while two or more sessions are
    /// live.
    pub(crate) fn lay_out(&mut self, width: u16, height: u16, home: Option<HomeState>) -> bool {
        let before = self.layout;
        self.floor = home.and_then(|_| layout::floor_line(width, height));
        self.layout = home
            .filter(|home| !home.on_home && self.floor.is_none())
            .map(|home| {
                let by_count = home.live >= 2;
                let want = Want {
                    rail: by_count,
                    rail_by_count: by_count,
                    panel: !self.panel_hidden,
                };
                layout::split(width, height, &home.shares, &want)
            });
        self.layout != before
    }

    /// The session screen's regions.
    pub(crate) fn layout(&self) -> Option<Layout> {
        self.layout
    }

    /// The one line a screen below the floor shows in place of
    /// everything, home included.
    pub(crate) fn floor_line(&self) -> Option<&str> {
        self.floor.as_deref()
    }

    /// The width the conversation and everything drawn in the column wraps
    /// at: the column's, or `screen` without the layout.
    pub(crate) fn column_width(&self, screen: u16) -> u16 {
        self.layout.map_or(screen, |layout| layout.column.width)
    }

    /// The rows above the conversation: the header's one while the layout
    /// applies.
    pub(crate) fn header_rows(&self) -> usize {
        usize::from(self.layout.is_some())
    }

    /// Where the panel and the rail are drawn.
    pub(crate) fn regions(&self) -> Regions {
        Regions {
            panel: self.layout.and_then(|layout| layout.panel),
            rail: self.layout.and_then(|layout| layout.rail),
        }
    }
}

/// The session's name: the feed row's, else the latest `session_named`,
/// else nothing.
fn name<'a>(row: Option<&'a str>, named: Option<&'a str>) -> &'a str {
    row.filter(|name| !name.is_empty())
        .or(named)
        .unwrap_or_default()
}

/// The header's text: the session's name, each control character drawn
/// as a space.
pub(crate) fn header(row: Option<&str>, named: Option<&str>) -> String {
    name(row, named)
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

/// The terminal title: `fiber` with no session attached, else the state
/// glyph and the session's name before ` · fiber`, each left out when it
/// is empty. The title's bytes drop control characters.
pub(crate) fn title(attached: bool, glyph: Option<&str>, name: &str) -> String {
    if !attached {
        return "fiber".to_owned();
    }
    let parts: Vec<&str> = [glyph.unwrap_or_default(), name]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return "fiber".to_owned();
    }
    format!("{} · fiber", parts.join(" "))
}

impl App {
    /// The screen's chrome, as the latest settle laid it out.
    pub(crate) fn chrome(&self) -> &Chrome {
        &self.chrome
    }

    /// The width everything drawn in the conversation column wraps at.
    pub(crate) fn column_width(&self) -> u16 {
        self.chrome.column_width(self.screen.width())
    }

    /// The attached session's feed row.
    fn attached_row(&self) -> Option<&crate::home::Row> {
        let session = self.session()?;
        self.home.as_ref()?.sessions.row(session)
    }

    /// The header's text for the attached session.
    pub(crate) fn header(&self) -> String {
        header(
            self.attached_row().map(|row| row.name.as_str()),
            self.name(),
        )
    }

    /// The terminal title (`docs/tui.md`, "State glyphs"): `fiber` on
    /// home, the attached session's glyph and name otherwise.
    pub(crate) fn title(&self) -> String {
        let row = self.attached_row();
        title(
            self.session().is_some(),
            row.map(crate::home::glyph),
            name(row.map(|row| row.name.as_str()), self.name()),
        )
    }

    /// Lays the screen out from its size and home's counts and wraps the
    /// pages at the column's width, so a change that shows or hides a
    /// region rewraps before the next frame. Where the panel and the rail
    /// are is recorded when they move.
    pub(super) fn relayout(&mut self) {
        let home = self.home.as_ref().map(|home| HomeState {
            shares: Shares {
                rail: home.launch.rail_share,
                panel: home.launch.panel_share,
            },
            live: home.sessions.live().len(),
            on_home: self.on_home(),
        });
        let moved = self
            .chrome
            .lay_out(self.screen.width(), self.screen.height(), home);
        let width = self.column_width();
        self.screen.wrap_at(width);
        if moved {
            self.set_regions(self.chrome.regions());
        }
    }

    /// ⌥P and `/panel`: shows or hides the panel.
    pub(super) fn toggle_panel(&mut self) -> Effect {
        self.chrome.toggle_panel();
        self.settle();
        Effect::None
    }
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
