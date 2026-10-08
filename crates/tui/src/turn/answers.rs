//! What a question form leaves in its turn's card (`docs/tui.md`, "A
//! question form"): the call that raised it reads "answered" or "declined"
//! in the ledger, and a form the person answered leaves a "you answered"
//! rule where the answer landed, as a steering message does.
//!
//! Both come from durable lines only, so a replayed page, a second client
//! and a resumed session draw the same card.

use contract::Envelope;
use contract::events::{
    Answer, FormAnswer, Interaction, InteractionRequested, InteractionResolved, ResolvedBy,
};
use ratatui::text::Line;

use super::group::Call;
use super::{Entry, Turn};
use crate::app::read;
use crate::format;
use crate::rows::Rows;

/// The form a call raised, and how it was resolved.
#[derive(Debug, Clone)]
pub(crate) struct Asked {
    request_id: String,
    /// Each question's header, in field order.
    headers: Vec<String>,
    outcome: Outcome,
}

/// How a form was resolved; one resolution per request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Pending,
    /// The person answered it.
    Answered,
    /// The person declined it.
    Declined,
    /// Fiber declined it: a cancelled turn, a `close` or a shutdown.
    ByFiber,
}

impl Asked {
    /// What the ledger row ends with in place of the call's status: the
    /// person answered or declined it. `None` while it waits and when Fiber
    /// resolved it, so the status stands.
    pub(crate) fn suffix(&self) -> Option<&'static str> {
        match self.outcome {
            Outcome::Answered => Some(" · answered"),
            Outcome::Declined => Some(" · declined"),
            Outcome::Pending | Outcome::ByFiber => None,
        }
    }
}

/// The "you answered" rule: one row per question in `ask_user`'s result
/// format, then the note.
#[derive(Debug, Clone)]
pub(crate) struct Answered {
    rows: Vec<String>,
}

impl Answered {
    /// Fields and answers pair in order; a field with no answer, or an
    /// answer with no field, draws nothing.
    fn new(headers: &[String], answers: &[FormAnswer], note: Option<&str>) -> Self {
        let mut rows: Vec<String> = headers
            .iter()
            .zip(answers)
            .map(|(header, answer)| match answer {
                FormAnswer::Skipped { .. } => format::answer_row(header, None),
                FormAnswer::Answered { labels, text } => {
                    format::answer_row(header, Some((labels, text.as_deref())))
                }
            })
            .collect();
        rows.extend(note.map(format::note_row));
        Self { rows }
    }

    pub(crate) fn rows(&self, out: &mut Rows) {
        out.push((Line::raw("you answered"), None));
        for row in &self.rows {
            out.push((Line::raw(row.clone()), None));
        }
    }
}

/// Folds `interaction_requested` and `interaction_resolved`; false when no
/// call took the line.
pub(super) fn fold(turns: &mut [Turn], envelope: &Envelope) -> bool {
    match envelope.kind.as_str() {
        "interaction_requested" => {
            read!(envelope, InteractionRequested).is_some_and(|asked| requested(turns, asked))
        }
        "interaction_resolved" => {
            read!(envelope, InteractionResolved).is_some_and(|line| resolved(turns, &line))
        }
        _ => false,
    }
}

/// A form names the calls that raised it. The same request raised again on
/// a resume changes nothing; a new request on the call replaces it.
fn requested(turns: &mut [Turn], asked: InteractionRequested) -> bool {
    let (Interaction::Form { fields }, Some(actions)) = (asked.interaction, asked.action_ids)
    else {
        return false;
    };
    let headers: Vec<String> = fields.into_iter().map(|field| field.header).collect();
    let mut changed = false;
    for action in &actions {
        let Some(call) = call(turns, &action.0) else {
            continue;
        };
        if call
            .asked
            .as_ref()
            .is_some_and(|known| known.request_id == asked.request_id.0)
        {
            continue;
        }
        call.asked = Some(Asked {
            request_id: asked.request_id.0.clone(),
            headers: headers.clone(),
            outcome: Outcome::Pending,
        });
        changed = true;
    }
    changed
}

/// The call `action` names, searching back through the turns as a
/// completion does.
fn call<'a>(turns: &'a mut [Turn], action: &str) -> Option<&'a mut Call> {
    turns
        .iter_mut()
        .rev()
        .flat_map(|turn| turn.groups.iter_mut())
        .find_map(|group| group.call(action))
}

/// The first resolution of a waiting form sets how it ended; the person's
/// answers add the rule to the latest turn holding it.
fn resolved(turns: &mut [Turn], line: &InteractionResolved) -> bool {
    let (outcome, mut form) = match line.by {
        ResolvedBy::Fiber => (Outcome::ByFiber, None),
        ResolvedBy::Person => match &line.answer {
            Answer::Form { answers, note } => (
                Outcome::Answered,
                Some((answers.as_slice(), note.as_deref())),
            ),
            Answer::Declined { .. } => (Outcome::Declined, None),
            Answer::Confirmed { .. } | Answer::Labels { .. } | Answer::Text { .. } => return false,
        },
    };
    let mut changed = false;
    for turn in turns.iter_mut().rev() {
        let mut rule = None;
        let waiting = turn
            .groups
            .iter_mut()
            .flat_map(|group| group.sections.iter_mut())
            .flat_map(|section| section.calls.iter_mut())
            .filter_map(|call| call.asked.as_mut())
            .filter(|asked| asked.request_id == line.request_id.0)
            .filter(|asked| asked.outcome == Outcome::Pending);
        for asked in waiting {
            asked.outcome = outcome;
            changed = true;
            if let Some((answers, note)) = form.take() {
                rule = Some(Answered::new(&asked.headers, answers, note));
            }
        }
        if let Some(rule) = rule {
            turn.entries.push(Entry::Answers(rule));
        }
    }
    changed
}

#[cfg(test)]
#[path = "answers_tests.rs"]
mod tests;
