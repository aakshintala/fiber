//! The session screen's side panel cards: their order and rows, drawn
//! into the panel rect (`docs/tui.md`, "The panel"). Card text starts past
//! the draggable edge Part 1 draws, with one margin column each side.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::Target;

/// One card the panel draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Card {
    /// The attached session: its directory, model, context, spend, speed,
    /// turns and MCP servers down (`docs/tui.md`, "The panel").
    Session,
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
        if name == "delegates" || name == "quota" {
            continue;
        }
        if name == "session" && !out.contains(&Card::Session) {
            out.push(Card::Session);
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

/// One drawn row: its line.
pub(crate) struct Row {
    pub(crate) line: Line<'static>,
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
            });
        }
        out.append(&mut drawn);
    }
    out
}

/// Draws every card's rows into the panel rect: card text at `area.x + 2`,
/// `area.width - 3` columns wide, the first card on `area.y + 1`. Styles
/// set the foreground only, so the region's tint stays.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, _targets: &mut Vec<Target>) {
    let text = text_width(area.width);
    let width = u16::try_from(text).unwrap_or(u16::MAX);
    let mut y = area.y.saturating_add(1);
    for row in rows(app, area.width) {
        if y >= area.bottom() {
            break;
        }
        buf.set_line(area.x.saturating_add(2), y, &row.line, width);
        y = y.saturating_add(1);
    }
}

/// One card's rows at `text` columns.
fn card_rows(app: &App, card: &Card, text: usize) -> Vec<Row> {
    match card {
        Card::Session => session_rows(app, text),
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
        });
        if let Some(trigger) = panel.trigger_at() {
            let handoff = format!(
                "│ handoff at {}: Fiber writes a summary and the work continues in a fresh context",
                format::tokens(trigger)
            );
            out.extend(format::wrap(&handoff, text).into_iter().map(|row| Row {
                line: Line::styled(row, style(Role::Muted)),
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

/// A plain row.
fn plain(text: String) -> Row {
    Row {
        line: Line::raw(text),
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
    }];
    out.extend(widget.lines.iter().map(|line| Row {
        line: Line::raw(format::cut(line, text)),
    }));
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
