//! One step (`docs/loop.md`, "One step"): `step_started` and the drains
//! before the request, the spending budget, the streamed reply, and the
//! reply's lines and how they end the step.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;

use contract::events::{
    AssistantMessageCompleted, Empty, Event, MessageOutcome, ToolCallRequested, TurnCompleted,
    TurnOutcome,
};
use contract::provider::{CallError, Delta, Finish, HostedCall, ModelRequest, Reply, ReplyAction};
use contract::shapes::Failure;
use contract::{ActionId, ErrorCode, TurnId};
use serde_json::Value;
use support::lock;

use crate::{Error, Loop, Step, cancel, completion, ended, handoff, mint, usage, util};

impl Loop {
    /// Writes the subdirectory lines the last step's calls queued: after
    /// the completed calls' results, before this step's request is built.
    fn flush_queued(&mut self, turn: &TurnId) -> Result<(), Error> {
        for event in self.changes.take_queued() {
            self.append(&event, turn, None)?;
        }
        Ok(())
    }

    /// One step (`docs/loop.md`, "One step").
    pub(crate) fn step(&mut self, turn: &TurnId) -> Result<Step, Error> {
        self.write_settled()?;
        // A cancel that landed ends the turn before anything is sent: no
        // `step_started`, no request, and queued steers stay queued for
        // the next turn.
        let cancel = Arc::clone(&self.cancel);
        let step = cancel.commit(cancel::Commit::Step, || {
            self.append(&Event::StepStarted(Empty {}), turn, None)
        });
        let Some(written) = step else {
            // A refused commit drops what `turn_end` returned unwritten, so
            // its messages never join a later turn.
            self.lifecycle.continued.clear();
            return Ok(Step::Ended(ended(TurnOutcome::Interrupted, None)));
        };
        written?;
        // A jobs notice comes first, before the drain's `steering_queue`.
        self.write_pending(turn)?;
        // What `turn_end` hooks returned, written before the drain so a
        // steer that arrived meanwhile follows it.
        self.write_held(turn)?;
        // Queued first: a steer held during an approval, or taken by the
        // end-of-turn check, arrived before this drain.
        self.drain(turn)?;
        self.apply_steering(turn)?;
        self.flush_queued(turn)?;
        if let Some(completed) = self.check_context(turn)? {
            return Ok(Step::Ended(completed));
        }
        self.send(turn)
    }

    /// When billed spend has reached `budget.usd`, the turn fails and the
    /// request is not sent (`docs/loop.md`, "Spending budget").
    pub(crate) fn over_budget(&self) -> Option<TurnCompleted> {
        self.budget_failure()
            .map(|failure| ended(TurnOutcome::Failed, Some(failure)))
    }

    /// The same budget refusal for turn steps and separate model requests.
    pub(crate) fn budget_failure(&self) -> Option<Failure> {
        let limit = self.budget?;
        if self.ledger.spend() < limit {
            return None;
        }
        Some(Failure {
            code: ErrorCode::BudgetExceeded,
            message: format!(
                "The session reached its spending budget of ${limit:.2} (budget.usd)."
            ),
            retry_after_ms: None,
            provider: None,
        })
    }

    /// The exit code a tree stop ends a delegate with: 128 plus SIGTERM,
    /// the code the parent's stop signal carries (`docs/invocation.md`,
    /// "Lifecycle").
    const TREE_STOP_CODE: i32 = 143;

    /// Stops the session's running delegates when the turn ends
    /// `budget_exceeded`, as `job_stop` stops one (`docs/loop.md`,
    /// "Spending budget"). Every budget end passes through here, and no
    /// other path stops them: `over_budget` stays pure, because the
    /// handoff note request also reads it and a failed note does not end
    /// the turn. A delegate whose one-way trip has been claimed stops
    /// itself with the tree-stop code instead of draining failed: the
    /// parent's stop signal is already coming for the same trip, and on
    /// a slow signal path the failed drain would otherwise record exit 1
    /// first. Roots never take this path, and failures without a claimed
    /// trip keep their own codes.
    pub(crate) fn budget_end(&self, completed: TurnCompleted) -> TurnCompleted {
        if let Some(jobs) = &self.ending.jobs {
            jobs.stop_delegates();
        }
        if self.is_delegate && lock(&self.spend_trip).tripped() {
            self.cancel.shutdown(Self::TREE_STOP_CODE);
        }
        completed
    }

