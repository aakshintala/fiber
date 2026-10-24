//! The clear question while the old session waits (`docs/tui.md`,
//! "Clearing"): the title, the dim wait line, and the choice rows,
//! Enter's focused.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::home::{ClearChoice, Spot};
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place, Row};

#[cfg(test)]
#[path = "clear_tests.rs"]
mod tests;

/// The clear choices: clearing anyway, or staying, each beside the
/// click it takes.
const CHOICES: [(&str, &str, ClearChoice); 2] = [
    ("Enter", "clear anyway", ClearChoice::Clear),
    ("Esc", "stay", ClearChoice::Stay),
];

/// One dim body row.
fn dim_row(text: String) -> Row {
    Row {
        spans: vec![Span::styled(text, Style::new().add_modifier(Modifier::DIM))],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// Draws the clear question as the framed `Clear` surface, returning the
/// slab it drew, or nothing with the question closed.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    let Some((name, waiting, _)) = app.clear_question() else {
        return Rect::default();
    };
    let article = if waiting == "question" { "a" } else { "an" };
    let mut body = vec![
        dim_row(format!("{name} waits on {article} {waiting}.")),
        Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        },
    ];
    // The choices are short, so each draws as one row: the rows line up
    // with the choice they click.
    let labels: Vec<(&str, &str)> = CHOICES
        .iter()
        .map(|(key, label, _)| (*key, *label))
        .collect();
    body.extend(overlay::choices(&labels, 0, 67));
    for (row, (_, _, choice)) in body.iter_mut().skip(2).zip(CHOICES.iter()) {
        row.targets
            .push((0, u16::MAX, TargetId::Home(Spot::Clear(*choice))));
    }
    let framed = overlay::Overlay {
        title: Some(("Clear".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::Home(Spot::Clear(ClearChoice::Stay))),
        body,
        footer: Some(overlay::hint("click a choice")),
        prefer: 71,
    };
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}
