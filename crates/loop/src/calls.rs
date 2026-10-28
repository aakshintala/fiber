//! Steps 5 to 7 of a step (`docs/loop.md`, "One step"): every tool call in a
//! reply is checked and decided in order, the approved ones run at once, one
//! thread each, and their results are written in the order the model asked
//! for them (`docs/architecture.md`, "Tool calls in a step").

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;

use contract::clock::{Clock, Wake};
use contract::commands::{RememberScope, ReplyAnswer};
use contract::events::{
    AskStep, CallStatus, DecidedBy, Decision, Event, Grant, PermissionRequested,
    PermissionResolved, Progress, ReviewerUse, RuleOffer, ToolCallCompleted, ToolCallRequested,
    ToolCallStarted, ToolReplaced,
};
use contract::jobs::Taken;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects};
use contract::tool::{Bound, Cancel, Effects, Output, Tool};
use contract::{ActionId, RequestId, SessionId, TurnId};
use log::Log;
use serde_json::{Map, Value};

use super::before_tool::Prepared;
use super::completion::{completed, denied, resolved};
use super::permission::Verdict;
use super::reviewer::{
    First, ReviewEndpoint, Sections, Stage1Worker, StagedSecond, StagedStage1, UnderReview,
    fanout_stage1, read_first, read_second, sections, under_item,
};
use crate::asking::Release;
use crate::inner_calls::{InnerRunning, InnerSlot};
use crate::interactions::Suspend;
use crate::progress::{SharedWake, Stream};
use crate::{Error, Loop, schema};

/// One call in a step: decided calls wait their turn while running calls
/// stream their output.
struct Running {
    id: ActionId,
    state: State,
}

/// What a call in a step is doing.
enum State {
    /// Decided without running: its completion is written in request order.
    Ready(Box<ToolCallCompleted>),
    /// Running on its own thread, streaming through its call's emitter.
    /// `declared` rides along for the change check at completion; `tool`
    /// and `arguments` for the hooks; `rewritten` for the result line.
    Running {
        bound: Bound,
        stream: Arc<Stream>,
        declared: DeclaredEffects,
        tool: String,
        arguments: Arc<Map<String, Value>>,
        rewritten: Option<Vec<String>>,
    },
    /// Its completion is written.
    Done,
}

/// Whether a call's completion is written.
fn is_done(call: &Running) -> bool {
    matches!(call.state, State::Done)
}

/// Each call without a written completion, in request order, with its
/// stream while it runs.
fn unwritten(running: &[Running]) -> Vec<(&ActionId, Option<&Stream>)> {
    let calls = running.iter().filter_map(|call| match &call.state {
        State::Running { stream, .. } => Some((&call.id, Some(stream.as_ref()))),
        State::Ready(_) => Some((&call.id, None)),
        State::Done => None,
    });
    calls.collect()
}

