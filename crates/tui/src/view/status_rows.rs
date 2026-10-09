//! The narrow layout's rows below the conversation: the status line from
//! each listed card's segments, the first widget's row, and the running
//! delegates rows (`docs/tui.md`, "The narrow layout"). The rows draw at
//! the bottom of the conversation column, under the input box.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::app::App;
use crate::app::panel::Spot;
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::view::panel::{Card, cards};

/// One status-line row: its line and the targets on it (column offset,
/// width, spot).
pub(crate) struct StatusRow {
    /// The row's line.
    pub(crate) line: Line<'static>,
    /// The click targets on the row: their column offset, width and spot.
    pub(crate) spots: Vec<(u16, u16, Spot)>,
}

/// The status line's non-empty rows at `width`, row 1 first.
pub(crate) fn status(app: &App, width: u16) -> Vec<StatusRow> {
    let wide = usize::from(width);
    let first = row_one(app, wide);
    let second = row_two(app, wide);
    [first, second].into_iter().flatten().collect()
}

/// Row 1: each listed card's segments in order, joined by ` · `.
fn row_one(app: &App, width: usize) -> Option<StatusRow> {
    let widgets = app.panel_state().widgets();
    let names: Vec<(&str, &str)> = widgets
        .iter()
        .map(|widget| (widget.extension.as_str(), widget.widget.as_str()))
        .collect();
    let mut text = String::new();
    let mut spots = Vec::new();
    for card in cards(app.panel_cards(), &names) {
        match card {
            Card::Session => push_segments(&mut text, &mut spots, session_segments(app)),
            Card::ChangedFiles => push_segments(&mut text, &mut spots, changed_files_segments(app)),
            Card::Jobs | Card::Delegates => {}
            Card::Widget(at) => push_segments(&mut text, &mut spots, widget_segment(app, at)),
        }
    }
    if text.is_empty() {
        return None;
    }
    let line = Line::raw(format::cut(&text, width));
    let spots = spots
        .into_iter()
        .map(|(start, wide, spot)| {
            let end = start
                .saturating_add(wide)
                .min(u16::try_from(width).unwrap_or(u16::MAX));
            (start, end.saturating_sub(start), spot)
        })
        .filter(|(_, wide, _)| *wide > 0)
        .collect();
    Some(StatusRow { line, spots })
}

/// Appends `segments` to `text`, joined by ` · `, recording the spots at
/// their column offsets.
fn push_segments(
    text: &mut String,
    spots: &mut Vec<(u16, u16, Spot)>,
    segments: Vec<(String, Option<Spot>)>,
) {
    for (segment, spot) in segments {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        if let Some(spot) = spot {
            let start = u16::try_from(format::width(text)).unwrap_or(u16::MAX);
            let wide = u16::try_from(format::width(&segment)).unwrap_or(u16::MAX);
            spots.push((start, wide, spot));
        }
        text.push_str(&segment);
    }
}

/// The Session card's status segments: "N waiting" while the rail is not
/// drawn, the model, the context share, and the attached row's spend.
fn session_segments(app: &App) -> Vec<(String, Option<Spot>)> {
    let panel = app.panel_state();
    let mut out = Vec::new();
    if let Some(waiting) = app.rail_waiting() {
        out.push((format!("{waiting} waiting"), Some(Spot::Waiting)));
    }
    let model = panel
        .model()
        .or_else(|| panel.status().map(|status| status.model.as_str()));
    if let Some(model) = model {
        out.push((model.to_owned(), None));
    }
    let window = panel.window().filter(|window| *window > 0);
    if let Some((window, context)) = window.and_then(|window| {
        panel
            .status()
            .and_then(|status| status.context.as_ref())
            .map(|context| (window, context))
    }) {
        let pct = context.tokens.saturating_mul(100) / window;
        out.push((format!("{pct}% context"), None));
    }
    if let Some(spend) = app.attached_spend() {
        out.push((format::money(spend), None));
    }
    out
}

/// The Changed files card's status segment: its totals, while any file
/// changed.
fn changed_files_segments(app: &App) -> Vec<(String, Option<Spot>)> {
    let changes = app.panel_state().changes();
    if changes.is_empty() {
        return Vec::new();
    }
    let (files, added, removed) = changes.iter().fold(
        (0u64, 0u64, 0u64),
        |(files, added, removed), (_, (a, r))| {
            (
                files.saturating_add(1),
                added.saturating_add(*a),
                removed.saturating_add(*r),
            )
        },
    );
    vec![(
        format!(
            "{} +{added} \u{2212}{removed}",
            format::count(files, "file", "files")
        ),
        None,
    )]
}

/// The widget card's status segment: its first line, when it has one.
fn widget_segment(app: &App, at: usize) -> Vec<(String, Option<Spot>)> {
    app.panel_state()
        .widgets()
        .get(at)
        .and_then(|widget| widget.lines.first())
        .map(|first| vec![(first.clone(), None)])
        .unwrap_or_default()
}

