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
    PermissionResolved, Progress, RuleOffer, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
    ToolReplaced,
};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects};
use contract::tool::{Bound, Cancel, Effects, Output, Tool};
use contract::{ActionId, ErrorCode, RequestId, SessionId, TurnId};
use serde_json::{Map, Value};

use super::completion::{completed, denied, failed, resolved};
use crate::asking::Release;
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
    /// and `arguments` for the hooks.
    Running {
        bound: Bound,
        stream: Arc<Stream>,
        declared: DeclaredEffects,
        tool: String,
        arguments: Arc<Map<String, Value>>,
    },
    /// Its completion is written.
    Done,
}

/// Whether a call's completion is written.
fn is_done(call: &Running) -> bool {
    matches!(call.state, State::Done)
}

/// Each running call with its stream.
fn asking(running: &[Running]) -> Vec<(&ActionId, &Stream)> {
    let streams = running.iter().filter_map(|call| match &call.state {
        State::Running { stream, .. } => Some((&call.id, stream.as_ref())),
        State::Ready(_) | State::Done => None,
    });
    streams.collect()
}

/// The written delta's payload serialised as JSON, in bytes
/// (`docs/tools.md`, "Progress"): what the next interval is paced on.
fn encoded(delta: &Progress) -> u64 {
    let bytes = serde_json::to_vec(delta)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// A call that runs: its tool, the arguments it runs with, and the effects
/// it declared.
pub(crate) type Approved = (Arc<dyn Tool>, Map<String, Value>, DeclaredEffects);

/// A call that names a tool and fits its schema: the tool, the arguments it
/// runs with, and the effects it declared.
pub(crate) type Checked = (Arc<dyn Tool>, Map<String, Value>, Effects);

/// A call's decision: it runs, or it completes as given without running.
pub(crate) type Decided = Result<Approved, Box<ToolCallCompleted>>;

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
    pub(crate) fn allow(self, request_id: RequestId) -> PermissionResolved {
        PermissionResolved {
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
/// returned (`docs/architecture.md`, "Tool seam").
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

impl Loop {
    /// `call` with the repair its tool's schema allows, if any
    /// (`docs/tools.md`, "Before a call runs").
    pub(crate) fn repaired(&self, mut call: ToolCallRequested) -> ToolCallRequested {
        if let Some((_, _, definition)) = self.tools.get(&call.name) {
            call.repair = schema::repair(&definition.input_schema, &call.arguments);
        }
        call
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
        self.run_batch(calls.collect(), turn)
    }

    /// [`Loop::run_calls`] over a batch some of whose calls are already
    /// decided: a call carrying its decision is not judged again, and the
    /// rest are judged in order as `run_calls` judges them.
    pub(crate) fn run_batch(
        &mut self,
        calls: Vec<(ActionId, ToolCallRequested, Option<Decided>)>,
        turn: &TurnId,
    ) -> Result<bool, Error> {
        let mut decided = Vec::with_capacity(calls.len());
        for (id, call, already) in calls {
            // Deciding stops at the first cancel: a call approved but not
            // yet started, and every call not yet decided, completes
            // `cancelled` with no `tool_call_started` and no permission
            // lines. A decision line already written stays.
            if self.turn_cancelled() {
                decided.push((id, call.name, Err(self.cancelled_before_ran())));
                continue;
            }
            let decision = match already {
                Some(decision) => decision,
                None => self.decide(&call, &id, turn)?,
            };
            // An idle deadline ended the approval. Drop the decision: no
            // completion is written, including for calls already decided.
            if self.idle_left {
                return Ok(false);
            }
            if self.turn_cancelled() {
                decided.push((id, call.name, Err(self.cancelled_before_ran())));
                continue;
            }
            decided.push((id, call.name, decision));
        }
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
            // Closes every call's ask on any return, so no worker the scope
            // joins is left blocked in one.
            let mut release = Release::default();
            for (id, name, decision) in decided {
                let state = match decision {
                    Err(completed) => State::Ready(completed),
                    // A cancel that landed while a later call was decided
                    // ends an approved call before it starts: no
                    // `tool_call_started`, completed `cancelled`. A decision
                    // line already written stays.
                    Ok(_) if self.turn_cancelled() => State::Ready(self.cancelled_before_ran()),
                    Ok((tool, arguments, declared)) => {
                        self.append(
                            &Event::ToolCallStarted(ToolCallStarted {
                                declared: declared.clone(),
                                arguments: None,
                                changed_by: None,
                            }),
                            turn,
                            Some(&id),
                        )?;
                        let bound = tool.bound();
                        let stream = Arc::new(Stream::new(Arc::clone(&wake), id.clone()));
                        release.add(Arc::clone(&stream));
                        let thread_stream = Arc::clone(&stream);
                        let call_cancel = Arc::clone(&cancel);
                        let arguments = Arc::new(arguments);
                        let thread_arguments = Arc::clone(&arguments);
                        scope.spawn(move || {
                            let output = tool.run_asking(
                                &thread_arguments,
                                call_cancel.as_ref(),
                                thread_stream.as_ref(),
                                thread_stream.as_ref(),
                            );
                            thread_stream.finish(output);
                        });
                        State::Running {
                            bound,
                            stream,
                            declared,
                            tool: name,
                            arguments,
                        }
                    }
                };
                running.push(Running { id, state });
            }
            loop {
                self.serve_interactions(&asking(&running), turn)?;
                let now = clock.now();
                // Every delta due now, in request order. A held change the
                // interval still covers stays held for the flush in
                // `complete_next` below.
                for call in running.iter() {
                    let State::Running { stream, .. } = &call.state else {
                        continue;
                    };
                    if let Some(delta) = stream.take_due(now) {
                        let bytes = self.write_delta(&delta, turn, &call.id);
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
                        self.write_delta(&delta, turn, &call.id);
                    }
                }
                // The next completion in request order, if its prefix is done.
                // A returned call's flush already went out in the pass above;
                // what `take_finished` still holds is only the safety net,
                // written before its completion all the same.
                if self.complete_next(&mut running, turn)? {
                    continue;
                }
                if running.iter().all(is_done) {
                    return Ok(self.turn_cancelled());
                }
                // Nothing to write: wait until the earliest held change or
                // `until` is due, an emit, a call returning, an ask or a
                // clock move. Each pass either wrote something above or
                // waits here, never spins.
                let calls = asking(&running);
                let earliest = calls
                    .iter()
                    .filter_map(|(_, stream)| stream.deadline())
                    .chain(Self::interaction_deadline(&calls))
                    .min();
                self.wait_step(&wake, &calls, earliest, turn)?;
            }
        })
    }

    /// The tool, arguments and effects of `call`, once it names a tool and
    /// its arguments fit that tool's schema; otherwise the failed
    /// completion it gets (`docs/loop.md`, "Tool calls that do not run").
    pub(crate) fn checked(
        &self,
        call: &ToolCallRequested,
    ) -> Result<Checked, Box<ToolCallCompleted>> {
        let Some((_, tool, definition)) = self.tools.get(&call.name) else {
            let names: Vec<String> = self.tools.keys().map(|n| format!("`{n}`")).collect();
            let exist = if names.is_empty() {
                "You have no tools.".to_owned()
            } else {
                format!("The tools are {}.", names.join(", "))
            };
            return Err(Box::new(failed(
                ErrorCode::UnknownTool,
                format!("No tool is named `{}`. {exist}", call.name),
            )));
        };
        let arguments = match &call.repair {
            Some(repair) => Value::Object(repair.repaired.clone()),
            None => call.arguments.clone(),
        };
        let Value::Object(arguments) = arguments else {
            return Err(Box::new(failed(
                ErrorCode::InvalidArguments,
                "The arguments are not a JSON object. Send them as one.".to_owned(),
            )));
        };
        let errors = schema::check(&definition.input_schema, &Value::Object(arguments.clone()));
        if !errors.is_empty() {
            return Err(Box::new(failed(
                ErrorCode::InvalidArguments,
                format!(
                    "The arguments do not match the tool's schema:\n{}",
                    errors.join("\n")
                ),
            )));
        }
        let effects = match tool.effects(&arguments) {
            Ok(effects) => effects,
            Err(e) => return Err(Box::new(failed(ErrorCode::ToolError, e.to_string()))),
        };
        Ok((Arc::clone(tool), arguments, effects))
    }

    /// Checks `call` and decides whether it runs (`docs/loop.md`, "Tool
    /// calls that do not run", and `docs/permissions.md`, "The order a call
    /// is judged in").
    fn decide(
        &mut self,
        call: &ToolCallRequested,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<Decided, Error> {
        let (tool, arguments, effects) = match self.checked(call) {
            Ok(checked) => checked,
            Err(failed) => return Ok(Err(failed)),
        };
        // The rules are read once for every call, including ones that later
        // fast-path, so a revoked rule applies to the next call judged
        // (`docs/tui.md`, "/rules"). `judge` runs the credential deny before
        // it looks at them, so an unreadable rules file never stops that
        // deny.
        let rules = self.rules.read();
        match super::permission::judge(
            &call.name,
            &effects,
            &rules,
            &self.grants,
            &self.workspace,
            &self.prompt.home,
            &self.credentials,
            &self.credential_files,
        ) {
            super::permission::Verdict::Deny { by, reason, why } => {
                let text = format!("{why} It did not run.");
                self.decided(
                    id,
                    turn,
                    resolved(None, Decision::Deny, by, Some(why), None),
                )?;
                Ok(Err(denied(reason, text)))
            }
            super::permission::Verdict::Ask(rule) => match self.ask(
                id,
                turn,
                &call.name,
                &effects.declared,
                AskStep::StandingAsk {
                    standing_rule: rule,
                },
            )? {
                Asked::Allow => Ok(Ok((tool, arguments, effects.declared))),
                Asked::Deny(completed) | Asked::Gone(completed) => Ok(Err(completed)),
                Asked::Cancelled => Ok(Err(self.cancelled_before_ran())),
                // The idle delay passed. Nothing more is written; `run_calls`
                // sees `idle_left` and the turn unwinds.
                Asked::Idle => Ok(Err(self.cancelled_before_ran())),
                Asked::Closed(request_id) => {
                    Ok(Err(self.unanswerable(id, turn, Some(request_id))?))
                }
            },
            super::permission::Verdict::Allow(decided) => {
                if let Some(by) = decided {
                    self.decided(id, turn, resolved(None, Decision::Allow, by, None, None))?;
                }
                Ok(Ok((tool, arguments, effects.declared)))
            }
            // Step 7 is the reviewer: the call is judged by a separate
            // model (`docs/permissions.md`, "The reviewer").
            super::permission::Verdict::Review => {
                self.review(call, id, turn, tool, arguments, &effects)
            }
        }
    }

    /// Writes a `permission_resolved` line for `id`.
    pub(crate) fn decided(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        resolved: PermissionResolved,
    ) -> Result<(), Error> {
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
        declared: &DeclaredEffects,
        step: AskStep,
    ) -> Result<Asked, Error> {
        if !self.answerable {
            // Step 7 checks `answerable` before asking; only a standing
            // ask reaches this denial.
            return Ok(Asked::Gone(self.unanswerable(id, turn, None)?));
        }
        let request = PermissionRequested {
            request_id: RequestId(super::mint("r_")),
            declared: declared.clone(),
            step,
        };
        self.append(&Event::PermissionRequested(request.clone()), turn, Some(id))?;
        self.await_answer(id, turn, tool, &request)
    }

    /// Denies a call no person can answer (`docs/permissions.md`,
    /// "Headless"): a session started by `fiber ask`, or one `close` has
    /// been taken. `request_id` is set when the request was already raised.
    pub(crate) fn unanswerable(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        request_id: Option<RequestId>,
    ) -> Result<Box<ToolCallCompleted>, Error> {
        let reason = "No person can answer an approval in this session.";
        self.decided(
            id,
            turn,
            resolved(
                request_id,
                Decision::Deny,
                DecidedBy::StandingRule,
                Some(reason.to_owned()),
                None,
            ),
        )?;
        Ok(denied("no_person", format!("{reason} It did not run.")))
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
    /// (`docs/tools.md`, "Bounded results"). Writes the call's job lines and
    /// the hooks' notices under `id` before the caller writes
    /// `tool_call_completed`.
    fn finish(
        &mut self,
        output: Output,
        (tool, arguments): (&str, &Map<String, Value>),
        bound: Bound,
        id: &ActionId,
        turn: &TurnId,
    ) -> Result<ToolCallCompleted, Error> {
        let Output {
            content,
            error,
            process,
            details,
            changes,
            control,
            jobs,
            servers,
        } = output;
        // What the call saw of its server's deaths and restarts, written
        // under the call's action, in order, before its job lines and
        // `tool_call_completed`.
        for record in servers {
            let event = match record {
                contract::tool::ServerRecord::Failed(failed) => Event::McpServerFailed(failed),
                contract::tool::ServerRecord::Ready(ready) => Event::McpServerReady(ready),
            };
            self.append(&event, turn, Some(id))?;
        }
        for record in jobs {
            let event = match record {
                contract::jobs::JobRecord::Started(started) => Event::JobStarted(started),
                contract::jobs::JobRecord::Completed(completed) => Event::JobCompleted(completed),
            };
            self.append(&event, turn, Some(id))?;
        }
        let (text, images): (Vec<ContentPart>, Vec<ContentPart>) = content
            .into_iter()
            .partition(|part| matches!(part, ContentPart::Text { .. }));
        // A running call keeps running until its tool returns, then
        // completes with what the tool gave, `cancelled` instead of
        // `completed`. A tool that returned an error keeps `failed` with
        // that error: those outcomes are never `cancelled`. The hooks see
        // the status the completion carries.
        let status = if error.is_some() {
            CallStatus::Failed
        } else if self.turn_cancelled() {
            CallStatus::Cancelled
        } else {
            CallStatus::Completed
        };
        let ran = super::hooks::Ran {
            tool,
            arguments,
            status,
            text,
            images,
            details,
            process: process.as_ref(),
        };
        let shaped = self.shape(ran, bound, id, turn)?;
        Ok(ToolCallCompleted {
            status,
            error,
            process,
            details: shaped.details,
            changes,
            control,
            artifact: shaped.artifact,
            content: shaped.content,
            changed_by: shaped.changed_by,
            ..completed(String::new(), None)
        })
    }
}

impl Loop {
    /// Writes one due or flushed delta under the call's action, and returns
    /// its encoded bytes for pacing. Errors are ignored: the line is
    /// ephemeral (`docs/architecture.md`, "Streaming").
    fn write_delta(&mut self, delta: &Progress, turn: &TurnId, id: &ActionId) -> u64 {
        let event = Event::ToolCallDelta(delta.clone());
        match self.append(&event, turn, Some(id)) {
            Ok(()) | Err(_) => {}
        }
        encoded(delta)
    }

    /// Writes the next completion in request order, once every earlier call
    /// is done: true after writing one. A later call's completion waits for
    /// its prefix, while its final flush does not (written above).
    fn complete_next(&mut self, running: &mut [Running], turn: &TurnId) -> Result<bool, Error> {
        // The first call whose completion is not written: everything
        // before it is done, so writing this one keeps request order.
        let Some(call) = running.iter_mut().find(|call| !is_done(call)) else {
            return Ok(false);
        };
        let mut completed = match &call.state {
            State::Done => return Ok(false),
            // A decided call the cancel reached before its completion was
            // written completes `cancelled`: a denial or failure decided
            // but not yet written, or a call approved but never started. A
            // `permission_resolved` line already written stays.
            State::Ready(_) if self.turn_cancelled() => self.cancelled_before_ran(),
            State::Ready(completed) => completed.clone(),
            State::Running {
                stream,
                bound,
                tool,
                arguments,
                ..
            } => match stream.take_finished() {
                Some((output, flushed)) => {
                    if let Some(delta) = flushed {
                        self.write_delta(&delta, turn, &call.id);
                    }
                    let call_of = (tool.as_str(), arguments.as_ref());
                    Box::new(self.finish(output, call_of, *bound, &call.id, turn)?)
                }
                None => return Ok(false),
            },
        };
        let declared = match &call.state {
            State::Running { declared, .. } => Some(declared.clone()),
            State::Ready(_) | State::Done => None,
        };
        // An over-budget section a completed call that declared a write
        // touched ends its result with the prune line, before the line is
        // written; the session's own edits below come after it.
        if let Some(declared) = declared.as_ref()
            && completed.status == CallStatus::Completed
        {
            for line in self.changes.prune_lines(&self.workspace, declared) {
                completed.content.push(ContentPart::Text { text: line });
            }
        }
        call.state = State::Done;
        self.append(&Event::ToolCallCompleted(*completed), turn, Some(&call.id))?;
        // The session's own edits and subdirectory files: a call that ran
        // re-reads each declared path that is a tracked instruction file,
        // and queues each new subdirectory file for the next step start.
        // A call that never ran touches nothing, so it checks nothing.
        if let Some(declared) = declared {
            for file in self.changes.call_completed(&self.workspace, &declared) {
                self.append(&Event::InstructionFile(file), turn, Some(&call.id))?;
            }
        }
        Ok(true)
    }
}

/// What waiting for a person's answer came back with. The decision lines
/// are written, except on `Closed`, which wrote nothing.
pub(crate) enum Asked {
    /// The person allowed the call.
    Allow,
    /// The person refused the call.
    Deny(Box<ToolCallCompleted>),
    /// The inbox closed with no answer.
    Gone(Box<ToolCallCompleted>),
    /// `close` was taken while waiting; the caller denies as its step does.
    Closed(RequestId),
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