/// The written delta's payload serialised as JSON, in bytes
/// (`docs/tools.md`, "Progress"): what the next interval is paced on.
fn encoded(delta: &Progress) -> u64 {
    let bytes = serde_json::to_vec(delta)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// A call's decision: it runs, or it completes as given without running.
///
/// Public as [`crate::ReviewDecision`] for the `review` jig's replay
/// (`docs/testing.md`, "Jigs").
pub type Decided = Result<Prepared, Box<ToolCallCompleted>>;

/// What a person's answer carries onto its `permission_resolved` line.
pub(crate) struct Answered {
    /// Allow or deny.
    pub(crate) decision: Decision,
    /// With a denial, what the person typed.
    pub(crate) feedback: Option<String>,
    /// On an allow that added a session grant.
    grant: Option<Grant>,
    /// On an allow that added a standing rule to the project's rules file.
    rule: Option<Grant>,
    /// Why a remembered rule is missing: saving it failed.
    reason: Option<String>,
}

impl Answered {
    /// The `permission_resolved` line for a person's allow of the call
    /// `request_id` answered: a remembered session grant rides `grant`, a
    /// remembered project rule rides `rule`, and a rule that could not be
    /// saved rides `reason`.
    pub(crate) fn allow(
        self,
        request_id: RequestId,
        hosts: Option<Vec<String>>,
    ) -> PermissionResolved {
        PermissionResolved {
            hosts,
            grant: self.grant,
            rule: self.rule,
            ..resolved(
                Some(request_id),
                Decision::Allow,
                DecidedBy::Person,
                self.reason,
                None,
            )
        }
    }
}

/// A registered tool: who registered it (`builtin`, or the extension or MCP
/// server), the tool, and its definition.
pub(crate) type Registered = (String, Arc<dyn Tool>, ToolDefinition);

/// `tools`, each paired with who registered it, by name. A later tool of a
/// name already taken replaces the earlier one, and each replacement is
/// returned (`docs/architecture.md`, "Tool seam"). No two extension tools
/// in `tools` share a name: the extensions settle that before the list is
/// built, and their tools come after the built-in and MCP tools.
pub(crate) fn register(
    tools: Vec<(String, Arc<dyn Tool>)>,
) -> (BTreeMap<String, Registered>, Vec<ToolReplaced>) {
    let mut registered = BTreeMap::new();
    let mut replaced = Vec::new();
    for (by, tool) in tools {
        let definition = tool.definition();
        let name = definition.name.clone();
        if let Some((from, _, _)) = registered.insert(name.clone(), (by.clone(), tool, definition))
        {
            replaced.push(ToolReplaced { name, from, to: by });
        }
    }
    (registered, replaced)
}

/// One `JobRecord` as its event, the delegate's finish bounded as it is
/// written: the one mapping `finish`, `write_end` and `write_job_end`
/// share.
pub(crate) fn job_event(log: &Log, record: contract::jobs::JobRecord) -> Event {
    match record {
        contract::jobs::JobRecord::Started(started) => Event::JobStarted(started),
        contract::jobs::JobRecord::DelegateStarted(started) => Event::DelegateStarted(started),
        contract::jobs::JobRecord::DelegateFinished(finished) => {
            Event::DelegateFinished(crate::delegated::bounded(log, finished))
        }
        contract::jobs::JobRecord::Completed(completed) => Event::JobCompleted(completed),
    }
}

impl Loop {
    /// `call` with the repair its tool's schema allows, if any
    /// (`docs/tools.md`, "Before a call runs").
    pub(crate) fn repaired(&self, mut call: ToolCallRequested) -> ToolCallRequested {
        if let Some((_, _, definition)) = self.tools.get(&call.name) {
            call.repair = schema::repair(&definition.input_schema, &call.arguments);
        }
        call
    }

    /// Writes one record taken from a job's queue. A record carrying the
    /// queuing call's action is written under that action and that
    /// action's turn, whichever taker writes it; one without is written
    /// under the writer's own. A queued action the loop never started
    /// falls back to the writer's turn. A claimed end runs through the
    /// `completion` hook first, so the log holds what the hook returned
    /// (`docs/extensions.md`, "Recording a change"); the `output_file`
    /// pass already ran at intake. Records [`Loop::write_end`] already
    /// hooked go through [`Loop::write_taken_raw`] instead.
    pub(crate) fn write_taken(
        &mut self,
        taken: Taken,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), Error> {
        // A `blocking` failure withholds the end with the documented
        // line; a hook that changed the notice replaces `output_tail`.
        let record = taken.record;
        let record = if let contract::jobs::JobRecord::Completed(completed) = record {
            contract::jobs::JobRecord::Completed(self.deliver_completion(completed, turn)?)
        } else {
            record
        };
        self.write_taken_raw(
            Taken {
                record,
                action: taken.action,
            },
            turn,
            action,
        )
    }

    /// [`Loop::write_taken`] without the `completion` hook: records
    /// `write_end` already ran through `deliver_end`.
    pub(crate) fn write_taken_raw(
        &mut self,
        taken: Taken,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), Error> {
        let event = job_event(&self.log, taken.record);
        match taken.action {
            Some(action) => {
                // Cloned before the write: the map is the loop thread's,
                // and the write borrows the loop mutably.
                let turn = match self.call_turns.get(&action) {
                    Some(turn) => Some(turn.clone()),
                    None => turn.cloned(),
                };
                self.write(&event, turn.as_ref(), Some(&action))?;
            }
            None => {
                self.write(&event, turn, action)?;
            }
        }
        Ok(())
    }

    /// Decides every call in order, then runs the approved ones concurrently and
    /// writes each call's completion in request order. Deciding waits for a
    /// person's reply, so every decision line is written before any
    /// `tool_call_started`. Returns true when a cancel ended the step: the
    /// caller ends the turn `interrupted` instead of taking a next step.
    pub(crate) fn run_calls(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested)>,
        turn: &TurnId,
    ) -> Result<bool, Error> {
        let calls = calls.into_iter().map(|(id, call)| (id, call, None));
        self.run_batch(calls.collect(), turn, None, None)
    }

    /// [`Loop::run_calls`] over a batch some of whose calls are already
    /// decided: a call carrying its decision is not judged again, and the
    /// rest are judged in order as `run_calls` judges them. `reraise` names
    /// a call that already has its `tool_call_started` and the request it
    /// was waiting on: it runs again with no second `tool_call_started`, and
    /// its ask is raised again under that `request_id` (`docs/events.md`,
    /// "Resume").
    pub(crate) fn run_batch(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested, Option<Decided>)>,
        turn: &TurnId,
        reraise: Option<(ActionId, RequestId)>,
        carried: Option<(ActionId, Vec<contract::hook::StateLine>)>,
    ) -> Result<bool, Error> {
        let undecided = calls
            .iter()
            .filter(|(_, _, already)| already.is_none())
            .count();
        let endpoint = if undecided > 1 && self.budget.is_none() {
            self.review_endpoint()
        } else {
            None
        };
        let decided = match endpoint {
            // More than one call still to judge, with no budget to spend:
            // the batch sends every review's first stage at once, then
            // writes everything back in request order. A reviewer that
            // could not be set up falls back to the serial path.
            Some(endpoint) => self.decide_concurrent(calls, turn, carried, endpoint)?,
            None => self.decide_serial(calls, turn, carried)?,
        };
        let Some(decided) = decided else {
            // An idle deadline ended an approval. Drop the decision: no
            // completion is written, including for calls already decided.
            return Ok(false);
        };
        let cancel = Arc::clone(&self.cancel);
        thread::scope(|scope| {
            let wake = Arc::new(SharedWake::default());
            // The signal wakes the park below, so a cancel with nothing
            // due and nothing returned still ends the wait.
            let step_wake: Arc<dyn Wake> = wake.clone();
            cancel.subscribe(Arc::downgrade(&step_wake));
            // A clock move wakes the wait below: without it a held change
            // would wait for the call to end instead of its interval, and
            // the timer that flushes it would never fire (`docs/tools.md`,
            // "Progress").
            let clock: Arc<dyn Clock> = Arc::clone(self.log.clock());
            let clock_wake: Arc<dyn Wake> = wake.clone();
            clock.subscribe(Arc::downgrade(&clock_wake));
            let mut running: Vec<Running> = Vec::new();
            let mut inner = Vec::new();
            // Closes every call's ask on any return, so no worker the scope
            // joins is left blocked in one.
            let mut release = Release::default();
            let mut suspend = Suspend::default();
            let mut reraised = false;
            for (id, name, decision, carried) in decided {
                let state = match decision {
                    Err(completed) => State::Ready(completed),
                    // A cancel that landed while a later call was decided
                    // ends an approved call before it starts: no
                    // `tool_call_started`, completed `cancelled`. A decision
                    // line already written stays.
                    Ok(_) if self.turn_cancelled() => State::Ready(self.cancelled_before_ran()),
                    Ok(prepared) => {
                        let again = reraise
                            .as_ref()
                            .filter(|(action, _)| *action == id)
                            .map(|(_, request)| request.clone());
                        if again.is_none() {
                            let (arguments, changed_by) = prepared.rewrite();
                            self.write_carried(carried, Some(turn))?;
                            self.append(
                                &Event::ToolCallStarted(ToolCallStarted {
                                    always_reviewed: prepared.effects.always_reviewed,
                                    declared: prepared.effects.declared.clone(),
                                    arguments,
                                    changed_by,
                                }),
                                turn,
                                Some(&id),
                            )?;
                        }
                        let bound = prepared.tool.bound();
                        // `fiber ask`, a delegate, after `close`, or no inbox
                        // wake: nobody can answer (`docs/tools.md`, "Asking
                        // the person").
                        let answerable = self.answerable && self.inbox_wake.is_some();
                        let slot = Arc::new(InnerSlot::new(
                            Arc::clone(&wake),
                            id.clone(),
                            name.clone(),
                            0,
                        ));
                        let stream = Arc::new(
                            Stream::new(Arc::clone(&wake), id.clone(), answerable).with_inner(slot),
                        );
                        // The call's turn, read back when a record the call
                        // queued is written, whichever taker writes it.
                        self.call_turns.insert(id.clone(), turn.clone());
                        if let Some(request) = again
                            && answerable
                        {
                            stream.asking().reraise(request);
                            reraised = true;
                        }
                        release.add(Arc::clone(&stream));
                        let thread_stream = Arc::clone(&stream);
                        let call_cancel = Arc::clone(&cancel);
                        let tool = Arc::clone(&prepared.tool);
                        let arguments = Arc::new(prepared.arguments);
                        let thread_arguments = Arc::clone(&arguments);
                        // The exact prepared effects ride into the run: a
                        // reload between `tool_call_started` and spawn must
                        // not change the call's declared decision.
                        let effects = Arc::new(prepared.effects);
                        let thread_effects = Arc::clone(&effects);
                        // The entered run's worker context: at `info` this
                        // is `None` and the name stays uncloned, so an
                        // untimed level changes nothing about the spawn.
                        let worker = self.diag.worker().map(|diag| (diag, name.clone()));
                        scope.spawn(move || {
                            // The entered run's timer, from entry to return,
                            // before the stream takes the output: result
                            // buffering and ordered completion stay outside.
                            let output = {
                                let _run = match worker.as_ref() {
                                    Some((diag, name)) => {
                                        diag.timed_tool(contract::diag::Op::ToolRun, name)
                                    }
                                    None => None,
                                };
                                tool.run_with_effects(
                                    &thread_arguments,
                                    &thread_effects,
                                    call_cancel.as_ref(),
                                    thread_stream.as_ref(),
                                    thread_stream.as_ref(),
                                )
                            };
                            thread_stream.finish(output);
                        });
                        State::Running {
                            bound,
                            stream,
                            declared: effects.declared.clone(),
                            tool: name,
                            arguments,
                            rewritten: prepared.rewritten,
                        }
                    }
                };
                running.push(Running { id, state });
            }
            // A request raised again whose call does not run, or runs with
            // nobody to answer, is Fiber's to decline before the call
            // completes (`docs/invocation.md`, "Lifecycle").
            if let Some((_, request)) = &reraise
                && !reraised
            {
                self.declined(request.clone(), Some(turn))?;
            }
            loop {
                // Own the stream handles while taking requests, so the pass can
                // add new inner streams without borrowing its running list.
                let streams: Vec<_> = running
                    .iter()
                    .filter_map(|call| match &call.state {
                        State::Running { stream, .. } => {
                            Some((call.id.clone(), Arc::clone(stream)))
                        }
                        State::Ready(_) | State::Done => None,
                    })
                    .chain(
                        inner
                            .iter()
                            .map(|call: &InnerRunning| (call.id.clone(), Arc::clone(&call.stream))),
                    )
                    .collect();
                let streams: Vec<_> = streams
                    .iter()
                    .map(|(id, stream)| (id, stream.as_ref()))
                    .collect();
                self.take_inner(scope, &streams, &mut inner, &mut release, &wake, Some(turn))?;
                let wrote_inner = self.serve_inner(
                    &mut inner,
                    &|id| {
                        running
                            .iter()
                            .find(|call| &call.id == id)
                            .is_none_or(|call| match &call.state {
                                State::Running { stream, .. } => stream.returned(),
                                State::Ready(_) | State::Done => true,
                            })
                    },
                    Some(turn),
                )?;
                let mut calls = unwritten(&running);
                calls.extend(
                    inner
                        .iter()
                        .map(|call| (&call.id, Some(call.stream.as_ref()))),
                );
                self.serve_interactions(&calls, Some(turn))?;
                let now = clock.now();
                // Every delta due now, in request order. A held change the
                // interval still covers stays held for the flush in
                // `complete_next` below.
                for call in running.iter() {
                    let State::Running { stream, .. } = &call.state else {
                        continue;
                    };
                    if let Some(delta) = stream.take_due(now) {
                        let bytes = self.write_delta(&delta, Some(turn), &call.id);
                        // Read after the write: the interval starts when the
                        // delta went out, measured after it
                        // (`docs/tools.md`, "Progress").
                        stream.wrote(bytes, clock.now());
                    }
                }
                // Every returned call's final flush, the moment its tool
                // returns: only calls that returned give up what they hold,
                // so a later call's flush never waits for an earlier call.
                for call in running.iter() {
                    let State::Running { stream, .. } = &call.state else {
                        continue;
                    };
                    if let Some(delta) = stream.take_flush() {
                        self.write_delta(&delta, Some(turn), &call.id);
                    }
                }
                // The next completion in request order, if its prefix is done.
                // A returned call's flush already went out in the pass above;
                // what `take_finished` still holds is only the safety net,
                // written before its completion all the same.
                if self.complete_next(&mut running, &inner, turn)? || wrote_inner {
                    continue;
                }
                if running.iter().all(is_done) {
                    return Ok(self.turn_cancelled());
                }
                // Nothing to write: wait until the earliest held change or
                // `until` is due, an emit, a call returning, an ask or a
                // clock move. Each pass either wrote something above or
                // waits here, never spins.
                let mut calls = unwritten(&running);
                calls.extend(
                    inner
                        .iter()
                        .map(|call| (&call.id, Some(call.stream.as_ref()))),
                );
                let earliest = calls
                    .iter()
                    .filter_map(|(_, stream)| stream.and_then(Stream::deadline))
                    .chain(Self::interaction_deadline(&calls))
                    .min();
                // Suspended on a question: nothing more is written, as an
                // idle approval writes nothing more.
                if self.wait_step(&wake, &calls, earliest, &mut suspend, turn)? {
                    return Ok(false);
                }
            }
        })
    }

    /// Checks `call`, runs its hooks and decides whether it runs
    /// (`docs/loop.md`, "Tool calls that do not run", and
    /// `docs/permissions.md`, "The order a call is judged in"). The
    /// decision judges the prepared call, not the one the model asked for.
    fn decide(
        &mut self,
        call: &ToolCallRequested,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<(Decided, Vec<contract::hook::StateLine>), Error> {
        let (prepared, state) = self.prepared(call, Some(turn))?;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failed) => return Ok((Err(failed), Vec::new())),
        };
        // The permission order starts once the call names a registered
        // tool and fits its schema: the guard spans the judge, the
        // reviewer and a person's wait, and closes at the decision,
        // before any run. `call.name` is the session map's key: `checked`
        // above refused anything else, so no lookup or allocation here.
        let _permission = self
            .diag
            .timed_tool(contract::diag::Op::ToolPermission, &call.name);
        let judged = self.judge_call(&call.name, &prepared.effects);
        let decided = match judged {
            // Step 7 is the reviewer: the call is judged by a separate
            // model (`docs/permissions.md`, "The reviewer").
            Verdict::Review => self.review(call, id, turn, prepared),
            judged @ (Verdict::Deny { .. } | Verdict::Ask(_) | Verdict::Allow(_)) => {
                self.decide_judged(judged, &call.name, prepared, id, turn)
            }
        };
        decided.map(|decision| (decision, state))
    }

    /// Steps 1 to 6 over one prepared call: the credential deny, the standing
    /// rules and fast paths, and the session grants — everything before the
    /// reviewer (`docs/permissions.md`, "The order a call is judged in").
    /// The serial path, the batch's phase A and grant re-classification
    /// share it. Each caller holds its own permission guard across the call,
    /// so the timing spans the same work as before.
    fn judge_call(&self, tool: &str, effects: &Effects) -> Verdict {
        // The rules are read once for every call, including ones that later
        // fast-path, so a revoked rule applies to the next call judged
        // (`docs/tui.md`, "/rules"). `judge` runs the credential deny before
        // it looks at them, so an unreadable rules file never stops that
        // deny.
        let rules = self.rules.read();
        super::permission::judge(
            tool,
            effects,
            &rules,
            &self.grants,
            &self.known_hosts,
            &self.workspace,
            self.changes.added(),
            &self.prompt.home,
            &self.credentials,
            &self.credential_files,
        )
    }

    /// Applies one call's judged non-`Review` verdict: the Deny, Ask and
    /// Allow arms of the permission order. The serial path, the batch's
    /// phase A and grant re-classification share it: `decided` writes to
    /// the phase's sink — the phase-A line buffer, or the log at
    /// write-back. The caller matches `Review` out first; it never
    /// arrives here.
    fn decide_judged(
        &mut self,
        verdict: Verdict,
        tool: &str,
        prepared: Prepared,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<Decided, Error> {
        match verdict {
            Verdict::Deny { by, reason, why } => {
                let text = format!("{why} It did not run.");
                self.decided(
                    id,
                    turn,
                    resolved(None, Decision::Deny, by, Some(why), None),
                )?;
                Ok(Err(denied(reason, text)))
            }
            Verdict::Ask(rule) => match self.ask(
                id,
                turn,
                tool,
                &prepared.effects,
                AskStep::StandingAsk {
                    standing_rule: rule,
                },
            )? {
                Asked::Allow => Ok(Ok(prepared)),
                Asked::Deny(completed) | Asked::Gone(completed) => Ok(Err(completed)),
                Asked::Cancelled => Ok(Err(self.cancelled_before_ran())),
                // The idle delay passed. Nothing more is written; `run_calls`
                // sees `idle_left` and the turn unwinds.
                Asked::Idle => Ok(Err(self.cancelled_before_ran())),
                // The close already denied the request by `cancel`: the call
                // completes as one no person can answer (`docs/events.md`,
                // `permission_resolved`).
                Asked::Closed => Ok(Err(no_person())),
            },
            Verdict::Allow(decided) => {
                if let Some(by) = decided {
                    self.decided(id, turn, resolved(None, Decision::Allow, by, None, None))?;
                }
                Ok(Ok(prepared))
            }
            Verdict::Review => {
                unreachable!("the permission order's reviewer step never reaches the judged arms")
            }
        }
    }

    /// Today's decide loop, one call at a time: a single undecided call —
    /// or a reply under a budget, or with no reviewer — keeps this path
    /// bit for bit. `None` unwinds on an idle deadline, with no completion
    /// written.
    fn decide_serial(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested, Option<Decided>)>,
        turn: &TurnId,
        mut carried: Option<(ActionId, Vec<contract::hook::StateLine>)>,
    ) -> Result<Option<DecidedBatch>, Error> {
        let mut decided = Vec::with_capacity(calls.len());
        for (id, call, already) in calls {
            // Deciding stops at the first cancel: a call approved but not
            // yet started, and every call not yet decided, completes
            // `cancelled` with no `tool_call_started` and no permission
            // lines. A decision line already written stays.
            if self.turn_cancelled() {
                decided.push((id, call.name, Err(self.cancelled_before_ran()), Vec::new()));
                continue;
            }
            let (decision, state) = match already {
                Some(decision) => {
                    let state = if carried.as_ref().is_some_and(|(action, _)| *action == id) {
                        carried.take().map(|(_, state)| state).unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    (decision, state)
                }
                None => self.decide(&call, &id, turn)?,
            };
            // An idle deadline ended the approval. Drop the decision: no
            // completion is written, including for calls already decided.
            if self.idle_left {
                return Ok(None);
            }
            if self.turn_cancelled() {
                decided.push((id, call.name, Err(self.cancelled_before_ran()), Vec::new()));
                continue;
            }
            decided.push((id, call.name, decision, state));
        }
        Ok(Some(decided))
    }

    /// Decides one batch's calls when today's serial path does not apply:
    /// more than one call still to judge, with no budget to spend and a
    /// reviewer set up. Phase A classifies every call in order, phase B
    /// sends every reviewed call's first stage at once, and phase C writes
    /// everything back in request order. `None` unwinds on an idle
    /// deadline, as the serial path does.
    fn decide_concurrent(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested, Option<Decided>)>,
        turn: &TurnId,
        carried: Option<(ActionId, Vec<contract::hook::StateLine>)>,
        endpoint: ReviewEndpoint,
    ) -> Result<Option<DecidedBatch>, Error> {
        // Phase A: classify every call in order — hooks and `judge` —
        // buffering every pending decision. Non-`Review` verdicts are
        // classified, not decided: their `permission_resolved` lines wait
        // for phase-C write-back. Hook notices, hook state handling and
        // the standing-ask round-trip stay immediate inline exactly as
        // today.
        self.decision_buffer = Some(Vec::new());
        let mut carried = carried;
        let pending = self.classify_batch(calls, turn, &mut carried);
        self.decision_buffer = None;
        let Some(mut pending) = pending? else {
            // An idle deadline ended an approval: phase A already flushed
            // what it buffered, in order. No completion is written.
            return Ok(None);
        };
        // Phase B: send every reviewed call's first stage at once.
        // Requests are prebuilt in request order over the fixed `reviewed`
        // vec, so their bytes match the serial path's; only the provider
        // round trips overlap.
        let prompt = sections();
        let worker = Stage1Worker {
            provider: Arc::clone(&endpoint.provider),
            observe: self.reviewer_observe.clone(),
            cancel: Arc::clone(&self.cancel),
        };
        let mut sent = self.reviewer_sent;
        let mut requests = Vec::new();
        let mut metas: Vec<(ReviewerUse, usize)> = Vec::new();
        for entry in pending.iter() {
            if !matches!(entry.outcome, PendingOutcome::NeedsReview) {
                continue;
            }
            let Some(prepared) = entry.prepared.as_ref() else {
                unreachable!("phase A prepares every call it binds for review");
            };
            let under = UnderReview {
                call: &entry.call,
                id: &entry.id,
                turn,
                prepared,
            };
            let built =
                self.build_stage1(&under, &endpoint, &prompt.shared, &prompt.first, None, sent);
            sent = Some(built.cut);
            requests.push(built.request);
            metas.push((built.reviewer_use, built.cut));
        }
        let replies = fanout_stage1(&worker, requests);
        let mut made = metas
            .into_iter()
            .zip(replies)
            .map(|((reviewer_use, cut), reply)| StagedStage1 {
                reply,
                resent: None,
                second: None,
                reviewer_use,
                cut,
            });
        let mut staged: Vec<Option<StagedStage1>> = Vec::with_capacity(pending.len());
        for entry in pending.iter() {
            if !matches!(entry.outcome, PendingOutcome::NeedsReview) {
                staged.push(None);
                continue;
            }
            staged.push(made.next());
        }
        // Phase C1: pre-send every second stage the first stages asked
        // for, in request order. Phase C2 writes everything back.
        self.prefetch_seconds(&pending, &mut staged, &endpoint, &prompt, turn)?;
        // Phase C2: write everything back in request order.
        self.write_back(&mut pending, staged, &endpoint, turn)
    }

    /// Phase A of a concurrent batch: classifies every call in order and
    /// buffers every pending decision. `None` unwinds on an idle deadline:
    /// earlier calls' buffered lines flush in order, and no completion is
    /// written.
    fn classify_batch(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested, Option<Decided>)>,
        turn: &TurnId,
        carried: &mut Option<(ActionId, Vec<contract::hook::StateLine>)>,
    ) -> Result<Option<Vec<PendingCall>>, Error> {
        let mut pending = Vec::with_capacity(calls.len());
        for (id, call, already) in calls {
            // Deciding stops at the first cancel: a call approved but not
            // yet started, and every call not yet decided, completes
            // `cancelled` with no `tool_call_started` and no permission
            // lines. A decision line already written stays.
            if self.turn_cancelled() {
                pending.push(PendingCall {
                    id,
                    tool: call.name.clone(),
                    call,
                    outcome: PendingOutcome::Cancelled,
                    verdict: None,
                    lines: Vec::new(),
                    prepared: None,
                    state: Vec::new(),
                });
                continue;
            }
            let mut entry = match already {
                Some(decision) => {
                    let state = if carried.as_ref().is_some_and(|(action, _)| *action == id) {
                        carried.take().map(|(_, state)| state).unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    PendingCall {
                        id,
                        tool: call.name.clone(),
                        call,
                        outcome: PendingOutcome::Decided(decision),
                        verdict: None,
                        lines: Vec::new(),
                        prepared: None,
                        state,
                    }
                }
                None => self.classify_one(&call, &id, turn)?,
            };
            // An idle deadline ended the approval. Flush what phase A
            // buffered so far, in order, and unwind with no completion
            // written. Later calls were never decided, so their lines
            // stay out.
            if self.idle_left {
                pending.push(entry);
                for entry in &mut pending {
                    for line in entry.lines.drain(..) {
                        self.append(&Event::PermissionResolved(line), turn, Some(&entry.id))?;
                    }
                }
                return Ok(None);
            }
            // A cancel that landed while this call classified discards its
            // decision; lines already buffered stay and flush in order.
            if self.turn_cancelled() {
                entry.outcome = PendingOutcome::Cancelled;
                entry.state = Vec::new();
            }
            pending.push(entry);
        }
        Ok(Some(pending))
    }

    /// Classifies one undecided call for the batch's phase A: hooks and
    /// `judge`, exactly as the serial path judges them. Non-`Review`
    /// verdicts are classified, not decided: their lines buffer for
    /// write-back. Returns the call's entry with its drained lines.
    fn classify_one(
        &mut self,
        call: &ToolCallRequested,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<PendingCall, Error> {
        let (prepared, state) = self.prepared(call, Some(turn))?;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failed) => {
                return Ok(PendingCall {
                    id: id.clone(),
                    tool: call.name.clone(),
                    call: call.clone(),
                    outcome: PendingOutcome::Decided(Err(failed)),
                    verdict: None,
                    lines: Vec::new(),
                    prepared: None,
                    state,
                });
            }
        };
        // The permission order starts once the call names a registered
        // tool and fits its schema: the guard spans the judge and a
        // person's wait, and closes at the decision, before any run.
        // `call.name` is the session map's key: `checked` above refused
        // anything else, so no lookup or allocation here.
        let _permission = self
            .diag
            .timed_tool(contract::diag::Op::ToolPermission, &call.name);
        let verdict = self.judge_call(&call.name, &prepared.effects);
        match verdict {
            // Step 7 is the reviewer: the call is judged by a separate
            // model (`docs/permissions.md`, "The reviewer").
            Verdict::Review => Ok(PendingCall {
                id: id.clone(),
                tool: call.name.clone(),
                call: call.clone(),
                outcome: PendingOutcome::NeedsReview,
                verdict: Some(Verdict::Review),
                lines: Vec::new(),
                prepared: Some(prepared),
                state,
            }),
            key @ (Verdict::Deny { .. } | Verdict::Ask(_) | Verdict::Allow(_)) => {
                let stored = prepared.clone();
                let decision = self.decide_judged(key.clone(), &call.name, prepared, id, turn)?;
                // Classification is serial per call, so everything buffered
                // is this call's.
                let lines = match self.decision_buffer.as_mut() {
                    Some(buffer) => std::mem::take(buffer),
                    None => {
                        unreachable!("phase A buffers every decision it classifies")
                    }
                };
                Ok(PendingCall {
                    id: id.clone(),
                    tool: call.name.clone(),
                    call: call.clone(),
                    outcome: PendingOutcome::Decided(decision),
                    verdict: Some(key),
                    lines,
                    prepared: Some(stored),
                    state,
                })
            }
        }
    }

    /// Phase C1 of a concurrent batch: pre-sends every second stage the
    /// fanned-out first stages asked for, in request order, before any
    /// call applies. Requests build from the fixed `reviewed` vec exactly
    /// as the serial path builds them, so the bytes match; nothing
    /// records here. A grant installed while an earlier call applies
    /// moots an already sent reply instead of preventing its send.
    fn prefetch_seconds(
        &mut self,
        pending: &[PendingCall],
        staged: &mut [Option<StagedStage1>],
        endpoint: &ReviewEndpoint,
        prompt: &Sections,
        turn: &TurnId,
    ) -> Result<(), Error> {
        for (entry, slot) in pending.iter().zip(staged.iter_mut()) {
            if !matches!(entry.outcome, PendingOutcome::NeedsReview) {
                continue;
            }
            let Some(prepared) = entry.prepared.as_ref() else {
                unreachable!("phase A prepares every call it binds for review");
            };
            let under = UnderReview {
                call: &entry.call,
                id: &entry.id,
                turn,
                prepared,
            };
            let Some(staged) = slot.as_mut() else {
                unreachable!("phase B fans out every call bound for review");
            };
            // Resolve the first stage, re-asking once when unreadable,
            // exactly as the serial path's first ask does.
            let check = match &staged.reply {
                Err(_) => false,
                Ok(first) => match read_first(&first.text()) {
                    Ok(First::Allow) => false,
                    Ok(First::Check) => true,
                    Err(unread) => {
                        let note = format!(
                            "Your reply could not be read: {unread}. Reply in the form the instructions give."
                        );
                        let resent =
                            self.resend_stage1(&under, endpoint, prompt, &note, staged.cut);
                        let needs = matches!(&resent, Ok(second)
                            if matches!(read_first(&second.text()), Ok(First::Check)));
                        staged.resent = Some(resent);
                        needs
                    }
                },
            };
            if !check {
                continue;
            }
            // The first stage asked for more: pre-send the second stage,
            // re-asking once when unreadable.
            let item = under_item(
                prepared,
                &entry.call.name,
                &self.workspace_label,
                self.changes.added(),
            );
            let reply = self.send_stage2(endpoint, prompt, &item, staged.cut, None);
            let second = match reply {
                reply @ Err(_) => StagedSecond {
                    reply,
                    resent: None,
                },
                Ok(first) => match read_second(&first.text()) {
                    Ok(_) => StagedSecond {
                        reply: Ok(first),
                        resent: None,
                    },
                    Err(unread) => {
                        let note = format!(
                            "Your reply could not be read: {unread}. Reply in the form the instructions give."
                        );
                        let resent =
                            self.send_stage2(endpoint, prompt, &item, staged.cut, Some(&note));
                        StagedSecond {
                            reply: Ok(first),
                            resent: Some(resent),
                        }
                    }
                },
            };
            staged.second = Some(second);
        }
        Ok(())
    }

    /// Phase C of a concurrent batch: writes everything back in request
    /// order — buffered non-review lines, then per call its stage-1 and
    /// pre-sent stage-2 `usage_recorded` lines and `permission_resolved`.
    /// Escalations are answered here in order, and a grant or rule
    /// installed by an answer re-classifies the remaining calls under
    /// fresh rules. The shared cursor commits past the whole batch once,
    /// after write-back. `None` unwinds on an idle deadline, as the
    /// serial path does.
    fn write_back(
        &mut self,
        pending: &mut [PendingCall],
        staged: Vec<Option<StagedStage1>>,
        endpoint: &ReviewEndpoint,
        turn: &TurnId,
    ) -> Result<Option<DecidedBatch>, Error> {
        let mut decided = Vec::with_capacity(pending.len());
        let mut staged = staged;
        let mut committed = self.reviewer_sent;
        let mut index = 0;
        while index < pending.len() {
            // A cancel that landed while an earlier call applied ends this
            // call before it starts: no `tool_call_started`, completed
            // `cancelled`. A decision line already buffered stays and
            // flushes in order.
            if self.turn_cancelled() {
                let Some(entry) = pending.get_mut(index) else {
                    unreachable!("write-back visits each classified call once, in order");
                };
                for line in entry.lines.drain(..) {
                    self.append(&Event::PermissionResolved(line), turn, Some(&entry.id))?;
                }
                decided.push((
                    entry.id.clone(),
                    entry.tool.clone(),
                    Err(self.cancelled_before_ran()),
                    Vec::new(),
                ));
                index += 1;
                continue;
            }
            let Some(entry) = pending.get_mut(index) else {
                unreachable!("write-back visits each classified call once, in order");
            };
            let staged = staged.get_mut(index).and_then(Option::take);
            let (decision, cut) = self.apply_entry(entry, staged, endpoint, turn)?;
            if let Some(cut) = cut {
                committed = Some(cut);
            }
            let Some(entry) = pending.get_mut(index) else {
                unreachable!("write-back visits each classified call once, in order");
            };
            let state = std::mem::take(&mut entry.state);
            decided.push((entry.id.clone(), entry.tool.clone(), decision, state));
            // An idle deadline ended an approval. No completion is
            // written, including for calls already decided — but a
            // decision line already written stays.
            if self.idle_left {
                if committed != self.reviewer_sent {
                    self.reviewer_sent = committed;
                }
                return Ok(None);
            }
            self.rejudge_remaining(not_yet_applied(&mut *pending, index))?;
            index += 1;
        }
        // One commit past the whole batch, after write-back.
        if committed != self.reviewer_sent {
            self.reviewer_sent = committed;
        }
        Ok(Some(decided))
    }

    /// Applies one classified call at write-back and returns its decision
    /// with the staged cut when a fanned-out stage-1 went out for it.
    /// The entry's outcome is spent: later re-classification only visits
    /// calls this loop has not applied yet.
    fn apply_entry(
        &mut self,
        entry: &mut PendingCall,
        staged: Option<StagedStage1>,
        endpoint: &ReviewEndpoint,
        turn: &TurnId,
    ) -> Result<(Decided, Option<usize>), Error> {
        match std::mem::replace(&mut entry.outcome, PendingOutcome::Cancelled) {
            PendingOutcome::Decided(decision) => {
                for line in entry.lines.drain(..) {
                    self.append(&Event::PermissionResolved(line), turn, Some(&entry.id))?;
                }
                Ok((decision, None))
            }
            PendingOutcome::Cancelled => {
                for line in entry.lines.drain(..) {
                    self.append(&Event::PermissionResolved(line), turn, Some(&entry.id))?;
                }
                Ok((Err(self.cancelled_before_ran()), None))
            }
            PendingOutcome::NeedsReview => {
                let Some(prepared) = entry.prepared.as_ref() else {
                    unreachable!("phase A prepares every call it binds for review");
                };
                let under = UnderReview {
                    call: &entry.call,
                    id: &entry.id,
                    turn,
                    prepared,
                };
                let Some(staged) = staged else {
                    unreachable!("phase B fans out every call bound for review");
                };
                let cut = staged.cut;
                let decision = self.apply_stage1(&under, endpoint, staged, turn)?;
                Ok((decision, Some(cut)))
            }
            PendingOutcome::Rejudged(key) => {
                let cut = staged.as_ref().map(|staged| staged.cut);
                if let Some(staged) = staged {
                    // The stage-1 request already went out in phase B: its
                    // usage still records at write-back, before the fresh
                    // path judges.
                    let reviewer = ReviewerUse::Stage1 {
                        action_id: entry.id.clone(),
                    };
                    self.record_review_usage(&staged.reply, endpoint, reviewer, turn)?;
                }
                let Some(prepared) = entry.prepared.take() else {
                    unreachable!("phase A prepares every call grant re-classification judges");
                };
                match key {
                    // Newly bound for review: judge it serially now — its
                    // stage-1 never fanned out.
                    Verdict::Review => {
                        Ok((self.review(&entry.call, &entry.id, turn, prepared)?, cut))
                    }
                    key @ (Verdict::Deny { .. } | Verdict::Ask(_) | Verdict::Allow(_)) => Ok((
                        self.decide_judged(key, &entry.tool, prepared, &entry.id, turn)?,
                        cut,
                    )),
                }
            }
        }
    }

    /// Re-classifies every not-yet-applied call under fresh rules: a grant
    /// or rule installed by an answer only reaches later calls when they
    /// are judged again. A call still bound for review keeps its buffered
    /// first-stage outcome; any other moved judgment re-takes the serial
    /// path at write-back.
    fn rejudge_remaining(&mut self, remaining: &mut [PendingCall]) -> Result<(), Error> {
        for entry in remaining.iter_mut() {
            // Already decided or cancelled: the judgment is final.
            let Some(stored) = entry.verdict.as_ref() else {
                continue;
            };
            let Some(prepared) = entry.prepared.as_ref() else {
                continue;
            };
            let fresh = self.judge_call(&entry.tool, &prepared.effects);
            if fresh == *stored {
                continue;
            }
            // The judgment moved: drop the buffered lines and take the
            // fresh path at write-back. A call still bound for review
            // keeps its buffered first-stage outcome.
            entry.lines.clear();
            entry.verdict = Some(fresh.clone());
            entry.outcome = PendingOutcome::Rejudged(fresh);
        }
        Ok(())
    }

    /// Writes a `permission_resolved` line for `id`: appended, or buffered
    /// while a concurrent batch classifies, for phase-C write-back in
    /// request order.
    pub(crate) fn decided(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        resolved: PermissionResolved,
    ) -> Result<(), Error> {
        // One-shot test fault, outside every commit: the error test proves
        // a failed decision write ends the turn with the original error.
        #[cfg(test)]
        if crate::tool_timing_tests::take_fail_decision_write() {
            return Err(crate::tool_timing_tests::log_fault());
        }
        if let Some(buffer) = self.decision_buffer.as_mut() {
            buffer.push(resolved);
            return Ok(());
        }
        self.append(&Event::PermissionResolved(resolved), turn, Some(id))
    }

    /// Asks a person about a call, then waits for their reply as
    /// [`Loop::await_answer`] does (`docs/permissions.md`, "What the log
    /// records").
    pub(crate) fn ask(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        tool: &str,
        effects: &Effects,
        step: AskStep,
    ) -> Result<Asked, Error> {
        if !self.answerable {
            // Step 7 checks `answerable` before asking; only a standing
            // ask reaches this denial.
            return Ok(Asked::Gone(self.unanswerable(id, turn)?));
        }
        let request = PermissionRequested {
            always_reviewed: effects.always_reviewed,
            request_id: RequestId(super::mint("r_")),
            declared: effects.declared.clone(),
            step,
        };
        self.append(&Event::PermissionRequested(request.clone()), turn, Some(id))?;
        self.await_answer(id, turn, tool, &request, effects.hosts.clone())
    }

    /// Denies a call no person can answer (`docs/permissions.md`,
    /// "Headless"): a session started by `fiber ask`, or one `close` has
    /// been taken.
    pub(crate) fn unanswerable(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<Box<ToolCallCompleted>, Error> {
        let reason = "No person can answer an approval in this session.";
        self.decided(
            id,
            turn,
            resolved(
                None,
                Decision::Deny,
                DecidedBy::StandingRule,
                Some(reason.to_owned()),
                None,
            ),
        )?;
        Ok(no_person())
    }

    /// Checks `reply` against `offer`, the request's rule offer (`None` on a
    /// standing ask, which offers nothing to remember), and applies what an
    /// allow remembers (`docs/permissions.md`, "Remembering a decision"): a
    /// session grant goes onto `grants` before the next call is judged, and a
    /// project rule is appended to the project's rules file. A project rule
    /// that cannot be saved still allows the call, with a reason saying so.
    /// `None`: the reply does not fit, so the caller rejects it and the loop
    /// keeps waiting.
    pub(crate) fn answered(
        &mut self,
        tool: &str,
        offer: Option<&RuleOffer>,
        reply: &ReplyAnswer,
    ) -> Option<Answered> {
        let super::permission::Answer {
            decision,
            feedback,
            remember,
        } = super::permission::answer(offer, reply)?;
        let mut answered = Answered {
            decision,
            feedback,
            grant: None,
            rule: None,
            reason: None,
        };
        let Some(remembered) = remember else {
            return Some(answered);
        };
        match remembered.scope {
            RememberScope::Session => {
                let grant = remembered.grant(tool);
                self.grants.push(grant.clone());
                answered.grant = Some(grant);
            }
            RememberScope::Project => {
                let session = SessionId(self.cache_key.clone());
                match self.rules.remember(tool, &remembered.prefix, &session) {
                    Ok(()) => {
                        answered.rule = Some(remembered.grant(tool));
                    }
                    Err(error) => {
                        answered.reason = Some(format!("The rule could not be saved: {error}."));
                    }
                }
            }
        }
        Some(answered)
    }

    /// The completion of a call of `tool` with `arguments` that ran and
    /// returned `output`, as the hooks left it, its text cut to `bound`
    /// (`docs/tools.md`, "Bounded results"). `rewritten` names the
    /// extensions that rewrote the arguments, heading the text with the
    /// result line. Writes the call's job lines and
    /// the hooks' notices under `id` before the caller writes
    /// `tool_call_completed`.
    #[allow(
        clippy::too_many_arguments,
        reason = "each input shapes this call's completion; cancellation belongs to the call"
    )]
    pub(crate) fn finish(
        &mut self,
        output: Output,
        (tool, arguments): (&str, &Map<String, Value>),
        rewritten: Option<&[String]>,
        bound: Bound,
        cancelled: bool,
        id: &ActionId,
        turn: Option<&TurnId>,
    ) -> Result<(ToolCallCompleted, Vec<contract::hook::StateLine>), Error> {
        let Output {
            content,
            error,
            process,
            details,
            changes,
            control,
            jobs,
            settle,
            servers,
            spilled,
            hosts_surfaced,
            loaded,
        } = output;
        // The call's output reached `finish`: its turn entry leaves the
        // map, so the map holds only calls whose output has not.
        self.call_turns.remove(id);
        // What the call saw of its server's deaths and restarts, written
        // under the call's action, in order, before its job lines and
        // `tool_call_completed`.
        for record in servers {
            let event = match record {
                contract::tool::ServerRecord::Failed(failed) => Event::McpServerFailed(failed),
                contract::tool::ServerRecord::Ready(ready) => Event::McpServerReady(ready),
            };
            self.write(&event, turn, Some(id))?;
        }
        // The call's prefix of its jobs' queues, written under the
        // queuing call's action and turn, before its own job lines.
        if let Some(settle) = settle {
            for taken in (settle.0)() {
                self.write_taken(taken, turn, Some(id))?;
            }
        }
        for record in jobs {
            let event = job_event(&self.log, record);
            self.write(&event, turn, Some(id))?;
        }
        let (mut text, mut images): (Vec<ContentPart>, Vec<ContentPart>) = content
            .into_iter()
            .partition(|part| matches!(part, ContentPart::Text { .. }));
        // A running call keeps running until its tool returns, then
        // completes with what the tool gave, `cancelled` instead of
        // `completed`. A cancelled search that loaded tools completes
        // with the `search-cancelled` line instead (`docs/tools.md`,
        // "Tool search"). A tool that returned an error keeps `failed` with
        // that error: those outcomes are never `cancelled`. The hooks see
        // the status the completion carries.
        let mut status = if error.is_some() {
            CallStatus::Failed
        } else if cancelled {
            CallStatus::Cancelled
        } else {
            CallStatus::Completed
        };
        let mut error = error;
        // A completed call carrying `control.name` names the session, or
        // fails `name_pinned` while the person's name pins it
        // (`docs/tools.md`, "What a result carries"). The pin is judged
        // before the hooks run, so they see the failed call; the name is
        // written after, so it follows their notices.
        let write = match self.naming.judge(status, control.as_ref()) {
            crate::naming::Named::Nothing => None,
            crate::naming::Named::Pinned(failure) => {
                status = CallStatus::Failed;
                error = Some(failure);
                text = vec![ContentPart::Text {
                    text: crate::naming::PINNED.to_owned(),
                }];
                images = Vec::new();
                None
            }
            crate::naming::Named::Write(named) => Some(named),
        };
        // A cancelled search loads nothing: its text is the one
        // `search-cancelled` line and no image survives, so the hooks and
        // the cap see the line the model would be sent. A `before_tool`
        // rewrite line would claim the call ran with its arguments, so it
        // is dropped: the completion is exactly the line. With `loaded`
        // `None` the content is as the tool gave it.
        let mut rewritten = rewritten;
        if status == CallStatus::Cancelled && loaded.is_some() {
            text = vec![ContentPart::Text {
                text: crate::prompt::message("search-cancelled").to_owned(),
            }];
            images = Vec::new();
            rewritten = None;
        }
        let ran = super::hooks::Ran {
            tool,
            arguments,
            status,
            text,
            images,
            details,
            process: process.as_ref(),
            rewritten,
            spilled,
        };
        let mut shaped = self.shape(ran, bound, id, turn)?;
        if let Some(failure) = shaped.error.take() {
            status = CallStatus::Failed;
            error = Some(failure);
        }
        if let Some(named) = write
            && status == CallStatus::Completed
        {
            self.write(&Event::SessionNamed(named), turn, Some(id))?;
        }
        // `loaded` travels only when the call completed and the shaping
        // kept the tool's output: any other status or a withheld result
        // carries none.
        let loaded = if status == CallStatus::Completed && !shaped.withheld {
            loaded
        } else {
            None
        };
        Ok((
            ToolCallCompleted {
                status,
                error,
                process,
                details: shaped.details,
                changes,
                control,
                artifact: shaped.artifact,
                kept: shaped.kept,
                content: shaped.content,
                changed_by: shaped.changed_by,
                hosts_surfaced,
                loaded,
                ..completed(String::new(), None)
            },
            shaped.state,
        ))
    }
}

