//! Inner calls run inside their anchor on the loop's step machinery.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};

use contract::ActionId;
use contract::clock::Wake;
use contract::events::{CallStatus, ToolCallCompleted};
use contract::tool::{Cancel, InnerCalls};
use serde_json::Value;
use support::lock;

use crate::TurnCancel;
use crate::progress::{SharedWake, Stream};

pub(crate) const MAX_INNER_DEPTH: u8 = 8;

/// One response shared by the requester, the loop and the closing slot.
#[derive(Default)]
struct Answer {
    completed: Mutex<Option<Box<ToolCallCompleted>>>,
    changed: Condvar,
}

impl Answer {
    fn resolve(&self, completed: Box<ToolCallCompleted>) {
        let mut answer = lock(&self.completed);
        answer.get_or_insert(completed);
        self.changed.notify_all();
    }

    fn wait(&self) -> Box<ToolCallCompleted> {
        let mut answer = lock(&self.completed);
        loop {
            if let Some(completed) = answer.take() {
                return completed;
            }
            answer = self
                .changed
                .wait(answer)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A request not yet taken by the loop thread.
pub(crate) struct Raised {
    pub(crate) extension: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
    answer: Box<dyn FnOnce(Box<ToolCallCompleted>) + Send>,
}

impl Raised {
    pub(crate) fn new(
        extension: String,
        name: String,
        arguments: Value,
        answer: Box<dyn FnOnce(Box<ToolCallCompleted>) + Send>,
    ) -> Self {
        Self {
            extension,
            name,
            arguments,
            answer,
        }
    }

    pub(crate) fn answer(self, completed: Box<ToolCallCompleted>) {
        (self.answer)(completed);
    }
}

#[derive(Default)]
struct Requests {
    raised: Vec<Raised>,
    waiting: BTreeMap<usize, Arc<Answer>>,
    closed: bool,
}

/// The runner for a stream's inner calls. Raising and closing share one lock.
pub(crate) struct InnerSlot {
    pub(crate) action: ActionId,
    tool: String,
    pub(crate) depth: u8,
    wake: Arc<SharedWake>,
    requests: Mutex<Requests>,
}

impl InnerSlot {
    pub(crate) fn new(wake: Arc<SharedWake>, action: ActionId, tool: String, depth: u8) -> Self {
        Self {
            action,
            tool,
            depth,
            wake,
            requests: Mutex::default(),
        }
    }

    pub(crate) fn take_raised(&self) -> Vec<Raised> {
        std::mem::take(&mut lock(&self.requests).raised)
    }

    pub(crate) fn close(&self) {
        let mut requests = lock(&self.requests);
        requests.closed = true;
        requests.raised.clear();
        for answer in std::mem::take(&mut requests.waiting).into_values() {
            answer.resolve(cancelled());
        }
    }
}

impl InnerCalls for InnerSlot {
    fn run(
        &self,
        extension: &str,
        name: &str,
        arguments: Value,
    ) -> Result<Box<ToolCallCompleted>, String> {
        if name == self.tool {
            return Err(format!("host.tool: the tool `{name}` cannot run itself"));
        }
        if self.depth >= MAX_INNER_DEPTH {
            return Err(format!(
                "host.tool: inner-call depth exceeds {MAX_INNER_DEPTH}"
            ));
        }
        let answer = Arc::new(Answer::default());
        {
            let mut requests = lock(&self.requests);
            if requests.closed {
                return Ok(cancelled());
            }
            // An answer's address is unique while this map holds its Arc.
            requests
                .waiting
                .insert(Arc::as_ptr(&answer).addr(), Arc::clone(&answer));
            let reply = Arc::clone(&answer);
            requests.raised.push(Raised::new(
                extension.to_owned(),
                name.to_owned(),
                arguments,
                Box::new(move |completed| reply.resolve(completed)),
            ));
        }
        self.wake.wake();
        let completed = answer.wait();
        lock(&self.requests)
            .waiting
            .remove(&Arc::as_ptr(&answer).addr());
        Ok(completed)
    }
}

fn cancelled() -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Cancelled,
        ..crate::completion::completed(String::new(), None)
    })
}

#[derive(Default)]
struct Signal {
    inner: Mutex<(bool, Vec<Weak<dyn Wake>>)>,
}

