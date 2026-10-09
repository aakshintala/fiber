//! The panel's Delegates card: two rows for each running delegate of the
//! attached session, at most [`DELEGATES_SHOWN`] delegates at once
//! (`docs/tui.md`, "The panel").

use std::ops::Range;

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
pub(crate) fn rows(app: &App, text: usize) -> Vec<Row> {
    let panel = app.panel_state();
    let running = panel.running_delegates();
    // A stored offset past the end clamps when drawn, as the panel's does.
    let skip = panel
        .delegate_scroll()
        .min(running.len().saturating_sub(DELEGATES_SHOWN));
    let mut out = Vec::new();
    for (started, description) in running.into_iter().skip(skip).take(DELEGATES_SHOWN) {
        if let Some(row) = app.delegate_row(&started.delegate_session_id) {
            // A subscribed delegate's own summary names its state, and
            // its glyph spins while it works (`docs/tui.md`, "State
            // glyphs"); the model stays the fold's, which names the run
            // (`docs/events.md`, `session_status`).
            let tone = crate::view::rail::tone(row);
            let state = Span::styled(
                format!(
                    "{} {}",
                    app.motion().glyph(row),
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
            // An unsubscribed delegate runs by definition: its glyph spins
            // on the tick.
            let state = Span::styled(
                format!("{} {}", app.motion().spinner(), started.harness),
                style(Role::Accent),
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

/// Asks for the next frame for a spinning delegate whose state row draws:
/// `drawn` holds the drawn row indices relative to the card's first
/// content row. State rows are the even indices; a drawn description row
/// alone asks for nothing, and neither does a still delegate.
pub(crate) fn ask(app: &App, drawn: Range<usize>) {
    let panel = app.panel_state();
    let running = panel.running_delegates();
    let skip = panel
        .delegate_scroll()
        .min(running.len().saturating_sub(DELEGATES_SHOWN));
    for (at, (started, _)) in running
        .into_iter()
        .skip(skip)
        .take(DELEGATES_SHOWN)
        .enumerate()
    {
        let spinning = match app.delegate_row(&started.delegate_session_id) {
            Some(row) => crate::motion::Motion::spins(row),
            // Unsubscribed delegates run by definition.
            None => true,
        };
        if spinning && drawn.contains(&at.saturating_mul(2)) {
            app.motion().ask_frame();
        }
    }
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "delegates_motion_tests.rs"]
mod motion_tests;
