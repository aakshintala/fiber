//! The session screen's side panel cards: their order and rows, drawn
//! into the panel rect (`docs/tui.md`, "The panel"). Card text starts past
//! the draggable edge Part 1 draws, with one margin column each side.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::panel::{Branch, Spot};
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::{Target, TargetId};

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
        // debt: the Delegates card draws nothing until it is built; upgrade trigger: its lane on #669.
        // debt: the Quota card draws nothing until a client can read quota; upgrade trigger: #1200 lands.
        // `delegates`, `quota` and unknown names fall through and place nothing.
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

/// One drawn row: its line and the target it is, if any.
pub(crate) struct Row {
    pub(crate) line: Line<'static>,
    pub(crate) spot: Option<Spot>,
}

/// Every card's rows, top to bottom, with one blank row between cards;
/// `width` is the panel rect's.
pub(crate) fn rows(app: &App, width: u16) -> Vec<Row> {
    let widgets = app.panel_state().widgets();
    let names: Vec<(&str, &str)> = widgets
        .iter()
        .map(|widget| (widget.extension.as_str(), widget.widget.as_str()))
        .collect();
    let text = text_width(width);
    let mut out = Vec::new();
    for card in cards(app.panel_cards(), &names) {
        let mut drawn = card_rows(app, &card, text);
        if drawn.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Row {
                line: Line::raw(""),
                spot: None,
            });
        }
        out.append(&mut drawn);
    }
    out
}

/// Draws every card's rows into the panel rect: card text at `area.x + 2`,
/// `area.width - 3` columns wide, the first card on `area.y + 1`. Styles
/// set the foreground only, so the region's tint stays.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let text = text_width(area.width);
    let width = u16::try_from(text).unwrap_or(u16::MAX);
    let rows = rows(app, area.width);
    // A screen that grew never shows a gap: a scroll past the end clamps
    // when drawn.
    let height = usize::from(area.height.saturating_sub(1));
    let skip = app
        .panel_state()
        .scroll()
        .min(rows.len().saturating_sub(height));
    let x = area.x.saturating_add(2);
    let mut y = area.y.saturating_add(1);
    for row in rows.iter().skip(skip) {
        if y >= area.bottom() {
            break;
        }
        buf.set_line(x, y, &row.line, width);
        if let Some(spot) = row.spot {
            let wide = u16::try_from(row.line.width().min(text)).unwrap_or(u16::MAX);
            targets.push(Target {
                id: TargetId::Panel(spot),
                rect: Rect::new(x, y, wide, 1),
            });
        }
        y = y.saturating_add(1);
    }
}

