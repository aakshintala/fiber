//! The quit question's drawing: who is working and the three ways out,
//! with Enter's focused (`docs/tui.md`, "Quit"). It centres over the
//! conversation column with a session attached, and over home's area on
//! home, in the overlay frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::home::{QuitChoice, Spot, close_all_count, quit_count};
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place, Row};

/// The quit choices: leaving working sessions running, closing them all
/// now, or staying, each beside the click it takes.
const CHOICES: [(&str, &str, QuitChoice); 3] = [
    ("Enter", "leave them running", QuitChoice::Leave),
    ("c", "close all", QuitChoice::CloseAll),
    ("Esc", "stay", QuitChoice::Stay),
];

/// The close-all choices: closing them all now, or cancelling, each
/// beside the click it takes.
const CLOSE_ALL_CHOICES: [(&str, &str, QuitChoice); 2] = [
    ("Enter", "close all", QuitChoice::CloseAll),
    ("Esc", "cancel", QuitChoice::Stay),
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

/// Draws the quit question over `area`: the title, the count of working
/// sessions, the three choices with Enter's focused, and what the mouse
/// does. A click on a choice does what its key does. Returns the drawn
/// slab, empty when the question is not open.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    if let Some((sessions, working, elsewhere)) = app.close_all_question() {
        return draw_close_all(area, buf, targets, sessions, working, elsewhere);
    }
    let Some((working, elsewhere)) = app.quit_question() else {
        return Rect::default();
    };
    draw_question(
        area,
        buf,
        targets,
        "Quit",
        quit_count(working, elsewhere),
        &CHOICES,
        "click a choice · they keep running meanwhile",
    )
}

/// Draws the close-all question over `area`: the title, the frozen
/// count of live, working and elsewhere sessions, the two choices with
/// Enter's focused, and what the mouse does. A click on a choice does
/// what its key does.
fn draw_close_all(
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    sessions: usize,
    working: usize,
    elsewhere: usize,
) -> Rect {
    draw_question(
        area,
        buf,
        targets,
        "Close all",
        close_all_count(sessions, working, elsewhere),
        &CLOSE_ALL_CHOICES,
        "click a choice",
    )
}

/// One question overlay: the title, its count line, its choices with
/// Enter's focused, and what the mouse does.
fn draw_question(
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    title: &str,
    count: String,
    choices: &[(&str, &str, QuitChoice)],
    footer: &str,
) -> Rect {
    let mut body = vec![
        dim_row(count),
        Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        },
    ];
    // The choices are short, so each draws as one row: the rows line up
    // with the choice they click.
    let labels: Vec<(&str, &str)> = choices
        .iter()
        .map(|(key, label, _)| (*key, *label))
        .collect();
    body.extend(overlay::choices(&labels, 0, 67));
    for (row, (_, _, choice)) in body.iter_mut().skip(2).zip(choices.iter()) {
        row.targets
            .push((0, u16::MAX, TargetId::Home(Spot::Quit(*choice))));
    }
    let framed = overlay::Overlay {
        title: Some((title.to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::Home(Spot::Quit(QuitChoice::Stay))),
        body,
        footer: Some(overlay::hint(footer)),
        prefer: 71,
    };
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}

#[cfg(test)]
#[path = "quit_tests.rs"]
mod tests;
