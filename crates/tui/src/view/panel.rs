//! The session screen's side panel cards: their order and rows, drawn
//! into the panel rect (`docs/tui.md`, "The panel"). Card text starts past
//! the draggable edge Part 1 draws, with one margin column each side.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::panel::{Branch, Spot};
use crate::changed_files_view;
use crate::format;
use crate::mouse::{Target, TargetId};
use crate::surface;
use crate::theme::{Role, style};

pub(crate) mod delegates;
pub(crate) mod quota;
mod selection;

/// The permission mode every session runs in (`docs/permissions.md`,
/// "One mode"): the literal, shared by the Session title row and the
/// narrow status line's segment.
pub(super) const PERMISSION_MODE: &str = "auto";

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
    /// Each provider credential label's reported quota limits.
    Quota,
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
        if name == "quota" && !out.contains(&Card::Quota) {
            out.push(Card::Quota);
            continue;
        }
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
    // Without colour each card is a thin box: the sides take one column
    // each, so the text fits two narrower (`docs/tui.md`, "Look").
    let text = card_text_width(width);
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
/// tint from the column's second column, its text at `area.x + 3`,
/// `area.width - 4` columns wide, the first card on `area.y + 1`. The
/// blank row between cards shows the region's `panel` tint. Without
/// colour each card is a thin box instead: no tint is laid, the edge
/// rows already draw ┌─┐ and └─┘, and each text row takes │ sides on the
/// card's columns, so targets keep the narrowed visible text.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    // The text width rows_and_delegates fits to: two narrower where a
    // box takes the sides.
    let text = card_text_width(area.width);
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
    // Without colour the rows already carry the box: no tint is laid,
    // so every cell keeps the terminal's default colours.
    let boxed = surface::box_fit(area.width);
    // The doc counts two columns of padding from the slab, so the text
    // starts two columns past it (`docs/tui.md`, "The panel").
    let x = area.x.saturating_add(3);
    let mut y = area.y.saturating_add(1);
    let mut drawn: usize = 0;
    for row in rows.iter().skip(skip) {
        if y >= area.bottom() {
            break;
        }
        if row.edge {
            buf.set_line(card_x, y, &row.line, card_w);
        } else {
            if let Some(tint) = row.tint
                && !boxed
            {
                buf.set_style(
                    Rect::new(card_x, y, card_w, 1),
                    Style::new().bg(tint.color()),
                );
            }
            buf.set_line(x, y, &row.line, width);
            if boxed {
                surface::draw_sides(buf, Rect::new(card_x, y, card_w, 1));
            }
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
    // A drawn delegates state row's spinner moves on the tick. The
    // card's first two rows are its edge and title: the ask range counts
    // from the first delegate row. An empty range asks nothing, so the
    // range needs no guard.
    // The selection reads the same shown rows the draw kept.
    selection::note(
        app,
        &rows,
        skip,
        x,
        area.y.saturating_add(1),
        area.bottom(),
        text,
    );
    super::marks::paint_selection(app, buf, crate::app::SurfaceId::Panel, area);
    // An empty range asks nothing: `ask` fires only on a contained state row.
    if let Some(span) = span {
        let from = skip.max(span.start);
        let to = skip.saturating_add(drawn).min(span.end);
        let first = span.start.saturating_add(2);
        delegates::ask(
            app,
            from.max(first).saturating_sub(first)..to.saturating_sub(first),
        );
    }
}

/// A card's title row at `text` columns: `name` bold on the left, the
/// figures right-aligned (`docs/tui.md`, "The panel"). The name is cut
/// from the left when the two cannot fit.
fn title_row(name: &str, figures: Vec<Span<'static>>, text: usize) -> Row {
    let right_w: usize = figures
        .iter()
        .map(|span| format::width(&span.content))
        .sum();
    let room = text.saturating_sub(right_w.saturating_add(1));
    let shown = crate::text::cut_left(name, room.max(1));
    let used = format::width(&shown);
    let pad = text.saturating_sub(used + right_w).max(1);
    let mut spans = vec![Span::styled(
        shown,
        style(Role::Text).add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(figures);
    Row {
        line: Line::from(spans),
        spot: None,
        tint: None,
        edge: false,
    }
}

/// `n` running: "1 running", "0 running".
fn running_figure(n: usize) -> String {
    let count = u64::try_from(n).unwrap_or(u64::MAX);
    if count == 1 {
        "1 running".to_owned()
    } else {
        format!("{count} running")
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
pub(super) fn fit_spans(spans: Vec<Span<'static>>, text: usize) -> Vec<Span<'static>> {
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

/// One card's rows at `text` columns: its title row, then its content
/// rows. A card with no content rows draws nothing: no edge, no title
/// and no blank row (`docs/tui.md`, "The panel"). Title rows carry no
/// target, except the Jobs title, whose `▸` toggles the list.
fn card_rows(app: &App, card: &Card, text: usize) -> Vec<Row> {
    match card {
        Card::Session => session_rows(app, text),
        Card::ChangedFiles => {
            let content = changed_files_rows(app, text);
            if content.is_empty() {
                return Vec::new();
            }
            let count = app.panel_state().changes().len();
            let mut out = vec![title_row(
                "Changed files",
                vec![Span::styled(count.to_string(), style(Role::Muted))],
                text,
            )];
            out.extend(content);
            out
        }
        Card::Jobs => {
            let running = app.panel_state().running_jobs();
            if running.is_empty() {
                return Vec::new();
            }
            let figure = running_figure(running.len());
            let mut title = title_row(
                "Jobs",
                vec![
                    Span::styled(figure, style(Role::Attention)),
                    Span::styled(" \u{25b8}", style(Role::Muted)),
                ],
                text,
            );
            title.spot = Some(Spot::Jobs);
            let mut out = vec![title];
            out.extend(jobs_rows(app));
            out
        }
        Card::Delegates => {
            let content = delegates::rows_with_done(app, text);
            if content.is_empty() {
                return Vec::new();
            }
            let running = app.panel_state().running_delegates().len();
            let figure = if running > 0 {
                Span::styled(running_figure(running), style(Role::Attention))
            } else {
                Span::styled(running_figure(running), style(Role::Muted))
            };
            let mut out = vec![title_row("Delegates", vec![figure], text)];
            out.extend(content);
            out
        }
        Card::Quota => quota::rows(app, text),
        Card::Widget(at) => widget_rows(app, *at),
    }
}
/// The Session card's rows at `text` columns in `docs/tui.md` order:
/// the working-directory title, the branch slot, the model slot, the
/// context bar with its handoff and explainer rows, then every figure
/// row. Each slot draws from the first frame; the branch text outside
/// a repository and the trailing tools line stay conditional. Labels
/// read dim with their figures right-aligned in the text colour; the
/// bar is 17 cells of `▆` toward the handoff point.
fn session_rows(app: &App, text: usize) -> Vec<Row> {
    let panel = app.panel_state();
    let mut out = Vec::new();
    // The title row holds the working-directory slot from the first
    // frame, so no row moves when the first status names it.
    let workspace = panel
        .status()
        .map(|status| status.workspace.as_str())
        .unwrap_or("");
    out.push(title_row(
        workspace,
        vec![Span::styled(PERMISSION_MODE, style(Role::Attention))],
        text,
    ));
    out.push(branch_row(app));
    let model = panel
        .model()
        .or_else(|| panel.status().map(|status| status.model.as_str()));
    // The model slot stays fixed from the first frame: blank until a
    // preamble names the model, with no placeholder text.
    match model {
        Some(model) => {
            // The model in accent, the thinking level right-aligned in
            // attention. No `∴`: no thinking-state source is held.
            // debt: show the `∴` while the model thinks; upgrade trigger: a thinking flag on the panel fold.
            let line = match panel.thinking() {
                Some(thinking) => sides(
                    vec![Span::styled(model.to_owned(), style(Role::Accent))],
                    Span::styled(thinking.to_owned(), style(Role::Attention)),
                    text,
                ),
                None => Line::from(vec![Span::styled(model.to_owned(), style(Role::Accent))]),
            };
            out.push(Row {
                line,
                spot: Some(Spot::Model),
                tint: None,
                edge: false,
            });
        }
        None => out.push(Row {
            line: Line::from(""),
            spot: Some(Spot::Model),
            tint: None,
            edge: false,
        }),
    }
    let window = panel.window().filter(|window| *window > 0);
    let context = window.and_then(|window| {
        panel
            .status()
            .and_then(|status| status.context.as_ref())
            .map(|context| (window, context))
    });
    // The context bar and the two lines under it draw from the first
    // frame. Figures not known yet show dim dashes, so no row moves
    // when preamble and status folds land (`docs/tui.md`, "The panel").
    // The handoff line follows the preamble alone; the bar fill, the
    // size and the share follow the status, with dim dashes until then.
    let handoff = match panel.trigger_at() {
        Some(trigger) => format!("handoff at {}", short_tokens(trigger)),
        None => "handoff at —".to_owned(),
    };
    let (fill, size, share) = match context {
        Some((window, context)) => {
            let pct = context.tokens.saturating_mul(100) / window;
            (
                bar_spans(context.tokens, window, panel.trigger_at(), 17),
                Span::raw(short_tokens(context.tokens)),
                format!("{pct}% of window"),
            )
        }
        // The empty bar: 17 cells of track and a dim dash for the size.
        None => (
            bar_spans(0, 1, panel.trigger_at(), 17),
            Span::styled("—", style(Role::Muted)),
            "—".to_owned(),
        ),
    };
    // The bar: 17 cells toward the handoff point, the marker, and
    // the context size right-aligned (`docs/tui.md`, "The panel").
    out.push(Row {
        line: sides(fill, size, text),
        spot: Some(Spot::Context),
        tint: None,
        edge: false,
    });
    out.push(Row {
        line: sides(
            vec![Span::styled(handoff, style(Role::Muted))],
            Span::styled(share, style(Role::Muted)),
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
    // Every figure row draws from the first frame: a figure not known
    // yet shows a dim dash, so no row moves when its data arrives
    // (`docs/tui.md`, "The panel").
    match panel.status() {
        Some(status) => {
            let tokens = &status.spend.tokens;
            let written = tokens
                .cache_write
                .values()
                .fold(0u64, |sum, n| sum.saturating_add(*n));
            let inside = tokens
                .input
                .saturating_add(tokens.cache_read)
                .saturating_add(written);
            out.push(figure_row(
                "tokens in / out",
                format!("{} / {}", short_tokens(inside), short_tokens(tokens.output)),
                Some(Spot::Usage),
                text,
            ));
            // A cache-hit rate needs input read in: with none read, the
            // row holds its placeholder rather than a division.
            if inside > 0 {
                let hits = tokens
                    .cache_read
                    .saturating_mul(100)
                    .checked_div(inside)
                    .unwrap_or(0);
                out.push(figure_row(
                    "cache hits",
                    format!("{hits}%"),
                    Some(Spot::Usage),
                    text,
                ));
            } else {
                out.push(reserved_row("cache hits", Some(Spot::Usage), text));
            }
            // With a budget the cost row always shows spend against it; a
            // cost still null counts as zero until it settles
            // (`docs/loop.md`, "Spending budget"). A known billed cost
            // draws whenever known, `$0.00` included.
            let cost = format::spend_against_budget(status.spend.cost, panel.budget())
                .unwrap_or_else(|| match status.spend.cost {
                    Some(cost) => format::money(cost),
                    None => "unknown".to_owned(),
                });
            out.push(figure_row("cost", cost, Some(Spot::Usage), text));
            // The subscription sum draws on every frame once a status
            // holds it, `$0.00` included.
            out.push(figure_row(
                "cost on subscription",
                format::money(status.spend.subscription_cost),
                Some(Spot::Usage),
                text,
            ));
        }
        None => {
            for label in [
                "tokens in / out",
                "cache hits",
                "cost",
                "cost on subscription",
            ] {
                out.push(reserved_row(label, Some(Spot::Usage), text));
            }
        }
    }
    match panel.speed() {
        Some(speed) => {
            out.push(figure_row(
                "speed, last reply",
                format!("{speed} tokens/s"),
                None,
                text,
            ));
        }
        // Before the first timed reply the speed is unknown.
        None => out.push(reserved_row("speed, last reply", None, text)),
    }
    // Zero turns are known, not unknown: the row draws `0` before the
    // first turn completes.
    out.push(figure_row("turns", panel.turns().to_string(), None, text));
    if !panel.down().is_empty() {
        // One `✗ <server> down` per down server, joined by a space and
        // cut from the left when too long for the card.
        let down: Vec<String> = panel
            .down()
            .iter()
            .map(|server| format!("\u{2717} {server} down"))
            .collect();
        let value = crate::text::cut_left(&down.join(" "), text.saturating_sub("tools".len() + 1));
        out.push(Row {
            line: sides(
                vec![Span::styled("tools", style(Role::Muted))],
                Span::styled(value, style(Role::Error)),
                text,
            ),
            spot: Some(Spot::Tools),
            tint: None,
            edge: false,
        });
    }
    // With the rail hidden and sessions waiting, the card ends with the
    // line that shows it (`docs/tui.md`, "The panel").
    if let Some(waiting) = app.rail_waiting().filter(|waiting| *waiting > 0) {
        out.push(Row {
            line: Line::from(vec![
                Span::styled("!", style(Role::Attention).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(" {waiting} waiting · hidden · show"),
                    style(Role::Attention),
                ),
            ]),
            spot: Some(Spot::Waiting),
            tint: None,
            edge: false,
        });
    }
    out
}

/// A figure row: its plain label dim, the figure right-aligned in the
/// text colour, opening `spot` when clicked (`docs/tui.md`, "The
/// panel"). No target when `spot` is none.
fn figure_row(label: &str, value: String, spot: Option<Spot>, text: usize) -> Row {
    Row {
        line: sides(
            vec![Span::styled(label.to_owned(), style(Role::Muted))],
            Span::styled(value, style(Role::Text)),
            text,
        ),
        spot,
        tint: None,
        edge: false,
    }
}

/// A reserved figure row: its plain label dim, a dim dash right-aligned
/// where the figure goes, holding the row's place until its data
/// arrives (`docs/tui.md`, "The panel"). Same target as the figure row.
fn reserved_row(label: &str, spot: Option<Spot>, text: usize) -> Row {
    Row {
        line: sides(
            vec![Span::styled(label.to_owned(), style(Role::Muted))],
            Span::styled("—", style(Role::Muted)),
            text,
        ),
        spot,
        tint: None,
        edge: false,
    }
}

/// How many of the bar's `cells` fill toward the handoff point:
/// `tokens` against the trigger, or the window with none. A zero goal
/// never divides: empty, not a panic. At or past the goal every cell
/// fills.
pub(super) fn bar_fill(tokens: u64, window: u64, trigger: Option<u64>, cells: usize) -> usize {
    let goal = trigger.unwrap_or(window);
    if goal == 0 {
        return 0;
    }
    if tokens >= goal {
        return cells;
    }
    usize::try_from(tokens.saturating_mul(u64::try_from(cells).unwrap_or(u64::MAX)) / goal)
        .unwrap_or(cells)
        .min(cells)
}

/// The context bar's cells: `cells` `▆`, filled toward the handoff
/// point in `info` over the rest in `rule`, then the handoff marker
/// when one exists (`docs/tui.md`, "The panel").
pub(super) fn bar_spans(
    tokens: u64,
    window: u64,
    trigger: Option<u64>,
    cells: usize,
) -> Vec<Span<'static>> {
    let fill = bar_fill(tokens, window, trigger, cells);
    let mut spans = Vec::with_capacity(cells.saturating_add(1));
    for _ in 0..fill {
        spans.push(Span::styled("\u{2586}", style(Role::Info)));
    }
    for _ in fill..cells {
        spans.push(Span::styled("\u{2586}", style(Role::Rule)));
    }
    if trigger.is_some() {
        spans.push(Span::styled("\u{2502}", style(Role::Attention)));
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

/// The Session card's branch slot: "git" dim and the branch in
/// secondary. It stays blank and has no target until a branch is known;
/// outside a repository the line stays blank (`docs/tui.md`, "Git").
fn branch_row(app: &App) -> Row {
    let Some(branch) = branch_text(app) else {
        return Row {
            line: Line::from(""),
            spot: None,
            tint: None,
            edge: false,
        };
    };
    Row {
        line: Line::from(vec![
            Span::styled("git", style(Role::Muted)),
            Span::raw("  "),
            Span::styled(branch, style(Role::Secondary)),
        ]),
        spot: Some(Spot::Branch),
        tint: None,
        edge: false,
    }
}

/// The branch the Session card and the narrow status line show: the
/// last query's answer, else the status's git.
pub(super) fn branch_text(app: &App) -> Option<String> {
    let panel = app.panel_state();
    match panel.branch() {
        Some(Branch::Named(name)) => Some(name.clone()),
        Some(Branch::Detached) => Some("detached".to_owned()),
        Some(Branch::Absent) => None,
        None => {
            let git = panel.status().and_then(|status| status.git.as_ref())?;
            Some(git.branch.as_deref().unwrap_or("detached").to_owned())
        }
    }
}

/// A token count without its trailing `token` word.
pub(super) fn short_tokens(n: u64) -> String {
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
/// row's right edge, then one last row: dim "… N more" on the left when
/// files exceed five, with the totals over every path right-aligned, or
/// the totals alone with five files or fewer. Per-file counts come from
/// `tool_call_completed`'s `changes` (`docs/tui.md`, "The panel").
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
        let shown = crate::text::cut_left(path, room);
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
    let mut spans = Vec::new();
    // The last row the mock shows: dim "… N more" on the left when
    // files exceed five, with the totals right-aligned, or the totals
    // alone. The count is the title's figure.
    if files > 5 {
        spans.push(Span::styled(
            format!("\u{2026} {} more", files.saturating_sub(5)),
            style(Role::Muted),
        ));
    }
    let left_w: usize = spans.iter().map(|span| format::width(&span.content)).sum();
    let counts = format!("+{added} \u{2212}{removed}");
    let pad = " ".repeat(text.saturating_sub(left_w + format::width(&counts)).max(1));
    spans.push(Span::raw(pad));
    spans.push(Span::styled(format!("+{added}"), style(Role::Added)));
    spans.push(Span::raw(" ".to_owned()));
    spans.push(Span::styled(
        format!("\u{2212}{removed}"),
        style(Role::Removed),
    ));
    out.push(Row {
        line: Line::from(spans),
        spot: Some(Spot::ChangedFiles),
        tint: None,
        edge: false,
    });
    out
}

/// The Jobs card's open list rows: one row per running job. The title
/// row carries the count, so a closed card draws the title alone.
fn jobs_rows(app: &App) -> Vec<Row> {
    let running = app.panel_state().running_jobs();
    if running.is_empty() {
        return Vec::new();
    }
    if !app.panel_state().jobs_open() {
        return Vec::new();
    }
    running
        .into_iter()
        .map(|(id, description)| {
            let spot = app.serial_of_job(id).map(Spot::Job);
            Row {
                line: Line::raw(format!("  {description}")),
                spot,
                tint: None,
                edge: false,
            }
        })
        .collect()
}

/// The card text's width: the rect less the slab offset and one margin
/// column each side, two narrower where a box takes the sides
/// (`docs/tui.md`, "Look").
pub(super) fn card_text_width(width: u16) -> usize {
    if surface::box_fit(width) {
        text_width(width).saturating_sub(2)
    } else {
        text_width(width)
    }
}

/// The card text's width: the panel less its slab offset and the two
/// columns of padding on the left and one on the right (`docs/tui.md`,
/// "The panel").
fn text_width(width: u16) -> usize {
    usize::from(width).saturating_sub(4)
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
