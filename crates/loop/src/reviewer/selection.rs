//! The handoff selection (`docs/permissions.md`, "At a handoff"): which of
//! the person's messages still bind, asked at each completed handoff and
//! recorded as `reviewer_kept`.

use std::sync::Arc;

use contract::events::{Event, KeptMessage, Notice, ReviewerKept};
use contract::provider::{CallError, Input};
use contract::shapes::True;
use contract::{ErrorCode, TurnId};

use super::shown::Shown;
use super::{ReviewEndpoint, sections};
use crate::{Error, Loop};

/// The listed numbers (1-based) a selection reply keeps, ascending and
/// deduplicated; `Err` never quotes `text`.
pub(crate) fn read_selection(text: &str, listed: usize) -> Result<Vec<usize>, String> {
    const UNREADABLE: &str =
        "expected the numbers of the messages to keep, separated by commas, or `none`";
    let mut words = text.split_whitespace();
    match words.next() {
        None => return Err(UNREADABLE.to_owned()),
        Some(first) if super::clean(first) == "none" => return Ok(Vec::new()),
        Some(_) => {}
    }
    let mut numbers = Vec::new();
    let mut run = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            run.push(ch);
        } else {
            // A run too long to parse, or a number outside `1..=listed`,
            // is ignored: keeping an extra message is the safe direction.
            if let Ok(number) = run.parse::<u64>()
                && let Ok(number) = usize::try_from(number)
                && number >= 1
                && number <= listed
            {
                numbers.push(number);
            }
            run.clear();
        }
    }
    numbers.sort();
    numbers.dedup();
    if numbers.is_empty() {
        return Err(UNREADABLE.to_owned());
    }
    Ok(numbers)
}

/// How many of the oldest `sizes` drop so that the rest fit `window`.
pub(crate) fn dropped_oldest(sizes: &[u64], window: u64) -> usize {
    let mut sum = sizes
        .iter()
        .fold(0u64, |sum, size| sum.saturating_add(*size));
    let mut dropped = 0;
    for size in sizes {
        if sum <= window {
            break;
        }
        sum = sum.saturating_sub(*size);
        dropped += 1;
    }
    dropped
}

/// The `reviewer_selection_failed` notice's message: `why` is Fiber's own
/// cause (a failure message, "the request was cancelled", the budget, or
/// the read error, which never quotes the reply); `dropped` is how many of
/// the oldest the cap dropped.
pub(crate) fn fallback_notice(why: &str, dropped: usize) -> String {
    const KEPT: &str = "The reviewer could not choose which of the person's messages still bind";
    if dropped == 0 {
        format!("{KEPT} ({why}), so it keeps every one.")
    } else if dropped == 1 {
        format!(
            "{KEPT} ({why}), so it keeps them all except the oldest message, dropped to fit \
             its context window."
        )
    } else {
        format!(
            "{KEPT} ({why}), so it keeps them all except the {dropped} oldest, dropped to \
             fit its context window."
        )
    }
}