impl Loop {
    /// Writes one due or flushed delta under the call's action, and returns
    /// its encoded bytes for pacing. Errors are ignored: the line is
    /// ephemeral (`docs/architecture.md`, "Streaming").
    pub(crate) fn write_delta(
        &mut self,
        delta: &Progress,
        turn: Option<&TurnId>,
        id: &ActionId,
    ) -> u64 {
        let event = Event::ToolCallDelta(delta.clone());
        match self.write(&event, turn, Some(id)) {
            Ok(()) | Err(_) => {}
        }
        encoded(delta)
    }

    /// Writes the next completion in request order, once every earlier call
    /// is done: true after writing one. A later call's completion waits for
    /// its prefix, while its final flush does not (written above).
    fn complete_next(
        &mut self,
        running: &mut [Running],
        inner: &[InnerRunning],
        turn: &TurnId,
    ) -> Result<bool, Error> {
        // The first call whose completion is not written: everything
        // before it is done, so writing this one keeps request order.
        let Some(call) = running.iter_mut().find(|call| !is_done(call)) else {
            return Ok(false);
        };
        if Self::inner_anchored(inner, &call.id) {
            return Ok(false);
        }
        let (mut completed, state) = match &call.state {
            State::Done => return Ok(false),
            // A decided call the cancel reached before its completion was
            // written completes `cancelled`: a denial or failure decided
            // but not yet written, or a call approved but never started. A
            // `permission_resolved` line already written stays.
            State::Ready(_) if self.turn_cancelled() => (self.cancelled_before_ran(), Vec::new()),
            State::Ready(completed) => (completed.clone(), Vec::new()),
            State::Running {
                stream,
                bound,
                tool,
                arguments,
                rewritten,
                ..
            } => match stream.take_finished() {
                Some((output, flushed)) => {
                    if let Some(delta) = flushed {
                        self.write_delta(&delta, Some(turn), &call.id);
                    }
                    let call_of = (tool.as_str(), arguments.as_ref());
                    let rewritten = rewritten.as_deref();
                    if let Some(slot) = stream.inner_slot() {
                        slot.close();
                    }
                    let (completed, state) = self.finish(
                        output,
                        call_of,
                        rewritten,
                        *bound,
                        self.turn_cancelled(),
                        &call.id,
                        Some(turn),
                    )?;
                    (Box::new(completed), state)
                }
                None => return Ok(false),
            },
        };
        let declared = match &call.state {
            State::Running { declared, .. } => Some(declared.clone()),
            State::Ready(_) | State::Done => None,
        };
        call.state = State::Done;
        self.after_completed(
            declared.as_ref(),
            &mut completed,
            &call.id,
            Some(turn),
            state,
        )?;
        Ok(true)
    }

