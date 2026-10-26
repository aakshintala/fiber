//! The clear question while the old session waits (`docs/tui.md`,
//! "Clearing"): the title, the dim wait line, and the choice rows,
//! Enter's focused.

use crate::app::App;
use crate::home::{ClearChoice, Spot};
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[cfg(test)]
#[path = "clear_tests.rs"]
mod tests;

/// The clear choices: clearing anyway, or staying, each beside the
/// click it takes.
const CHOICES: [(&str, &str, ClearChoice); 2] = [
    ("Enter", "clear anyway", ClearChoice::Clear),
    ("Esc", "stay", ClearChoice::Stay),
];

/// Draws the clear question as the framed `Clear` surface, returning the
/// slab it drew, or nothing with the question closed.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    let Some((name, waiting, _)) = app.clear_question() else {
        return Rect::default();
    };
    let article = if waiting == "question" { "a" } else { "an" };
    let choices: Vec<(&str, &str, TargetId)> = CHOICES
        .iter()
        .map(|(key, label, choice)| (*key, *label, TargetId::Home(Spot::Clear(*choice))))
        .collect();
    let question = overlay::choice_question(
        "Clear",
        format!("{name} waits on {article} {waiting}."),
        TargetId::Home(Spot::Clear(ClearChoice::Stay)),
        &choices,
        "click a choice",
    );
    let slab = overlay::draw(buf, area, &question, Place::Centre, targets);
    let joins = vec![crate::rows::Join::Break; question.body.len()];
    overlay::note_overlay(app, buf, slab, &question, area, Place::Centre, &joins);
    slab
}
