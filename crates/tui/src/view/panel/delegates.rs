//! The panel's Delegates card: two rows for each held delegate of the
//! attached session, running first, at most [`DELEGATES_SHOWN`] delegates
//! at once (`docs/tui.md`, "The panel").

use std::ops::Range;

use contract::events::{DelegateStarted, Outcome};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::Row;
use crate::app::App;
use crate::app::panel::Spot;
use crate::theme::{Role, style};

/// How many running entries fit in the narrow card.
const DELEGATES_SHOWN: usize = 3;

/// The card's rows at `text` columns: running delegates only, in job
/// start order, two rows each, no title row. The narrow layout reads the
/// running delegates through here (`docs/tui.md`, "The narrow layout").
pub(crate) fn rows(app: &App, text: usize) -> Vec<Row> {
    // The combined window filtered to running, so the narrow rows and
    // the card's window agree on which delegates show.
    let out: Vec<Row> = app
        .panel_state()
        .running_delegates()
        .into_iter()
        .skip(app.panel_state().delegate_scroll())
        .take(DELEGATES_SHOWN)
        .flat_map(|(started, description)| running_rows(app, started, description))
        .collect();
    super::fit_rows(out, text)
}

/// The card's six-row whole-entry window: running delegates, the fold,
/// then finished delegates while open.
pub(crate) fn rows_with_done(app: &App, text: usize) -> Vec<Row> {
    let all = entries(app);
    let offset = app.panel_state().delegate_scroll().min(all.len());
    let mut used = 0;
    let mut out = Vec::new();
    for entry in all.into_iter().skip(offset) {
        let rows = match entry {
            Entry::Fold(count) => vec![Row {
                line: Line::from(Span::styled(
                    format!(
                        "{count} finished {}",
                        if app.panel_state().delegates_open() {
                            "▾"
                        } else {
                            "▸"
                        }
                    ),
                    style(Role::Muted),
                )),
                spot: Some(Spot::DelegateFold),
                tint: None,
                edge: false,
            }],
            Entry::Running(started, description) => running_rows(app, started, description)
                .into_iter()
                .collect(),
            Entry::Finished(started, description, outcome) => finished_rows(
                app,
                started,
                description,
                outcome,
                app.serial_of_job(&started.job_id).map(Spot::Delegate),
            )
            .into_iter()
            .collect(),
        };
        if used + rows.len() > 6 {
            break;
        }
        used += rows.len();
        out.extend(rows);
    }
    super::fit_rows(out, text)
}

/// One held delegate in the combined window: running first, then
/// finished, each in job start order.
enum Entry<'a> {
    Running(&'a DelegateStarted, &'a str),
    Fold(usize),
    Finished(&'a DelegateStarted, &'a str, Outcome),
}

/// Visible entries in start order, with the finished prefix controlled by the fold.
fn entries(app: &App) -> Vec<Entry<'_>> {
    let panel = app.panel_state();
    let mut all: Vec<_> = panel
        .running_delegates()
        .into_iter()
        .map(|(started, description)| Entry::Running(started, description))
        .collect();
    let finished = panel.finished_delegates();
    if !finished.is_empty() {
        all.push(Entry::Fold(finished.len()));
    }
    if panel.delegates_open() {
        all.extend(
            finished.into_iter().map(|(started, description, outcome)| {
                Entry::Finished(started, description, outcome)
            }),
        );
    }
    all
}

/// A running delegate's two rows: `●` in attention, spinning while it
/// works, then the description; under it, dim, calls, elapsed and step.
/// Either row opens the delegate: the rows are named by the job's serial,
/// never by position.
pub(crate) fn running_rows(app: &App, started: &DelegateStarted, description: &str) -> [Row; 2] {
    let spot = app.serial_of_job(&started.job_id).map(Spot::Delegate);
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
            Some(status) => progress_line(
                status.calls,
                crate::view::item::elapsed_ms(app, status.started, None),
                Some((status.spend.cost, status.spend.subscription_cost)),
                status.step.as_deref(),
            ),
            None => Line::from(vec![Span::styled(
                format!("  {} · {}", crate::view::rail::word(row), started.model),
                style(Role::Muted),
            )]),
        },
        None => Line::from(vec![Span::styled(
            format!("  {} · {}", started.harness, started.model),
            style(Role::Muted),
        )]),
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
            line: second,
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
        Some((calls, elapsed)) => progress_line(
            calls,
            elapsed,
            app.panel_state().finished_cost(&started.job_id),
            Some(word),
        ),
        None => Line::from(vec![Span::styled(format!("  {word}"), style(Role::Muted))]),
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
            line: second,
            spot,
            tint: None,
            edge: false,
        },
    ]
}

/// The same cost-bearing detail row for a live summary and finished delegate.
fn progress_line(
    calls: u64,
    elapsed: u64,
    cost: Option<(Option<f64>, f64)>,
    tail: Option<&str>,
) -> Line<'static> {
    let muted = style(Role::Muted);
    let mut spans = vec![Span::styled(
        format!(
            "  {} · {}",
            crate::format::count(calls, "call", "calls"),
            crate::format::duration(elapsed)
        ),
        muted,
    )];
    if let Some((billed, subscription)) = cost {
        if let Some(billed) = billed.filter(|amount| *amount > 0.0) {
            spans.push(Span::styled(
                format!(" · {}", crate::format::money(billed)),
                muted,
            ));
        }
        if subscription > 0.0 {
            spans.push(Span::styled(
                format!(" · {} sub", crate::format::money(subscription)),
                muted.add_modifier(Modifier::DIM),
            ));
        }
        if billed == Some(0.0) && subscription == 0.0 {
            spans.push(Span::styled(" · $0.00", muted));
        }
    }
    if let Some(tail) = tail {
        spans.push(Span::styled(format!(" · {tail}"), muted));
    }
    Line::from(spans)
}

/// Asks for the next frame for a spinning delegate whose state row draws:
/// `drawn` holds the drawn row indices relative to the combined window's
/// first delegate row. State rows are the even indices; a drawn
/// description row alone asks for nothing, and neither does a still or
/// finished delegate. Running entries come first, so only they can hold
/// an even index the window's running prefix covers.
pub(crate) fn ask(app: &App, drawn: Range<usize>) {
    let panel = app.panel_state();
    // Running entries are the entire moving prefix. Offsets at the fold
    // or finished suffix skip past it; none of those rows can ask for motion.
    for (index, (started, _)) in panel
        .running_delegates()
        .into_iter()
        .skip(panel.delegate_scroll())
        .take(DELEGATES_SHOWN)
        .enumerate()
    {
        let spinning = match app.delegate_row(&started.delegate_session_id) {
            Some(row) => crate::motion::Motion::spins(row),
            // Unsubscribed delegates run by definition.
            None => true,
        };
        if spinning && drawn.contains(&(index * 2)) {
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