impl Signal {
    fn cancel(&self) {
        let wakers = {
            let mut inner = lock(&self.inner);
            inner.0 = true;
            std::mem::take(&mut inner.1)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
    }
}

impl Wake for Signal {
    fn wake(&self) {
        self.cancel();
    }
}

/// Cancelled by the turn or by its anchor ending, without cancelling the anchor.
pub(crate) struct InnerCancel {
    turn: Arc<TurnCancel>,
    signal: Arc<Signal>,
}

impl InnerCancel {
    pub(crate) fn new(turn: Arc<TurnCancel>) -> Self {
        let signal = Arc::new(Signal::default());
        let relay: Arc<dyn Wake> = signal.clone();
        turn.subscribe(Arc::downgrade(&relay));
        if turn.is_cancelled() {
            signal.cancel();
        }
        Self { turn, signal }
    }

    pub(crate) fn cancel(&self) {
        self.signal.cancel();
    }

    /// Follows a command's end, including one that already happened.
    pub(crate) fn follow(&self, source: &dyn Cancel) {
        let relay: Arc<dyn Wake> = self.signal.clone();
        source.subscribe(Arc::downgrade(&relay));
        if source.is_cancelled() {
            self.signal.cancel();
        }
    }
}

impl Cancel for InnerCancel {
    fn is_cancelled(&self) -> bool {
        lock(&self.signal.inner).0 || self.turn.is_cancelled()
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut inner = lock(&self.signal.inner);
        // The turn can be cancelled before its relay reaches this signal.
        // A subscription after that cancel must not replay it.
        if !inner.0 && !self.turn.is_cancelled() {
            inner.1.push(waker);
        }
    }
}

/// A started inner call, held until its own inner calls have completed.
pub(crate) struct InnerRunning {
    pub(crate) id: ActionId,
    anchor: Option<ActionId>,
    request: Raised,
    pub(crate) stream: Arc<Stream>,
    cancel: Arc<InnerCancel>,
    bound: contract::tool::Bound,
    declared: contract::shapes::DeclaredEffects,
    arguments: Arc<serde_json::Map<String, Value>>,
    rewritten: Option<Vec<String>>,
}

/// The anchor and cancellation of a request taken by the loop.
struct Origin<'a> {
    ran_by: contract::events::Anchor,
    parent: Option<ActionId>,
    depth: u8,
    // Preparation can wait on a hook while the anchor returns.
    stopped: &'a dyn Fn() -> bool,
    cancel: Arc<InnerCancel>,
}

impl crate::Loop {
    /// Runs a command's request alone, including its chain, with no turn.
    pub(crate) fn run_alone(
        &mut self,
        request: contract::inbox::InnerCallRequest,
    ) -> Result<(), crate::Error> {
        if request.cancel.is_cancelled() || self.shutting_down() {
            return Ok(());
        }
        let contract::inbox::InnerCallRequest {
            extension,
            command,
            name,
            arguments,
            cancel: request_cancel,
            taken,
            answer,
        } = request;
        taken();
        let cancel = Arc::new(InnerCancel::new(Arc::clone(&self.cancel)));
        cancel.follow(request_cancel.as_ref());
        let request = Raised::new(extension, name, arguments, answer);
        let clock = Arc::clone(self.log.clock());
        std::thread::scope(|scope| {
            let wake = Arc::new(SharedWake::default());
            let relay: Arc<dyn Wake> = wake.clone();
            cancel.subscribe(Arc::downgrade(&relay));
            clock.subscribe(Arc::downgrade(&relay));
            let mut inner = Vec::new();
            // Cancel workers and close their slots before the scope joins,
            // including on a log error or a failure during preparation.
            let mut release = crate::asking::Release::default();
            let origin = Origin {
                ran_by: contract::events::Anchor::Command(command),
                parent: None,
                depth: 1,
                stopped: &|| request_cancel.is_cancelled(),
                cancel,
            };
            self.start_inner(
                scope,
                request,
                origin,
                (&mut inner, &mut release),
                &wake,
                None,
            )?;
            loop {
                let streams: Vec<_> = inner
                    .iter()
                    .map(|call| (call.id.clone(), Arc::clone(&call.stream)))
                    .collect();
                let streams: Vec<_> = streams
                    .iter()
                    .map(|(id, stream)| (id, stream.as_ref()))
                    .collect();
                self.take_inner(scope, &streams, &mut inner, &mut release, &wake, None)?;
                let wrote = self.serve_inner(&mut inner, &|_| false, None)?;
                let calls: Vec<_> = inner
                    .iter()
                    .map(|call| (&call.id, Some(call.stream.as_ref())))
                    .collect();
                self.serve_interactions(&calls, None)?;
                if inner.is_empty() {
                    return Ok(());
                }
                if wrote {
                    continue;
                }
                let earliest = calls
                    .iter()
                    .filter_map(|(_, stream)| stream.and_then(Stream::deadline))
                    .chain(Self::interaction_deadline(&calls))
                    .min();
                wake.park(clock.as_ref(), earliest);
            }
        })
    }

