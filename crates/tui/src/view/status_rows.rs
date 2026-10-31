//! The narrow layout's rows below the conversation: the status line from
//! each listed card's segments, the first widget's row, and the running
//! delegates rows (`docs/tui.md`, "The narrow layout"). The rows draw at
//! the bottom of the conversation column, under the input box.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::panel::Spot;
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::text::to_u16;
use crate::theme::{Role, style};
use crate::view::panel::{Card, bar_spans, branch_text, cards, fit_spans};

/// One status-line row: its line and the targets on it (column offset,
/// width, spot).
pub(crate) struct StatusRow {
    /// The row's line.
    pub(crate) line: Line<'static>,
    /// The click targets on the row: their column offset, width and spot.
    pub(crate) spots: Vec<(u16, u16, Spot)>,
}

/// The status line's non-empty rows at `width`, row 1 first. Each row is
/// already cut to `width - 1`: the draw lays it one column in, ending at
/// the terminal's right edge.
pub(crate) fn status(app: &App, width: u16) -> Vec<StatusRow> {
    let wide = usize::from(width);
    let first = row_one(app, wide);
    let second = row_two(app, wide);
    [first, second].into_iter().flatten().collect()
}

/// One card's segments: its spans with the spots on them at
/// segment-relative offsets.
struct Segment {
    spans: Vec<Span<'static>>,
    spots: Vec<(usize, usize, Spot)>,
}

impl Segment {
    /// A segment of `spans` with no spot.
    fn plain(spans: Vec<Span<'static>>) -> Self {
        Segment {
            spans,
            spots: Vec::new(),
        }
    }

    /// A segment of `spans` opening `spot` across the whole segment.
    fn spotted(spans: Vec<Span<'static>>, spot: Spot) -> Self {
        let wide: usize = spans.iter().map(|span| format::width(&span.content)).sum();
        Segment {
            spans,
            spots: vec![(0, wide, spot)],
        }
    }
}

/// Segments joined by a dim `  │  `: each segment's spans with the spots
/// on them, at their column offsets.
struct Joined {
    spans: Vec<Span<'static>>,
    spots: Vec<(u16, u16, Spot)>,
}

/// Appends `segment`'s spans, joined by a dim `  │  `, recording its
/// spots shifted past the joiner.
fn push_segment(joined: &mut Joined, segment: Segment) {
    if !joined.spans.is_empty() {
        joined
            .spans
            .push(Span::styled("  \u{2502}  ", style(Role::Muted)));
    }
    let base: usize = joined
        .spans
        .iter()
        .map(|span| format::width(&span.content))
        .sum();
    for (at, wide, spot) in segment.spots {
        joined.spots.push((
            u16::try_from(base.saturating_add(at)).unwrap_or(u16::MAX),
            u16::try_from(wide).unwrap_or(u16::MAX),
            spot,
        ));
    }
    joined.spans.extend(segment.spans);
}

/// A status row cut to `width - 1`: its line over the joined spans, each
/// keeping its style and ending in `…` past the width, and its targets
/// clamped to the visible columns. A row that fits keeps every cell.
fn cut_row(joined: Joined, width: usize) -> StatusRow {
    let text = width.saturating_sub(1);
    let wide: usize = joined
        .spans
        .iter()
        .map(|span| format::width(&span.content))
        .sum();
    let line = if text == 0 {
        Line::default()
    } else if wide > text {
        Line::from(fit_spans(joined.spans, text))
    } else {
        Line::from(joined.spans)
    };
    let spots = joined
        .spots
        .into_iter()
        .filter_map(|(start, wide, spot)| {
            let end = start
                .saturating_add(wide)
                .min(u16::try_from(text).unwrap_or(u16::MAX));
            let start = start.min(end);
            // A spot keeps the visible columns it covers: none past the
            // edge, none without width.
            (start < end).then(|| (start, end.saturating_sub(start), spot))
        })
        .collect();
    StatusRow { line, spots }
}

