//! The panel's Delegates card: two rows for each held delegate of the
//! attached session, running first, at most [`DELEGATES_SHOWN`] delegates
//! at once (`docs/tui.md`, "The panel").

use std::ops::Range;

use contract::events::{DelegateStarted, Outcome};
use ratatui::text::{Line, Span};

use super::Row;
use crate::app::App;
use crate::app::panel::Spot;
use crate::theme::{Role, style};

/// How many delegates the card shows at once: two rows each, at most 6
/// rows (`docs/tui.md`, "The panel").
pub(crate) const DELEGATES_SHOWN: usize = 3;

/// The card's rows at `text` columns: running delegates only, in job
/// start order, two rows each, no title row. The narrow layout reads the
/// running delegates through here (`docs/tui.md`, "The narrow layout").
pub(crate) fn rows(app: &App, text: usize) -> Vec<Row> {
    // The combined window filtered to running, so the narrow rows and
    // the card's window agree on which delegates show.
    let out: Vec<Row> = window(app)
        .into_iter()
        .filter(|entry| entry.outcome.is_none())
        .flat_map(|entry| running_rows(app, entry.started, entry.description, entry.spot))
        .collect();
    super::fit_rows(out, text)
}

/// The card's rows at `text` columns: the combined window over running
/// delegates first, then finished ones, two rows each, no title row. The
/// panel's card adds the title.
pub(crate) fn rows_with_done(app: &App, text: usize) -> Vec<Row> {
    let out: Vec<Row> = window(app)
        .into_iter()
        .flat_map(|entry| match entry.outcome {
            None => running_rows(app, entry.started, entry.description, entry.spot),
            Some(outcome) => {
                finished_rows(app, entry.started, entry.description, outcome, entry.spot)
            }
        })
        .collect();
    super::fit_rows(out, text)
}

/// One held delegate in the combined window: running first, then
/// finished, each in job start order.
struct Entry<'a> {
    started: &'a DelegateStarted,
    description: &'a str,
    outcome: Option<Outcome>,
    spot: Option<Spot>,
}

/// The combined window: running delegates in job start order, then
/// finished ones in job start order, from the scroll offset clamped
/// against the held delegates. A finished delegate never pushes a running
/// one out of view.
fn window(app: &App) -> Vec<Entry<'_>> {
    let panel = app.panel_state();
    let mut combined: Vec<Entry<'_>> = panel
        .running_delegates()
        .into_iter()
        .map(|(started, description)| Entry {
            started,
            description,
            outcome: None,
            spot: app.serial_of_job(&started.job_id).map(Spot::Delegate),
        })
        .chain(
            panel
                .finished_delegates()
                .into_iter()
                .map(|(started, description, outcome)| Entry {
                    started,
                    description,
                    outcome: Some(outcome),
                    spot: app.serial_of_job(&started.job_id).map(Spot::Delegate),
                }),
        )
        .collect();
    // A stored offset past the end clamps when drawn, as the panel's does.
    let skip = panel
        .delegate_scroll()
        .min(combined.len().saturating_sub(DELEGATES_SHOWN));
    combined.drain(..skip.min(combined.len()));
    combined.truncate(DELEGATES_SHOWN);
    combined
}

/// A running delegate's two rows: `●` in attention, spinning while it
/// works, then the description; under it, dim, calls, elapsed and step.
/// Either row opens the delegate: the rows are named by the job's serial,
/// never by position.
fn running_rows(
    app: &App,
    started: &DelegateStarted,
    description: &str,
    spot: Option<Spot>,
) -> [Row; 2] {
    let glyph = match app.delegate_row(&started.delegate_session_id) {
        // A subscribed delegate's own summary names its state, and its
        // glyph spins while it works (`docs/tui.md`, "State glyphs").
        Some(row) if crate::motion::Motion::spins(row) => app.motion().spinner().to_owned(),
        Some(_) => "●".to_owned(),
        // An unsubscribed delegate runs by definition: its glyph spins
        // on the tick.
        None => app.motion().spinner().to_owned(),
    };
    let second = match app.delegate_row(&started.delegate_session_id) {
        Some(row) => match &row.status {
            Some(status) => figures(
                status.calls,
                crate::view::item::elapsed_ms(app, status.started, None),
                status.step.as_deref(),
            ),
            None => format!("  {} · {}", crate::view::rail::word(row), started.model),
        },
        None => format!("  {} · {}", started.harness, started.model),
    };
    [
        Row {
            line: Line::from(vec![
                Span::styled(glyph, style(Role::Attention)),
                Span::raw(format!(" {description}")),
            ]),
            spot,
            tint: None,
            edge: false,
        },
        Row {
            line: Line::from(vec![Span::styled(second, style(Role::Muted))]),
            spot,
            tint: None,
            edge: false,
        },
    ]
}

/// A finished delegate's two rows from its `Outcome`: the glyph, the
/// description, and a dim word. Attention is never used for a finished
/// delegate, so a later change is this one place.
fn finished_rows(
    app: &App,
    started: &DelegateStarted,
    description: &str,
    outcome: Outcome,
    spot: Option<Spot>,
) -> [Row; 2] {
    let (glyph, tone, word) = match outcome {
        Outcome::Completed => ("✓", Role::Success, "done"),
        Outcome::Failed => ("✗", Role::Muted, "failed"),
        Outcome::Cancelled => ("⊘", Role::Muted, "cancelled"),
    };
    let second = match app.panel_state().finished_figures(&started.job_id) {
        Some((calls, elapsed)) => figures(calls, elapsed, Some(word)),
        None => format!("  {word}"),
    };
    [
        Row {
            line: Line::from(vec![
                Span::styled(glyph, style(tone)),
                Span::raw(format!(" {description}")),
            ]),
            spot,
            tint: None,
            edge: false,
        },
        Row {
            line: Line::from(vec![Span::styled(second, style(Role::Muted))]),
            spot,
            tint: None,
            edge: false,
        },
    ]
}

/// The same figures for a live summary and a finished delegate.
fn figures(calls: u64, elapsed: u64, tail: Option<&str>) -> String {
    let mut row = format!(
        "  {} · {}",
        crate::format::count(calls, "call", "calls"),
        crate::format::duration(elapsed)
    );
    if let Some(tail) = tail {
        row.push_str(&format!(" · {tail}"));
    }
    row
}

/// Asks for the next frame for a spinning delegate whose state row draws:
/// `drawn` holds the drawn row indices relative to the combined window's
/// first delegate row. State rows are the even indices; a drawn
/// description row alone asks for nothing, and neither does a still or
/// finished delegate. Running entries come first, so only they can hold
/// an even index the window's running prefix covers.
pub(crate) fn ask(app: &App, drawn: Range<usize>) {
    for (at, entry) in window(app).into_iter().enumerate() {
        if entry.outcome.is_some() {
            continue;
        }
        let spinning = match app.delegate_row(&entry.started.delegate_session_id) {
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
