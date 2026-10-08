//! What the view paints over the conversation's rows once they are drawn
//! (`docs/tui.md`, "Selection and copy", "Links"): the selection's
//! highlight, and the links' targets with their underline.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::App;
use crate::mouse::Target;

/// The selection's highlight.
/// debt: a fixed colour, not a theme role; upgrade when colour roles land
/// (see #685).
const SELECTION: Style = Style::new().bg(Color::Indexed(24));

/// A link's underline: markdown links already draw underlined, and bare
/// URLs gain it here (`docs/tui.md`, "Links").
const LINK: Style = Style::new().add_modifier(Modifier::UNDERLINED);

/// Paints the selection's cells in the conversation `area`, then pushes
/// one click target per row each visible link covers and underlines its
/// cells. With no selection and no link on screen this costs one check
/// each (`docs/tui.md`, "Performance").
pub(super) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    for rect in app.selection_cells(area) {
        buf.set_style(rect.intersection(area), SELECTION);
    }
    for link in app.visible_links(area) {
        for rect in link.rects {
            let rect = rect.intersection(area);
            if rect.is_empty() {
                continue;
            }
            buf.set_style(rect, LINK);
            targets.push(Target { id: link.id, rect });
        }
    }
}

#[cfg(test)]
#[path = "marks_tests.rs"]
mod tests;
