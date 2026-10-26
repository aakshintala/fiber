//! The running delegates or jobs list (`docs/tui.md`, "Swapped views").
//! Every cell of an item's rows carries its serial; heading and hint rows
//! open nothing.

use contract::events::DelegateStarted;

use crate::app::App;
use crate::swapped::{Frame, Ink, List, Spot};
use crate::theme::Role;

type Row = Vec<(String, Option<Spot>, Ink)>;

/// The running delegates' two rows each, in start order.
pub(crate) fn delegates_frame(app: &App, list: List) -> Frame {
    let delegates = app.panel_state().running_delegates();
    let count = delegates.len();
    let mut rows = Vec::new();
    for (started, description) in delegates {
        let spot = app.serial_of_job(&started.job_id).map(Spot::Item);
        let elapsed = elapsed(app, &started.job_id);
        rows.push(vec![(
            format!("◆ {description}{elapsed}"),
            spot,
            Ink::Plain,
        )]);
        rows.push(delegate_detail(app, started, spot));
    }
    frame("Running delegates", "Delegates", count, rows, list)
}

/// The running jobs' one row each, in start order.
pub(crate) fn jobs_frame(app: &App, list: List) -> Frame {
    let jobs = app.panel_state().running_jobs();
    let count = jobs.len();
    let rows = jobs
        .into_iter()
        .map(|(id, description)| {
            vec![(
                format!("◆ {description}{} · {}", elapsed(app, id), id.0),
                app.serial_of_job(id).map(Spot::Item),
                Ink::Plain,
            )]
        })
        .collect();
    frame("Running jobs", "Jobs", count, rows, list)
}

fn elapsed(app: &App, job: &contract::JobId) -> String {
    app.started_of_job(job)
        .map(|started| {
            let ms = crate::view::item::elapsed_ms(app, started, None);
            format!(" · {}", crate::view::rail::elapsed(0, ms))
        })
        .unwrap_or_default()
}

fn delegate_detail(app: &App, started: &DelegateStarted, spot: Option<Spot>) -> Row {
    let session = &started.delegate_session_id.0;
    let Some(row) = app.delegate_row(&started.delegate_session_id) else {
        return vec![(format!("  {} · {session}", started.model), spot, Ink::Muted)];
    };
    let Some(status) = row.status.as_ref() else {
        return vec![(format!("  {} · {session}", started.model), spot, Ink::Muted)];
    };
    let level = status
        .thinking
        .as_ref()
        .map(|level| format!(" · {level}"))
        .unwrap_or_default();
    let calls = crate::format::count(status.calls, "call", "calls");
    let spend = crate::format::money(row.spend);
    let mut cells = vec![(
        format!("  {}{level} · {calls} · {session} · {spend} ", status.model),
        spot,
        Ink::Muted,
    )];
    if let Some(context) = status.context.as_ref().filter(|context| context.window > 0) {
        let (filled, role, pct) = bar(context.tokens, context.window);
        cells.push(("▆".repeat(filled), spot, Ink::Role(role)));
        cells.push(("░".repeat(10 - filled), spot, Ink::Role(Role::Rule)));
        cells.push((format!(" {pct}%"), spot, Ink::Muted));
    }
    cells
}

/// A zero window is excluded at the wire boundary before computing the bar.
fn bar(tokens: u64, window: u64) -> (usize, Role, u64) {
    let pct = tokens.saturating_mul(100) / window;
    let filled = usize::try_from((tokens.saturating_mul(10) / window).min(10)).unwrap_or(10);
    let role = if pct >= 85 {
        Role::Error
    } else if pct >= 60 {
        Role::Warning
    } else {
        Role::Accent
    };
    (filled, role, pct)
}

fn frame(title: &str, heading: &str, count: usize, mut rows: Vec<Row>, list: List) -> Frame {
    let empty = rows.is_empty();
    if !empty {
        rows.insert(
            0,
            vec![(format!("{heading} · {count} running"), None, Ink::Heading)],
        );
        rows.push(Vec::new());
        rows.push(vec![(
            "a row opens that item's view".to_owned(),
            None,
            Ink::Muted,
        )]);
    }
    Frame {
        title: title.to_owned(),
        command: "delegates · jobs".to_owned(),
        rows,
        list,
        below: empty
            .then(|| "Nothing running.".to_owned())
            .into_iter()
            .collect(),
        field: None,
        footer: String::new(),
    }
}

#[cfg(test)]
#[path = "running_view_tests.rs"]
mod tests;
