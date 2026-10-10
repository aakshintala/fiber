//! The session screen's side panel cards: their order and rows, drawn
//! into the panel rect (`docs/tui.md`, "The panel"). Card text starts past
//! the draggable edge Part 1 draws, with one margin column each side.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::panel::{Branch, Spot};
use crate::changed_files_view;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};
use crate::surface;

pub(crate) mod delegates;

/// One card the panel draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Card {
    /// The attached session: its directory, model, context, spend, speed,
    /// turns and MCP servers down (`docs/tui.md`, "The panel").
    Session,
    /// The files with the most lines changed, and totals (`docs/tui.md`,
    /// "The panel").
    ChangedFiles,
    /// How many jobs run, listed while open (`docs/tui.md`, "The panel").
    Jobs,
    /// The running delegates, two rows each (`docs/tui.md`, "The panel").
    Delegates,
    /// An extension's widget, by its index among the widgets in arrival
    /// order.
    Widget(usize),
}

/// The cards the panel draws, in order: `list`'s names first, then every
/// present widget the list does not name, in arrival order. A name that
/// places no card places nothing, and a card placed once is not placed
/// again.
pub(crate) fn cards(list: &[String], widgets: &[(&str, &str)]) -> Vec<Card> {
    let mut out = Vec::new();
    for name in list {
        // debt: the Quota card draws nothing until a client can read quota; upgrade trigger: #1200 lands.
        // `quota` and unknown names fall through and place nothing.
        if name == "session" && !out.contains(&Card::Session) {
            out.push(Card::Session);
            continue;
        }
        if name == "changed_files" && !out.contains(&Card::ChangedFiles) {
            out.push(Card::ChangedFiles);
            continue;
        }
        if name == "jobs" && !out.contains(&Card::Jobs) {
            out.push(Card::Jobs);
            continue;
        }
        if name == "delegates" && !out.contains(&Card::Delegates) {
            out.push(Card::Delegates);
            continue;
        }
        if let Some(at) = widgets
            .iter()
            .position(|(extension, widget)| format!("{extension}/{widget}") == *name)
            && !out.contains(&Card::Widget(at))
        {
            out.push(Card::Widget(at));
        }
    }
    for (at, _) in widgets.iter().enumerate() {
        if !out.contains(&Card::Widget(at)) {
            out.push(Card::Widget(at));
        }
    }
    out
}

/// One drawn row: its line, the target it is, if any, the tint behind a
/// card's text row, and whether it is a card's edge row.
pub(crate) struct Row {
    pub(crate) line: Line<'static>,
    pub(crate) spot: Option<Spot>,
    pub(crate) tint: Option<Role>,
    pub(crate) edge: bool,
}

/// Every card's rows, top to bottom, with one blank row between cards;
/// `width` is the panel rect's.
pub(crate) fn rows(app: &App, width: u16) -> Vec<Row> {
    rows_and_delegates(app, width).0
}

/// [`rows`], with the Delegates card's row indices when it draws.
pub(crate) fn rows_and_delegates(app: &App, width: u16) -> (Vec<Row>, Option<Range<usize>>) {
    let widgets = app.panel_state().widgets();
    let names: Vec<(&str, &str)> = widgets
        .iter()
        .map(|widget| (widget.extension.as_str(), widget.widget.as_str()))
        .collect();
    let text = text_width(width);
    let mut out = Vec::new();
    let mut span = None;
    for card in cards(app.panel_cards(), &names) {
        let drawn = fit_rows(card_rows(app, &card, text), text);
        if drawn.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Row {
                line: Line::raw(""),
                spot: None,
                tint: None,
                edge: false,
            });
        }
        // Each card sits on its own surface tint with half-block edges
        // (`docs/tui.md`, "Look"); the blank row between cards stays
        // untinted.
        let edge = usize::from(width.saturating_sub(1));
        let card_start = out.len();
        out.push(Row {
            line: surface::edge_row(edge, Role::Surface, true),
            spot: None,
            tint: None,
            edge: true,
        });
        if card == Card::Delegates {
            span = Some(card_start..card_start + drawn.len() + 2);
        }
        out.extend(drawn.into_iter().map(|mut row| {
            row.tint = Some(Role::Surface);
            row
        }));
        out.push(Row {
            line: surface::edge_row(edge, Role::Surface, false),
            spot: None,
            tint: None,
            edge: true,
        });
    }
    (out, span)
}