/// Row 1: each listed card's segments in order, joined by a dim `  │  `.
fn row_one(app: &App, width: usize) -> Option<StatusRow> {
    let widgets = app.panel_state().widgets();
    let names: Vec<(&str, &str)> = widgets
        .iter()
        .map(|widget| (widget.extension.as_str(), widget.widget.as_str()))
        .collect();
    let mut joined = Joined {
        spans: Vec::new(),
        spots: Vec::new(),
    };
    for card in cards(app.panel_cards(), &names) {
        match card {
            Card::Session => {
                for segment in session_segments(app) {
                    push_segment(&mut joined, segment);
                }
            }
            Card::ChangedFiles => {
                if let Some(segment) = changed_files_segment(app) {
                    push_segment(&mut joined, segment);
                }
            }
            Card::Quota => {
                if let Some(spans) = super::panel::quota::segment_spans(app) {
                    push_segment(&mut joined, Segment::spotted(spans, Spot::Quota));
                }
            }
            Card::Jobs | Card::Delegates => {}
            Card::Widget(at) => {
                if let Some(segment) = widget_segment(app, at) {
                    push_segment(&mut joined, segment);
                }
            }
        }
    }
    if joined.spans.is_empty() {
        return None;
    }
    Some(cut_row(joined, width))
}

/// The Session card's status segments: "N waiting" while the rail is not
/// drawn, the working directory, "git" and the branch, the model and
/// thinking level, the permission mode, the context bar, the spend, the
/// cache share, and the tools line. Every new segment needs the status,
/// so the row count matches the base's for every state.
fn session_segments(app: &App) -> Vec<Segment> {
    let panel = app.panel_state();
    let status = panel.status();
    let mut out = Vec::new();
    if let Some(waiting) = app.rail_waiting() {
        out.push(Segment::spotted(
            vec![Span::styled(
                format!("{waiting} waiting"),
                style(Role::Attention),
            )],
            Spot::Waiting,
        ));
    }
    if let Some(status) = status {
        out.push(Segment::plain(vec![Span::raw(status.workspace.clone())]));
    }
    if let Some(branch) = status.and_then(|_| branch_text(app)) {
        let spans = vec![
            Span::styled("git", style(Role::Muted)),
            Span::raw(" ".to_owned()),
            Span::styled(branch, style(Role::Secondary)),
        ];
        // A click on the branch only: the working directory opens
        // nothing.
        let at = format::width("git ");
        let wide = spans
            .iter()
            .skip(2)
            .map(|span| format::width(&span.content))
            .sum();
        out.push(Segment {
            spans,
            spots: vec![(at, wide, Spot::Branch)],
        });
    }
    let model = panel
        .model()
        .or_else(|| status.map(|status| status.model.as_str()));
    if let Some(model) = model {
        let mut spans = vec![Span::styled(model.to_owned(), style(Role::Accent))];
        if let Some(thinking) = panel.thinking() {
            spans.push(Span::raw(" ".to_owned()));
            spans.push(Span::styled(thinking.to_owned(), style(Role::Attention)));
        }
        out.push(Segment::plain(spans));
    }
    if status.is_some() {
        out.push(Segment::plain(vec![Span::styled(
            crate::view::panel::PERMISSION_MODE,
            style(Role::Attention),
        )]));
    }
    let window = panel.window().filter(|window| *window > 0);
    if let Some((window, context)) = window.and_then(|window| {
        status
            .and_then(|status| status.context.as_ref())
            .map(|context| (window, context))
    }) {
        out.push(context_segment(context.tokens, window, panel.trigger_at()));
    }
    if let Some(spend) = app.attached_spend() {
        // With a budget the segment reads billed spend against it, as
        // the Session card does: a subscription bills nothing per call
        // (`docs/loop.md`, "Spending budget").
        // Without a parsed status there is no billed cost to compare,
        // so the segment reads the attached spend plain.
        let text = match app.panel_state().status() {
            Some(status) => {
                format::spend_against_budget(status.spend.cost, app.panel_state().budget())
                    .unwrap_or_else(|| format::money(spend))
            }
            None => format::money(spend),
        };
        out.push(Segment::spotted(vec![Span::raw(text)], Spot::Usage));
    }
    if let Some(status) = status {
        let tokens = &status.spend.tokens;
        let inside = tokens
            .input
            .saturating_add(tokens.cache_read)
            .saturating_add(
                tokens
                    .cache_write
                    .values()
                    .fold(0u64, |sum, n| sum.saturating_add(*n)),
            );
        if inside > 0 {
            let hits = tokens
                .cache_read
                .saturating_mul(100)
                .checked_div(inside)
                .unwrap_or(0);
            // Dim `cache` with its share, then the time until it
            // resets, opening the usage view (`docs/tui.md`, "The
            // narrow layout").
            let mut spans = vec![
                Span::styled("cache", style(Role::Muted)),
                Span::raw(" ".to_owned()),
            ];
            let at: usize = spans.iter().map(|span| format::width(&span.content)).sum();
            let share = format!("{hits}%",);
            spans.push(Span::raw(share));
            if let Some(expiry) = status.cache_expires {
                let now = app.motion().wall_ms().unwrap_or(0);
                spans.push(Span::raw(" ".to_owned()));
                if now >= expiry {
                    spans.push(Span::styled("expired".to_owned(), style(Role::Attention)));
                } else {
                    spans.push(Span::raw(crate::view::panel::cache_countdown(
                        expiry.saturating_sub(now),
                    )));
                }
            }
            let wide: usize = spans
                .iter()
                .map(|span| format::width(&span.content))
                .sum::<usize>()
                .saturating_sub(at);
            out.push(Segment {
                spans,
                spots: vec![(at, wide, Spot::Usage)],
            });
        }
        if !panel.down().is_empty() {
            // Dim `tools` with one error value per down server.
            let mut spans = vec![
                Span::styled("tools", style(Role::Muted)),
                Span::raw(" ".to_owned()),
            ];
            for (at, server) in panel.down().iter().enumerate() {
                if at > 0 {
                    spans.push(Span::raw(" ".to_owned()));
                }
                spans.push(Span::styled(
                    format!("\u{2717} {server} down"),
                    style(Role::Error),
                ));
            }
            out.push(Segment::spotted(spans, Spot::Tools));
        }
    }
    out
}