/// One card's rows at `text` columns.
fn card_rows(app: &App, card: &Card, text: usize) -> Vec<Row> {
    match card {
        Card::Session => session_rows(app, text),
        Card::ChangedFiles => changed_files_rows(app, text),
        Card::Jobs => jobs_rows(app, text),
        Card::Widget(at) => widget_rows(app, *at, text),
    }
}
/// The Session card's rows at `text` columns, each left out when its
/// value is absent (`docs/tui.md`, "The panel").
fn session_rows(app: &App, text: usize) -> Vec<Row> {
    let panel = app.panel_state();
    let mut out = Vec::new();
    if let Some(status) = panel.status() {
        out.push(plain(format!(
            "directory  {}",
            cut_left(&status.workspace, text.saturating_sub("directory  ".len()))
        )));
    }
    if let Some(row) = branch_row(app, text) {
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
        out.push(plain(format::cut(&row, text)));
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
        out.push(plain(format!(
            "context  {pct}% of {}",
            format::tokens(window)
        )));
        out.push(Row {
            line: context_bar(context.tokens, window, panel.trigger_at(), text),
            spot: None,
        });
        if let Some(trigger) = panel.trigger_at() {
            let handoff = format!(
                "│ handoff at {}: Fiber writes a summary and the work continues in a fresh context",
                format::tokens(trigger)
            );
            out.extend(format::wrap(&handoff, text).into_iter().map(|row| Row {
                line: Line::styled(row, style(Role::Muted)),
                spot: None,
            }));
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
        out.push(plain(format!(
            "tokens in / out  {} / {}",
            short_tokens(inside),
            short_tokens(tokens.output)
        )));
        if inside > 0 {
            let hits = tokens
                .cache_read
                .saturating_mul(100)
                .checked_div(inside)
                .unwrap_or(0);
            out.push(plain(format!("cache hits  {hits}%")));
        }
        match status.spend.cost {
            // debt: spend shows against budget.usd once preamble_built carries the budget; upgrade trigger: #1201 lands.
            Some(cost) if cost > 0.0 => {
                out.push(plain(format!("cost billed  {}", format::money(cost))));
            }
            None => out.push(plain("cost billed  unknown".to_owned())),
            Some(_) => {}
        }
        if status.spend.subscription_cost > 0.0 {
            out.push(plain(format!(
                "cost on subscription  {}",
                format::money(status.spend.subscription_cost)
            )));
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
        out.push(plain(format!("tools  {}", servers.join(", "))));
    }
    out
}

/// The context bar at `text` cells: `▆` toward the handoff point, `░`
/// past it, and `│` at the handoff point when one is set (`docs/tui.md`,
/// "The panel").
fn context_bar(tokens: u64, window: u64, trigger: Option<u64>, text: usize) -> Line<'static> {
    let cells = u64::try_from(text).unwrap_or(u64::MAX);
    let filled = tokens
        .saturating_mul(cells)
        .saturating_div(window)
        .min(cells);
    let marker = trigger.map(|trigger| {
        trigger
            .saturating_mul(cells)
            .saturating_div(window)
            .min(cells.saturating_sub(1))
    });
    let mut spans = Vec::new();
    for at in 0..text {
        let cell = u64::try_from(at).unwrap_or(u64::MAX);
        if marker == Some(cell) {
            spans.push(Span::styled("│", style(Role::Text)));
        } else if cell < filled {
            spans.push(Span::styled("▆", style(Role::Accent)));
        } else {
            spans.push(Span::styled("░", style(Role::Muted)));
        }
    }
    Line::from(spans)
}

/// The Session card's branch row: the last query's answer, else the
/// status's git. A click runs `git status` (`docs/tui.md`, "Git").
fn branch_row(app: &App, text: usize) -> Option<Row> {
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
        line: Line::raw(format::cut(&format!("branch  {branch}"), text)),
        spot: Some(Spot::Branch),
    })
}

/// A plain row with no target.
fn plain(text: String) -> Row {
    Row {
        line: Line::raw(text),
        spot: None,
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

/// A widget card's rows at `text` columns: a dim title row, then each
/// of the widget's lines.
fn widget_rows(app: &App, at: usize, text: usize) -> Vec<Row> {
    let Some(widget) = app.panel_state().widgets().get(at) else {
        return Vec::new();
    };
    let mut out = vec![Row {
        line: Line::styled(
            format::cut(&format!("{} · {}", widget.extension, widget.widget), text),
            style(Role::Muted),
        ),
        spot: None,
    }];
    out.extend(widget.lines.iter().map(|line| Row {
        line: Line::raw(format::cut(line, text)),
        spot: None,
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
    let mut paths: Vec<(&str, u64, u64)> = changes
        .iter()
        .map(|(path, (added, removed))| (path.as_str(), *added, *removed))
        .collect();
    paths.sort_by(|a, b| {
        b.1.saturating_add(b.2)
            .cmp(&a.1.saturating_add(a.2))
            .then_with(|| a.0.cmp(b.0))
    });
    let mut out = Vec::new();
    for (path, added, removed) in paths.iter().take(5) {
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
            spot: None,
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
    out.push(plain(format!(
        "{} changed  +{added} \u{2212}{removed}",
        format::count(files, "file", "files")
    )));
    out
}

/// The Jobs card's rows at `text` columns: one line saying how many run,
/// shown only while a job runs, then one row per job while open. A
/// delegate is a job with a `delegate_started`, and its own card shows it
/// (`docs/tui.md`, "The panel").
fn jobs_rows(app: &App, text: usize) -> Vec<Row> {
    let panel = app.panel_state();
    let running: Vec<&str> = panel
        .jobs()
        .iter()
        .filter(|(id, _)| !panel.delegate_jobs().contains(id))
        .map(|(_, description)| description.as_str())
        .collect();
    if running.is_empty() {
        return Vec::new();
    }
    let mut out = vec![Row {
        line: Line::raw(format::cut(
            &format!(
                "{} running",
                format::count(
                    u64::try_from(running.len()).unwrap_or(u64::MAX),
                    "job",
                    "jobs"
                )
            ),
            text,
        )),
        spot: Some(Spot::Jobs),
    }];
    if panel.jobs_open() {
        out.extend(
            running
                .into_iter()
                .map(|description| plain(format::cut(&format!("  {description}"), text))),
        )
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