    /// Writes the completion between its prune line and instruction tracking,
    /// shared by model and inner calls.
    pub(crate) fn after_completed(
        &mut self,
        declared: Option<&DeclaredEffects>,
        completed: &mut ToolCallCompleted,
        id: &ActionId,
        turn: Option<&TurnId>,
        state: Vec<contract::hook::StateLine>,
    ) -> Result<(), Error> {
        // An over-budget section a completed call that declared a write
        // touched ends its result with the prune line, before the line is
        // written; the session's own edits below come after it.
        if let Some(declared) = declared
            && completed.status == CallStatus::Completed
        {
            for line in self.changes.prune_lines(&self.workspace, declared) {
                completed.content.push(ContentPart::Text { text: line });
            }
        }
        self.write_carried(state, turn)?;
        self.write(&Event::ToolCallCompleted(completed.clone()), turn, Some(id))?;
        self.publish_completion_rows(completed);
        // The session's own edits and subdirectory files: a call that ran
        // re-reads each declared path that is a tracked instruction file,
        // and queues each new subdirectory file for the next step start.
        // A call that never ran touches nothing, so it checks nothing.
        if let Some(declared) = declared {
            for file in self.changes.call_completed(&self.workspace, declared) {
                self.write(&Event::InstructionFile(file), turn, Some(id))?;
            }
        }
        Ok(())
    }
}