/// Draws every card's rows into the panel rect: each card on its surface
/// tint from the column's second column, its text at `area.x + 2`,
/// `area.width - 3` columns wide, the first card on `area.y + 1`. The
/// blank row between cards shows the region's `panel` tint.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let text = text_width(area.width);
    let width = u16::try_from(text).unwrap_or(u16::MAX);
    let (rows, span) = rows_and_delegates(app, area.width);
    // A screen that grew never shows a gap: a scroll past the end clamps
    // when drawn.
    let height = usize::from(area.height.saturating_sub(1));
    let skip = app
        .panel_state()
        .scroll()
        .min(rows.len().saturating_sub(height));
    let card_x = area.x.saturating_add(1);
    let card_w = area.width.saturating_sub(1);
    let x = area.x.saturating_add(2);
    let mut y = area.y.saturating_add(1);
    let mut drawn: usize = 0;
    for row in rows.iter().skip(skip) {
        if y >= area.bottom() {
            break;
        }
        if row.edge {
            buf.set_line(card_x, y, &row.line, card_w);
        } else {
            if let Some(tint) = row.tint {
                buf.set_style(
                    Rect::new(card_x, y, card_w, 1),
                    Style::new().bg(tint.color()),
                );
            }
            buf.set_line(x, y, &row.line, width);
            if let Some(spot) = row.spot {
                let wide = u16::try_from(row.line.width().min(text)).unwrap_or(u16::MAX);
                targets.push(Target {
                    id: TargetId::Panel(spot),
                    rect: Rect::new(x, y, wide, 1),
                });
            }
        }
        y = y.saturating_add(1);
        drawn = drawn.saturating_add(1);
    }
    // A drawn delegates state row's spinner moves on the tick.
    if let Some(span) = span {
        let from = skip.max(span.start);
        let to = skip.saturating_add(drawn).min(span.end);
        // Empty or reversed ranges contain no delegate state row.
        delegates::ask(
            app,
            from.saturating_sub(span.start)..to.saturating_sub(span.start),
        );
    }
}

/// `rows` with each row wider than `text` cut to fit, its last cell a
/// `…` in the style of the span it cuts (`docs/tui.md`, "The panel"). A
/// row keeps its target, tint and spans before the cut.
fn fit_rows(rows: Vec<Row>, text: usize) -> Vec<Row> {
    rows.into_iter()
        .map(|mut row| {
            if row.line.width() > text {
                row.line.spans = fit_spans(std::mem::take(&mut row.line.spans), text);
            }
            row
        })
        .collect()
}

/// `spans` cut to `text` columns, ending in `…`.
fn fit_spans(spans: Vec<Span<'static>>, text: usize) -> Vec<Span<'static>> {
    let room = text.saturating_sub(1);
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let width = format::width(&span.content);
        if used + width <= room {
            used += width;
            out.push(span);
            continue;
        }
        let kept = format::cut(&span.content, room - used);
        out.push(Span::styled(format!("{kept}\u{2026}"), span.style));
        return out;
    }
    out
}

