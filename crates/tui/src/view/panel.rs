//! The session screen's side panel cards: their order and rows, drawn
//! into the panel rect (`docs/tui.md`, "The panel"). Card text starts past
//! the draggable edge Part 1 draws, with one margin column each side.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::app::App;
use crate::format;
use crate::markdown::{Role, style};
use crate::mouse::Target;

/// One card the panel draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Card {
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

/// One card's rows at `text` columns: a dim title row, then each of the
/// widget's lines.
fn card_rows(app: &App, card: &Card, text: usize) -> Vec<Row> {
    let Card::Widget(at) = card;
    let Some(widget) = app.panel_state().widgets().get(*at) else {
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