    /// Sends `request` and streams the reply, emitting each fragment as an
    /// ephemeral event as it arrives: text and tool-call arguments under
    /// `message`, reasoning under its own action, opened with
    /// `reasoning_started` at its first fragment. Returns the reasoning
    /// actions opened, in order, with the reply.
    pub(crate) fn stream(
        &mut self,
        request: &ModelRequest,
        turn: &TurnId,
        message: &ActionId,
        attempt: NonZeroU32,
    ) -> Result<(Result<Reply, CallError>, VecDeque<ActionId>), Error> {
        let cancel = Arc::clone(&self.cancel);
        let Self {
            log,
            provider,
            observe,
            model,
            conversation,
            reviewed,
            known_hosts,
            changes,
            handoff,
            diag,
            ..
        } = self;
        let mut transcript = util::Transcript {
            conversation,
            reviewed,
            had: &mut changes.had,
            carry: &mut handoff.carry,
            known: known_hosts,
        };
        let mut emit = |event: &Event, action: &ActionId| {
            transcript.write(log, &model.reference, event, Some(turn), Some(action))
        };
        // debt: a reasoning fragment does not say which reasoning item it
        // belongs to, so a run of reasoning fragments with nothing between is
        // taken as one action. Two readable items back to back would share an
        // id; exact once `Delta::Reasoning` carries the item's index, as
        // `tool_call_arguments_delta` does.
        let mut opened: VecDeque<ActionId> = VecDeque::new();
        let mut in_reasoning = false;
        let mut failed = None;
        // The session's factory builds the call with its attempt; without
        // one the call goes straight to the provider, as before.
        let observe = observe.clone();
        let call = match observe {
            Some(observe) => observe(request, attempt),
            None => provider.call(request),
        };
        let reply = cancel::run_cancellable_timed(
            &cancel,
            call,
            &mut |delta| {
                #[cfg(test)]
                if crate::timing_tests::take_fail_stream_write() {
                    failed.get_or_insert(crate::timing_tests::log_io_error());
                    return;
                }
                let written = match delta {
                    Delta::Text(text) => {
                        in_reasoning = false;
                        emit(&Event::AssistantMessageDelta(text), message)
                    }
                    Delta::ToolCallArguments(arguments) => {
                        in_reasoning = false;
                        emit(&Event::ToolCallArgumentsDelta(arguments), message)
                    }
                    Delta::Reasoning(text) => {
                        let id = match opened.back() {
                            Some(id) if in_reasoning => Ok(id.clone()),
                            _ => {
                                let id = ActionId(mint("a_"));
                                opened.push_back(id.clone());
                                emit(&Event::ReasoningStarted(Empty {}), &id).map(|()| id)
                            }
                        };
                        in_reasoning = true;
                        id.and_then(|id| emit(&Event::ReasoningDelta(text), &id))
                    }
                };
                if let Err(e) = written {
                    failed.get_or_insert(e);
                }
            },
            Some(diag.writer()),
        );
        match failed {
            Some(e) => Err(e),
            None => Ok((reply, opened)),
        }
    }

