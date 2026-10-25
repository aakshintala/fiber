//! The quit question's drawing: who is working and the three ways out,
//! with Enter's focused (`docs/tui.md`, "Quit"). It centres over the
//! conversation column with a session attached, and over home's area on
//! home, in the overlay frame.

use crate::app::App;
use crate::home::{QuitChoice, Spot, close_all_count, quit_count};
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

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

/// The stop choices: stopping the session now, or cancelling, each
/// beside the click it takes.
const STOP_CHOICES: [(&str, &str, QuitChoice); 2] = [
    ("Enter", "stop", QuitChoice::CloseAll),
    ("Esc", "cancel", QuitChoice::Stay),
];

/// Draws the quit question over `area`: the title, the count of working
/// sessions, the three choices with Enter's focused, and what the mouse
/// does. A click on a choice does what its key does. Returns the drawn
/// slab, empty when the question is not open.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    if let Some((sessions, working, elsewhere)) = app.close_all_question() {
        return draw_close_all(area, buf, targets, sessions, working, elsewhere);
    }
    if let Some((name, working, elsewhere)) = app.stop_question() {
        return draw_stop(area, buf, targets, &name, working, elsewhere);
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

/// Draws the stop question over `area`: the title naming the session,
/// a dim note of whether it was working and open elsewhere when the
/// question opened, the two choices with Enter's focused, and what the
/// mouse does. A click on a choice does what its key does.
fn draw_stop(
    area: Rect,
    buf: &mut Buffer,
    targets: &mut Vec<Target>,
    name: &str,
    working: bool,
    elsewhere: bool,
) -> Rect {
    let note = match (working, elsewhere) {
        (false, false) => "",
        (true, false) => "working",
        (false, true) => "open elsewhere",
        (true, true) => "working, open elsewhere",
    };
    draw_question(
        area,
        buf,
        targets,
        &format!("Stop {name}?"),
        note.to_owned(),
        &STOP_CHOICES,
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
    let choices: Vec<(&str, &str, TargetId)> = choices
        .iter()
        .map(|(key, label, choice)| (*key, *label, TargetId::Home(Spot::Quit(*choice))))
        .collect();
    let question = overlay::choice_question(
        title,
        count,
        TargetId::Home(Spot::Quit(QuitChoice::Stay)),
        &choices,
        footer,
    );
    overlay::draw(buf, area, &question, Place::Centre, targets)
}

#[cfg(test)]
#[path = "quit_tests.rs"]
mod tests;