impl Loop {
    /// At a completed handoff (`docs/permissions.md`, "At a handoff"): asks
    /// the reviewer which of the person's messages still bind, records its
    /// selection as `reviewer_kept`, and resets the reviewer's input to
    /// those messages, word for word, then what follows. Only the numbers
    /// leave a selection reply: no reviewer text enters a later prompt.
    pub(crate) fn reviewer_handoff(&mut self, turn: &TurnId) -> Result<(), Error> {
        let (endpoint, window) = match &self.reviewer {
            Ok(reviewer) => (
                ReviewEndpoint {
                    provider: Arc::clone(&reviewer.provider),
                    reference: reviewer.model.reference.clone(),
                    cost: reviewer.model.cost.clone(),
                    subscription: reviewer.model.subscription,
                    cache_lifetime: reviewer.cache_lifetime,
                    thinking: super::request::lowest(&reviewer.thinking_levels),
                },
                reviewer.context_window,
            ),
            Err(_) => return Ok(()),
        };
        // The person messages in the current input, oldest first, each with
        // its text and its size against the reviewer's window.
        let mut persons: Vec<(KeptMessage, String, u64)> = Vec::new();
        for item in &self.reviewed {
            // A person item is always a user item (`shown.rs` only builds
            // `Shown::Person` with one), so anything else is skipped.
            if let Shown::Person(message) = &item.shown
                && let Input::User { text, .. } = &item.input
            {
                persons.push((
                    *message,
                    text.clone(),
                    crate::handoff::estimate(&item.input),
                ));
            }
        }
        if persons.is_empty() {
            self.append(
                &Event::ReviewerKept(ReviewerKept {
                    kept: Vec::new(),
                    failed: None,
                }),
                turn,
                None,
            )?;
            self.reviewer_sent = None;
            return Ok(());
        }
        let prompt = sections();
        let mut listing = prompt.handoff.clone();
        for (n, (_, text, _)) in persons.iter().enumerate() {
            listing.push_str(&format!("\n{}. {text}", n + 1));
        }
        let base: Vec<Input> = self
            .reviewed
            .iter()
            .map(|item| item.input.clone())
            .collect();
        let mut note: Option<String> = None;
        let mut numbers: Option<Vec<usize>> = None;
        let mut why = String::new();
        for _ in 0..2 {
            if self.review_over_budget() {
                return self.selection_fallback(
                    turn,
                    &persons,
                    window,
                    "the spending budget was reached",
                );
            }
            let mut conversation = base.clone();
            conversation.push(Input::User {
                text: listing.clone(),
                images: Vec::new(),
            });
            if let Some(note) = &note {
                conversation.push(Input::User {
                    text: note.clone(),
                    images: Vec::new(),
                });
            }
            match self.send_review(
                turn,
                &endpoint,
                &prompt.shared,
                conversation,
                self.reviewer_sent,
                None,
            )? {
                Ok(reply) => {
                    let text = reply.text();
                    match read_selection(&text, persons.len()) {
                        Ok(kept) => {
                            numbers = Some(kept);
                            break;
                        }
                        Err(error) => {
                            // The re-ask note is fixed text from the prompt file: the
                            // reply's parse error never reaches the model. It stays in
                            // `why` for the fallback notice, which the person reads.
                            why = error;
                            note = Some(prompt.handoff_reask.clone());
                        }
                    }
                }
                Err(CallError::Failed { failure, .. }) => {
                    return self.selection_fallback(turn, &persons, window, &failure.message);
                }
                Err(CallError::Cancelled { .. }) => {
                    return self.selection_fallback(
                        turn,
                        &persons,
                        window,
                        "the request was cancelled",
                    );
                }
            }
        }
        let Some(numbers) = numbers else {
            return self.selection_fallback(turn, &persons, window, &why);
        };
        let mut kept: Vec<KeptMessage> = Vec::with_capacity(numbers.len());
        let mut sizes: Vec<u64> = Vec::with_capacity(numbers.len());
        for number in &numbers {
            let Some((message, _, size)) = persons.get(number.saturating_sub(1)) else {
                continue;
            };
            kept.push(*message);
            sizes.push(*size);
        }
        let dropped = dropped_oldest(&sizes, window);
        let kept: Vec<KeptMessage> = kept.into_iter().skip(dropped).collect();
        self.append(
            &Event::ReviewerKept(ReviewerKept { kept, failed: None }),
            turn,
            None,
        )?;
        self.reviewer_sent = None;
        Ok(())
    }

    /// The fallback: keeps every person message in the current input, after
    /// the window cap, and says so in a notice.
    fn selection_fallback(
        &mut self,
        turn: &TurnId,
        persons: &[(KeptMessage, String, u64)],
        window: u64,
        why: &str,
    ) -> Result<(), Error> {
        let sizes: Vec<u64> = persons.iter().map(|(_, _, size)| *size).collect();
        let dropped = dropped_oldest(&sizes, window);
        let kept: Vec<KeptMessage> = persons
            .iter()
            .skip(dropped)
            .map(|(message, _, _)| *message)
            .collect();
        self.append(
            &Event::ReviewerKept(ReviewerKept {
                kept,
                failed: Some(True),
            }),
            turn,
            None,
        )?;
        self.append(
            &Event::Notice(Notice {
                code: ErrorCode::ReviewerSelectionFailed,
                message: fallback_notice(why, dropped),
                extension: None,
            }),
            turn,
            None,
        )?;
        self.reviewer_sent = None;
        Ok(())
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
