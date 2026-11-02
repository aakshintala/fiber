//! The narrow layout's rows below the conversation: the status line from
//! each listed card's segments, the first widget's row, and the running
//! delegates rows (`docs/tui.md`, "The narrow layout"). The rows draw at
//! the bottom of the conversation column, under the input box.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::panel::{Branch, Spot};
use crate::ext::{
    BranchName, ChangedFile, ChangedFiles, Input, Quota, RawSpan, SessionCard, SessionContext,
    SlotDraw, Spend, StatusRows, WidgetCard,
};
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::text::to_u16;
use crate::theme::{Role, style};
use crate::view::panel::{Card, branch_text, cards, fit_spans};

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
/// segment-relative offsets. Shared with the input box's status row,
/// which joins and cuts its sides the same way.
pub(super) struct Segment {
    pub(super) spans: Vec<Span<'static>>,
    pub(super) spots: Vec<(usize, usize, Spot)>,
}

impl Segment {
    /// A segment of `spans` with no spot.
    pub(super) fn plain(spans: Vec<Span<'static>>) -> Self {
        Segment {
            spans,
            spots: Vec::new(),
        }
    }

    /// A segment of `spans` opening `spot` across the whole segment.
    pub(super) fn spotted(spans: Vec<Span<'static>>, spot: Spot) -> Self {
        let wide: usize = spans.iter().map(|span| format::width(&span.content)).sum();
        Segment {
            spans,
            spots: vec![(0, wide, spot)],
        }
    }
}

/// Segments joined by a dim `  │  `: each segment's spans with the spots
/// on them, at their column offsets. Shared with the input box's
/// status row, which joins its sides the same way.
pub(super) struct Joined {
    pub(super) spans: Vec<Span<'static>>,
    pub(super) spots: Vec<(u16, u16, Spot)>,
}

/// Appends `segment`'s spans, joined by a dim `  │  `, recording its
/// spots shifted past the joiner.
pub(super) fn push_segment(joined: &mut Joined, segment: Segment) {
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
/// Shared with the input box's status row, which cuts each side the
/// same way.
pub(super) fn cut_row(joined: Joined, width: usize) -> StatusRow {
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
/// drawn, "git" and the branch, the spend, the cache share, and the
/// tools line. The working directory, the model and thinking level, the
/// permission mode and the context bar moved to the input box's status
/// row (`docs/tui.md`, "The input box"). A card with no kept segment
/// leaves no row.
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
            // Dim `cache` with its share, opening the usage view.
            let mut spans = vec![
                Span::styled("cache", style(Role::Muted)),
                Span::raw(" ".to_owned()),
            ];
            let at: usize = spans.iter().map(|span| format::width(&span.content)).sum();
            let share = format!("{hits}%",);
            let wide = format::width(&share);
            spans.push(Span::raw(share));
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

/// How many built-in status rows draw: the narrow layout's kept rows
/// while narrow, every status row on wide with the panel hidden, none
/// with the panel shown. The input box draws them under its status row,
/// and the terminal cursor counts them there too.
pub(crate) fn kept(app: &App, width: u16) -> usize {
    if let Some(fit) = app.narrow_fit() {
        return fit.status;
    }
    if lines_live(app) {
        status(app, width).len()
    } else {
        0
    }
}

/// Whether the status lines draw: the column has a layout and its panel
/// is not shown — narrow, or wide with the panel hidden. With the panel
/// shown the cards carry the same figures.
fn lines_live(app: &App) -> bool {
    app.chrome()
        .layout()
        .is_some_and(|layout| layout.panel.is_none())
}

/// Every changed file in the order the built-in ranks them: the Changed
/// files card's ranking, borrowed for the status fold.
fn ranked_files(changes: &std::collections::BTreeMap<String, (u64, u64)>) -> Vec<ChangedFile<'_>> {
    crate::changed_files_view::ranked(changes)
        .into_iter()
        .map(|(path, added, removed)| ChangedFile {
            path,
            added,
            removed,
        })
        .collect()
}

/// The branch the Session card and the status line show: the last
/// query's answer, else the status's git (`docs/tui.md`, "Git").
fn branch_name(panel: &crate::app::panel::PanelState) -> Option<BranchName<'_>> {
    match panel.branch() {
        Some(Branch::Named(name)) => Some(BranchName::Named(name.as_str())),
        Some(Branch::Detached) => Some(BranchName::Detached),
        Some(Branch::Absent) => None,
        None => panel
            .status()
            .and_then(|status| status.git.as_ref())
            .map(|git| match git.branch.as_deref() {
                Some(name) => BranchName::Named(name),
                None => BranchName::Detached,
            }),
    }
}