/// The calls write-back has not applied yet: everything after `index`.
/// A mutant of `+` to `*` re-includes the call just applied, whose buffered
/// lines already flushed and whose decision already pushed, so re-judging it
/// changes nothing a consumer sees; the remaining calls' re-judgment is
/// tested through the grant re-classification tests in
/// `crates/loop/tests/permissions.rs`.
#[cfg_attr(false, mutants::skip)]
fn not_yet_applied(pending: &mut [PendingCall], index: usize) -> &mut [PendingCall] {
    let Some(tail) = pending.get_mut(index + 1..) else {
        unreachable!("write-back visits each classified call once, in order");
    };
    tail
}

/// The completion of a call whose request the close denied by `cancel`:
/// no person can answer it (`docs/permissions.md`, "Headless"). What
/// [`Loop::unanswerable`] completes a headless call with, without its
/// decision line, which the close already wrote.
pub(crate) fn no_person() -> Box<ToolCallCompleted> {
    denied(
        "no_person",
        "No person can answer an approval in this session. It did not run.".to_owned(),
    )
}

/// What deciding hands the run phase: each call's action, tool name,
/// decision and hook state, in request order. `None` unwinds on an idle
/// deadline, with no completion written.
type DecidedBatch = Vec<(ActionId, String, Decided, Vec<contract::hook::StateLine>)>;

