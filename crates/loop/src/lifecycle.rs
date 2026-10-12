//! The `turn_start`, `before_model_call` and `turn_end` call sites
//! (`docs/extensions.md`, "The hook points"): what each hook is shown,
//! and how its answer reaches the turn.

use contract::events::{
    ContextAdded, Event, InputItem, SteeringApplied, TurnCompleted, TurnOutcome,
};
use contract::hook::{
    BeforeModelCall, CheckAnswer, CheckOutcome, ContextAnswer, ContextOutcome, TurnEnd, TurnStart,
};
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{ErrorCode, TurnId};

use crate::{Error, Loop};

/// What `turn_end` hooks returned and the next step writes, and what a
/// resumed turn's `session_start` contexts wait in (Part 3): held across
/// calls, never in `queued`, so a cancel never carries them into another
/// turn.
#[derive(Default)]
pub(crate) struct State {
    /// Messages `turn_end` hooks continued the turn with, oldest first.
    pub(crate) continued: Vec<contract::hook::Context>,
}

impl Loop {
    /// Runs the session's `turn_start` hooks on the turn's input, in order,
    /// after its acknowledgements and before its first model request
    /// (`docs/loop.md`, "Starting a turn"). `Some` ends the turn through
    /// `end_turn`; `None` runs its steps.
    pub(crate) fn turn_start(
        &mut self,
        turn: &TurnId,
        input: &[InputItem],
    ) -> Result<Option<TurnCompleted>, Error> {
        // A cancel or a shutdown runs no hook: the turn ends `interrupted`
        // instead.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        let Some(hooks) = self.hooks.clone() else {
            return Ok(None);
        };
        // Items this build does not know carry nothing a hook could read.
        let shown: Vec<InputItem> = input
            .iter()
            .filter(|item| !matches!(item, InputItem::Unknown))
            .cloned()
            .collect();
        let answer = hooks.turn_start(&TurnStart {
            input: &shown,
            cancel: &*self.cancel,
        });
        let contract::hook::ContextAnswer { outcome, notices } = answer;
        for notice in notices {
            self.append(&Event::Notice(notice), turn, None)?;
        }
        // A cancel that landed while the hooks ran applies none of the
        // answer: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        match outcome {
            ContextOutcome::Added(contexts) => {
                for added in contexts {
                    self.append(
                        &Event::ContextAdded(ContextAdded {
                            text: added.text,
                            extension: added.extension,
                            hook: "turn_start".to_owned(),
                        }),
                        turn,
                        None,
                    )?;
                }
                Ok(None)
            }
            ContextOutcome::Failed(failure) => Ok(Some(crate::ended(
                TurnOutcome::Failed,
                Some(Failure {
                    code: ErrorCode::HookFailed,
                    message: format!(
                        "The `turn_start` hook of extension {} failed, \
                         so the turn did not run: {}",
                        failure.extension, failure.message
                    ),
                    retry_after_ms: None,
                    provider: None,
                }),
            ))),
        }
    }

    /// Runs the session's `before_model_call` hooks on the step's built
    /// request, once the budget allows it and before it is sent
    /// (`docs/loop.md`, "Spending budget"). `Some` ends the step without
    /// sending; `None` sends the request.
    pub(crate) fn model_call_allowed(
        &mut self,
        turn: &TurnId,
    ) -> Result<Option<TurnCompleted>, Error> {
        // A cancel or a shutdown sends no request: the turn ends
        // `interrupted` instead.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        let Some(hooks) = self.hooks.clone() else {
            return Ok(None);
        };
        // The spend so far, the delegates' calls included: the same fold
        // the budget check reads.
        let answer = hooks.before_model_call(&BeforeModelCall {
            model: &self.model.reference,
            usage: &self.ledger.usage(),
            cancel: &*self.cancel,
        });
        let CheckAnswer { outcome, notices } = answer;
        for notice in notices {
            self.append(&Event::Notice(notice), turn, None)?;
        }
        // A cancel that landed while the hooks ran sends nothing, whatever
        // they answered: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        match outcome {
            CheckOutcome::Passed => Ok(None),
            // A refusal ends the turn exactly as the budget does, stopping
            // the session's running delegates with it.
            CheckOutcome::Refused(refusal) => Ok(Some(self.budget_end(crate::ended(
                TurnOutcome::Failed,
                Some(Failure {
                    code: ErrorCode::BudgetExceeded,
                    message: format!(
                        "Extension {} refused the model request: {}",
                        refusal.extension, refusal.reason
                    ),
                    retry_after_ms: None,
                    provider: None,
                }),
            )))),
            // A failure stops no delegate: only a refusal is "exactly as
            // `budget.usd` does".
            CheckOutcome::Failed(failure) => Ok(Some(crate::ended(
                TurnOutcome::Failed,
                Some(Failure {
                    code: ErrorCode::HookFailed,
                    message: format!(
                        "The `before_model_call` hook of extension {} failed, \
                         so the request was not sent: {}",
                        failure.extension, failure.message
                    ),
                    retry_after_ms: None,
                    provider: None,
                }),
            ))),
        }
    }

    /// What a step's reply with no tool call leaves to do: the end-of-turn
    /// drain, then the `turn_end` hooks when nothing is waiting. `Some`
    /// ends the turn; `None` takes another step.
    pub(crate) fn replied(
        &mut self,
        turn: &TurnId,
        reply: &str,
    ) -> Result<Option<TurnCompleted>, Error> {
        // A cancel taken before this point runs no hook: the turn ends
        // `interrupted` here, rather than at the next step's commit.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        // The end-of-turn drain first: a steer waiting there continues the
        // turn without calling `turn_end` that time.
        self.drain(turn)?;
        if !self.queued.is_empty() {
            return Ok(None);
        }
        let Some(hooks) = self.hooks.clone() else {
            return Ok(Some(crate::ended(TurnOutcome::Completed, None)));
        };
        let answer = hooks.turn_end(&TurnEnd {
            reply,
            cancel: &*self.cancel,
        });
        let ContextAnswer { outcome, notices } = answer;
        for notice in notices {
            self.append(&Event::Notice(notice), turn, None)?;
        }
        // A cancel that landed while the hooks ran keeps none of the
        // answer: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        match outcome {
            // Held for the next step, which writes them after its commit:
            // never queued, so a message never joins a later turn. An empty
            // answer ends the turn.
            ContextOutcome::Added(contexts) => {
                if contexts.is_empty() {
                    return Ok(Some(crate::ended(TurnOutcome::Completed, None)));
                }
                self.lifecycle.continued.extend(contexts);
                Ok(None)
            }
            // A failure drops the chain's earlier messages with it: the
            // thing the turn guarded does not happen.
            ContextOutcome::Failed(failure) => {
                self.lifecycle.continued.clear();
                Ok(Some(crate::ended(
                    TurnOutcome::Failed,
                    Some(Failure {
                        code: ErrorCode::HookFailed,
                        message: format!(
                            "The `turn_end` hook of extension {} failed: {}",
                            failure.extension, failure.message
                        ),
                        retry_after_ms: None,
                        provider: None,
                    }),
                )))
            }
        }
    }

    /// Writes what the hooks held for the next step: `turn_end` messages as
    /// `steering_applied`, after `step_started` and the jobs notice, before
    /// the drain. Each is the extension's own message, with no command.
    pub(crate) fn write_held(&mut self, turn: &TurnId) -> Result<(), Error> {
        for held in std::mem::take(&mut self.lifecycle.continued) {
            self.append(
                &Event::SteeringApplied(SteeringApplied {
                    content: vec![ContentPart::Text { text: held.text }],
                    sender: Sender {
                        origin: Origin::Extension {
                            extension: held.extension,
                        },
                        command_id: None,
                    },
                    changed_by: None,
                }),
                turn,
                None,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
