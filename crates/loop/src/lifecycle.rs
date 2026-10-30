//! The `turn_start`, `before_model_call` and `turn_end` call sites
//! (`docs/extensions.md`, "The hook points"): what each hook is shown,
//! and how its answer reaches the turn.

use contract::events::{
    ContextAdded, Event, InputItem, PreambleReason, SteeringApplied, TurnCompleted, TurnOutcome,
    group_notices, grouped_lead,
};
use contract::hook::{
    BeforeModelCall, CheckAnswer, CheckOutcome, ContextAnswer, ContextOutcome, SessionStart,
    StartReason, StateLine, TurnEnd, TurnStart,
};
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{ErrorCode, SessionId, TurnId};

use std::sync::Arc;

use crate::{Error, Loop, cancel};

#[cfg(test)]
thread_local! {
    /// A shutdown the test runs after the `turn_start` post-hook check,
    /// before the guarded answer write: the order a shutdown landing
    /// between the two would force (`docs/testing.md`, "Waits and
    /// timeouts" allows the hook where no outside seam reaches the race).
    static AFTER_TURN_START_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    /// A shutdown the test runs after `step_started`, before the held
    /// messages' guarded write: the same forced order on the next step's
    /// path.
    static BEFORE_HELD_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    /// A shutdown the test runs after a handoff's `session_start` final
    /// check, before the guarded completion write: the order a shutdown
    /// landing between the two would force.
    static AFTER_HANDOFF_START_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// Runs the hook [`AFTER_TURN_START_CHECK`] holds, if any, then clears it.
#[cfg(test)]
fn pause_after_turn_start_check() {
    AFTER_TURN_START_CHECK.with(|hook| {
        if let Some(paused) = hook.borrow_mut().take() {
            paused();
        }
    });
}

/// Runs the hook [`BEFORE_HELD_WRITE`] holds, if any, then clears it.
#[cfg(test)]
fn pause_before_held_write() {
    BEFORE_HELD_WRITE.with(|hook| {
        if let Some(paused) = hook.borrow_mut().take() {
            paused();
        }
    });
}

/// Runs the hook [`AFTER_HANDOFF_START_CHECK`] holds, if any, then clears
/// it.
#[cfg(test)]
pub(crate) fn pause_after_handoff_start_check() {
    AFTER_HANDOFF_START_CHECK.with(|hook| {
        if let Some(paused) = hook.borrow_mut().take() {
            paused();
        }
    });
}

/// Parks the next handoff completion on `paused`: it runs after the
/// `session_start` final check, before the guarded completion write.
#[cfg(test)]
pub(crate) fn pause_after_handoff_start(paused: Box<dyn FnOnce()>) {
    AFTER_HANDOFF_START_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(paused);
    });
}

/// What `turn_end` hooks returned and the next step writes, and what a
/// resumed turn's `session_start` contexts wait in: held across calls,
/// never in `queued`, so a cancel never carries them into another turn.
/// `ran` is set before the hook runs, so a failed or cancelled run is
/// never retried in the process; a handoff never touches it.
pub(crate) struct State {
    /// Messages `turn_end` hooks continued the turn with, oldest first.
    pub(crate) continued: Vec<contract::hook::Context>,
    /// Why this process's first turn runs `session_start`: `start` on a
    /// new session, `resume` on a resumed one, `rewind` once the rewound
    /// loop adopts it. A handoff always tells the hook `handoff`.
    pub(crate) reason: StartReason,
    /// The session's id, as the hook is told it.
    session: SessionId,
    /// Whether this process already ran `session_start`.
    ran: bool,
    /// A resumed turn's `session_start` contexts, waiting for the next
    /// step to write them after its results.
    start_contexts: Vec<(ContextAdded, Vec<StateLine>)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            continued: Vec::new(),
            reason: StartReason::Start,
            session: SessionId(String::new()),
            ran: false,
            start_contexts: Vec::new(),
        }
    }
}

impl State {
    /// The state for a loop whose preamble builds for `reason`: a new
    /// session starts, anything resumed continues, whatever the build's
    /// own wording (`reload`, `switch`) calls it.
    pub(crate) fn new(reason: PreambleReason, session: &SessionId) -> Self {
        let reason = match reason {
            PreambleReason::Start => StartReason::Start,
            PreambleReason::Resume | PreambleReason::Reload | PreambleReason::Switch => {
                StartReason::Resume
            }
        };
        Self {
            reason,
            session: session.clone(),
            ..Self::default()
        }
    }
}

