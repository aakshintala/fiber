//! The reconnect banner (`docs/tui.md`, "A dropped connection"): one row
//! in the working line's place, above the steering queue, while the
//! terminal reconnects.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::markdown::{Role, style};

/// Draws the banner on the row above `bottom`, cut at `area`'s width, and
/// moves `bottom` up to it; nothing while the link is up or refused.
pub(super) fn draw(app: &App, area: Rect, buf: &mut Buffer, bottom: &mut u16) {
    if let Some(text) = app.banner() {
        super::put(buf, area, bottom, &text, style(Role::Warning));
    }
}

#[cfg(test)]
#[path = "banner_tests.rs"]
mod tests;