/// The status rows' renderer input: the same figures the Session card
/// draws, folded the way its rows read them.
fn status_input<'a>(app: &'a App, files: &'a [ChangedFile<'a>], flat: bool) -> StatusRows<'a> {
    let panel = app.panel_state();
    let status = panel.status();
    let known = app.session().and_then(|session| {
        let window = panel.window_for(session).filter(|window| *window > 0)?;
        let context = panel.status_for(session)?.context.as_ref()?;
        Some((window, context))
    });
    StatusRows {
        cards: app.panel_cards().to_vec(),
        session: SessionCard {
            workspace: status.map(|status| status.workspace.as_str()),
            branch: status.and_then(|_| branch_name(panel)),
            model: panel
                .model()
                .or_else(|| status.map(|status| status.model.as_str())),
            thinking: panel.thinking(),
            thinking_now: panel.thinking_now(),
            context: known.map(|(window, context)| SessionContext {
                tokens: context.tokens,
                window,
                trigger_at: panel.trigger_at(),
            }),
            spend: status
                .map(|status| Spend::Status(&status.spend))
                .or_else(|| app.attached_spend().map(|cost| Spend::Attached { cost })),
            budget: panel.budget(),
            cache_expires: None,
            speed: panel.speed(),
            turns: panel.turns(),
            tools_down: panel.down(),
            // The input leaves waiting out at zero; the built-in row
            // keeps drawing whatever the rail reports.
            waiting: app.rail_waiting().filter(|waiting| *waiting > 0),
            flat,
        },
        changed_files: ChangedFiles { files, flat },
        delegates_running: panel.running_delegates().len(),
        jobs_running: panel.running_jobs().len(),
        widgets: panel
            .widgets()
            .iter()
            .map(|widget| WidgetCard {
                extension: widget.extension.as_str(),
                widget: widget.widget.as_str(),
                lines: widget.lines.as_slice(),
                open: app.widget_open(),
                flat,
            })
            .collect(),
        waiting: app.rail_waiting().filter(|waiting| *waiting > 0),
        quota: super::panel::quota::segment_spans(app)
            .is_some()
            .then_some(Quota {
                rows: panel.quota.rows.as_slice(),
                notice_at: panel.quota.notice_at,
            }),
        flat,
    }
}

/// The built-in status line as extension spans: every row, unshed, one
/// span line each. The paint below sheds them; the renderer sees the
/// whole line.
fn lines_builtin_spans(app: &App, width: u16) -> Vec<Vec<RawSpan>> {
    status(app, width)
        .into_iter()
        .map(|row| crate::ext::spans::from_ratatui(&row.line.spans))
        .collect()
}

/// How many rows the status line reserves under the input box's status
/// row: a replacement's line count, or the kept built-in rows. None
/// with the panel shown.
pub(crate) fn count(app: &App, width: u16) -> usize {
    if !lines_live(app) {
        return 0;
    }
    let bound = super::notice_rows::frame_bound(app);
    let ranked = ranked_files(app.panel_state().changes());
    let folded = status_input(app, &ranked, bound.flat);
    let builtin = &|_: &Input<'_>, width: u16| lines_builtin_spans(app, width);
    match bound.slot_lines(
        SlotDraw::Count,
        "status_rows",
        &Input::StatusRows(folded),
        width,
        builtin,
    ) {
        Some(lines) => lines.len(),
        None => kept(app, width),
    }
}

/// Draws the status line under the input box's status row: a
/// replacement's lines bottom-up with no target, or the kept built-in
/// rows starting in the status row's column with their targets. Rows
/// that would land above the area shed from the top; every kept row is
/// still reserved, so the caret math holds. Moves `bottom` above them.
pub(super) fn paint(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    if !lines_live(app) {
        return;
    }
    let bound = super::notice_rows::frame_bound(app);
    let ranked = ranked_files(app.panel_state().changes());
    let folded = status_input(app, &ranked, bound.flat);
    let builtin = &|_: &Input<'_>, width: u16| lines_builtin_spans(app, width);
    let Some(fitted) = bound.slot_lines(
        SlotDraw::Paint,
        "status_rows",
        &Input::StatusRows(folded),
        area.width,
        builtin,
    ) else {
        paint_builtin(app, area, buf, bottom, targets);
        return;
    };
    // A replacement owns its rows with no target: keys still switch
    // cards. The first line lands lowest, as a steering replacement's
    // first row does.
    let total = fitted.len().min(usize::from(u16::MAX));
    let room = bottom.saturating_sub(area.y) as usize;
    let keep = total.min(room);
    *bottom = bottom.saturating_sub(to_u16(keep)).max(area.y);
    let x = area.x.saturating_add(1);
    let wide = area.width.saturating_sub(1);
    for (dy, line) in fitted.iter().take(keep).enumerate() {
        let row = bottom.saturating_add(to_u16(dy));
        buf.set_line(x, row, &line.line, wide);
    }
}

/// Paints the kept built-in rows bottom-up, the last kept row lowest,
/// each starting in the status row's column with its targets.
fn paint_builtin(
    app: &App,
    area: Rect,
    buf: &mut Buffer,
    bottom: &mut u16,
    targets: &mut Vec<Target>,
) {
    let rows = status(app, area.width)
        .into_iter()
        .take(kept(app, area.width))
        .collect::<Vec<_>>();
    // Shed the top rows when short, as the notice rows do; reserve
    // every kept row either way.
    let room = bottom.saturating_sub(area.y) as usize;
    let skip = rows.len().saturating_sub(room);
    let mut y = *bottom;
    *bottom = y.saturating_sub(to_u16(rows.len())).max(area.y);
    let x = area.x.saturating_add(1);
    let wide = area.width.saturating_sub(1);
    for row in rows.iter().skip(skip).rev() {
        let Some(at) = y.checked_sub(1).filter(|at| *at >= area.y) else {
            break;
        };
        buf.set_line(x, at, &row.line, wide);
        y = at;
        for (start, wide, spot) in &row.spots {
            targets.push(Target {
                id: TargetId::Panel(*spot),
                rect: Rect::new(x.saturating_add(*start), at, *wide, 1),
            });
        }
    }
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