/// One card's rows at `text` columns.
fn card_rows(app: &App, card: &Card, text: usize) -> Vec<Row> {
    match card {
        Card::Session => session_rows(app, text),
        Card::ChangedFiles => changed_files_rows(app, text),
        Card::Jobs => jobs_rows(app),
        Card::Delegates => delegates::rows(app, text),
        Card::Widget(at) => widget_rows(app, *at),
    }
}
/// The Session card's rows at `text` columns, each left out when its
/// value is absent (`docs/tui.md`, "The panel").
fn session_rows(app: &App, text: usize) -> Vec<Row> {
    let panel = app.panel_state();
    let mut out = Vec::new();
    if let Some(waiting) = app.rail_waiting() {
        out.push(Row {
            line: Line::raw(format!("{waiting} waiting")),
            spot: Some(Spot::Waiting),
            tint: None,
            edge: false,
        });
    }
    if let Some(status) = panel.status() {
        out.push(plain(format!(
            "directory  {}",
            cut_left(&status.workspace, text.saturating_sub("directory  ".len()))
        )));
    }
    if let Some(row) = branch_row(app) {
        out.push(row);
    }
    let model = panel
        .model()
        .or_else(|| panel.status().map(|status| status.model.as_str()));
    if let Some(model) = model {
        let mut row = format!("model  {model}");
        if let Some(thinking) = panel.thinking() {
            row.push_str(&format!(" · thinking {thinking}"));
        }
        out.push(targeted(row, Spot::Model));
    }
    let window = panel.window().filter(|window| *window > 0);
    let context = window.and_then(|window| {
        panel
            .status()
            .and_then(|status| status.context.as_ref())
            .map(|context| (window, context))
    });
    if let Some((window, context)) = context {
        let pct = context.tokens.saturating_mul(100) / window;
        // The bar: 17 cells toward the handoff point, the marker, and
        // the context size right-aligned (`docs/tui.md`, "The panel").
        out.push(Row {
            line: sides(
                bar_spans(context.tokens, window, panel.trigger_at()),
                Span::raw(short_tokens(context.tokens)),
                text,
            ),
            spot: Some(Spot::Context),
            tint: None,
            edge: false,
        });
        match panel.trigger_at() {
            Some(trigger) => {
                out.push(Row {
                    line: sides(
                        vec![Span::styled(
                            format!("handoff at {}", short_tokens(trigger)),
                            style(Role::Muted),
                        )],
                        Span::styled(format!("{pct}% of window"), style(Role::Muted)),
                        text,
                    ),
                    spot: Some(Spot::Context),
                    tint: None,
                    edge: false,
                });
                out.push(Row {
                    line: Line::styled("then a summary, fresh context", style(Role::Muted)),
                    spot: Some(Spot::Context),
                    tint: None,
                    edge: false,
                });
            }
            None => {
                out.push(Row {
                    line: Line::styled(format!("{pct}% of window"), style(Role::Muted)),
                    spot: Some(Spot::Context),
                    tint: None,
                    edge: false,
                });
            }
        }
    }
    if let Some(status) = panel.status() {
        let tokens = &status.spend.tokens;
        let written = tokens
            .cache_write
            .values()
            .fold(0u64, |sum, n| sum.saturating_add(*n));
        let inside = tokens
            .input
            .saturating_add(tokens.cache_read)
            .saturating_add(written);
        out.push(targeted(
            format!(
                "tokens in / out  {} / {}",
                short_tokens(inside),
                short_tokens(tokens.output)
            ),
            Spot::Usage,
        ));
        if inside > 0 {
            let hits = tokens
                .cache_read
                .saturating_mul(100)
                .checked_div(inside)
                .unwrap_or(0);
            out.push(targeted(format!("cache hits  {hits}%"), Spot::Usage));
        }
        // With a budget the cost row always shows spend against it; a
        // cost still null counts as zero until it settles
        // (`docs/loop.md`, "Spending budget").
        match (status.spend.cost, panel.budget()) {
            (Some(cost), Some(budget)) => {
                out.push(targeted(
                    format!(
                        "cost billed  {} of {}",
                        format::money(cost),
                        format::money(budget)
                    ),
                    Spot::Usage,
                ));
            }
            (None, Some(budget)) => {
                out.push(targeted(
                    format!("cost billed  unknown of {}", format::money(budget)),
                    Spot::Usage,
                ));
            }
            (Some(cost), None) if cost > 0.0 => {
                out.push(targeted(
                    format!("cost billed  {}", format::money(cost)),
                    Spot::Usage,
                ));
            }
            (None, None) => out.push(targeted("cost billed  unknown".to_owned(), Spot::Usage)),
            (Some(_), None) => {}
        }
        if status.spend.subscription_cost > 0.0 {
            out.push(targeted(
                format!(
                    "cost on subscription  {}",
                    format::money(status.spend.subscription_cost)
                ),
                Spot::Usage,
            ));
        }
    }
    if let Some(speed) = panel.speed() {
        out.push(plain(format!("output speed, last reply  {speed} tokens/s")));
    }
    if panel.turns() > 0 {
        out.push(plain(format!("turns  {}", panel.turns())));
    }
    if !panel.down().is_empty() {
        let servers: Vec<String> = panel
            .down()
            .iter()
            .map(|server| format!("{server} down"))
            .collect();
        out.push(Row {
            line: Line::raw(format!("tools  {}", servers.join(", "))),
            spot: Some(Spot::Tools),
            tint: None,
            edge: false,
        });
    }
    out
}

