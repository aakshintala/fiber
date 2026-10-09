//! The panel's Delegates card: two rows for each running delegate of the
//! attached session, at most [`DELEGATES_SHOWN`] delegates at once
//! (`docs/tui.md`, "The panel").

use ratatui::text::{Line, Span};

use super::Row;
use crate::app::App;
use crate::format;
use crate::markdown::{Role, style};

/// How many delegates the card shows at once: two rows each, at most 6
/// rows (`docs/tui.md`, "The panel").
pub(crate) const DELEGATES_SHOWN: usize = 3;

/// The card's rows at `text` columns, in job start order: `<glyph> <word>
/// <model>`, then the job's description. No delegate running draws no
/// row.
pub(super) fn rows(app: &App, text: usize) -> Vec<Row> {
    let mut out = Vec::new();
    for (started, description) in app
        .panel_state()
        .running_delegates()
        .into_iter()
        .take(DELEGATES_SHOWN)
    {
        // debt: a still glyph, not the spinner; upgrade trigger: #686's tick lands.
        let state = Span::styled(format!("● {}", started.harness), style(Role::Accent));
        out.push(Row {
            line: Line::from(vec![
                state,
                Span::raw("  "),
                Span::raw(started.model.clone()),
            ]),
            spot: None,
        });
        out.push(Row {
            line: Line::raw(format::cut(&format!("  {description}"), text)),
            spot: None,
        });
    }
    out
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
