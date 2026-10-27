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
use jiff::tz::TimeZone;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::group::Call;
use super::{Entry, Turn};
use crate::app::read;
use crate::ext::Input;
use crate::format::{self, answer_parts};
use crate::local_time::time_of_day;
use crate::rows::{RowText, Rows};
use crate::theme::{Role, style};

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

/// What a form left in its turn's card: the answers under a rule with
/// the resolution's time, or one dim row when the person declined it.
#[derive(Debug, Clone)]
pub(crate) enum Answered {
    /// The person answered it: one row per question in `ask_user`'s
    /// result format, then the note, under the rule with the
    /// resolution's time.
    Given {
        parts: Vec<format::AnswerParts>,
        note: Option<String>,
        ts: u64,
    },
    /// The person declined it: one dim row, no rule.
    Declined,
}

impl Answered {
    /// Its card entry: the questions with the resolution's time, or the
    /// decline.
    pub(crate) fn turn_entry(&self) -> crate::ext::TurnAnswers<'_> {
        match self {
            Self::Given { parts, note, ts } => crate::ext::TurnAnswers::Given {
                parts: parts
                    .iter()
                    .map(|part| crate::ext::TurnAnswerPart {
                        header: &part.header,
                        text: &part.text,
                        skipped: part.skipped,
                    })
                    .collect(),
                note: note.as_deref(),
                ts: *ts,
            },
            Self::Declined => crate::ext::TurnAnswers::Declined,
        }
    }

    /// Fields and answers pair in order; a field with no answer, or an
    /// answer with no field, draws nothing.
    fn new(headers: &[String], answers: &[FormAnswer], note: Option<&str>, ts: u64) -> Self {
        let parts: Vec<format::AnswerParts> = headers
            .iter()
            .zip(answers)
            .map(|(header, answer)| match answer {
                FormAnswer::Skipped { .. } => answer_parts(header, None),
                FormAnswer::Answered { labels, text } => {
                    answer_parts(header, Some((labels, text.as_deref())))
                }
            })
            .collect();
        Self::Given {
            parts,
            note: note.map(str::to_owned),
            ts,
        }
    }

    /// A declined form's row.
    fn declined() -> Self {
        Self::Declined
    }

    /// The card's rows at `width` columns in `zone`: the rule like a
    /// steering message's, then one row per question with its header in
    /// `accent` and the answer bold or `skipped` dim, then the note; or
    /// the one dim declined row. A bound `answers` renderer replaces the
    /// rule and the rows it covers: no replacement line carries a target.
    pub(crate) fn rows(&self, width: u16, zone: &TimeZone, out: &mut Rows) {
        let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
        let input = crate::ext::Answers {
            answers: self.turn_entry(),
            flat,
        };
        let wrapped = Input::Answers(input);
        let builtin = |input: &Input<'_>, w: u16| {
            if !matches!(input, Input::Answers(_)) {
                return Vec::new();
            }
            let mut scratch = Rows::default();
            self.builtin_rows(w, zone, &mut scratch);
            let (rows, _) = scratch.into_parts();
            rows.into_iter()
                .map(|(line, _)| crate::ext::spans::from_ratatui(&line.spans))
                .collect()
        };
        let rendered = match out.fills_for("answers") {
            crate::ext::SlotSelection::Draw { extension } => {
                out.ext_lines("answers", &extension, &wrapped, width, &builtin)
            }
            crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
        };
        match rendered {
            Some(fitted) => {
                for line in fitted.iter() {
                    out.push_fitted(line, None);
                }
            }
            None => self.builtin_rows(width, zone, out),
        }
    }

    /// The rule with the resolution's time, the question rows and the
    /// note; or the one dim declined row, as above.
    fn builtin_rows(&self, width: u16, zone: &TimeZone, out: &mut Rows) {
        match self {
            Self::Given { parts, note, ts } => {
                let middle = match time_of_day(*ts, zone) {
                    Some(time) => format!(" · {time} "),
                    None => " ".to_owned(),
                };
                let columns = usize::from(width);
                let dashes = columns.saturating_sub("you answered".len() + format::width(&middle));
                let mut spans = vec![
                    Span::styled(
                        "you answered",
                        style(Role::Accent).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(middle, Style::default().add_modifier(Modifier::DIM)),
                ];
                if dashes > 0 {
                    spans.push(Span::styled("─".repeat(dashes), style(Role::Rule)));
                }
                let tail = dashes.saturating_add(1);
                out.push_text(
                    (Line::from(spans), None),
                    RowText {
                        tail: u16::try_from(tail).unwrap_or(u16::MAX),
                        ..RowText::plain()
                    },
                );
                for part in parts {
                    out.push_text(
                        (
                            Line::from(vec![
                                Span::styled(part.header.clone(), style(Role::Accent)),
                                Span::styled(
                                    part.text.clone(),
                                    if part.skipped {
                                        Style::default().add_modifier(Modifier::DIM)
                                    } else {
                                        Style::default().add_modifier(Modifier::BOLD)
                                    },
                                ),
                            ]),
                            None,
                        ),
                        RowText::plain(),
                    );
                }
                if let Some(note) = note {
                    out.push_text(
                        (
                            Line::from(vec![
                                Span::styled("note", style(Role::Accent)),
                                Span::raw(" "),
                                Span::styled(
                                    format::quoted(note),
                                    Style::default().add_modifier(Modifier::BOLD),
                                ),
                            ]),
                            None,
                        ),
                        RowText::plain(),
                    );
                }
            }
            Self::Declined => out.push((
                Line::styled(
                    "declined: the turn ended so you could answer in your own words",
                    Style::default().add_modifier(Modifier::DIM),
                ),
                None,
            )),
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
        "interaction_resolved" => read!(envelope, InteractionResolved)
            .is_some_and(|line| resolved(turns, &line, envelope.ts)),
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
/// How a form ended: its answers, or declined by the person.
#[derive(Debug)]
enum End<'a> {
    Answers(&'a [FormAnswer], Option<&'a str>),
    Declined,
}

fn resolved(turns: &mut [Turn], line: &InteractionResolved, ts: u64) -> bool {
    let (outcome, mut end) = match line.by {
        ResolvedBy::Fiber => (Outcome::ByFiber, None),
        ResolvedBy::Person => match &line.answer {
            Answer::Form { answers, note } => (
                Outcome::Answered,
                Some(End::Answers(answers.as_slice(), note.as_deref())),
            ),
            Answer::Declined { .. } => (Outcome::Declined, Some(End::Declined)),
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
            if let Some(end) = end.take() {
                rule = Some(match end {
                    End::Answers(answers, note) => Answered::new(&asked.headers, answers, note, ts),
                    End::Declined => Answered::declined(),
                });
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