/// Row 2: "N delegates running" and "N jobs running", each while its card
/// is listed and its count is above zero. Neither opens anything yet, so
/// the row holds no targets.
fn row_two(app: &App, width: usize) -> Option<StatusRow> {
    let listed = |name: &str| app.panel_cards().iter().any(|card| card == name);
    let panel = app.panel_state();
    let delegates = panel.running_delegates().len();
    let jobs = panel.jobs().len().saturating_sub(delegates);
    let mut segments = Vec::new();
    if listed("delegates") && delegates > 0 {
        segments.push(running(delegates, "delegate"));
    }
    if listed("jobs") && jobs > 0 {
        segments.push(running(jobs, "job"));
    }
    if segments.is_empty() {
        return None;
    }
    Some(StatusRow {
        line: Line::raw(format::cut(&segments.join(" · "), width)),
        spots: Vec::new(),
    })
}

/// `n` running delegates or jobs: "1 job running", "2 delegates running".
fn running(n: usize, what: &str) -> String {
    let many = format!("{what}s");
    format!(
        "{} running",
        format::count(u64::try_from(n).unwrap_or(u64::MAX), what, &many)
    )
}

/// The widget row's lines at `width`: the first widget in card order,
/// collapsed to its first line or open to every line. None without a
/// widget.
pub(crate) fn widget(app: &App, width: u16) -> Vec<Line<'static>> {
    let wide = usize::from(width);
    let widgets = app.panel_state().widgets();
    let names: Vec<(&str, &str)> = widgets
        .iter()
        .map(|widget| (widget.extension.as_str(), widget.widget.as_str()))
        .collect();
    let at = cards(app.panel_cards(), &names)
        .into_iter()
        .find_map(|card| match card {
            Card::Widget(at) => Some(at),
            Card::Session | Card::ChangedFiles | Card::Jobs | Card::Delegates => None,
        });
    let Some(lines) = at
        .and_then(|at| widgets.get(at))
        .map(|widget| &widget.lines)
    else {
        return Vec::new();
    };
    let Some(first) = lines.first() else {
        return Vec::new();
    };
    if app.widget_open() {
        std::iter::once(format!("▾ {first}"))
            .chain(lines.iter().skip(1).cloned())
            .map(|row| Line::raw(format::cut(&row, wide)))
            .collect()
    } else {
        vec![Line::raw(format::cut(&format!("▸ {first}"), wide))]
    }
}

/// The running delegates' rows at `width`, at most 4: the Delegates
/// card's rows at the column's width, the first four.
pub(crate) fn delegates(app: &App, width: u16) -> Vec<Line<'static>> {
    super::panel::delegates::rows(app, usize::from(width))
        .into_iter()
        .map(|row| row.line)
        .take(4)
        .collect()
}

/// How many status rows draw at the column's bottom.
pub(crate) fn kept(app: &App) -> usize {
    app.narrow_fit().map_or(0, |fit| fit.status)
}

/// The bottom the input box or its replacement panel draws above: the
/// column's bottom less the kept status rows. Drawing and the terminal
/// cursor share it, so the caret never lands on a status row.
pub(crate) fn input_bottom(app: &App, area: Rect) -> u16 {
    area.bottom()
        .saturating_sub(super::to_u16(kept(app)))
        .max(area.y)
}

/// Draws the status line at the bottom of the column: row 1 above row 2,
/// keeping the first `fit.status` rows of the narrow layout's fit.
pub(super) fn draw_status(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let end = input_bottom(app, area);
    let mut y = area.bottom();
    for row in status(app, area.width).into_iter().take(kept(app)).rev() {
        let Some(at) = y.checked_sub(1).filter(|at| *at >= area.y) else {
            continue;
        };
        buf.set_line(area.x, at, &row.line, area.width);
        y = at;
        for (start, wide, spot) in row.spots {
            targets.push(Target {
                id: TargetId::Panel(spot),
                rect: Rect::new(area.x.saturating_add(start), at, wide, 1),
            });
        }
    }
    *bottom = end;
}

/// Draws the widget row above the input box and its completion panel: a
/// click on its first row expands or collapses it.
pub(super) fn draw_widget(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    if app.narrow_fit().is_none() {
        return;
    }
    let rows = widget(app, area.width);
    for (index, line) in rows.iter().enumerate().rev() {
        let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
            continue;
        };
        buf.set_line(area.x, y, line, area.width);
        *bottom = y;
        if index == 0 {
            let wide = u16::try_from(line.width().min(usize::from(area.width))).unwrap_or(u16::MAX);
            targets.push(Target {
                id: TargetId::Panel(Spot::Widget),
                rect: Rect::new(area.x, y, wide, 1),
            });
        }
    }
}

/// Draws the running delegates rows above the steering queue, keeping
/// the narrow layout's fit of them.
pub(super) fn draw_delegates(app: &App, area: Rect, buf: &mut Buffer, bottom: &mut u16) {
    let keep = app.narrow_fit().map_or(0, |fit| fit.delegates);
    for line in delegates(app, area.width).into_iter().take(keep).rev() {
        let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
            continue;
        };
        buf.set_line(area.x, y, &line, area.width);
        *bottom = y;
    }
}

#[cfg(test)]
#[path = "status_rows_tests.rs"]
mod tests;