    /// Takes requests on the loop thread, using preparation and only the deny steps.
    pub(crate) fn take_inner<'scope, 'env>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        streams: &[(&ActionId, &Stream)],
        inner: &mut Vec<InnerRunning>,
        release: &mut crate::asking::Release,
        wake: &Arc<SharedWake>,
        turn: Option<&contract::TurnId>,
    ) -> Result<(), crate::Error> {
        for (_, stream) in streams {
            let Some(slot) = stream.inner_slot() else {
                continue;
            };
            for request in slot.take_raised() {
                let origin = Origin {
                    ran_by: contract::events::Anchor::OuterCall(slot.action.clone()),
                    parent: Some(slot.action.clone()),
                    depth: slot.depth + 1,
                    stopped: &|| stream.returned(),
                    cancel: Arc::new(InnerCancel::new(Arc::clone(&self.cancel))),
                };
                self.start_inner(scope, request, origin, (inner, release), wake, turn)?;
            }
        }
        Ok(())
    }

    /// Prepares and starts one inner call, using only the deny steps.
    fn start_inner<'scope, 'env>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        request: Raised,
        origin: Origin<'_>,
        (inner, release): (&mut Vec<InnerRunning>, &mut crate::asking::Release),
        wake: &Arc<SharedWake>,
        turn: Option<&contract::TurnId>,
    ) -> Result<(), crate::Error> {
        use contract::events::{Decision, Event, RanBy, ToolCallRequested, ToolCallStarted};
        let id = ActionId(crate::mint("a_"));
        let call = self.repaired(ToolCallRequested {
            name: request.name.clone(),
            arguments: request.arguments.clone(),
            provider_id: None,
            repair: None,
            provider_item: None,
            ran_by: Some(RanBy {
                extension: request.extension.clone(),
                anchor: origin.ran_by,
            }),
        });
        self.write(&Event::ToolCallRequested(call.clone()), turn, Some(&id))?;
        let (decision, state) = if self.turn_cancelled() || (origin.stopped)() {
            (Err(self.cancelled_before_ran()), Vec::new())
        } else {
            self.prepared(&call, turn)?
        };
        let prepared = match decision {
            Err(mut done) => {
                self.after_completed(None, &mut done, &id, turn, Vec::new())?;
                request.answer(done);
                return Ok(());
            }
            Ok(prepared) => prepared,
        };
        // The inner permission order starts once the call is prepared:
        // the guard spans the deny steps and closes before any start
        // line. `call.name` is the session map's key, refused above when
        // it named nothing registered.
        let verdict = {
            let _permission = self
                .diag
                .timed_tool(contract::diag::Op::ToolPermission, &call.name);
            crate::permission::inner_verdict(
                &call.name,
                &prepared.effects,
                &self.rules.read(),
                &self.workspace,
                &self.credentials,
                &self.credential_files,
            )
        };
        if let crate::permission::Verdict::Deny { by, reason, why } = verdict {
            let message = format!("{why} It did not run.");
            self.write(
                &Event::PermissionResolved(crate::completion::resolved(
                    None,
                    Decision::Deny,
                    by,
                    Some(why),
                    None,
                )),
                turn,
                Some(&id),
            )?;
            let mut done = crate::completion::denied(reason, message);
            self.after_completed(None, &mut done, &id, turn, Vec::new())?;
            request.answer(done);
            return Ok(());
        }
        if self.turn_cancelled() || (origin.stopped)() {
            let mut done = self.cancelled_before_ran();
            self.after_completed(None, &mut done, &id, turn, Vec::new())?;
            request.answer(done);
            return Ok(());
        }
        let (arguments, changed_by) = prepared.rewrite();
        self.write_carried(state, turn)?;
        self.write(
            &Event::ToolCallStarted(ToolCallStarted {
                always_reviewed: prepared.effects.always_reviewed,
                declared: prepared.effects.declared.clone(),
                arguments,
                changed_by,
            }),
            turn,
            Some(&id),
        )?;
        let cancel = origin.cancel;
        let stream = Arc::new(Stream::new(Arc::clone(wake), id.clone(), false).with_inner(
            Arc::new(InnerSlot::new(
                Arc::clone(wake),
                id.clone(),
                call.name.clone(),
                origin.depth,
            )),
        ));
        // As `run_batch` records each model call's turn.
        if let Some(turn) = turn {
            self.call_turns.insert(id.clone(), turn.clone());
        }
        release.add_cancel(Arc::clone(&cancel));
        release.add(Arc::clone(&stream));
        let arguments = Arc::new(prepared.arguments);
        let worker_stream = Arc::clone(&stream);
        let worker_cancel = Arc::clone(&cancel);
        let worker_arguments = Arc::clone(&arguments);
        // The exact prepared effects ride into the inner run, as they do
        // into an outer one.
        let effects = Arc::new(prepared.effects);
        let worker_effects = Arc::clone(&effects);
        let tool = prepared.tool;
        let bound = tool.bound();
        // The entered inner run's worker context, gated like the outer
        // call's, so `info` clones nothing.
        let worker = self.diag.worker().map(|diag| (diag, call.name.clone()));
        scope.spawn(move || {
            // The entered inner run's timer, before the stream takes the
            // output, as the outer call's is.
            let output = {
                let _run = match worker.as_ref() {
                    Some((diag, name)) => diag.timed_tool(contract::diag::Op::ToolRun, name),
                    None => None,
                };
                tool.run_with_effects(
                    &worker_arguments,
                    &worker_effects,
                    worker_cancel.as_ref(),
                    worker_stream.as_ref(),
                    worker_stream.as_ref(),
                )
            };
            worker_stream.finish(output);
        });
        inner.push(InnerRunning {
            id,
            anchor: origin.parent,
            request,
            stream,
            cancel,
            bound,
            declared: effects.declared.clone(),
            arguments,
            rewritten: prepared.rewritten,
        });
        Ok(())
    }

    /// Serves all inner calls in completion order, holding each anchor until its
    /// inner calls end. An anchor returning cancels its still-running calls.
    pub(crate) fn serve_inner(
        &mut self,
        inner: &mut Vec<InnerRunning>,
        finished: &dyn Fn(&ActionId) -> bool,
        turn: Option<&contract::TurnId>,
    ) -> Result<bool, crate::Error> {
        let now = self.log.clock().now();
        let mut wrote = false;
        for call in inner.iter() {
            let anchor_stopped = call.anchor.as_ref().is_some_and(|id| {
                match inner.iter().find(|anchor| &anchor.id == id) {
                    Some(anchor) => anchor.stream.returned() || anchor.cancel.is_cancelled(),
                    None => finished(id),
                }
            });
            if anchor_stopped {
                call.cancel.cancel();
            }
            if let Some(delta) = call.stream.take_due(now) {
                let bytes = self.write_delta(&delta, turn, &call.id);
                call.stream.wrote(bytes, self.log.clock().now());
                wrote = true;
            }
            if let Some(delta) = call.stream.take_flush() {
                self.write_delta(&delta, turn, &call.id);
                wrote = true;
            }
        }
        // Remove only leaves: their parent remains until its completion is written.
        while let Some((index, (output, flushed))) =
            inner.iter().enumerate().find_map(|(index, call)| {
                if Self::inner_anchored(inner, &call.id) {
                    return None;
                }
                call.stream.take_finished().map(|output| (index, output))
            })
        {
            let call = inner.remove(index);
            if let Some(slot) = call.stream.inner_slot() {
                slot.close();
            }
            if let Some(delta) = flushed {
                self.write_delta(&delta, turn, &call.id);
            }
            let (mut done, state) = self.finish(
                output,
                (&call.request.name, &call.arguments),
                call.rewritten.as_deref(),
                call.bound,
                call.cancel.is_cancelled(),
                &call.id,
                turn,
            )?;
            self.after_completed(Some(&call.declared), &mut done, &call.id, turn, state)?;
            call.request.answer(Box::new(done));
            wrote = true;
        }
        Ok(wrote)
    }

    pub(crate) fn inner_anchored(inner: &[InnerRunning], action: &ActionId) -> bool {
        inner
            .iter()
            .any(|call| call.anchor.as_ref() == Some(action))
    }
}

#[cfg(test)]
#[path = "inner_calls_tests.rs"]
mod tests;