/// Phase A's outcome for one call of a concurrent batch.
enum PendingOutcome {
    /// Decided without the reviewer, or already decided: the completion.
    Decided(Decided),
    /// Bound for the reviewer: everything phase B builds from.
    NeedsReview,
    /// Re-classified after a grant or rule install moved the judgment:
    /// take the fresh path at write-back.
    Rejudged(Verdict),
    /// A cancel ended the approval before this call was decided.
    Cancelled,
}

/// One call classified by the batch's phase A: its outcome, and everything
/// write-back needs in request order.
struct PendingCall {
    /// The call's action.
    id: ActionId,
    /// The tool's registered name, for asks and judgments.
    tool: String,
    /// The model's call.
    call: ToolCallRequested,
    /// What phase A found: decided, bound for review, or cancelled.
    outcome: PendingOutcome,
    /// Phase A's judgment, when the call was judged: re-classification
    /// compares the fresh judgment against it.
    verdict: Option<Verdict>,
    /// The `permission_resolved` lines phase A buffered for this call.
    lines: Vec<PermissionResolved>,
    /// The prepared call, kept for review application and for a
    /// fresh-`Review` re-classification.
    prepared: Option<Prepared>,
    /// Hook state the run carries.
    state: Vec<contract::hook::StateLine>,
}

