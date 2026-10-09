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

/// The card's rows at `text` columns, in job start order from its scroll
/// offset: `<glyph> <word>  <model>`, then the job's description. No
/// delegate running draws no row.
pub(super) fn rows(app: &App, text: usize) -> Vec<Row> {
    let panel = app.panel_state();
    let running = panel.running_delegates();
    // A stored offset past the end clamps when drawn, as the panel's does.
    let skip = panel
        .delegate_scroll()
        .min(running.len().saturating_sub(DELEGATES_SHOWN));
    let mut out = Vec::new();
    for (started, description) in running.into_iter().skip(skip).take(DELEGATES_SHOWN) {
        if let Some(row) = app.delegate_row(&started.delegate_session_id) {
            // A subscribed delegate's own summary names its state
            // (`docs/tui.md`, "State glyphs"); the model stays the
            // fold's, which names the run (`docs/events.md`,
            // `session_status`).
            let tone = crate::view::rail::tone(row);
            let state = Span::styled(
                format!(
                    "{} {}",
                    crate::home::glyph(row),
                    crate::view::rail::word(row)
                ),
                style(tone),
            );
            out.push(Row {
                line: Line::from(vec![
                    state,
                    Span::raw("  "),
                    Span::raw(started.model.clone()),
                ]),
                spot: None,
                tint: None,
                edge: false,
            });
        } else {
            // debt: a still glyph, not the spinner; upgrade trigger: #686's tick lands.
            let state = Span::styled(format!("● {}", started.harness), style(Role::Accent));
            out.push(Row {
                line: Line::from(vec![
                    state,
                    Span::raw("  "),
                    Span::raw(started.model.clone()),
                ]),
                spot: None,
                tint: None,
                edge: false,
            });
        }
        out.push(Row {
            line: Line::raw(format::cut(&format!("  {description}"), text)),
            spot: None,
            tint: None,
            edge: false,
        });
    }
    out
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