/// The context segment: "ctx" dim, an 8-cell bar with the size, opening
/// the context breakdown. The bar shares the panel's fill against the
/// handoff goal, with no handoff marker: the trigger folds into the
/// window it would otherwise govern.
fn context_segment(tokens: u64, window: u64, trigger: Option<u64>) -> Segment {
    let mut spans = vec![
        Span::styled("ctx", style(Role::Muted)),
        Span::raw(" ".to_owned()),
    ];
    spans.extend(bar_spans(tokens, trigger.unwrap_or(window), None, 8));
    spans.push(Span::raw(" ".to_owned()));
    spans.push(Span::raw(crate::view::panel::short_tokens(tokens)));
    Segment::spotted(spans, Spot::Context)
}

/// The Changed files card's status segment: `±` dim, the file count and
/// the `added` and `removed` totals, while any file changed.
fn changed_files_segment(app: &App) -> Option<Segment> {
    let changes = app.panel_state().changes();
    if changes.is_empty() {
        return None;
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
    Some(Segment::spotted(
        vec![
            Span::styled("\u{00b1}", style(Role::Muted)),
            Span::raw(" ".to_owned()),
            Span::raw(format::count(files, "file", "files")),
            Span::raw(" ".to_owned()),
            Span::styled(format!("+{added}"), style(Role::Added)),
            Span::raw(" ".to_owned()),
            Span::styled(format!("\u{2212}{removed}"), style(Role::Removed)),
        ],
        Spot::ChangedFiles,
    ))
}

/// The widget card's status segment: its first line, when it has one.
fn widget_segment(app: &App, at: usize) -> Option<Segment> {
    app.panel_state()
        .widgets()
        .get(at)
        .and_then(|widget| widget.lines.first())
        .map(|first| Segment::plain(vec![Span::raw(first.clone())]))
}

/// Row 2: "N delegates running" and "N jobs running", each while its card
/// is listed and its count is above zero, so they are never cut. Each
/// opens its running list (`docs/tui.md`, "The narrow layout").
fn row_two(app: &App, width: usize) -> Option<StatusRow> {
    let listed = |name: &str| app.panel_cards().iter().any(|card| card == name);
    let panel = app.panel_state();
    let delegates = panel.running_delegates().len();
    let jobs = panel.running_jobs().len();
    let mut joined = Joined {
        spans: Vec::new(),
        spots: Vec::new(),
    };
    if listed("delegates") && delegates > 0 {
        push_segment(
            &mut joined,
            Segment::spotted(
                vec![
                    Span::styled("\u{25c6}", style(Role::Secondary)),
                    Span::raw(" ".to_owned()),
                    Span::raw(running(delegates, "delegate")),
                ],
                Spot::DelegateList,
            ),
        );
    }
    if listed("jobs") && jobs > 0 {
        push_segment(
            &mut joined,
            Segment::spotted(
                vec![
                    Span::styled("\u{2699}", style(Role::Attention)),
                    Span::raw(" ".to_owned()),
                    Span::raw(running(jobs, "job")),
                ],
                Spot::JobList,
            ),
        );
    }
    if joined.spans.is_empty() {
        return None;
    }
    Some(cut_row(joined, width))
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
            Card::Session | Card::ChangedFiles | Card::Jobs | Card::Delegates | Card::Quota => None,
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
/// card's rows at the column's width, the first four, each with the spot
/// a click opens (`docs/tui.md`, "The narrow layout").
pub(crate) fn delegates(app: &App, width: u16) -> Vec<(Line<'static>, Option<Spot>)> {
    super::panel::delegates::rows(app, usize::from(width))
        .into_iter()
        .map(|row| (row.line, row.spot))
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
    area.bottom().saturating_sub(to_u16(kept(app))).max(area.y)
}

/// Draws the status line at the bottom of the column: row 1 above row 2,
/// keeping the first `fit.status` rows of the narrow layout's fit. Each
/// row sits on the `panel` tint one column in, ending at the terminal's
/// right edge; with no colour no tint is laid.
pub(super) fn draw_status(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let end = input_bottom(app, area);
    let mut y = area.bottom();
    let boxed = crate::surface::boxed();
    for row in status(app, area.width).into_iter().take(kept(app)).rev() {
        let Some(at) = y.checked_sub(1).filter(|at| *at >= area.y) else {
            continue;
        };
        if !boxed {
            buf.set_style(
                Rect::new(area.x, at, area.width, 1),
                Style::new().bg(Role::Panel.color()),
            );
        }
        let x = area.x.saturating_add(1);
        let wide = area.width.saturating_sub(1);
        buf.set_line(x, at, &row.line, wide);
        y = at;
        for (start, wide, spot) in row.spots {
            targets.push(Target {
                id: TargetId::Panel(spot),
                rect: Rect::new(x.saturating_add(start), at, wide, 1),
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
/// the narrow layout's fit of them. Each row with a spot is a click
/// target opening that delegate.
pub(super) fn draw_delegates(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let keep = app.narrow_fit().map_or(0, |fit| fit.delegates);
    // A drawn state row's spinner moves on the tick.
    super::panel::delegates::ask(app, 0..keep);
    for (line, spot) in delegates(app, area.width).into_iter().take(keep).rev() {
        let Some(y) = bottom.checked_sub(1).filter(|y| *y >= area.y) else {
            continue;
        };
        buf.set_line(area.x, y, &line, area.width);
        if let Some(spot) = spot {
            let wide = u16::try_from(line.width().min(usize::from(area.width))).unwrap_or(u16::MAX);
            if wide > 0 {
                targets.push(Target {
                    id: TargetId::Panel(spot),
                    rect: Rect::new(area.x, y, wide, 1),
                });
            }
        }
        *bottom = y;
    }
}

#[cfg(test)]
#[path = "status_rows_tests.rs"]
mod tests;