/// How many of the bar's 17 cells fill toward the handoff point:
/// `tokens` against the trigger, or the window with none, clamped to
/// the bar.
fn bar_fill(tokens: u64, window: u64, trigger: Option<u64>) -> usize {
    let goal = trigger.unwrap_or(window);
    let filled = tokens
        .saturating_mul(17)
        .checked_div(goal)
        .unwrap_or(0)
        .min(17);
    usize::try_from(filled).unwrap_or(17)
}

/// The context bar's cells: 17 `▆`, filled toward the handoff point,
/// then the handoff marker when one exists (`docs/tui.md`, "The
/// panel").
fn bar_spans(tokens: u64, window: u64, trigger: Option<u64>) -> Vec<Span<'static>> {
    let fill = bar_fill(tokens, window, trigger);
    let mut spans = Vec::with_capacity(18);
    for _ in 0..fill {
        spans.push(Span::styled("▆", style(Role::Info)));
    }
    for _ in fill..17 {
        spans.push(Span::styled("░", style(Role::Rule)));
    }
    if trigger.is_some() {
        spans.push(Span::styled("│", style(Role::Attention)));
    }
    spans
}

/// A row from `left` spans with `right` right-aligned at `text`
/// columns: at least one space of pad between them, the left cut with
/// `…` when the two cannot fit (`docs/tui.md`, "The panel").
fn sides(left: Vec<Span<'static>>, right: Span<'static>, text: usize) -> Line<'static> {
    let right_w = format::width(&right.content);
    let room = text.saturating_sub(right_w + 1);
    // `fit_spans` keeps a cell for the `…`, so only rows too long for
    // the room go through it; a row that fits keeps every cell.
    let left_w: usize = left.iter().map(|span| format::width(&span.content)).sum();
    let mut out = if left_w <= room {
        left
    } else {
        fit_spans(left, room.max(1))
    };
    let used: usize = out.iter().map(|span| format::width(&span.content)).sum();
    let pad = text.saturating_sub(used + right_w).max(1);
    out.push(Span::raw(" ".repeat(pad)));
    out.push(right);
    Line::from(out)
}

/// The Session card's branch row: the last query's answer, else the
/// status's git. A click runs `git status` (`docs/tui.md`, "Git").
fn branch_row(app: &App) -> Option<Row> {
    let panel = app.panel_state();
    let branch = match panel.branch() {
        Some(Branch::Named(name)) => name.clone(),
        Some(Branch::Detached) => "detached".to_owned(),
        Some(Branch::Absent) => return None,
        None => {
            let git = panel.status().and_then(|status| status.git.as_ref())?;
            git.branch.as_deref().unwrap_or("detached").to_owned()
        }
    };
    Some(Row {
        line: Line::raw(format!("branch  {branch}")),
        spot: Some(Spot::Branch),
        tint: None,
        edge: false,
    })
}

/// A plain row with no target.
fn plain(text: String) -> Row {
    Row {
        line: Line::raw(text),
        spot: None,
        tint: None,
        edge: false,
    }
}

/// A row that opens `spot` when clicked.
fn targeted(text: String, spot: Spot) -> Row {
    Row {
        line: Line::raw(text),
        spot: Some(spot),
        tint: None,
        edge: false,
    }
}