    /// Writes a reply's actions, its usage and its completion, and returns
    /// its tool calls and how it finished. A reasoning action with readable
    /// text takes the next action `opened` while it streamed; any other opens
    /// now.
    pub(crate) fn write_reply(
        &mut self,
        reply: Reply,
        mut opened: VecDeque<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(handoff::Calls, Finish), Error> {
        let mut calls = Vec::new();
        let usage = reply.usage();
        for action in reply.actions {
            match action {
                ReplyAction::Text(completed) => {
                    self.append(&Event::TextCompleted(completed), turn, Some(message))?;
                }
                ReplyAction::Reasoning(completed) => {
                    let streamed = if completed.text.is_empty() {
                        None
                    } else {
                        opened.pop_front()
                    };
                    let id = match streamed {
                        Some(id) => id,
                        None => {
                            let id = ActionId(mint("a_"));
                            self.append(&Event::ReasoningStarted(Empty {}), turn, Some(&id))?;
                            id
                        }
                    };
                    self.append(&Event::ReasoningCompleted(completed), turn, Some(&id))?;
                }
                ReplyAction::ToolCall(call) => {
                    let id = ActionId(mint("a_"));
                    let call = self.repaired(call);
                    self.append(&Event::ToolCallRequested(call.clone()), turn, Some(&id))?;
                    calls.push((id, call));
                }
                ReplyAction::Hosted(hosted) => self.write_hosted(&hosted, turn)?,
                ReplyAction::HostedResult { .. } => {
                    // `resolve_orphans` turns each of these into a `Hosted`
                    // or drops it before any write, so none reaches here.
                }
            }
        }
        let model = &self.model;
        let recorded = usage::recorded(
            usage,
            reply.cost,
            &model.reference,
            model.cost.as_ref(),
            model.subscription,
        );
        let lookup = self.provider.cost_lookup();
        self.write_usage(recorded, lookup, Some(turn), Some(message))?;
        self.append(
            &Event::AssistantMessageCompleted(AssistantMessageCompleted {
                outcome: MessageOutcome::Completed,
                error: None,
            }),
            turn,
            Some(message),
        )?;
        Ok((calls, reply.finish))
    }

    /// Writes a paused reply's actions, usage and completion like any reply.
    /// The caller kept the reply's raw blocks before this moved it: the loop
    /// sends them back unchanged with the next request. The hosted pair a
    /// pause split reaches the log here, through the reply's own `Hosted`
    /// actions.
    pub(crate) fn write_paused(
        &mut self,
        reply: Reply,
        opened: VecDeque<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(), Error> {
        let tokens = reply.tokens.clone();
        self.write_reply(reply, opened, turn, message)?;
        self.measure(&tokens);
        Ok(())
    }

    /// Joins a hosted result that arrived without its call against the
    /// paused blocks gathered so far in this step, by the provider's id: a
    /// pair split by a pause is logged whole. One no paused block names is
    /// dropped, as a result with no call always was.
    pub(crate) fn resolve_orphans(reply: &mut Reply, paused: &[Value]) {
        reply.actions.retain_mut(|action| {
            let ReplyAction::HostedResult {
                provider_id,
                completed,
            } = action
            else {
                return true;
            };
            let found = paused.iter().find(|block| {
                block["type"] == "server_tool_use"
                    && block["id"].as_str() == Some(provider_id.0.as_str())
            });
            match found {
                Some(block) => {
                    let call = ToolCallRequested {
                        name: block["name"].as_str().unwrap_or_default().to_owned(),
                        arguments: block.get("input").cloned().unwrap_or(Value::Null),
                        provider_id: Some(provider_id.clone()),
                        repair: None,
                        ran_by: None,
                        provider_item: Some(block.clone()),
                    };
                    let completed = completed.clone();
                    *action = ReplyAction::Hosted(HostedCall { call, completed });
                    true
                }
                None => false,
            }
        });
    }

    /// Writes a reply and ends the step: a tool call ends it with a next
    /// one, once every call has completed.
    pub(crate) fn record(
        &mut self,
        reply: Reply,
        opened: VecDeque<ActionId>,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<Step, Error> {
        let tokens = reply.tokens.clone();
        // The text `turn_end` hooks are shown: read before the reply is
        // written, which moves it.
        let note = crate::handoff::reply_note(&reply);
        let (calls, finish) = self.write_reply(reply, opened, turn, message)?;
        self.measure(&tokens);
        if finish == Finish::OutputLimit {
            // None of a cut-off reply's calls runs (`docs/loop.md`, "A reply
            // cut off by the output limit"). One with no call ends the step
            // as any reply with no call does ("Ending a turn").
            let called = !calls.is_empty();
            for (id, _) in calls {
                self.append(
                    &Event::ToolCallCompleted(completion::truncated()),
                    turn,
                    Some(&id),
                )?;
            }
            if std::mem::replace(&mut self.cut_off, true) {
                return Ok(Step::Ended(ended(
                    TurnOutcome::Failed,
                    Some(Failure {
                        code: ErrorCode::OutputTruncated,
                        message: "Two replies in a row reached the output limit.".to_owned(),
                        retry_after_ms: None,
                        provider: None,
                    }),
                )));
            }
            return Ok(if called {
                Step::Next
            } else {
                Step::Replied(note)
            });
        }
        self.cut_off = false;
        if calls.is_empty() {
            return Ok(Step::Replied(note));
        }
        if self.run_calls(calls, turn)? || self.idle_left {
            // A cancel ended the step: the turn ends `interrupted` instead
            // of taking a next step. An idle approval, or a question the
            // step suspended on, writes nothing more.
            return Ok(Step::Ended(ended(TurnOutcome::Interrupted, None)));
        }
        if let Some(blocked) = self.take_blocked_end() {
            // Headless, the block budget ran out: the step's calls
            // completed, and the turn ends `failed` with code `blocked`
            // (`docs/permissions.md`, "Headless").
            return Ok(Step::Ended(blocked));
        }
        Ok(self.after_calls(turn)?.map_or(Step::Next, Step::Ended))
    }
}