/// What waiting for a person's answer came back with. The decision line is
/// written on every variant but `Idle`.
pub(crate) enum Asked {
    /// The person allowed the call.
    Allow,
    /// The person refused the call.
    Deny(Box<ToolCallCompleted>),
    /// The inbox closed with no answer.
    Gone(Box<ToolCallCompleted>),
    /// `close` was taken while waiting; the deny-by-cancel line is written.
    Closed,
    /// A cancel woke the wait: the caller completes the call `cancelled`.
    /// The deny-by-cancel line is written.
    Cancelled,
    /// The idle delay passed. No decision line is written. The turn unwinds
    /// without `tool_call_completed` or `turn_completed`.
    Idle,
}

/// `path` with its longest existing ancestor canonicalised. `None` when the
/// part that does not exist yet holds `.` or `..`, which only the file
/// system can resolve.
pub(crate) fn resolve(path: &Path) -> Option<PathBuf> {
    let parts: Vec<Component<'_>> = path.components().collect();
    (0..=parts.len()).rev().find_map(|split| {
        let (existing, rest) = parts.split_at(split);
        let real = existing.iter().collect::<PathBuf>().canonicalize().ok()?;
        Some(rest.iter().try_fold(real, |p, c| match c {
            Component::Normal(name) => Some(p.join(name)),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        }))
    })?
}

#[cfg(test)]
#[path = "calls_tests.rs"]
mod tests;