/// `text` cut from the left to at most `max` columns, with a leading `…`.
fn cut_left(text: &str, max: usize) -> String {
    if format::width(text) <= max {
        return text.to_owned();
    }
    let mut kept = Vec::new();
    let mut width = 0;
    for ch in text.chars().rev() {
        let mut buf = [0u8; 4];
        let cell = format::width(ch.encode_utf8(&mut buf));
        if width + cell > max.saturating_sub(1) {
            break;
        }
        width += cell;
        kept.push(ch);
    }
    format!("…{}", kept.iter().rev().collect::<String>())
}

/// A token count without its trailing `token` word.
fn short_tokens(n: u64) -> String {
    let text = format::tokens(n);
    text.strip_suffix(" tokens")
        .or_else(|| text.strip_suffix(" token"))
        .unwrap_or(&text)
        .to_owned()
}

/// A widget card's rows : a dim title row, then each
/// of the widget's lines.
fn widget_rows(app: &App, at: usize) -> Vec<Row> {
    let Some(widget) = app.panel_state().widgets().get(at) else {
        return Vec::new();
    };
    let mut out = vec![Row {
        line: Line::styled(
            format!("{} · {}", widget.extension, widget.widget),
            style(Role::Muted),
        ),
        spot: None,
        tint: None,
        edge: false,
    }];
    out.extend(widget.lines.iter().map(|line| Row {
        line: Line::raw(line.clone()),
        spot: None,
        tint: None,
        edge: false,
    }));
    out
}

/// The Changed files card's rows at `text` columns: the five paths with
/// the most lines changed, ties by path ascending, with the counts at the
/// row's right edge, then the totals over every path. Per-file counts come
/// from `tool_call_completed`'s `changes` (`docs/tui.md`, "The panel").
fn changed_files_rows(app: &App, text: usize) -> Vec<Row> {
    let changes = app.panel_state().changes();
    if changes.is_empty() {
        return Vec::new();
    }
    let paths = changed_files_view::ranked(changes);
    let mut out = Vec::new();
    for (rank, (path, added, removed)) in paths.iter().take(5).enumerate() {
        let counts = format!("+{added} \u{2212}{removed}");
        let room = text.saturating_sub(format::width(&counts).saturating_add(1));
        let shown = cut_left(path, room);
        let pad = " ".repeat(text.saturating_sub(format::width(&shown) + format::width(&counts)));
        out.push(Row {
            line: Line::from(vec![
                Span::raw(format!("{shown}{pad}")),
                Span::styled(format!("+{added}"), style(Role::Added)),
                Span::raw(" ".to_owned()),
                Span::styled(format!("\u{2212}{removed}"), style(Role::Removed)),
            ]),
            spot: Some(Spot::File(rank)),
            tint: None,
            edge: false,
        });
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
    out.push(targeted(
        format!(
            "{} changed  +{added} \u{2212}{removed}",
            format::count(files, "file", "files")
        ),
        Spot::ChangedFiles,
    ));
    out
}

/// The Jobs card's rows at `text` columns: one line saying how many run,
/// shown only while a job runs, then one row per job while open. A
/// delegate is a job with a `delegate_started`, and its own card shows it
/// (`docs/tui.md`, "The panel"). Each listed job row carries its job's
/// serial, so a click opens that job.
fn jobs_rows(app: &App) -> Vec<Row> {
    let running = app.panel_state().running_jobs();
    if running.is_empty() {
        return Vec::new();
    }
    let mut out = vec![Row {
        line: Line::raw(format!(
            "{} running",
            format::count(
                u64::try_from(running.len()).unwrap_or(u64::MAX),
                "job",
                "jobs"
            )
        )),
        spot: Some(Spot::Jobs),
        tint: None,
        edge: false,
    }];
    if app.panel_state().jobs_open() {
        out.extend(running.into_iter().map(|(id, description)| {
            let spot = app.serial_of_job(id).map(Spot::Job);
            Row {
                line: Line::raw(format!("  {description}")),
                spot,
                tint: None,
                edge: false,
            }
        }));
    }
    out
}

/// The card text's width: the rect less the edge and one margin column
/// each side.
fn text_width(width: u16) -> usize {
    usize::from(width).saturating_sub(3)
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
