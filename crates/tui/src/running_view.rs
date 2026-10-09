//! The running delegates or jobs list: the cards' rows in a swapped view
//! (`docs/tui.md`, "Swapped views": "The running delegates or jobs, from
//! "N delegates running" or "N jobs running" in the narrow layout").
//!
//! Each row's text is one cell carrying the job's serial, so a click on
//! the text opens that item; the blank cells past the text only select
//! the row (`docs/tui.md`, "Swapped views": a row opens that item's view).

use contract::events::DelegateStarted;

use crate::app::App;
use crate::swapped::{Frame, Ink, List, Spot};

/// The running delegates' frame: the Delegates card's two rows per
/// delegate, in the order they started.
pub(crate) fn delegates_frame(app: &App, list: List) -> Frame {
    let mut rows: Vec<(String, Option<u64>)> = Vec::new();
    for (started, description) in app.panel_state().running_delegates() {
        let serial = app.serial_of_job(&started.job_id);
        rows.push((state_line(app, started), serial));
        rows.push((format!("  {description}"), serial));
    }
    frame("Running delegates", rows, list)
}

/// The running jobs' frame: one row per job, in the order they started.
pub(crate) fn jobs_frame(app: &App, list: List) -> Frame {
    let rows: Vec<(String, Option<u64>)> = app
        .panel_state()
        .running_jobs()
        .into_iter()
        .map(|(id, description)| (format!("  {description}"), app.serial_of_job(id)))
        .collect();
    frame("Running jobs", rows, list)
}

/// A delegate's state row, as its card draws it: its own summary's glyph
/// and word while held, else the spinner and its harness, then its model.
fn state_line(app: &App, started: &DelegateStarted) -> String {
    if let Some(row) = app.delegate_row(&started.delegate_session_id) {
        format!(
            "{} {}  {}",
            app.motion().glyph(row),
            crate::view::rail::word(row),
            started.model
        )
    } else {
        format!(
            "{} {}  {}",
            app.motion().spinner(),
            started.harness,
            started.model
        )
    }
}

/// One frame of the list: each row's text is one cell carrying its job's
/// serial. With nothing running no row draws and the line says so.
fn frame(title: &str, rows: Vec<(String, Option<u64>)>, list: List) -> Frame {
    let empty = rows.is_empty();
    Frame {
        title: title.to_owned(),
        rows: rows
            .into_iter()
            .map(|(text, serial)| vec![(text, serial.map(Spot::Item), Ink::Plain)])
            .collect(),
        list,
        below: empty
            .then(|| "Nothing running.".to_owned())
            .into_iter()
            .collect(),
        field: None,
        footer: "↑↓ move · Enter open · Esc close".to_owned(),
    }
}

#[cfg(test)]
#[path = "running_view_tests.rs"]
mod tests;
