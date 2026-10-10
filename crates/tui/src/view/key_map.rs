//! The key map overlay's drawing: every binding by area with its other
//! paths (`docs/tui.md`, "Bindings"). It docks at the bottom, full width,
//! in the overlay frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::bindings::BINDINGS;
use crate::format::{width, wrap, wrap_joined};
use crate::keymap::{self, Columns};
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place, Row};

/// The dim lines under the title: what the map lists, and its counts.
fn dim_lines() -> (String, String) {
    let others = BINDINGS
        .iter()
        .filter(|binding| !binding.other_paths.is_empty())
        .count();
    (
        "Every binding by area, with its other paths.".to_owned(),
        format!("{} actions, {} with other paths", BINDINGS.len(), others),
    )
}

/// One dim body row.
fn dim_row(text: &str) -> Row {
    Row {
        spans: vec![Span::styled(
            text.to_owned(),
            Style::new().add_modifier(Modifier::DIM),
        )],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The tabs row: "All" and then each area, two columns apart. The chosen
/// tab is bold dark on white, the rest dim.
fn tabs_row(tab: usize) -> Row {
    let mut spans = Vec::new();
    for (at, name) in std::iter::once("All").chain(keymap::areas()).enumerate() {
        if at != 0 {
            spans.push(Span::raw("  ".to_owned()));
        }
        if at == tab {
            spans.push(Span::styled(
                name.to_owned(),
                Style::new()
                    .fg(crate::look::BAR_TEXT)
                    .bg(crate::look::TAB_BAR)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                name.to_owned(),
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
    }
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The search line: what it matches, the prompt, the typed query and its
/// caret.
fn search_row(query: &str) -> Row {
    Row {
        spans: vec![
            Span::styled(
                "Type to search shortcuts".to_owned(),
                Style::new().add_modifier(Modifier::DIM),
            ),
            Span::styled("› ".to_owned(), Style::new().add_modifier(Modifier::DIM)),
            Span::styled(query.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled("█".to_owned(), Style::new().add_modifier(Modifier::DIM)),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One column's wrapped rows, each padded to the column's width.
fn column(text: &str, wide: u16) -> Vec<String> {
    let wide = usize::from(wide).max(1);
    wrap(text, wide)
        .into_iter()
        .map(|line| format!("{line}{}", " ".repeat(wide.saturating_sub(width(&line)))))
        .collect()
}

/// The action column's row texts with each row split into the
/// description's and the condition's part: the condition hangs with the
/// action it follows.
fn action_rows(description: &str, condition: &str, wide: u16) -> Vec<(String, String)> {
    let wide = usize::from(wide).max(1);
    let full = if condition.is_empty() {
        description.to_owned()
    } else {
        format!("{description} ({condition})")
    };
    let desc_len = description.chars().count();
    let mut out = Vec::new();
    let mut consumed = 0;
    for (row, join) in wrap_joined(&full, wide) {
        let start = consumed + usize::from(join == crate::rows::Join::WrapSpace);
        let mut head = String::new();
        let mut tail = String::new();
        for (at, ch) in row.chars().enumerate() {
            if start + at < desc_len {
                head.push(ch);
            } else {
                tail.push(ch);
            }
        }
        consumed = start + row.chars().count();
        out.push((head, tail));
    }
    if out.is_empty() {
        out.push((String::new(), String::new()));
    }
    out
}
/// One binding's display rows: the area dim, the action with any
/// condition dim in parentheses after it, and the keys dim, each column
/// wrapping inside its own width. The focused binding reads bold
/// throughout on the bar; a dim ↓ takes the last shown row's gutter while
/// bindings hide below and that row is not the barred one.
fn binding_rows(
    binding: &crate::bindings::Binding,
    shown: &str,
    cols: &Columns,
    focused: bool,
    down: bool,
) -> Vec<Row> {
    let (_, _, paths) = keymap::row_text(binding, shown);
    let mut areas = column(binding.area, cols.area);
    let mut acted = action_rows(binding.description, binding.when, cols.action);
    let mut keys = column(&paths, cols.keys);
    let rows = areas.len().max(acted.len()).max(keys.len());
    while areas.len() < rows {
        areas.push(" ".repeat(usize::from(cols.area)));
    }
    while acted.len() < rows {
        acted.push((String::new(), String::new()));
    }
    while keys.len() < rows {
        keys.push(" ".repeat(usize::from(cols.keys)));
    }
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut out = Vec::new();
    for row in 0..rows {
        let gutter = if focused && row == 0 {
            Span::styled("› ".to_owned(), bold)
        } else if down && row == rows - 1 {
            Span::styled("↓ ".to_owned(), dim)
        } else {
            Span::raw("  ".to_owned())
        };
        let area_style = if focused { bold } else { dim };
        let action_style = if focused { bold } else { Style::default() };
        let cond_style = if focused { bold } else { dim };
        let keys_style = if focused { bold } else { dim };
        let (head, tail) = acted.get(row).cloned().unwrap_or_default();
        let area_text = areas.get(row).cloned().unwrap_or_default();
        let keys_text = keys.get(row).cloned().unwrap_or_default();
        let fill = usize::from(cols.action)
            .saturating_sub(width(&head))
            .saturating_sub(width(&tail));
        out.push(Row {
            spans: vec![
                gutter,
                Span::styled(area_text, area_style),
                Span::raw("  ".to_owned()),
                Span::styled(head, action_style),
                Span::styled(tail, cond_style),
                Span::raw(" ".repeat(fill)),
                Span::raw("  ".to_owned()),
                Span::styled(keys_text, keys_style),
            ],
            right: Vec::new(),
            targets: Vec::new(),
            barred: focused,
        });
    }
    out
}

/// Draws the key map over `area`: the title, the dim lines, the tabs, the
/// search line and the bindings, docked at the bottom full width.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
    let Some(map) = app.keymap() else {
        return;
    };
    let keys = app.keys();
    let visible = map.visible(keys);
    let total = visible.len();
    let inner = overlay::inner(area.width);
    let cols = keymap::columns(&visible, keys, inner);
    let grown = keymap::heights(&visible, keys, &cols);
    let chrome = keymap::chrome(usize::from(area.height));
    let top = map.top().min(total.saturating_sub(1));
    let focus = map.focus().min(total.saturating_sub(1));
    let shown = keymap::fits_from(top, &grown, chrome.body);
    let end = top.saturating_add(shown).min(total);
    let below = total.saturating_sub(end);
    let mut body = Vec::new();
    if chrome.dims {
        let (about, counts) = dim_lines();
        body.push(dim_row(&about));
        body.push(dim_row(&counts));
    }
    if chrome.blank {
        body.push(Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        });
    }
    body.push(tabs_row(map.tab()));
    body.push(search_row(map.query()));
    // While bindings hide above, the last line counts them by binding.
    let counts = top > 0;
    let room = chrome.body.saturating_sub(usize::from(counts));
    if end == top && room > 0 {
        // The focused binding is taller than the body rows left: it
        // shows its top rows only, cut at the body's last row, with no
        // ↓ gutter marker. The window keeps it whole, so it is the
        // focused one.
        if let Some(binding) = visible.get(top).copied() {
            let cut = binding_rows(binding, &keys.shown(binding), &cols, top == focus, false);
            body.extend(cut.into_iter().take(room));
        }
    } else {
        for (at, binding) in visible.iter().enumerate().skip(top).take(shown) {
            let down = below > 0 && at == end.saturating_sub(1) && at != focus;
            body.extend(binding_rows(
                binding,
                &keys.shown(binding),
                &cols,
                at == focus && total > 0,
                down,
            ));
        }
    }
    if counts {
        let mut parts = vec![format!("↑ {} more", top)];
        if below > 0 {
            parts.push(format!("↓ {} more", below));
        }
        body.push(dim_row(&parts.join(" · ")));
    }
    let framed = overlay::Overlay {
        title: Some(("Key map".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::CloseOverlay),
        body,
        footer: chrome
            .footer
            .then(|| overlay::legend(&[("↑↓", "move"), ("←→", "tabs"), ("esc", "closes")])),
        prefer: u16::MAX,
    };
    overlay::draw(buf, area, &framed, Place::Dock, targets);
}

#[cfg(test)]
#[path = "key_map_tests.rs"]
mod tests;
