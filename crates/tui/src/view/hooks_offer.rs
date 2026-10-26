//! The hooks install offer while a refused `start` waits on it
//! (`docs/extensions.md`, "Hooks declared in configuration"): the title,
//! the dim blocking-names line, and the choice rows, Enter's focused.

use crate::app::App;
use crate::home::{HooksChoice, Spot};
use crate::mouse::{Target, TargetId};
use crate::view::overlay::{self, Place};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// The offer's choices: installing and starting, or cancelling, each
/// beside the click it takes.
const CHOICES: [(&str, &str, HooksChoice); 2] = [
    ("Enter", "install and start", HooksChoice::Install),
    ("Esc", "cancel", HooksChoice::Cancel),
];

/// The offer's names for display: sorted and comma-separated, with
/// control characters as spaces, so the frame wraps plain text.
fn display(names: &[String]) -> String {
    let mut names: Vec<String> = names
        .iter()
        .map(|name| crate::text::controls_to_space(name))
        .collect();
    names.sort();
    names.join(", ")
}

/// Draws the hooks install offer as the framed `HooksOffer` surface,
/// returning the slab it drew, or nothing with the offer closed or its
/// names still on the worker.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    let Some(names) = app.hooks_offer_names() else {
        return Rect::default();
    };
    let choices: Vec<(&str, &str, TargetId)> = CHOICES
        .iter()
        .map(|(key, label, choice)| (*key, *label, TargetId::Home(Spot::HooksOffer(*choice))))
        .collect();
    let question = overlay::choice_question(
        "Install hooks?",
        format!(
            "Configuration declares blocking hooks: {}.",
            display(&names)
        ),
        TargetId::Home(Spot::HooksOffer(HooksChoice::Cancel)),
        &choices,
        "click a choice",
    );
    let before = targets.len();
    let slab = overlay::draw(buf, area, &question, Place::Centre, targets);
    let joins = vec![crate::rows::Join::Break; question.body.len()];
    overlay::note_overlay(app, buf, slab, &question, area, Place::Centre, &joins);
    // One slab-wide target under the frame's own targets, as the delete
    // question keeps: the ✕ and the choices keep their clicks, and every
    // other frame cell swallows its click instead of reaching the rows
    // under it.
    if !slab.is_empty() {
        targets.insert(
            before.min(targets.len()),
            Target {
                id: TargetId::Home(Spot::Question),
                rect: slab,
            },
        );
    }
    slab
}

#[cfg(test)]
#[path = "hooks_offer_tests.rs"]
mod tests;