/// What a `session_start` chain decided: its contexts unwritten, its
/// notices already written, or nothing applied under a cancel.
pub(crate) enum Started {
    /// The contexts the hooks added, in run order, unwritten.
    Contexts(Vec<(ContextAdded, Vec<StateLine>)>),
    /// A `blocking` hook failed with this `hook_failed` failure.
    Failed(Failure),
    /// A cancel or a shutdown landed: nothing is applied.
    Cancelled,
}

/// Whether a suspended turn's `session_start` run lets it finish.
pub(crate) enum Flow {
    /// The turn finishes as resumed.
    Continue,
    /// The suspension stays for the next resume.
    Stop,
}

impl Loop {
    /// Runs the session's `session_start` hooks for `reason`, and writes
    /// the chain's notices at once (`docs/extensions.md`, "When a hook
    /// fails"). A passing chain's contexts come back unwritten, in run
    /// order; a `blocking` failure comes back with the caller's wording
    /// (`failed_so`: what did not happen). A cancel or a shutdown before
    /// the call runs no hook, and one after it applies none of the
    /// answer: only the notices are written either way.
    fn session_start_chain(
        &mut self,
        turn: Option<&TurnId>,
        reason: StartReason,
        failed_so: &str,
    ) -> Result<Started, Error> {
        // A cancel or a shutdown runs no hook. Before the first turn is
        // armed this is a shutdown only.
        if self.turn_cancelled() {
            return Ok(Started::Cancelled);
        }
        let Some(hooks) = self.hooks.clone() else {
            return Ok(Started::Contexts(Vec::new()));
        };
        let answer = hooks.session_start(&SessionStart {
            reason,
            session_id: &self.lifecycle.session,
            workspace: &self.workspace_label,
            cancel: &*self.cancel,
        });
        let _state = hooks.take_change_state();
        self.drain_state(&hooks, turn)?;
        let ContextAnswer { outcome, notices } = answer;
        for notice in group_notices(notices, grouped_lead) {
            self.write(&Event::Notice(notice), turn, None)?;
        }
        // A cancel that landed while the hooks ran applies none of the
        // answer: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Started::Cancelled);
        }
        match outcome {
            // A failure drops the chain's earlier contexts with it: the
            // thing the start guarded does not happen.
            ContextOutcome::Added(contexts) => Ok(Started::Contexts(
                contexts
                    .into_iter()
                    .map(|added| {
                        (
                            ContextAdded {
                                text: added.text,
                                extension: added.extension,
                                hook: "session_start".to_owned(),
                            },
                            added.state,
                        )
                    })
                    .collect(),
            )),
            ContextOutcome::Failed(failure) => Ok(Started::Failed(Failure {
                code: ErrorCode::HookFailed,
                message: format!(
                    "The `session_start` hook of extension {} failed, so {}: {}",
                    failure.extension, failed_so, failure.message
                ),
                retry_after_ms: None,
                provider: None,
            })),
        }
    }

    /// Runs `session_start` once per process, at its first turn: a new,
    /// resumed or rewound session's start (`docs/extensions.md`, "The
    /// hook points"). Empty when already run, with no hooks, or when
    /// cancelled. A `blocking` failure is `Error::HookFailed`, so the
    /// session does not start.
    pub(crate) fn session_start_once(
        &mut self,
    ) -> Result<Vec<(ContextAdded, Vec<StateLine>)>, Error> {
        if self.lifecycle.ran {
            return Ok(Vec::new());
        }
        self.lifecycle.ran = true;
        match self.session_start_chain(None, self.lifecycle.reason, "the session did not start")? {
            Started::Contexts(contexts) => Ok(contexts),
            Started::Failed(failure) => Err(Error::HookFailed(Box::new(failure))),
            Started::Cancelled => Ok(Vec::new()),
        }
    }

    /// Runs `session_start` once for a suspended turn's finish, before its
    /// batch completes (`docs/extensions.md`, "The hook points"). A
    /// passing chain's contexts wait in `start_contexts` for the next
    /// step, never between a call and its result. A `blocking` failure
    /// keeps the suspension for the next resume, then fails; a cancel
    /// keeps it and stops.
    pub(crate) fn start_suspended(
        &mut self,
        suspended: &crate::resume::Suspended,
    ) -> Result<Flow, Error> {
        if self.lifecycle.ran {
            return Ok(Flow::Continue);
        }
        self.lifecycle.ran = true;
        match self.session_start_chain(None, self.lifecycle.reason, "the session did not start")? {
            Started::Contexts(contexts) => {
                self.lifecycle.start_contexts.extend(contexts);
                Ok(Flow::Continue)
            }
            Started::Failed(failure) => {
                self.keep_suspended(suspended)?;
                Err(Error::HookFailed(Box::new(failure)))
            }
            Started::Cancelled => {
                self.keep_suspended(suspended)?;
                Ok(Flow::Stop)
            }
        }
    }

    /// Runs `session_start` with reason `handoff`, once the note is ready
    /// (`docs/extensions.md`, "The hook points"). The hook sees the
    /// reason and not the note. Never touches `ran`.
    pub(crate) fn handoff_start(&mut self, turn: &TurnId) -> Result<Started, Error> {
        self.session_start_chain(
            Some(turn),
            StartReason::Handoff,
            "the handoff did not complete",
        )
    }
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
        let _state = hooks.take_change_state();
        self.drain_state(&hooks, Some(turn))?;
        let contract::hook::ContextAnswer { outcome, notices } = answer;
        for notice in group_notices(notices, grouped_lead) {
            self.append(&Event::Notice(notice), turn, None)?;
        }
        // A cancel that landed while the hooks ran applies none of the
        // answer: only the notices above are written.
        if self.turn_cancelled() {
            return Ok(Some(crate::ended(TurnOutcome::Interrupted, None)));
        }
        match outcome {
            ContextOutcome::Added(contexts) => {
                let contexts: Vec<_> = contexts
                    .into_iter()
                    .map(|added| {
                        (
                            ContextAdded {
                                text: added.text,
                                extension: added.extension,
                                hook: "turn_start".into(),
                            },
                            added.state,
                        )
                    })
                    .collect();
                // Guarded with `Commit::Step` so a shutdown lands before
                // the writes or after them: no context is applied on a
                // shutdown (`docs/extensions.md`, "Hooks").
                #[cfg(test)]
                pause_after_turn_start_check();
                let cancel = Arc::clone(&self.cancel);
                let written = cancel.commit(cancel::Commit::Step, || -> Result<(), Error> {
                    for (added, state) in &contexts {
                        self.write_carried(state.clone(), Some(turn))?;
                        self.append(&Event::ContextAdded(added.clone()), turn, None)?;
                    }
                    Ok(())
                });
                match written {
                    None => Ok(Some(crate::ended(TurnOutcome::Interrupted, None))),
                    Some(result) => result.map(|()| None),
                }
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
        self.drain_state(&hooks, Some(turn))?;
        let CheckAnswer { outcome, notices } = answer;
        for notice in group_notices(notices, grouped_lead) {
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
        let _state = hooks.take_change_state();
        self.drain_state(&hooks, Some(turn))?;
        let ContextAnswer { outcome, notices } = answer;
        for notice in group_notices(notices, grouped_lead) {
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

    /// Writes what the hooks held for the next step: a resumed turn's
    /// `session_start` contexts as `context_added`, then `turn_end`
    /// messages as `steering_applied`, after `step_started` and the jobs
    /// notice, before the drain. Each is the extension's own message,
    /// with no command. Each `session_start` context is guarded with its
    /// own `Commit::Step`: a refused commit keeps the rest unwritten for
    /// the next successful step. The `turn_end` messages are guarded
    /// together, so a shutdown landing after `step_started` applies none
    /// of them: a refused commit drops them unwritten
    /// (`docs/invocation.md`, "What is written").
    pub(crate) fn write_held(&mut self, turn: &TurnId) -> Result<(), Error> {
        // Oldest first: the next successful step writes what a refused
        // one kept, after the results the resumed turn just wrote, never
        // between a call and its result.
        while let Some(first) = self.lifecycle.start_contexts.first().cloned() {
            #[cfg(test)]
            tests::before_state_context_commit();
            let cancel = Arc::clone(&self.cancel);
            let written = cancel.commit(cancel::Commit::Step, || {
                self.write_carried(first.1, Some(turn))?;
                self.append(&Event::ContextAdded(first.0), turn, None)
            });
            match written {
                // Refused: the contexts stay for the next step.
                None => return Ok(()),
                Some(result) => {
                    result?;
                    self.lifecycle.start_contexts.remove(0);
                }
            }
        }
        if self.lifecycle.continued.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        pause_before_held_write();
        let held = std::mem::take(&mut self.lifecycle.continued);
        let cancel = Arc::clone(&self.cancel);
        let written = cancel.commit(cancel::Commit::Step, || -> Result<(), Error> {
            for item in &held {
                self.write_carried(item.state.clone(), Some(turn))?;
                self.append(
                    &Event::SteeringApplied(SteeringApplied {
                        content: vec![ContentPart::Text {
                            text: item.text.clone(),
                        }],
                        sender: Sender {
                            origin: Origin::Extension {
                                extension: item.extension.clone(),
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
        });
        match written {
            // Refused: `held` was already taken, so the messages are
            // dropped unwritten and never join a later turn.
            None => Ok(()),
            Some(result) => result,
        }
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
